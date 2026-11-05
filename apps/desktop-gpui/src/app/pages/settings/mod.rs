//! The Settings modal (`apps/web/src/routes/_chat.settings.tsx`): an `AppRouteModal` over the
//! window with `SettingsSidebarNav`, a breadcrumb header and one section at a time. Rows and
//! the scope layer are in `widgets.rs`.

mod agents;
mod chat;
pub(in crate::app) mod code;
mod connections;
mod desktop_rows;
mod diffs;
mod fields;
mod general;
mod keybindings;
mod model_select;

mod providers;
mod search;
mod snap_shot;
mod system;
mod theme_library;
mod typography;
mod widgets;

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::slider::SliderState;
use gpui_kit::component::{Disableable as _, Icon, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, Entity, FocusHandle, Focusable as _, FontWeight, InteractiveElement as _,
    IntoElement, KeyDownEvent, ParentElement as _, ScrollHandle, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, Window, div, px,
};
use serde_json::{Map, Value};

use crate::app::Workspace;
use crate::settings::scope::{ScopeKind, ScopeSearch};
use crate::settings::{SearchItem, ThemePreference, contract};
use crate::theme::{self, Tokens};
use crate::themes::ThemeChoice;

/// The sections in sidebar order (`SETTINGS_NAV_GROUPS`, `SETTINGS_SECTION_LABELS` in
/// `apps/web/src/components/settings/settingsSearch.ts`). A `None` heading is set apart by space alone.
pub(in crate::app) const SETTINGS: &[(Option<&str>, &[(&str, &str)])] = &[
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

/// Pages without a nav entry: id, breadcrumb label, and the section highlighted for them
/// (`SETTINGS_DETAIL_PAGE_PARENTS`).
const DETAIL_PAGES: &[(&str, &str, &str)] =
    &[("diagnostics", "Diagnostics", "maintenance"), ("open-source-licenses", "Open source licenses", "about")];

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
fn nav_section(id: &str) -> &str {
    DETAIL_PAGES.iter().find(|(page, ..)| *page == id).map_or(id, |(.., parent)| parent)
}

/// `SETTINGS_SECTION_ICONS`; SnapShots uses the web's own capture glyph, drawn here as Scan.
fn section_icon(id: &str) -> IconName {
    match id {
        "general" => IconName::Settings2,
        "appearance" => IconName::Palette,
        "chat" => IconName::MessageSquareText,
        "diffs" => IconName::FileDiff,
        "keybindings" => IconName::Keyboard,
        "snap-shot" => IconName::Scan,
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
    }
}

/// The modal's own state, kept while it is closed so the scope and folds persist.
pub(in crate::app) struct SettingsUi {
    /// The scope the sentence picked, kept across sections (`retainSettingsScope`).
    scope: ScopeSearch,
    search: Entity<InputState>,
    active_result: usize,
    /// The row a search result jumped to; highlighted until the next navigation.
    target: Option<SharedString>,
    scroll: ScrollHandle,
    /// Set by a search jump, cleared once the target row scrolled into view.
    scroll_pending: Rc<Cell<bool>>,
    sliders: HashMap<&'static str, (Entity<SliderState>, Subscription)>,
    /// The row whose inheritance popover is open; its marker stays visible meanwhile.
    open_marker: Option<SharedString>,
    /// Open "Advanced" folds, by section id.
    advanced_open: HashSet<&'static str>,
    /// The Typography rows' family pickers, by setting key.
    font_pickers: HashMap<&'static str, typography::FontPicker>,
    /// The grouping mode the Project grouping switch turns back on (`readLastEnabledProjectGroupingMode`).
    last_grouping_mode: &'static str,
    /// The modal surface, focused on open like the web's popup so Escape and `/` reach it.
    focus: FocusHandle,
    focus_on_render: bool,
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
                if let Some(item) = this.settings_results(cx).get(this.settings_ui.active_result).cloned() {
                    this.open_settings_search_result(&item, window, cx);
                }
            }
            _ => {}
        });
        Self {
            scope: ScopeSearch::default(),
            search,
            active_result: 0,
            target: None,
            scroll: ScrollHandle::new(),
            scroll_pending: Rc::default(),
            sliders: HashMap::new(),
            open_marker: None,
            advanced_open: HashSet::new(),
            font_pickers: HashMap::new(),
            last_grouping_mode: "repository",
            focus: cx.focus_handle(),
            focus_on_render: false,
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
    ("Context window indicator", &["contextWindowMeterEnabled"]),
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

/// The restore labels whose settings differ from their defaults, and the patch that resets
/// every restorable key the contract knows.
fn restore_plan(value: impl Fn(&str) -> Value, theme: &ThemeChoice) -> (Vec<&'static str>, Map<String, Value>) {
    let contract = contract();
    let known = |key: &str| contract.server_defaults.contains_key(key) || contract.client_defaults.contains_key(key);
    let halves = &theme.halves;
    let theme_labels = [
        (theme.base.is_some(), "Theme"),
        (theme.mode != ThemePreference::System, "Follow system"),
        (halves.light.is_some() || halves.dark.is_some(), "Theme mix"),
    ];
    let changed = theme_labels.into_iter().filter(|(changed, _)| *changed).map(|(_, label)| label).chain(
        RESTORE
            .iter()
            .filter(|(_, keys)| {
                keys.iter().filter(|key| known(key)).any(|key| value(key) != *contract.default_value(key))
            })
            .map(|(label, _)| *label),
    );
    let patch = RESTORE
        .iter()
        .flat_map(|(_, keys)| keys.iter().filter(|key| known(key)))
        .map(|key| ((*key).to_owned(), contract.default_value(key).clone()))
        .collect();
    (changed.collect(), patch)
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
        let content = self.render_settings_section(&section, window, cx);
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
                        .bg(t.background.opacity(0.6))
                        .on_click(cx.listener(|this, _, _, cx| this.close_settings(cx))),
                )
                .child(
                    div().absolute().inset_0().p(px(40.)).flex().items_center().justify_center().child(
                        h_flex()
                            .id("settings-popup")
                            .track_focus(&self.settings_ui.focus)
                            .key_context("SettingsModal")
                            .on_key_down(cx.listener(Self::on_settings_key_down))
                            .occlude()
                            .size_full()
                            .max_w(px(1280.))
                            .max_h(px(960.))
                            .rounded(px(16.))
                            .border_1()
                            .border_color(t.border)
                            .bg(t.background)
                            .shadow_2xl()
                            .overflow_hidden()
                            .child(self.render_settings_nav(&section, window, cx))
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .h_full()
                                    .child(self.render_settings_header(&section, cx))
                                    .child(v_flex().flex_1().min_h_0().child(content)),
                            ),
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
        let modal_focused = self.settings_ui.focus.is_focused(window);
        if window.focused(cx).is_some() && !search_focused && !modal_focused {
            return false;
        }
        if !self.settings_ui.search.read(cx).value().is_empty() {
            self.settings_ui.search.update(cx, |search, cx| search.set_value("", window, cx));
            return true;
        }
        self.close_settings(cx);
        true
    }

    fn on_settings_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let modifiers = keystroke.modifiers;
        let find = keystroke.key == "f" && modifiers.secondary(cx) && !modifiers.alt && !modifiers.shift;
        let slash = keystroke.key == "/" && !modifiers.modified() && self.settings_ui.focus.is_focused(window);
        if find || slash {
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

    fn settings_results(&self, cx: &gpui_kit::App) -> Vec<SearchItem> {
        let query = self.settings_ui.search.read(cx).value();
        search::search(&query, contract().search.iter().filter(|item| search::available(item)), |to| {
            section_label(to).unwrap_or_default()
        })
        .into_iter()
        .cloned()
        .collect()
    }

    /// Opens the result's section and scrolls its row into view (`handleSearchResultClick`).
    fn open_settings_search_result(&mut self, item: &SearchItem, window: &mut Window, cx: &mut Context<Self>) {
        self.settings_ui.search.update(cx, |search, cx| search.set_value("", window, cx));
        let target = item.target_id.clone().unwrap_or_else(|| item.id.clone());
        // A row inside a fold opens it first.
        if matches!(target.as_str(), "panel-animations" | "environment-identification" | "composer-context") {
            self.settings_ui.advanced_open.insert("appearance");
        }
        self.open_settings(item.to.clone(), cx);
        self.settings_ui.target = Some(target.into());
        self.settings_ui.scroll_pending.set(true);
        self.settings_ui.focus_on_render = true;
    }

    fn select_settings_section(&mut self, id: &'static str, cx: &mut Context<Self>) {
        self.settings_ui.target = None;
        self.settings_ui.scroll.set_offset(gpui_kit::point(px(0.), px(0.)));
        self.open_settings(id, cx);
    }

    fn render_settings_nav(&mut self, section: &str, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = *theme::tokens(cx);
        let sb = t.sidebar;
        let query = self.settings_ui.search.read(cx).value();
        let searching = !query.trim().is_empty();
        let search_focused = self.settings_ui.search.read(cx).focus_handle(cx).is_focused(window);
        let search_row = h_flex()
            .id("settings-search")
            .h(px(32.))
            .px(px(8.))
            .gap(px(8.))
            .rounded(px(6.))
            .text_sm()
            .font_weight(FontWeight::MEDIUM)
            .text_color(sb.muted_foreground)
            .hover(|s| s.bg(sb.row_hover))
            .when(search_focused, |el| el.bg(sb.row_hover))
            .child(Icon::new(IconName::Search).size(px(16.)).text_color(sb.muted_foreground.opacity(0.8)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(Input::new(&self.settings_ui.search).appearance(false).px_0().py_0().h(px(20.)).text_sm()),
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
                        let open = item.clone();
                        h_flex()
                            .id(SharedString::from(format!("settings-search-result-{}", item.id)))
                            .min_h(px(40.))
                            .items_start()
                            .px(px(10.))
                            .py(px(8.))
                            .gap(px(8.))
                            .rounded(px(8.))
                            .cursor_pointer()
                            .when(is_active, |el| el.bg(sb.row_active))
                            .hover(|s| s.bg(sb.row_hover))
                            .on_mouse_move(cx.listener(move |this, _, _, cx| {
                                if this.settings_ui.active_result != index {
                                    this.settings_ui.active_result = index;
                                    cx.notify();
                                }
                            }))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_settings_search_result(&open, window, cx)
                            }))
                            .child(Icon::new(section_icon(&item.to)).size(px(16.)).text_color(sb.icon))
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
            v_flex()
                .gap(px(16.))
                .children(SETTINGS.iter().enumerate().map(|(index, (heading, items))| {
                    v_flex()
                        .id(("settings-nav-group", index))
                        .gap(px(4.))
                        .children(heading.map(|heading| {
                            div()
                                .px(px(8.))
                                .text_xs()
                                .line_height(px(16.))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(sb.muted_foreground.opacity(0.8))
                                .child(heading)
                        }))
                        .children(items.iter().map(|(id, label)| {
                            let id: &'static str = id;
                            let active = nav_section(section) == id;
                            h_flex()
                                .id(SharedString::from(format!("settings-nav-{id}")))
                                .h(px(32.))
                                .px(px(10.))
                                .gap(px(8.))
                                .rounded(px(8.))
                                .cursor_pointer()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(if active { sb.foreground } else { sb.muted_foreground.opacity(0.8) })
                                .when(active, |el| el.bg(sb.row_active))
                                .hover(|s| s.bg(sb.row_hover).text_color(sb.foreground))
                                .on_click(cx.listener(move |this, _, _, cx| this.select_settings_section(id, cx)))
                                .child(Icon::new(section_icon(id)).size(px(16.)).text_color(if active {
                                    sb.foreground
                                } else {
                                    sb.icon
                                }))
                                .child(div().truncate().child(*label))
                        }))
                }))
                .into_any_element()
        };
        v_flex()
            .id("settings-nav")
            .w(px(240.))
            .flex_none()
            .h_full()
            .border_r_1()
            .border_color(sb.border)
            .bg(sb.background)
            .text_color(sb.foreground)
            .overflow_y_scroll()
            .p(px(8.))
            .gap(px(8.))
            .child(search_row)
            .child(body)
            .into_any_element()
    }

    fn render_settings_header(&mut self, section: &str, cx: &mut Context<Self>) -> AnyElement {
        let t = *theme::tokens(cx);
        let restore = (section == "general").then(|| {
            let (changed, _) = self.restore_defaults_plan();
            Button::new("settings-restore-defaults")
                .ghost()
                .h(px(24.))
                .px(px(7.))
                .gap(px(4.))
                .rounded(px(8.))
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .disabled(changed.is_empty())
                .child(Icon::new(IconName::RotateCcw).size(px(14.)).mx(px(4.)))
                .child("Restore device defaults")
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
                        .on_click(cx.listener(|this, _, _, cx| this.close_settings(cx))),
                ),
            )
            .into_any_element()
    }

    fn restore_defaults_plan(&self) -> (Vec<&'static str>, Map<String, Value>) {
        let view = self.settings_view();
        restore_plan(|key| view.value(key).clone(), &self.client_settings.theme_choice())
    }

    /// `RestoreDeviceDefaultsButton`: confirms, then resets the theme and every listed setting.
    fn confirm_restore_defaults(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (changed, patch) = self.restore_defaults_plan();
        if changed.is_empty() {
            return;
        }
        let description: SharedString = format!("This will reset: {}.", changed.join(", ")).into();
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
        cx.notify();
    }

    /// One section's content, behind the scope checks of `SettingsScopeBoundary`.
    fn render_settings_section(&mut self, section: &str, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if !widgets::device_only(section) && section != "projects" {
            let view = self.settings_view();
            let message = match &view.scope.kind {
                ScopeKind::Unavailable { message } => Some((*message).to_owned()),
                ScopeKind::Environment if view.targets.is_empty() => {
                    Some(format!("Reconnect {} to change its settings.", view.environment().label))
                }
                _ => None,
            };
            if let Some(message) = message {
                let t = *theme::tokens(cx);
                let note = div().px(px(16.)).text_sm().text_color(t.muted_foreground).child(message);
                return self.settings_page(section, &view, [note.into_any_element()], cx);
            }
        }
        match section {
            "general" => self.render_settings_general(window, cx),
            "appearance" => self.render_settings_appearance(window, cx),
            "chat" => self.render_settings_chat(window, cx),
            "diffs" => self.render_settings_diffs(window, cx),
            "keybindings" => self.render_settings_keybindings(window, cx),
            "snap-shot" => self.render_settings_snap_shot(window, cx),
            "providers" => self.render_settings_providers(window, cx),
            "threads" => self.render_settings_threads(window, cx),
            "browser" => self.render_settings_browser(window, cx),
            "devices" => self.render_settings_devices(window, cx),
            "scheduled-tasks" => self.render_settings_scheduled_tasks(window, cx),
            "projects" => self.render_settings_projects(window, cx),
            "source-control" => self.render_settings_source_control(window, cx),
            "worktrees" => self.render_settings_worktrees(window, cx),
            "connections" => self.render_settings_connections(window, cx),
            "access" => self.render_settings_access(window, cx),
            "secrets" => self.render_settings_secrets(window, cx),
            // Diagnostics opens from Maintenance; its section agent gives it a page of its own.
            "maintenance" | "diagnostics" => self.render_settings_maintenance(window, cx),
            "labs" => self.render_settings_labs(window, cx),
            "about" => self.render_settings_about(window, cx),
            "archived" => self.render_settings_archived(window, cx),
            _ => self.render_settings_general(window, cx),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restore_lists_changed_settings_and_resets_known_keys() {
        let defaults = |key: &str| contract().default_value(key).clone();
        let (changed, patch) = restore_plan(defaults, &ThemeChoice::default());
        assert!(changed.is_empty());
        assert_eq!(patch.get("confirmThreadDelete"), Some(contract().default_value("confirmThreadDelete")));
        assert!(patch.keys().all(|k| contract().is_server_key(k) || contract().is_client_key(k)));

        let edited = |key: &str| match key {
            "fontSizeCode" => Value::from(20),
            _ => contract().default_value(key).clone(),
        };
        let theme = ThemeChoice { mode: ThemePreference::Dark, base: Some("grove".into()), ..Default::default() };
        let (changed, _) = restore_plan(edited, &theme);
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
