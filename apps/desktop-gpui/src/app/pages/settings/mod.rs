//! The Settings modal (`apps/web/src/routes/_chat.settings.tsx`): an `AppRouteModal` over the
//! window with `SettingsSidebarNav`, a breadcrumb header and one section at a time. Rows and
//! the scope layer are in `widgets.rs`.

pub(in crate::app) mod agents;
mod chat;
pub(in crate::app) mod code;
mod connections;
pub(in crate::app) use connections::check_filesystem_read;
pub(in crate::app) use connections::check_scope;
pub(in crate::app) use connections::logic::{machine_icon, one_password_account};
pub(in crate::app) use connections::operates_terminals;
pub(in crate::app) use connections::{SessionLoad, check_session};
mod desktop_rows;
mod diagnostics;
mod diffs;
mod fields;
mod general;
pub(in crate::app) mod keybindings;
mod licenses;
mod model_select;

mod providers;
mod row_layout;
pub(in crate::app) use providers::{chatgpt_usage_button, redacted_email, uses_chatgpt_sharing};
pub(in crate::app) use widgets::device_only;
mod search;
mod snap_shot;
mod star_prompt;
mod system;
pub(in crate::app) mod theme_import;
pub(in crate::app) mod theme_library;
mod typography;
pub(in crate::app) mod widgets;

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::base::FocusTrapElement as _;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::slider::SliderState;
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, BoxShadow, Context, Entity, FocusHandle, Focusable as _, FontWeight,
    InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Pixels, ScrollHandle, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, px,
};
use serde_json::{Map, Value};

use crate::app::Workspace;
use crate::settings::scope::{ScopeKind, ScopeSearch};
use crate::settings::{SearchItem, TYPOGRAPHY_ADVANCED_KEY, ThemePreference, contract};
use crate::theme::{self, Tokens};
use crate::themes::ThemeChoice;
use widgets::SettingsView;

/// A sidebar group: its heading, then each section's key and label.
pub(in crate::app) type Group = (Option<&'static str>, &'static [(&'static str, &'static str)]);

/// The sections in sidebar order (`SETTINGS_NAV_GROUPS`, `SETTINGS_SECTION_LABELS` in
/// `apps/web/src/components/settings/settingsSearch.ts`). A `None` heading is set apart by space alone.
pub(in crate::app) const SETTINGS: &[Group] = &[
    (
        None,
        &[
            ("general", "General"),
            ("appearance", "Appearance"),
            ("chat", "Chat"),
            ("diffs", "Diffs & pull requests"),
            ("keybindings", "Keybindings"),
            ("snap-shot", "SnapShots"),
        ],
    ),
    (
        Some("Agents"),
        &[
            ("providers", "Providers"),
            ("threads", "Threads"),
            ("browser", "Browser"),
            ("devices", "Devices"),
            ("scheduled-tasks", "Scheduled tasks"),
        ],
    ),
    (Some("Code"), &[("projects", "Projects"), ("source-control", "Source control"), ("worktrees", "Worktrees")]),
    (Some("Connections"), &[("connections", "Environments"), ("access", "Sharing & access"), ("secrets", "Secrets")]),
    (None, &[("maintenance", "Maintenance"), ("labs", "Labs"), ("about", "About"), ("archived", "Archive")]),
];

/// The socket of `scope`, the settings environment a view or dialog was made for: it keeps
/// acting there after Settings moves to another environment.
pub(in crate::app) fn scoped_client(
    workspace: &gpui_kit::WeakEntity<Workspace>,
    scope: Option<&SharedString>,
    cx: &gpui_kit::App,
) -> Option<crate::rpc::RpcClient> {
    workspace.upgrade()?.read(cx).env_client(scope?)
}

/// `isSecretsFeatureEnabled`: the web's build flag `T3CODE_FEATURE_SECRETS`, read here from
/// the environment. Off, the Secrets section and its search items do not exist.
pub(in crate::app) fn secrets_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        let flag = std::env::var("T3CODE_FEATURE_SECRETS").unwrap_or_default();
        matches!(flag.trim().to_lowercase().as_str(), "1" | "true")
    })
}

/// Whether a section exists in this build (`SETTINGS_NAV_GROUPS` drops Secrets without the flag).
pub(in crate::app) fn section_enabled(id: &str) -> bool {
    id != "secrets" || secrets_enabled()
}

/// Pages without a nav entry: id, breadcrumb label, and the section highlighted for them
/// (`SETTINGS_DETAIL_PAGE_PARENTS`).
const DETAIL_PAGES: &[(&str, &str, &str)] =
    &[("diagnostics", "Diagnostics", "maintenance"), ("open-source-licenses", "Open source licenses", "about")];

/// The shipped settings in enabled sections, each with its section's label.
pub(super) fn searchable_settings() -> impl Iterator<Item = (&'static SearchItem, &'static str)> {
    search::shipped()
        .iter()
        .filter(|(item, _)| section_enabled(&item.to))
        .filter_map(|(item, _)| Some((*item, section_label(&item.to)?)))
}

/// The section's label (`SETTINGS_SECTION_LABELS`).
pub(super) fn section_label(id: &str) -> Option<&'static str> {
    SETTINGS
        .iter()
        .flat_map(|(_, items)| items.iter())
        .find(|(item, _)| *item == id)
        .map(|(_, label)| *label)
        .or_else(|| DETAIL_PAGES.iter().find(|(page, ..)| *page == id).map(|(_, label, _)| *label))
}

/// The nav entry a page highlights.
/// The nav's sections in order.
fn nav_sections() -> Vec<&'static str> {
    SETTINGS.iter().flat_map(|(_, items)| items.iter().map(|(id, _)| *id)).filter(|id| section_enabled(id)).collect()
}

/// Where an arrow key moves keyboard focus among the nav's `sections` from `focused`
/// (`useSettingsSectionNavigation`): Up and Down by one and Home and End to the ends, stopping
/// at the ends. Every section is a root leaf, so Left and Right go nowhere.
fn section_nav_step<'a>(key: &str, sections: &[&'a str], focused: &str) -> Option<&'a str> {
    let index = sections.iter().position(|id| *id == focused)?;
    let next = match key {
        "down" => (index + 1).min(sections.len() - 1),
        "up" => index.saturating_sub(1),
        "home" => 0,
        "end" => sections.len() - 1,
        _ => return None,
    };
    Some(sections[next])
}

/// The vertical scroll offset that brings a row spanning `row` (top, bottom) into a viewport
/// spanning `viewport` with the least movement (`scrollIntoView({ block: "nearest" })`).
fn nearest_scroll_offset(offset: Pixels, viewport: (Pixels, Pixels), row: (Pixels, Pixels)) -> Pixels {
    if row.0 < viewport.0 {
        (offset + viewport.0 - row.0).min(px(0.))
    } else if row.1 > viewport.1 {
        offset - (row.1 - viewport.1)
    } else {
        offset
    }
}

/// Scrolls the nav row `id` into view on its first paint after the arrow keys move to it.
fn reveal_nav_row(id: &'static str, pending: Rc<Cell<Option<&'static str>>>, scroll: ScrollHandle) -> impl IntoElement {
    gpui_kit::canvas(
        move |bounds, window, _| {
            if pending.take() != Some(id) {
                return;
            }
            let viewport = scroll.bounds();
            let offset = scroll.offset();
            let y =
                nearest_scroll_offset(offset.y, (viewport.top(), viewport.bottom()), (bounds.top(), bounds.bottom()));
            if y != offset.y {
                scroll.set_offset(gpui_kit::point(offset.x, y));
                window.request_animation_frame();
            }
        },
        |_, (), _, _| {},
    )
    .absolute()
    .size_full()
}

fn nav_section(id: &str) -> &str {
    DETAIL_PAGES.iter().find(|(page, ..)| *page == id).map_or(id, |(.., parent)| parent)
}

/// `SETTINGS_SECTION_ICONS`; SnapShots is the web's own `SnapShotIcon` (a window in a capture frame).
fn section_icon(id: &str) -> Icon {
    let name = match id {
        "general" => IconName::Settings2,
        "appearance" => IconName::Palette,
        "chat" => IconName::MessageSquareText,
        "diffs" => IconName::FileDiff,
        "keybindings" => IconName::Keyboard,
        "snap-shot" => return Icon::empty().path("icons/app/snap-shot.svg"),
        "providers" => IconName::Bot,
        "threads" => IconName::MessagesSquare,
        "browser" => IconName::Globe,
        "devices" => IconName::Smartphone,
        "scheduled-tasks" => IconName::CalendarClock,
        "projects" => IconName::PanelsTopLeft,
        "source-control" => IconName::GitBranch,
        "worktrees" => IconName::FolderTree,
        "connections" => IconName::Server,
        "access" => IconName::ShieldCheck,
        "secrets" => IconName::KeyRound,
        "maintenance" => IconName::Wrench,
        "labs" => IconName::FlaskConical,
        "about" => IconName::Info,
        "archived" => IconName::Archive,
        _ => IconName::Settings2,
    };
    Icon::new(name)
}

/// The modal's own state, kept while it is closed so the scope and folds persist.
pub(in crate::app) struct SettingsUi {
    /// The scope the sentence picked, kept across sections (`retainSettingsScope`).
    pub(in crate::app) scope: ScopeSearch,
    /// The providers card is under 48rem, so its list sits above the editor (`@container/providers`).
    pub(in crate::app) providers_narrow: bool,
    /// The provider editor is too narrow for its rows' controls to sit beside their titles.
    pub(in crate::app) provider_rows_stacked: bool,
    /// The environment and provider instance a navigation asked Providers to open
    /// (`/settings/providers?environmentId&instanceId`), until the page takes it.
    pub(in crate::app) provider_target: Option<(SharedString, String)>,
    /// The environment and scheduled task a link asked Scheduled tasks to open
    /// (`/settings/scheduled-tasks?environmentId&taskId`), until its editor closes.
    pub(in crate::app) scheduled_task_link: Option<(SharedString, String)>,
    /// The environment Sharing & access was switched to ("Showing access for"); the primary
    /// while it is `None` or no longer listed.
    pub(in crate::app) access_environment: Option<SharedString>,
    search: Entity<InputState>,
    /// Below `md` the nav is an icon rail; searching expands it over the page (`compactSearchOpen`).
    compact_search_open: bool,
    active_result: usize,
    /// The row a search result jumped to, kept centered while the page settles.
    target: Option<SharedString>,
    /// Counts search jumps, so each one replays the target row's pulse.
    jumps: u64,
    scroll: ScrollHandle,
    /// The frames a search jump still re-centers its target row for: a newly shown page's rows
    /// pick stacked or side by side one frame late (`RowLayout`), which moves the row.
    scroll_pending: Rc<Cell<u8>>,
    sliders: HashMap<&'static str, (Entity<SliderState>, Subscription)>,
    /// The row whose inheritance popover is open; its marker stays visible meanwhile.
    open_marker: Option<SharedString>,
    /// Open "Advanced" folds, by section id.
    advanced_open: HashSet<&'static str>,
    /// The Typography rows' family pickers, by setting key.
    font_pickers: HashMap<&'static str, typography::FontPicker>,
    project_picker: Option<widgets::ScopeProjectPicker>,
    /// `GitHubStarPrompt`'s once-per-session guard.
    star_prompt_shown: bool,
    /// The grouping mode the Project grouping switch turns back on (`readLastEnabledProjectGroupingMode`).
    last_grouping_mode: &'static str,
    theme_library: theme_library::LibraryUi,
    /// The Appearance code preview's diff, made on first show.
    diff_preview: Option<Entity<gpui_kit::component::diff::DiffState>>,
    /// The modal surface, focused on open like the web's popup so Escape and `/` reach it.
    focus: FocusHandle,
    focus_on_render: bool,
    /// The nav's section rows, one focus handle each.
    nav_focus: HashMap<&'static str, FocusHandle>,
    /// The section the arrow keys last moved to and the page that was open then: the nav's one
    /// Tab stop while that page stays open (`useSettingsSectionNavigation`).
    nav_tab_stop: Option<(&'static str, SharedString)>,
    nav_scroll: ScrollHandle,
    /// The row the arrow keys just moved to, scrolled into view on its next paint.
    nav_reveal: Rc<Cell<Option<&'static str>>>,
    /// The last focused field's pending commit, run before Settings closes.
    focused_field: Option<fields::FieldFlush>,
    /// Each connected environment's session check, for its `settings:write` grant.
    grants: HashMap<String, (gpui_kit::Task<()>, connections::SessionLoad)>,
    /// Each environment's text generator model lists, shared by the pickers that offer them.
    text_generators: providers::TextGeneratorCatalog,
    _search_subscription: Subscription,
}

impl SettingsUi {
    pub(in crate::app) fn new(window: &mut Window, cx: &mut Context<Workspace>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search"));
        let subscription = cx.subscribe_in(&search, window, |this, _, event: &InputEvent, window, cx| match event {
            InputEvent::Change => {
                this.settings_ui.active_result = 0;
                cx.notify();
            }
            InputEvent::PressEnter { .. } => {
                if let Some(item) = this.settings_results(cx).get(this.settings_ui.active_result).copied() {
                    this.open_settings_search_result(item, window, cx);
                }
            }
            InputEvent::Focus => {
                this.settings_ui.compact_search_open = true;
                cx.notify();
            }
            InputEvent::Blur => {
                if this.settings_ui.search.read(cx).value().trim().is_empty() {
                    this.settings_ui.compact_search_open = false;
                    cx.notify();
                }
            }
        });
        Self {
            scope: ScopeSearch::default(),
            provider_target: None,
            access_environment: None,
            scheduled_task_link: None,
            search,
            compact_search_open: false,
            active_result: 0,
            target: None,
            jumps: 0,
            scroll: ScrollHandle::new(),
            scroll_pending: Rc::default(),
            sliders: HashMap::new(),
            open_marker: None,
            advanced_open: HashSet::new(),
            font_pickers: HashMap::new(),
            star_prompt_shown: false,
            project_picker: None,
            last_grouping_mode: "repository",
            theme_library: Default::default(),
            providers_narrow: false,
            provider_rows_stacked: false,
            diff_preview: None,
            focus: cx.focus_handle(),
            focus_on_render: false,
            nav_focus: HashMap::new(),
            nav_tab_stop: None,
            nav_scroll: ScrollHandle::new(),
            nav_reveal: Rc::default(),
            focused_field: None,
            grants: HashMap::new(),
            text_generators: providers::TextGeneratorCatalog::default(),
            _search_subscription: subscription,
        }
    }
}

/// Unified keys "Restore device defaults" resets, labelled for its confirmation as
/// `useSettingsRestore` lists them. A label shows when any of its keys differs from the default.
const RESTORE: &[(&str, &[&str])] = &[
    ("Contrast", &["appearanceContrast"]),
    ("Glass opacity", &["glassOpacity"]),
    ("Diff colors", &["diffColorScheme"]),
    ("Chat width", &["chatWidth"]),
    ("Effort display", &["effortDisplay"]),
    ("Render math", &["mathRenderingEnabled"]),
    ("Panel animations", &["panelAnimationDurationMs"]),
    ("Environment identification", &["environmentIdentificationMode"]),
    ("Time format", &["timestampFormat"]),
    ("Thread notifications", &["notificationMode"]),
    ("In-app notifications", &["inAppNotificationsEnabled"]),
    ("Project Grouping", &["sidebarProjectGroupingMode"]),
    ("Project order", &["sidebarProjectSortOrder"]),
    ("Working section", &["sidebarWorkingShelfEnabled"]),
    ("Auto-settle inactive threads", &["sidebarAutoSettleAfterDays"]),
    ("Keep threads with a PR", &["sidebarAutoSettleKeepPullRequests"]),
    ("Auto-settle merged threads", &["sidebarAutoSettleOnMerge"]),
    ("Resume after usage limits", &["autoResumeLimitedThreads"]),
    ("Snooze until limits reset", &["snoozeLimitedThreads"]),
    ("Snooze options", &["snoozePresets"]),
    ("Word wrap", &["wordWrap"]),
    ("Composer context", &["persistComposerContextStrip"]),
    ("Interface font", &["fontFamilySans", "fontSizeInterface"]),
    ("Prompt font", &["fontFamilyComposer", "fontSizePrompt"]),
    ("Code font", &["fontFamilyCode", "fontSizeCode"]),
    ("Terminal font", &["fontFamilyTerminal", "fontSizeTerminal"]),
    ("Default diff file state", &["diffFilesCollapsed"]),
    ("Diff whitespace changes", &["diffIgnoreWhitespace"]),
    ("Diff layout", &["diffLayout"]),
    ("Open pull requests and changes panels proactively", &["proactivePanelsEnabled"]),
    ("Show skills in slash menu", &["showSkillsInSlashMenu"]),
    ("Rich text composer", &["composerRichTextEnabled"]),
    ("Send shortcut", &["sendShortcut"]),
    ("Follow-up behavior", &["followUpBehavior"]),
    ("Tool calls in chat", &["toolCallVisibility"]),
    ("Response streaming", &["responseStreamingMode"]),
    ("Provider update checks", &["enableProviderUpdateChecks"]),
    ("Continue threads after restarts", &["continueThreadsAfterServerUpdate"]),
    (
        "Background activity",
        &[
            "backgroundActivity",
            "backgroundActivityProfile",
            "automaticGitFetchInterval",
            "providerHealthRefreshInterval",
        ],
    ),
    ("New thread mode", &["defaultThreadEnvMode"]),
    ("New worktrees start from remote", &["newWorktreesStartFromOrigin"]),
    ("Add project base directory", &["addProjectBaseDirectory"]),
    ("Clone into host folders", &["ghqCloneLayout"]),
    ("Unpin confirmation", &["confirmThreadUnpin"]),
    ("Archive confirmation", &["confirmThreadArchive"]),
    ("Delete confirmation", &["confirmThreadDelete"]),
    ("Quit shortcut", &["confirmQuit"]),
    ("Text generation model", &["textGenerationModelSelection"]),
    ("Thread title instructions", &["threadTitleInstructions"]),
    ("Browser viewport", &["browserDefaultViewport"]),
    ("Browser zoom", &["browserDefaultZoomFactor"]),
    ("Browser appearance", &["browserDefaultAppearance"]),
    ("Recording frame rate", &["browserRecordingFrameRate"]),
    ("Recording key presses", &["browserRecordingShowKeyPresses"]),
    ("Recording mouse presses", &["browserRecordingShowMousePresses"]),
    ("Open links in", &["browserLinkTarget"]),
    ("Search engine", &["browserSearchEngine", "browserCustomSearchUrl"]),
    ("Floating preview", &["browserAutoShowFloatingPreview"]),
    ("Open pull requests in T3 Code", &["pullRequestLinksOpenInApp"]),
    ("Agent browser access", &["enableAgentBrowserAccess"]),
];

/// One HTTP client for the settings pages, so requests share its connection pool. Use it
/// only on `crate::runtime()`, where its connections live.
fn http() -> &'static reqwest::Client {
    static CLIENT: std::sync::LazyLock<reqwest::Client> = std::sync::LazyLock::new(reqwest::Client::new);
    &CLIENT
}

/// `formatRelativeTimeLabel` for a past time: "just now", "5m ago", "3h ago", "2d ago".
fn ago_label(elapsed_ms: i64) -> String {
    match crate::inbox::format_relative(elapsed_ms) {
        now if now == "now" => "just now".into(),
        label => format!("{label} ago"),
    }
}

fn restorable(key: &str) -> bool {
    let contract = contract();
    contract.server_defaults.contains_key(key) || contract.client_defaults.contains_key(key)
}

/// The restore labels whose settings differ from their defaults.
fn restore_changes(is_default: impl Fn(&str) -> bool, theme: &ThemeChoice) -> Vec<&'static str> {
    let halves = &theme.halves;
    let theme_labels = [
        (theme.base.is_some(), "Theme"),
        (theme.mode != ThemePreference::System, "Follow system"),
        (halves.light.is_some() || halves.dark.is_some(), "Theme mix"),
    ];
    let changed_keys = RESTORE
        .iter()
        .filter(|(_, keys)| keys.iter().any(|key| restorable(key) && !is_default(key)))
        .map(|(label, _)| *label);
    theme_labels.into_iter().filter(|(changed, _)| *changed).map(|(_, label)| label).chain(changed_keys).collect()
}

/// The patch that resets every restorable key the contract knows.
fn restore_patch() -> Map<String, Value> {
    RESTORE
        .iter()
        .flat_map(|(_, keys)| keys.iter().filter(|key| restorable(key)))
        .map(|key| ((*key).to_owned(), contract().default_value(key).clone()))
        .collect()
}

/// `max-md:w-14`: the settings nav's width as an icon rail.
const SETTINGS_RAIL_WIDTH: f32 = 56.;

/// Below `md` (48rem of viewport) the settings nav is an icon rail.
fn settings_nav_is_rail(viewport_width: gpui_kit::Pixels) -> bool {
    viewport_width < px(768.)
}

/// `AppRouteModal`'s viewport padding.
#[derive(Clone, Copy, Debug, PartialEq)]
struct ModalInsets {
    side: f32,
    top: f32,
}

/// `sm:p-6 sm:pt-(--app-modal-sm-top) lg:p-10`, for a window `width` CSS px wide whose native
/// caption strip is `titlebar` px tall; `None` below `sm`, where the modal fills the window.
fn modal_insets(width: f32, titlebar: f32) -> Option<ModalInsets> {
    match width {
        w if w < 640. => None,
        w if w < 1024. => Some(ModalInsets { side: 24., top: titlebar.max(24.) }),
        _ => Some(ModalInsets { side: 40., top: 40. }),
    }
}

impl Workspace {
    /// The modal layer, drawn over everything while Settings is open.
    pub(in crate::app) fn render_settings(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let section: SharedString = self.settings.clone()?;
        if std::mem::take(&mut self.settings_ui.focus_on_render) {
            window.focus(&self.settings_ui.focus, cx);
        }
        let t = *theme::tokens(cx);
        let view = self.settings_view();
        self.load_scope_project_files(&view, cx);
        self.check_settings_grants(cx);
        self.sync_scope_project_picker(&view, window, cx);
        let content = self.with_section_target(|this| this.render_settings_section(&section, &view, window, cx), &t);
        let titlebar = f32::from(crate::app::titlebar::native_titlebar_height(window));
        let insets = modal_insets(f32::from(window.viewport_size().width), titlebar);
        Some(
            div()
                .id("settings-modal")
                .absolute()
                .inset_0()
                .occlude()
                .child(
                    div()
                        .id("settings-backdrop")
                        .absolute()
                        .inset_0()
                        .bg(theme::dialog_backdrop(&t))
                        .on_click(cx.listener(|this, _, window, cx| this.close_settings(window, cx))),
                )
                .child(
                    div()
                        .absolute()
                        .inset_0()
                        .when_some(insets, |el, insets| el.px(px(insets.side)).pb(px(insets.side)).pt(px(insets.top)))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            h_flex()
                                .id("settings-popup")
                                // `AppRouteModal` is a modal `Dialog`: Tab stays inside the popup.
                                .focus_trap("settings-trap", &self.settings_ui.focus)
                                .key_context("SettingsModal")
                                .on_key_down(cx.listener(Self::on_settings_key_down))
                                .occlude()
                                .relative()
                                .size_full()
                                .bg(t.background)
                                .overflow_hidden()
                                .map(|el| match insets {
                                    Some(_) => el
                                        .max_w(px(1280.))
                                        .max_h(px(960.))
                                        .rounded(px(16.))
                                        .border_1()
                                        .border_color(t.border)
                                        .shadow_2xl(),
                                    // Below `sm` it fills the window and leaves the caption strip
                                    // empty, still moving the window as the titlebar does.
                                    None => el.pt(px(titlebar)).child(crate::app::titlebar::drag_region(
                                        div().absolute().top_0().left_0().right_0().h(px(titlebar)),
                                    )),
                                })
                                .child({
                                    let nav = self.render_settings_nav(&section, window, cx);
                                    let page = v_flex()
                                        .track_focus(&self.areas.settings_page)
                                        .flex_1()
                                        .min_w_0()
                                        .h_full()
                                        .child(self.render_settings_header(&section, &view, window, cx))
                                        .child(v_flex().flex_1().min_h_0().child(content));
                                    // The expanded rail lies over the page, so it paints after it. Its 56px
                                    // stay in the flow so the page keeps its width and rows don't restack.
                                    let over_page = settings_nav_is_rail(window.viewport_size().width)
                                        && self.settings_ui.compact_search_open;
                                    h_flex().relative().size_full().min_h_0().map(|el| {
                                        if over_page {
                                            el.child(div().w(px(SETTINGS_RAIL_WIDTH)).flex_none())
                                                .child(page)
                                                .child(nav)
                                        } else {
                                            el.child(nav).child(page)
                                        }
                                    })
                                }),
                        ),
                )
                .into_any_element(),
        )
    }

    pub(in crate::app) fn focus_settings_on_open(&mut self) {
        self.settings_ui.focus_on_render = true;
        self.settings_ui.target = None;
    }

    /// Escape clears the search, then closes; it leaves open menus and dialogs to close themselves.
    pub(in crate::app) fn settings_escape(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if window.has_active_dialog(cx) {
            return false;
        }
        let search_focused = self.settings_ui.search.read(cx).focus_handle(cx).is_focused(window);
        let modal_focused = self.settings_modal_focused(window);
        if window.focused(cx).is_some() && !search_focused && !modal_focused {
            return false;
        }
        if !self.settings_ui.search.read(cx).value().is_empty() {
            self.settings_ui.search.update(cx, |search, cx| search.set_value("", window, cx));
            return true;
        }
        self.close_settings(window, cx);
        true
    }

    fn on_settings_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let modifiers = keystroke.modifiers;
        let find = keystroke.key == "f" && modifiers.secondary(cx) && !modifiers.alt && !modifiers.shift;
        let slash = keystroke.key == "/" && !modifiers.modified() && self.settings_modal_focused(window);
        if find || slash {
            self.settings_ui.compact_search_open = true;
            self.settings_ui.search.update(cx, |search, cx| search.focus(window, cx));
            cx.stop_propagation();
            return;
        }
        let searching = self.settings_ui.search.read(cx).focus_handle(cx).is_focused(window);
        let count = self.settings_results(cx).len();
        if !searching || count == 0 {
            return;
        }
        let active = &mut self.settings_ui.active_result;
        match keystroke.key.as_str() {
            "down" => *active = (*active + 1) % count,
            "up" => *active = (*active + count - 1) % count,
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn settings_results(&self, cx: &gpui_kit::App) -> Vec<&'static SearchItem> {
        let mut results = search::search(&self.settings_ui.search.read(cx).value(), search::shipped());
        results.retain(|item| self.setting_offered(item));
        results
    }

    /// Whether the connected environments offer an item that depends on them
    /// (`useAvailableSettingsSearchItems`); the rest follow the build (`search::available`).
    pub(in crate::app) fn setting_offered(&self, item: &SearchItem) -> bool {
        (!item.fast_worktree_checkout_only
            || self.sessions.iter().any(|s| s.is_connected() && s.server_info.capability("fastWorktreeCheckout")))
            && (!item.text_generators_only
                || self.sessions.iter().any(|s| {
                    !s.server_info.config.is_null() && providers::text_generators_allowed(&s.server_info.managed_policy)
                }))
    }

    /// Opens the result's section and scrolls its row into view (`handleSearchResultClick`).
    pub(in crate::app) fn open_settings_search_result(
        &mut self,
        item: &SearchItem,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings_ui.search.update(cx, |search, cx| search.set_value("", window, cx));
        self.settings_ui.compact_search_open = false;
        let target = item.target_id.clone().unwrap_or_else(|| item.id.clone());
        if let Some(fold) = advanced_fold_for(&target) {
            self.settings_ui.advanced_open.insert(fold);
        }
        // `revealSettingsSearchTarget`: these rows exist only with separate fonts on, and the
        // terminal reads the same flag, so it stays on.
        if matches!(target.as_str(), "prompt-font" | "terminal-font") {
            let patch = Map::from_iter([(TYPOGRAPHY_ADVANCED_KEY.to_owned(), Value::Bool(true))]);
            self.client_settings.apply(&patch);
            theme::set_appearance(theme::Appearance::from_settings(&self.client_settings.values), window, cx);
        }
        self.open_settings(item.to.clone(), cx);
        self.settings_ui.target = Some(target.into());
        self.settings_ui.jumps += 1;
        self.settings_ui.scroll_pending.set(widgets::JUMP_SETTLE_FRAMES);
        self.settings_ui.focus_on_render = true;
    }

    /// The section tree's keys (`useSettingsSectionNavigation`); Enter and Space open the row.
    fn on_settings_nav_key(
        &mut self,
        focused: &'static str,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let modifiers = event.keystroke.modifiers;
        if modifiers.alt || modifiers.control || modifiers.platform {
            return;
        }
        let key = event.keystroke.key.as_str();
        match key {
            "left" | "right" => {}
            "enter" | "space" => self.open_nav_section(focused, cx),
            _ => {
                let Some(next) = section_nav_step(key, &nav_sections(), focused) else { return };
                let page = self.settings.clone().unwrap_or_default();
                self.settings_ui.nav_tab_stop = Some((next, page));
                self.settings_ui.nav_reveal.set(Some(next));
                if let Some(handle) = self.settings_ui.nav_focus.get(next) {
                    window.focus(handle, cx);
                }
                cx.notify();
            }
        }
        cx.stop_propagation();
    }

    /// Commits the focused field's draft before Settings unmounts it.
    pub(in crate::app) fn flush_settings_field(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(flush) = self.settings_ui.focused_field.take() {
            flush(self, window, cx);
        }
    }

    pub(super) fn select_settings_section(&mut self, id: &'static str, cx: &mut Context<Self>) {
        // Pressing a section takes focus from the search, which closes the expanded rail.
        if std::mem::take(&mut self.settings_ui.compact_search_open) {
            self.settings_ui.focus_on_render = true;
        }
        self.settings_ui.target = None;
        self.settings_ui.scroll.set_offset(gpui_kit::point(px(0.), px(0.)));
        self.open_settings(id, cx);
    }

    /// F6's Settings sections stop: the search or a section row has focus.
    pub(in crate::app) fn settings_nav_focused(&self, window: &Window, cx: &gpui_kit::App) -> bool {
        self.settings_ui.search.read(cx).focus_handle(cx).is_focused(window)
            || self.settings_ui.nav_focus.values().any(|focus| focus.is_focused(window))
    }

    /// F6 to the sections: the row that takes Tab there.
    pub(in crate::app) fn focus_settings_nav(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let section = self.settings.clone().unwrap_or_default();
        let tab_stop = match &self.settings_ui.nav_tab_stop {
            Some((id, page)) if *page == section => *id,
            _ => nav_section(&section),
        };
        if let Some(handle) = self.settings_ui.nav_focus.get(tab_stop) {
            window.focus(handle, cx);
        }
    }

    /// The modal or one of its nav rows has focus: Escape and `/` are the modal's then.
    fn settings_modal_focused(&self, window: &Window) -> bool {
        self.settings_ui.focus.is_focused(window)
            || self.settings_ui.nav_focus.values().any(|focus| focus.is_focused(window))
    }

    /// A nav row's press: focus stays on the row, so the arrow keys go on from it, unless the
    /// press closes the expanded rail's search.
    fn open_nav_section(&mut self, id: &'static str, cx: &mut Context<Self>) {
        let keeps_focus = !self.settings_ui.compact_search_open;
        self.select_settings_section(id, cx);
        if keeps_focus {
            self.settings_ui.focus_on_render = false;
        }
    }

    fn render_settings_nav(&mut self, section: &str, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = *theme::tokens(cx);
        let sb = t.sidebar;
        let query = self.settings_ui.search.read(cx).value();
        let searching = !query.trim().is_empty();
        let compact = settings_nav_is_rail(window.viewport_size().width);
        // `compactSearchOpen` only changes the layout below `md`.
        let expanded = !compact || self.settings_ui.compact_search_open;
        let search_row = expanded.then(|| {
            h_flex()
                .id("settings-search")
                .h(px(32.))
                .px(px(8.))
                .gap(px(8.))
                .rounded(px(6.))
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .text_color(sb.muted_foreground)
                .hover(|s| s.bg(sb.sheet_row_hover))
                .child(Icon::new(IconName::Search).size(px(16.)).text_color(sb.muted_foreground.opacity(0.8)))
                .child(
                    div().flex_1().min_w_0().child(
                        // `SidebarInput`: medium `text-sidebar-foreground`.
                        Input::new(&self.settings_ui.search)
                            .appearance(false)
                            .px_0()
                            .py_0()
                            .h(px(20.))
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(sb.foreground),
                    ),
                )
                .map(|el| {
                    if searching {
                        el.child(
                            div()
                                .id("settings-search-clear")
                                .size(px(20.))
                                .flex()
                                .flex_none()
                                .items_center()
                                .justify_center()
                                .rounded(px(4.))
                                .hover(|s| s.bg(t.accent))
                                .child(Icon::new(IconName::X).size(px(12.)).text_color(t.muted_foreground))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.settings_ui.search.update(cx, |search, cx| {
                                        search.set_value("", window, cx);
                                        search.focus(window, cx);
                                    });
                                })),
                        )
                    } else {
                        el.child(kbd("/", &t))
                    }
                })
        });
        let body = if searching {
            let results = self.settings_results(cx);
            let active = self.settings_ui.active_result.min(results.len().saturating_sub(1));
            if results.is_empty() {
                div()
                    .px(px(8.))
                    .py(px(24.))
                    .text_center()
                    .text_xs()
                    .text_color(sb.muted_foreground)
                    .child("No settings found")
                    .into_any_element()
            } else {
                v_flex()
                    .gap(px(4.))
                    .children(results.into_iter().enumerate().map(|(index, item)| {
                        let is_active = index == active;
                        let open = item;
                        h_flex()
                            .id(SharedString::from(format!("settings-search-result-{}", item.id)))
                            .min_h(px(40.))
                            .items_start()
                            .px(px(10.))
                            .py(px(8.))
                            .gap(px(8.))
                            .rounded(px(8.))
                            .cursor_pointer()
                            .when(is_active, |el| el.bg(sb.sheet_row_selected))
                            .hover(|s| s.bg(sb.sheet_row_hover))
                            .on_mouse_move(cx.listener(move |this, _, _, cx| {
                                if this.settings_ui.active_result != index {
                                    this.settings_ui.active_result = index;
                                    cx.notify();
                                }
                            }))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_settings_search_result(open, window, cx);
                            }))
                            .child(section_icon(&item.to).size(px(16.)).text_color(sb.icon))
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(
                                        div()
                                            .truncate()
                                            .text_sm()
                                            .line_height(px(20.))
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(sb.foreground)
                                            .child(item.title.clone()),
                                    )
                                    .child(
                                        div()
                                            .truncate()
                                            .text_size(px(11.))
                                            .line_height(px(16.))
                                            .text_color(sb.muted_foreground.opacity(0.75))
                                            .child(section_label(&item.to).unwrap_or_default()),
                                    ),
                            )
                    }))
                    .into_any_element()
            }
        } else {
            let rail_search = (compact && !expanded).then(|| {
                div()
                    .id("settings-rail-search")
                    .h(px(32.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .rounded(px(8.))
                    .cursor_pointer()
                    .hover(|s| s.bg(sb.sheet_row_hover))
                    .child(Icon::new(IconName::Search).size(px(16.)).text_color(sb.icon))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.settings_ui.compact_search_open = true;
                        this.settings_ui.search.update(cx, |search, cx| search.focus(window, cx));
                        cx.notify();
                    }))
            });
            let current = nav_section(section);
            let tab_stop = match &self.settings_ui.nav_tab_stop {
                Some((id, page)) if page.as_ref() == section => *id,
                _ => current,
            };
            for id in nav_sections() {
                self.settings_ui.nav_focus.entry(id).or_insert_with(|| cx.focus_handle());
            }
            let nav_focus = &self.settings_ui.nav_focus;
            let nav_scroll = &self.settings_ui.nav_scroll;
            let nav_reveal = &self.settings_ui.nav_reveal;
            let keyboard = window.last_input_was_keyboard();
            let groups =
                v_flex().gap(px(16.)).children(SETTINGS.iter().enumerate().map(|(index, (heading, items))| {
                    v_flex()
                        .id(("settings-nav-group", index))
                        .gap(px(4.))
                        // `max-md:border-t max-md:pt-4 max-md:first:border-t-0`
                        .when(compact && index > 0, |el| el.border_t_1().border_color(sb.border).pt(px(16.)))
                        .children(heading.filter(|_| expanded).map(|heading| {
                            div()
                                .px(px(8.))
                                .text_xs()
                                .line_height(px(16.))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(sb.muted_foreground.opacity(0.8))
                                .child(heading)
                        }))
                        .children(items.iter().filter(|(id, _)| section_enabled(id)).map(|(id, label)| {
                            let id: &'static str = id;
                            let active = current == id;
                            let focus = nav_focus[id].clone().tab_stop(id == tab_stop);
                            // `focus-visible:ring-2 focus-visible:ring-inset ring-ring`
                            let ring = (keyboard && focus.is_focused(window))
                                .then(|| vec![BoxShadow::new(px(0.), px(0.), t.ring).spread_radius(px(2.)).inset()]);
                            let reveal = (nav_reveal.get() == Some(id))
                                .then(|| reveal_nav_row(id, nav_reveal.clone(), nav_scroll.clone()));
                            h_flex()
                                .id(SharedString::from(format!("settings-nav-{id}")))
                                .relative()
                                .children(reveal)
                                .track_focus(&focus)
                                .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                                    this.on_settings_nav_key(id, event, window, cx);
                                }))
                                .when_some(ring, gpui_kit::Styled::shadow)
                                .h(px(32.))
                                .px(px(10.))
                                .gap(px(8.))
                                .rounded(px(8.))
                                .cursor_pointer()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(if active { sb.foreground } else { sb.muted_foreground.opacity(0.8) })
                                .when(active, |el| el.bg(sb.sheet_row_selected))
                                .hover(|s| s.bg(sb.sheet_row_hover).text_color(sb.foreground))
                                // A pressed button takes focus in a browser, so the arrow keys go on from it.
                                .on_click({
                                    let focus = focus.clone();
                                    cx.listener(move |this, _, window, cx| {
                                        window.focus(&focus, cx);
                                        this.open_nav_section(id, cx);
                                    })
                                })
                                .child(section_icon(id).size(px(16.)).text_color(if active {
                                    sb.foreground
                                } else {
                                    sb.icon
                                }))
                                .when(expanded, |el| el.child(div().truncate().child(*label)))
                        }))
                }));
            v_flex().gap(px(16.)).children(rail_search).child(groups).into_any_element()
        };
        let overlay = compact && expanded;
        v_flex()
            .id("settings-nav")
            .map(|el| match (compact, overlay) {
                // `max-md:absolute max-md:inset-y-0 max-md:start-0 max-md:z-10 max-md:w-full`
                (_, true) => el.absolute().top_0().bottom_0().left_0().w_full().occlude(),
                (true, false) => el.w(px(SETTINGS_RAIL_WIDTH)),
                (false, _) => el.w(px(240.)),
            })
            .flex_none()
            .h_full()
            .border_r_1()
            .border_color(sb.border)
            // Outside `[data-app-sidebar]`, `bg-sidebar` is the page's own `--sidebar`.
            .bg(sb.sheet)
            .text_color(sb.foreground)
            .overflow_y_scroll()
            .track_scroll(&self.settings_ui.nav_scroll)
            .p(px(8.))
            .gap(px(8.))
            .children(search_row)
            .child(body)
            .into_any_element()
    }

    fn render_settings_header(
        &mut self,
        section: &str,
        view: &SettingsView,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = *theme::tokens(cx);
        // `max-sm:sr-only`: below `sm` the button is its icon alone.
        let restore_label = window.viewport_size().width >= px(640.);
        let restore = (section == "general").then(|| {
            let changed = self.restore_defaults_changes(view);
            // `size="xs"`: the kit's XSmall content row carries the 12px label and 4px gap; the
            // button's `[&_svg]:-mx-0.5` outranks the icon's own `mx-1`.
            Button::new("settings-restore-defaults")
                .ghost()
                .xsmall()
                .h(px(24.))
                .px(px(7.))
                .rounded(px(8.))
                .font_weight(FontWeight::MEDIUM)
                .disabled(changed.is_empty())
                .child(Icon::new(IconName::RotateCcw).size(px(14.)).mx(px(-2.)).text_color(t.muted_foreground))
                .when(restore_label, |el| el.child("Restore device defaults"))
                .on_click(cx.listener(|this, _, window, cx| this.confirm_restore_defaults(window, cx)))
        });
        h_flex()
            .h(px(48.))
            .flex_none()
            .gap(px(12.))
            .pl(px(20.))
            .pr(px(8.))
            .border_b_1()
            .border_color(t.border)
            .child(
                h_flex()
                    .min_w_0()
                    .gap(px(12.))
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .child(div().flex_none().text_color(t.muted_foreground).child("Settings"))
                    .child(div().flex_none().text_color(t.muted_foreground.opacity(0.6)).child("/"))
                    .child(div().truncate().child(section_label(section).unwrap_or("Settings"))),
            )
            .child(
                h_flex().ml_auto().flex_none().gap(px(4.)).children(restore).child(
                    Button::new("settings-close")
                        .ghost()
                        .size(px(28.))
                        .p_0()
                        .rounded(px(8.))
                        .child(Icon::new(IconName::X).size(px(16.)))
                        .tooltip("Close (Esc)")
                        .on_click(cx.listener(|this, _, window, cx| this.close_settings(window, cx))),
                ),
            )
            .into_any_element()
    }

    fn restore_defaults_changes(&self, view: &SettingsView) -> Vec<&'static str> {
        let is_default = |key: &str| view.value(key) == contract().default_value(key);
        restore_changes(is_default, &self.client_settings.theme_choice())
    }

    /// `RestoreDeviceDefaultsButton`: confirms, then resets the theme and every listed setting.
    fn confirm_restore_defaults(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let changed = self.restore_defaults_changes(&self.settings_view());
        if changed.is_empty() {
            return;
        }
        let description: SharedString = format!("This will reset: {}.", changed.join(", ")).into();
        let patch = restore_patch();
        let workspace = cx.entity();
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let (workspace, patch) = (workspace.clone(), patch.clone());
            dialog
                .title("Restore default settings?")
                .description(description.clone())
                .ok_text("Restore")
                .ok_variant(ButtonVariant::Danger)
                .show_cancel(true)
                .on_ok(move |_, window, cx| {
                    workspace.update(cx, |this, cx| {
                        this.set_theme_choice(ThemeChoice::default(), window, cx);
                        this.update_settings(Value::Object(patch.clone()), window, cx);
                    });
                    true
                })
        });
    }

    /// Stores and paints a theme library pick (`setTheme`, `setAppearanceMode`, `setThemeHalf`).
    pub(in crate::app) fn set_theme_choice(
        &mut self,
        choice: ThemeChoice,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.client_settings.set_theme_choice(choice.clone());
        theme::set_preference(choice, window, cx);
        self.sync_composer_marks(cx);
        cx.notify();
    }

    /// One section's content, behind the scope checks of `SettingsScopeBoundary`.
    fn render_settings_section(
        &mut self,
        section: &str,
        view: &SettingsView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if !widgets::device_only(section) && section != "projects" {
            let message = match &view.scope.kind {
                ScopeKind::Unavailable { message } => Some((*message).to_owned()),
                ScopeKind::Environment if view.targets.is_empty() => {
                    Some(format!("Reconnect {} to change its settings.", view.scope.label))
                }
                _ => None,
            };
            if let Some(message) = message {
                let t = *theme::tokens(cx);
                let note = div().px(px(16.)).text_sm().text_color(t.muted_foreground).child(message);
                return self.settings_page(section, view, [note.into_any_element()], cx);
            }
        }
        match section {
            "general" => self.render_settings_general(view, window, cx),
            "appearance" => self.render_settings_appearance(view, window, cx),
            "chat" => self.render_settings_chat(view, window, cx),
            "diffs" => self.render_settings_diffs(view, window, cx),
            "keybindings" => self.render_settings_keybindings(view, window, cx),
            "snap-shot" => self.render_settings_snap_shot(view, window, cx),
            "providers" => self.render_settings_providers(window, cx),
            "threads" => self.render_settings_threads(view, window, cx),
            "browser" => self.render_settings_browser(view, window, cx),
            "devices" => self.render_settings_devices(view, window, cx),
            "scheduled-tasks" => self.render_settings_scheduled_tasks(view, window, cx),
            "projects" => self.render_settings_projects(view, window, cx),
            "source-control" => self.render_settings_source_control(view, window, cx),
            "worktrees" => self.render_settings_worktrees(view, window, cx),
            "connections" => self.render_settings_connections(view, window, cx),
            "access" => self.render_settings_access(view, window, cx),
            // The web redirects the route to General without the feature flag.
            "secrets" if secrets_enabled() => self.render_settings_secrets(view, window, cx),
            "maintenance" => self.render_settings_maintenance(view, window, cx),
            "diagnostics" => self.render_settings_diagnostics(view, window, cx),
            "labs" => self.render_settings_labs(view, window, cx),
            "about" => self.render_settings_about(view, window, cx),
            "archived" => self.render_settings_archived(view, window, cx),
            "open-source-licenses" => self.render_settings_licenses(view, window, cx),
            _ => self.render_settings_general(view, window, cx),
        }
    }

    /// The web's "Advanced" fold at the end of a section card (`AdvancedSettings`).
    pub(in crate::app) fn advanced_fold(
        &self,
        section: &'static str,
        rows: impl IntoIterator<Item = AnyElement>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = *theme::tokens(cx);
        let open = self.settings_ui.advanced_open.contains(section);
        v_flex()
            .child(
                h_flex()
                    .id(SharedString::from(format!("{section}-advanced")))
                    .min_h(px(40.))
                    .px(px(16.))
                    .gap(px(6.))
                    .cursor_pointer()
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(t.muted_foreground)
                    .hover(|s| s.text_color(t.foreground))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.settings_ui.advanced_open.remove(section) {
                            this.settings_ui.advanced_open.insert(section);
                        }
                        cx.notify();
                    }))
                    .child(Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight }).size(px(14.)))
                    .child("Advanced"),
            )
            .when(open, |el| {
                el.child(v_flex().border_t_1().border_color(t.border.opacity(0.5)).children(
                    rows.into_iter().enumerate().map(|(index, row)| {
                        div().when(index > 0, |el| el.border_t_1().border_color(t.border.opacity(0.5))).child(row)
                    }),
                ))
            })
            .into_any_element()
    }
}

/// The web's `Kbd`.
fn kbd(key: &'static str, t: &Tokens) -> impl IntoElement {
    div()
        .h(px(20.))
        .min_w(px(20.))
        .px(px(4.))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .rounded(px(4.))
        .bg(t.foreground.opacity(0.07))
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(t.muted_foreground)
        .child(key)
}

/// The `AdvancedSettings` fold each search target sits in (its `targetIds`), so a jump opens it
/// before the row is laid out.
fn advanced_fold_for(target: &str) -> Option<&'static str> {
    const FOLDS: &[(&str, &[&str])] = &[
        ("appearance", &["panel-animations", "environment-identification", "appearance-interface", "composer-context"]),
        ("browser", &["browser-default-viewport", "browser-default-zoom", "browser-default-appearance"]),
        ("chat", &["skills-in-slash-menu"]),
        ("threads", &["thread-title-instructions", "response-streaming"]),
        ("typography", &["separate-fonts", "font-smoothing"]),
        ("source-control", &["follow-change-request-templates", "source-control-writer-model"]),
        ("worktree-creation", &["worktree-submodules"]),
        ("worktree-cleanup", &["storage-worktrees", "storage-worktree-keep-when"]),
    ];
    FOLDS.iter().find(|(_, targets)| targets.contains(&target)).map(|(fold, _)| *fold)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_search_jump_opens_the_fold_its_target_sits_in() {
        assert_eq!(advanced_fold_for("skills-in-slash-menu"), Some("chat"));
        assert_eq!(advanced_fold_for("response-streaming"), Some("threads"));
        assert_eq!(advanced_fold_for("separate-fonts"), Some("typography"));
        assert_eq!(advanced_fold_for("appearance-interface"), Some("appearance"));
        assert_eq!(advanced_fold_for("storage-worktrees"), Some("worktree-cleanup"));
        assert_eq!(advanced_fold_for("prompt-font"), None, "shown by separate fonts, not a fold");
        assert_eq!(advanced_fold_for("word-wrap"), None);
    }

    #[test]
    fn arrow_keys_walk_the_settings_sections_and_stop_at_the_ends() {
        let sections = ["general", "appearance", "chat"];
        assert_eq!(section_nav_step("down", &sections, "general"), Some("appearance"));
        assert_eq!(section_nav_step("up", &sections, "appearance"), Some("general"));
        assert_eq!(section_nav_step("down", &sections, "chat"), Some("chat"), "no wrap at the end");
        assert_eq!(section_nav_step("up", &sections, "general"), Some("general"), "no wrap at the start");
        assert_eq!(section_nav_step("end", &sections, "general"), Some("chat"));
        assert_eq!(section_nav_step("home", &sections, "chat"), Some("general"));
        assert_eq!(section_nav_step("right", &sections, "general"), None, "sections are leaves");
        assert!(nav_sections().starts_with(&["general", "appearance"]));
    }

    #[test]
    fn a_nav_row_scrolls_into_view_by_the_nearest_edge() {
        let viewport = (px(100.), px(300.));
        assert_eq!(nearest_scroll_offset(px(-40.), viewport, (px(120.), px(152.))), px(-40.), "already in view");
        assert_eq!(nearest_scroll_offset(px(-40.), viewport, (px(290.), px(322.))), px(-62.), "below: bottom edge");
        assert_eq!(nearest_scroll_offset(px(-40.), viewport, (px(80.), px(112.))), px(-20.), "above: top edge");
        assert_eq!(nearest_scroll_offset(px(-10.), viewport, (px(60.), px(92.))), px(0.), "never past the top");
    }

    #[test]
    fn the_modal_insets_follow_the_sm_and_lg_breakpoints() {
        assert_eq!(modal_insets(639., 40.), None, "below sm the modal fills the window");
        assert_eq!(modal_insets(800., 40.), Some(ModalInsets { side: 24., top: 40. }), "it clears the caption strip");
        assert_eq!(modal_insets(800., 0.), Some(ModalInsets { side: 24., top: 24. }));
        assert_eq!(modal_insets(1024., 52.), Some(ModalInsets { side: 40., top: 40. }));
    }

    #[test]
    fn restore_lists_changed_settings_and_resets_known_keys() {
        assert!(restore_changes(|_| true, &ThemeChoice::default()).is_empty());
        let patch = restore_patch();
        assert_eq!(patch.get("confirmThreadDelete"), Some(contract().default_value("confirmThreadDelete")));
        assert!(patch.keys().all(|k| contract().is_server_key(k) || contract().is_client_key(k)));

        let theme = ThemeChoice { mode: ThemePreference::Dark, base: Some("grove".into()), ..Default::default() };
        let changed = restore_changes(|key| key != "fontSizeCode", &theme);
        assert_eq!(changed, ["Theme", "Follow system", "Code font"]);
    }

    #[test]
    fn every_section_has_a_label_and_the_index_points_at_sections() {
        assert_eq!(section_label("diffs"), Some("Diffs & pull requests"));
        let unknown: Vec<_> =
            contract().search.iter().filter(|i| section_label(&i.to).is_none()).map(|i| &i.to).collect();
        assert!(unknown.is_empty(), "{unknown:?}");
    }
}
