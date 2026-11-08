//! The floating thread details card at the canvas' top right
//! (`chat/ThreadDetailsCard.tsx`, `ThreadDetailsPanel.tsx`): the environment,
//! Open in, project scripts, branch, git actions, Changes, lineage, usage limits
//! and context, at the density the card's height allows.

use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants as _};
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Anchor, AnyElement, App, Context, ElementId, Hsla, InteractiveElement as _, IntoElement, KeyDownEvent,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, WeakEntity, Window, anchored,
    deferred, div, hsla, img, point, px, relative, rgb, svg,
};
use serde_json::{Value, json};

use super::git::{self, GitRequest, QuickKind};
use super::titlebar::{RIGHT_RESIZE_INSET, TITLEBAR_HEIGHT, icon};
use super::{Target, Workspace, new_id};
use crate::model::{DelegatedFrom, ProjectScript, ThreadShell, TokenUsage, UsageLimits};
use crate::theme::{self, Tokens};
use crate::thread_details;

pub(super) mod automations;
pub(super) mod servers;

const CARD_WIDTH: f32 = 280.;
/// `quickActionDisabledReason` without `source-control:write`.
const NO_SOURCE_CONTROL_WRITE: &str = "This connection cannot change source control.";
const CARD_INSET: f32 = 12.;
const ROW_HEIGHT: f32 = 32.;
const METER_ROW_HEIGHT: f32 = 24.;
/// A section's 8px top and 10px bottom padding.
const SECTION_PADDING: f32 = 18.;
/// A section heading: 32px plus its 4px bottom margin.
const HEADING_HEIGHT: f32 = 36.;

/// The card's opaque stand-ins for `dropdown-glass` (the popover at `--glass-opacity`
/// over the canvas); GPUI has no backdrop blur.
#[derive(Clone, Copy)]
struct CardColors {
    background: Hsla,
    border: Hsla,
    divider: Hsla,
    hover: Hsla,
    label: Hsla,
    muted: Hsla,
    track: Hsla,
    tick: Hsla,
    ring: Hsla,
}

impl CardColors {
    fn new(tokens: &Tokens) -> Self {
        let (border, divider, hover) = if tokens.dark {
            (rgb(0x252525).into(), hsla(0., 0., 1., 0.052), hsla(0., 0., 1., 0.08))
        } else {
            (rgb(0xe8e8e8).into(), rgb(0xeeeef0).into(), hsla(0., 0., 0., 0.035))
        };
        let background = tokens.background.blend(tokens.popover.opacity(tokens.glass));
        Self {
            background,
            border,
            divider,
            hover,
            label: tokens.foreground.opacity(0.8),
            muted: tokens.muted_foreground,
            tick: tokens.foreground.opacity(0.6),
            ring: tokens.ring,
            track: tokens.muted_foreground.opacity(if tokens.dark { 0.2 } else { 0.3 }),
        }
    }
}

/// `resolveThreadDetailsCardDensity`: compact drops the environment and lineage
/// rows, essential also drops Open in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Density {
    Essential,
    Compact,
    Full,
}

fn resolve_density(available: f32, full: f32, compact: f32) -> Density {
    if full <= available {
        Density::Full
    } else if compact <= available {
        Density::Compact
    } else {
        Density::Essential
    }
}

/// One label | bar | value row of the usage grid (`ThreadDetailsUsage.tsx` `UsageRow`).
struct Meter {
    id: SharedString,
    label: SharedString,
    /// Bar fill from 0 to 100; `None` hides the bar.
    fill: Option<f32>,
    fill_color: Hsla,
    /// Auto-compaction marker from 0 to 100.
    tick: Option<f32>,
    value: SharedString,
    value_color: Hsla,
    tooltip: SharedString,
}

/// A section and the rows it would take at each density, for the height estimate.
struct Section {
    element: AnyElement,
    /// Content height without padding.
    height: f32,
    /// Shown only at this density or above.
    min_density: Density,
}

/// `EDITORS` labels, with the file manager named for Windows.
pub(super) fn editor_label(id: &str) -> &str {
    match id {
        "cursor" => "Cursor",
        "trae" => "Trae",
        "kiro" => "Kiro",
        "vscode" => "VS Code",
        "vscode-insiders" => "VS Code Insiders",
        "vscodium" => "VSCodium",
        "zed" => "Zed",
        "antigravity" => "Antigravity",
        "idea" => "IntelliJ IDEA",
        "file-manager" if cfg!(target_os = "macos") => "Finder",
        "file-manager" if cfg!(target_os = "windows") => "File Explorer",
        "file-manager" => "Files",
        other => other,
    }
}

/// The editor's logo (`OpenInPicker.tsx` `resolveOpenInOptions`), full color where the web's is.
pub(super) fn editor_icon(id: &str, tokens: &Tokens) -> AnyElement {
    let size = px(16.);
    match id {
        "vscode" => img("icons/app/vscode.svg").size(size).flex_none().into_any_element(),
        "file-manager" if cfg!(target_os = "windows") => {
            img("icons/app/file-explorer.svg").size(size).flex_none().into_any_element()
        }
        "cursor" => svg()
            .path("icons/app/cursor.svg")
            .size(size)
            .flex_none()
            .text_color(rgb(if tokens.dark { 0xedecec } else { 0x26251e }))
            .into_any_element(),
        "file-manager" => {
            icon("icons/folder-closed.svg").with_size(size).text_color(tokens.muted_foreground).into_any_element()
        }
        _ => icon("icons/square-arrow-out-up-right.svg")
            .with_size(size)
            .text_color(tokens.muted_foreground)
            .into_any_element(),
    }
}

/// How long a card stream that ended while still wanted waits before it subscribes again.
const RESUBSCRIBE_AFTER: std::time::Duration = std::time::Duration::from_secs(5);

/// Clears an ended stream's task after [`RESUBSCRIBE_AFTER`], so the next render subscribes
/// again without looping on a server that keeps failing.
pub(super) async fn resubscribe_after_backoff(
    this: &WeakEntity<Workspace>,
    cx: &mut gpui_kit::AsyncWindowContext,
    clear: impl FnOnce(&mut Workspace),
) {
    cx.background_executor().timer(RESUBSCRIBE_AFTER).await;
    let _ = this.update(cx, |this, cx| {
        clear(this);
        cx.notify();
    });
}

/// `t3code:right-panel` `threadPanelVisibilityByThreadKey`, kept with this device's client settings.
pub(super) const DETAILS_VISIBILITY_KEY: &str = "threadPanelVisibilityByThreadKey";

/// One thread's card: shown inline, and shown as the popover.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Visibility {
    pub inline_open: bool,
    pub popover_open: bool,
}

/// `DEFAULT_THREAD_PANEL_VISIBILITY`.
const DEFAULT_VISIBILITY: Visibility = Visibility { inline_open: true, popover_open: false };

/// The card's visibility per thread (`threadPanelVisibilityByThreadKey`). A thread at the
/// default keeps no entry, and only a collapsed inline card outlives the app.
#[derive(Debug, Default, PartialEq)]
pub(super) struct DetailsVisibility(std::collections::HashMap<String, Visibility>);

impl DetailsVisibility {
    /// `migratePersistedRightPanelState`: only `inlineOpen: false` entries come back.
    pub(super) fn from_stored(stored: &Value) -> Self {
        let collapsed = stored.as_object().into_iter().flatten().filter(|(_, v)| v["inlineOpen"] == false);
        Self(collapsed.map(|(key, _)| (key.clone(), Visibility { inline_open: false, popover_open: false })).collect())
    }

    pub(super) fn get(&self, key: &str) -> Visibility {
        self.0.get(key).copied().unwrap_or(DEFAULT_VISIBILITY)
    }

    /// `updateThreadPanelVisibilityMap`; returns whether the stored subset changed.
    fn update(&mut self, key: &str, change: impl FnOnce(&mut Visibility)) -> bool {
        let before = self.get(key);
        let mut next = before;
        change(&mut next);
        if next == DEFAULT_VISIBILITY {
            self.0.remove(key);
        } else {
            self.0.insert(key.to_owned(), next);
        }
        before.inline_open != next.inline_open
    }

    /// `partialize`: the collapsed inline cards.
    fn stored(&self) -> Value {
        let collapsed = self.0.iter().filter(|(_, visibility)| !visibility.inline_open);
        Value::Object(
            collapsed.map(|(key, _)| (key.clone(), json!({"inlineOpen": false, "popoverOpen": false}))).collect(),
        )
    }
}

impl Workspace {
    /// The open target's key in [`DetailsVisibility`] (`scopedThreadKey`).
    fn details_key(&self) -> Option<String> {
        match self.target.as_ref()? {
            Target::Thread { env, id } => Some(format!("{env}:{id}")),
            Target::Draft { env, project_id } => Some(format!("{env}:draft:{project_id}")),
        }
    }

    /// The open target's card; nothing open shows no card.
    pub(super) fn details_visibility(&self) -> Visibility {
        self.details_key()
            .map_or(Visibility { inline_open: false, popover_open: false }, |key| self.details_visibility.get(&key))
    }

    fn update_details(&mut self, change: impl FnOnce(&mut Visibility)) {
        let Some(key) = self.details_key() else { return };
        if self.details_visibility.update(&key, change) {
            let stored = self.details_visibility.stored();
            self.client_settings.apply(&serde_json::Map::from_iter([(DETAILS_VISIBILITY_KEY.to_owned(), stored)]));
        }
    }

    /// Closes or opens the open thread's popover, without touching an unchanged entry.
    pub(super) fn set_details_popover(&mut self, open: bool) {
        if self.details_visibility().popover_open != open {
            self.update_details(|visibility| visibility.popover_open = open);
        }
    }

    /// Whether the card is on screen, inline or as the popover.
    pub(super) fn details_shown(&self, window: &Window) -> bool {
        let visibility = self.details_visibility();
        if self.details_inline(window) { visibility.inline_open } else { visibility.popover_open }
    }

    /// The toggle drives whichever presentation the canvas allows, for the open thread only.
    pub(super) fn toggle_details(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.details_inline(window) {
            self.update_details(|visibility| visibility.inline_open = !visibility.inline_open);
        } else {
            self.update_details(|visibility| visibility.popover_open = !visibility.popover_open);
        }
        cx.notify();
    }

    pub(super) fn escape_details_popover(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.details_visibility().popover_open && event.keystroke.key == "escape" {
            self.set_details_popover(false);
            cx.stop_propagation();
            cx.notify();
        }
    }

    pub(super) fn render_details_card(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.details_shown(window) {
            return None;
        }
        let tokens = *theme::tokens(cx);
        let colors = CardColors::new(&tokens);
        let inline = self.details_inline(window);
        let viewport = window.viewport_size();
        let available = if inline {
            f32::from(viewport.height - TITLEBAR_HEIGHT) - CARD_INSET * 2. - self.find_bar_inset()
        } else {
            f32::from(viewport.height) - 104.
        };
        // Built once at full density; when the card must shrink, only the two sections
        // whose rows depend on density are built again.
        let mut sections: [Option<Section>; 7] = [
            self.workspace_section(&tokens, &colors, cx),
            self.version_control_section(&tokens, &colors, cx),
            self.servers_section(&tokens, &colors, cx),
            self.automations_section(&tokens, &colors, cx),
            self.lineage_section(&tokens, &colors, cx).filter(|_| self.lineage_shown()),
            self.usage_section(&tokens, &colors).filter(|_| self.details_item_shown("usage")),
            self.context_section(&tokens, &colors).filter(|_| self.details_item_shown("context")),
        ];
        let height_at = |density: Density| {
            let shown = sections.iter().flatten().filter(|s| s.min_density <= density);
            shown.map(|s| s.height + SECTION_PADDING).sum::<f32>() + 2.
        };
        let density = resolve_density(available, height_at(Density::Full), height_at(Density::Compact));
        if density != Density::Full {
            sections[0] = self.workspace_section_at(density, &tokens, &colors, cx);
            sections[1] = self.version_control_section_at(density, &tokens, &colors, cx);
        }
        let sections: Vec<AnyElement> = sections
            .into_iter()
            .flatten()
            .filter(|section| section.min_density <= density)
            .map(|section| section.element)
            .enumerate()
            .map(|(index, section)| {
                v_flex()
                    .px_2()
                    .pt_2()
                    .pb(px(10.))
                    .when(index > 0, |el| el.border_t_1().border_color(colors.divider))
                    .child(section)
                    .into_any_element()
            })
            .collect();
        // Shown only when no section has content, so an emptied card can be refilled.
        let choose = sections.is_empty().then(|| {
            div().p_2().child(
                Button::new("thread-details-choose")
                    .ghost()
                    .xsmall()
                    .icon(gpui_kit::assets::IconName::SlidersHorizontal)
                    .label("Choose what to show")
                    .text_color(tokens.muted_foreground)
                    .dropdown_menu(thread_details_items_menu(cx.weak_entity())),
            )
        });
        let card = v_flex()
            .id("thread-details")
            .w_full()
            .overflow_y_scroll()
            .occlude()
            .rounded(px(16.))
            .border_1()
            .border_color(colors.border)
            .bg(colors.background)
            .children(sections)
            .children(choose)
            .context_menu(thread_details_items_menu(cx.weak_entity()));
        if inline {
            return Some(
                div()
                    .absolute()
                    .top(px(CARD_INSET + self.find_bar_inset()))
                    .bottom(px(CARD_INSET))
                    .right(px(CARD_INSET))
                    .w(px(CARD_WIDTH))
                    .child(card.max_h_full())
                    .into_any_element(),
            );
        }
        // `PopoverPopup variant="panel"` under the header, `align="end"`: 4px align
        // offset plus its 8px padding put the card where the inline one sits, 8px
        // below the header, capped at `100dvh - 6.5rem`.
        let right = self.right_panel_width(window).map_or(px(0.), px) + RIGHT_RESIZE_INSET;
        let corner = point(viewport.width - right - px(CARD_INSET), TITLEBAR_HEIGHT + px(8.));
        Some(
            deferred(
                anchored().anchor(Anchor::TopRight).position(corner).child(
                    div()
                        .w(px(CARD_WIDTH))
                        .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                            this.set_details_popover(false);
                            cx.notify();
                        }))
                        .child(card.max_h(viewport.height - px(104.))),
                ),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }

    /// Whether the thread details card shows an item (`threadDetailsItems` client setting).
    fn details_item_shown(&self, id: &str) -> bool {
        thread_details::visible(&self.client_settings.values, id)
    }

    /// The lineage section holds both the lineage and the side chats items.
    fn lineage_shown(&self) -> bool {
        self.details_item_shown("lineage") || self.details_item_shown("sideChats")
    }

    fn workspace_section(&self, tokens: &Tokens, colors: &CardColors, cx: &mut Context<Self>) -> Option<Section> {
        self.workspace_section_at(Density::Full, tokens, colors, cx).map(|mut section| {
            // Compact keeps Open in and the scripts row; essential keeps only scripts.
            section.min_density = Density::Essential;
            section.height = section.height.min(ROW_HEIGHT * 3.);
            section
        })
    }

    /// Environment, Open in and the project scripts row (`ThreadDetailsPanel` workspace section).
    fn workspace_section_at(
        &self,
        density: Density,
        tokens: &Tokens,
        colors: &CardColors,
        cx: &mut Context<Self>,
    ) -> Option<Section> {
        let rows: Vec<AnyElement> = [
            (density == Density::Full && self.details_item_shown("environment"))
                .then(|| self.environment_row(tokens, colors, cx))
                .flatten(),
            (density != Density::Essential && self.details_item_shown("openIn"))
                .then(|| self.open_in_row(tokens, colors, cx))
                .flatten(),
            self.details_item_shown("scripts").then(|| self.scripts_row(tokens, colors, cx)).flatten(),
        ]
        .into_iter()
        .flatten()
        .collect();
        (!rows.is_empty()).then(|| Section {
            height: rows.len() as f32 * ROW_HEIGHT,
            element: v_flex().children(rows).into_any_element(),
            min_density: Density::Essential,
        })
    }

    /// The environment row: machine icon, environment label, workspace kind at the far right.
    fn environment_row(&self, tokens: &Tokens, colors: &CardColors, cx: &mut Context<Self>) -> Option<AnyElement> {
        let label = self.server_info().environment_label.clone()?;
        let machine = match self.server_info().machine_kind() {
            "desktop" | "mac-studio" | "mac-mini" => "icons/monitor.svg",
            "laptop" => "icons/laptop.svg",
            "cloud" => "icons/cloud.svg",
            _ => "icons/server.svg",
        };
        let worktree = self.open_projection().and_then(|p| p.thread.worktree_path.clone());
        let workspace_root = self.target_project().map(|project| project.workspace_root.clone());
        // A git draft picks its workspace here (`RunContextSelector` as a select); a thread's is locked.
        if matches!(self.target, Some(Target::Draft { .. })) && self.is_git_repo() {
            let (workspace_icon, workspace_label) = self.env_mode_choice();
            let build = self.env_mode_menu(cx);
            let path = self.composer_cwd().filter(|_| workspace_label != "New worktree");
            let tip: SharedString = path.unwrap_or_else(|| workspace_label.to_owned()).into();
            return Some(
                select_row("details-run-context", tokens, colors, cx)
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(px(10.))
                            .child(icon(machine).with_size(px(18.)).mx(px(-1.)).text_color(tokens.muted_foreground))
                            .child(div().flex_1().min_w_0().truncate().text_left().child(label))
                            .child(
                                div()
                                    .id("details-workspace")
                                    .flex_none()
                                    .child(
                                        gpui_kit::component::Icon::new(workspace_icon)
                                            .with_size(px(16.))
                                            .text_color(tokens.muted_foreground),
                                    )
                                    .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx)),
                            ),
                    )
                    .child(select_chevron(tokens))
                    .dropdown_menu_with_anchor(Anchor::TopLeft, move |menu, window, cx| {
                        build(menu.min_w(px(264.)), window, cx)
                    })
                    .into_any_element(),
            );
        }
        let (workspace_icon, workspace_path) = match worktree {
            Some(path) => ("icons/folder-git.svg", Some(path)),
            None => ("icons/folder.svg", workspace_root),
        };
        // The locked row's label stays muted in dark mode (`BranchToolbar.tsx`).
        let label_color = if tokens.dark { tokens.muted_foreground } else { colors.label };
        Some(
            row(colors)
                .child(icon(machine).with_size(px(18.)).mx(px(-1.)).text_color(tokens.muted_foreground))
                .child(div().flex_1().min_w_0().truncate().text_color(label_color).child(label))
                .child(
                    div()
                        .id("details-workspace")
                        .flex_none()
                        .child(icon(workspace_icon).with_size(px(16.)).text_color(tokens.muted_foreground))
                        .when_some(workspace_path, |el, path| {
                            let path = SharedString::from(path);
                            el.tooltip(move |window, cx| Tooltip::new(path.clone()).build(window, cx))
                        }),
                )
                .into_any_element(),
        )
    }

    /// The editor the Open in row launches: the last one picked, else the first available (`usePreferredEditor`).
    pub(super) fn preferred_editor(&self) -> Option<&str> {
        let available = &self.server_info().available_editors;
        self.preferred_editor
            .as_deref()
            .filter(|editor| available.iter().any(|id| id == editor))
            .or_else(|| available.first().map(String::as_str))
    }

    /// `OpenInPicker` in panel mode: the row opens the preferred editor, the chevron picks it.
    fn open_in_row(&self, tokens: &Tokens, colors: &CardColors, cx: &mut Context<Self>) -> Option<AnyElement> {
        let project = self.target_project()?;
        if project.workspace_root.is_empty() {
            return None;
        }
        let preferred = self.preferred_editor().map(str::to_owned);
        let label: SharedString = match &preferred {
            Some(editor) => format!("Open in {}", editor_label(editor)).into(),
            None => "Open in editor".into(),
        };
        let leading = match &preferred {
            Some(editor) => editor_icon(editor, tokens),
            None => icon("icons/square-arrow-out-up-right.svg")
                .with_size(px(16.))
                .text_color(tokens.muted_foreground)
                .into_any_element(),
        };
        let view = cx.entity();
        let editors = self.server_info().available_editors.clone();
        let current = preferred.clone();
        let menu_tokens = *tokens;
        let build = move |mut menu: PopupMenu, window: &mut Window, _: &mut Context<PopupMenu>| {
            if editors.is_empty() {
                return menu.item(PopupMenuItem::new("No installed editors found").disabled(true));
            }
            for editor in &editors {
                let picked = editor.clone();
                let label = SharedString::from(editor_label(editor).to_owned());
                let checked = current.as_deref() == Some(editor.as_str());
                let item_editor = editor.clone();
                menu = menu.item(
                    PopupMenuItem::element(move |_, _| {
                        h_flex().gap_2().child(editor_icon(&item_editor, &menu_tokens)).child(label.clone())
                    })
                    .checked(checked)
                    .on_click(window.listener_for(&view, move |this, _, _, cx| {
                        this.preferred_editor = Some(picked.clone());
                        cx.notify();
                    })),
                );
            }
            menu
        };
        let enabled = preferred.is_some() && self.git_cwd().is_some();
        Some(
            split_row("details-open-in", colors, cx)
                .child(
                    split_primary("details-open-in-primary", colors, enabled, cx)
                        .child(leading)
                        .child(div().min_w_0().truncate().child(label))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(editor) = preferred.clone() {
                                this.open_in_editor(editor, cx);
                            }
                        })),
                )
                .child(split_hairline("details-open-in", colors))
                .child(
                    split_menu("details-open-in-menu", tokens, colors, cx)
                        .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, window, cx| {
                            build(menu.min_w(px(264.)), window, cx)
                        }),
                )
                .into_any_element(),
        )
    }

    /// `editor.openFavorite` (`OpenInPickerShortcut.ts`): opens the preferred editor while the
    /// Open in row can show.
    pub(super) fn open_favorite_editor(&mut self, cx: &mut Context<Self>) -> bool {
        if self.target_project().is_none_or(|project| project.workspace_root.is_empty()) {
            return false;
        }
        let Some(editor) = self.preferred_editor().map(str::to_owned) else { return false };
        self.open_in_editor(editor, cx);
        true
    }

    /// `shell.openInEditor` on the thread's workspace.
    fn open_in_editor(&mut self, editor: String, cx: &mut Context<Self>) {
        let (Some(client), Some(cwd)) = (self.client(), self.git_cwd()) else { return };
        self.preferred_editor = Some(editor.clone());
        cx.spawn(async move |this, cx| {
            if let Err(error) = client.request("shell.openInEditor", json!({"cwd": cwd, "editor": editor})).await {
                let _ = this.update(cx, |this, cx| {
                    this.error = Some(format!("Couldn't open the editor: {error}").into());
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// `ProjectScriptsControl` in panel mode: run the primary script, pick, import or add one from
    /// the menu; "Actions" when no script can be primary; "Add project script" with none at all.
    fn scripts_row(&self, tokens: &Tokens, colors: &CardColors, cx: &mut Context<Self>) -> Option<AnyElement> {
        use super::pages::settings::code::projects::scripts::{
            ScriptInput, command_for_project_script, importable_scripts, resolve_project_scripts, script_icon,
            shortcut_label_for_command,
        };
        let project = self.target_project()?;
        let project_id = project.id.clone();
        let saved = resolve_project_scripts(&self.server_info().settings, project);
        // `useT3ProjectFileScripts` on the thread's checkout and machine; read on first sight.
        let checkout = self.active_environment().cloned().zip(self.git_cwd());
        let importable: Vec<ScriptInput> = match checkout {
            Some(key) => match self.t3_project_files.get(&key) {
                Some(file) => importable_scripts(file, &saved).into_iter().map(ScriptInput::from_file_script).collect(),
                None => {
                    let view = cx.entity();
                    cx.defer(move |cx| {
                        view.update(cx, |this, cx| {
                            this.t3_project_file(&key.0, &key.1, cx);
                        });
                    });
                    Vec::new()
                }
            },
            None => Vec::new(),
        };
        if saved.is_empty() && importable.is_empty() {
            return Some(
                action_row("details-add-script", colors, true, cx)
                    .child(icon("icons/plus.svg").with_size(px(14.)).text_color(tokens.muted_foreground))
                    .child(div().ml(px(2.)).truncate().child("Add project script"))
                    .tooltip(|window, cx| Tooltip::new("Add project script").build(window, cx))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_project_script_editor(&project_id, None, window, cx);
                    }))
                    .into_any_element(),
            );
        }
        let primary = primary_script(&saved, self.last_invoked_script(&project_id))
            .map(|script| (SharedString::from(script.name.clone()), ScriptRun::new(&project_id, script)));
        let keybindings = self.server_info().keybindings_json();
        let scripts: Vec<ScriptItem> = saved
            .iter()
            .map(|script| ScriptItem {
                label: project_script_menu_label(script).into(),
                name: script.name.clone().into(),
                icon: script.icon.clone(),
                shortcut: shortcut_label_for_command(keybindings, command_for_project_script(&script.id).as_deref())
                    .map(Into::into),
                run: ScriptRun::new(&project_id, script),
            })
            .collect();
        // `scriptItems`: the scripts, then the `t3.json` ones to import, then "Add project script".
        let view = cx.entity();
        let build = move |mut menu: PopupMenu, window: &mut Window, cx: &mut Context<PopupMenu>| {
            let popup = cx.entity();
            for item in &scripts {
                let run = item.run.clone();
                menu = menu.item(
                    PopupMenuItem::element(script_item_row(item, &view, &popup))
                        .icon(gpui_kit::component::Icon::new(script_icon(&item.icon)))
                        .on_click(window.listener_for(&view, move |this, _, window, cx| {
                            this.run_project_script(&run, window, cx);
                        })),
                );
            }
            if !importable.is_empty() {
                if !scripts.is_empty() {
                    menu = menu.separator();
                }
                menu = menu.label("From t3.json");
            }
            for input in &importable {
                let (pid, input) = (project_id.clone(), input.clone());
                let name = SharedString::from(input.name.clone());
                menu = menu.item(
                    PopupMenuItem::element(move |_, cx| {
                        h_flex().w_full().gap_2().child(name.clone()).child(
                            icon("icons/download.svg")
                                .with_size(px(14.))
                                .ml_auto()
                                .text_color(theme::tokens(cx).muted_foreground),
                        )
                    })
                    .icon(gpui_kit::component::Icon::new(script_icon(&input.icon)))
                    .on_click(window.listener_for(&view, move |this, _, window, cx| {
                        this.import_project_script(&pid, &input, window, cx);
                    })),
                );
            }
            let pid = project_id.clone();
            menu.item(PopupMenuItem::new("Add project script").icon(icon("icons/plus.svg")).on_click(
                window.listener_for(&view, move |this, _, window, cx| {
                    this.open_project_script_editor(&pid, None, window, cx);
                }),
            ))
        };
        let Some((name, run)) = primary else {
            return Some(
                select_row("details-project-actions", tokens, colors, cx)
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(px(10.))
                            .child(icon("icons/wrench.svg").with_size(px(16.)).text_color(tokens.muted_foreground))
                            .child(div().flex_1().min_w_0().truncate().text_left().child("Actions")),
                    )
                    .child(select_chevron(tokens))
                    .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, window, cx| {
                        build(menu.min_w(px(264.)), window, cx)
                    })
                    .into_any_element(),
            );
        };
        let tooltip: SharedString = format!("Run {name}").into();
        Some(
            split_row("details-scripts", colors, cx)
                .child(
                    split_primary("details-scripts-primary", colors, true, cx)
                        .child(icon("icons/play.svg").with_size(px(16.)).text_color(tokens.muted_foreground))
                        .child(div().min_w_0().truncate().child(name))
                        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                        .on_click(cx.listener(move |this, _, window, cx| this.run_project_script(&run, window, cx))),
                )
                .child(split_hairline("details-scripts", colors))
                .child(
                    split_menu("details-scripts-menu", tokens, colors, cx)
                        .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, window, cx| {
                            build(menu.min_w(px(264.)), window, cx)
                        }),
                )
                .into_any_element(),
        )
    }

    /// Scripts run in the thread's terminal; the terminal drawer owns `RunInTerminal`.
    fn run_project_script(&mut self, run: &ScriptRun, window: &mut Window, cx: &mut Context<Self>) {
        let Some(cwd) = self.git_cwd().filter(|_| self.can_run_scripts()) else { return };
        self.remember_script(&run.project_id, &run.id);
        window.dispatch_action(Box::new(super::RunInTerminal::script(cwd, &run.script)), cx);
    }

    /// `hasTerminalWriteAccess`: scripts run only with `terminal:operate` on the thread's environment.
    pub(super) fn can_run_scripts(&self) -> bool {
        self.active_environment().is_some_and(|environment| self.env_grants(environment, "terminal:operate"))
    }

    /// `lastInvokedScriptByProjectId[projectId]`: the script last run in the project on this device.
    fn last_invoked_script(&self, project_id: &str) -> Option<&str> {
        self.client_settings.values.get(LAST_INVOKED_SCRIPT_KEY)[project_id].as_str()
    }

    /// Remembers `script_id` as the project's last run script, which the card then offers first.
    pub(super) fn remember_script(&mut self, project_id: &str, script_id: &str) {
        if self.last_invoked_script(project_id) == Some(script_id) {
            return;
        }
        let mut by_project =
            self.client_settings.values.get(LAST_INVOKED_SCRIPT_KEY).as_object().cloned().unwrap_or_default();
        by_project.insert(project_id.to_owned(), script_id.into());
        let patch = serde_json::Map::from_iter([(LAST_INVOKED_SCRIPT_KEY.to_owned(), Value::Object(by_project))]);
        self.client_settings.apply(&patch);
    }

    fn version_control_section(&self, tokens: &Tokens, colors: &CardColors, cx: &mut Context<Self>) -> Option<Section> {
        self.version_control_section_at(Density::Full, tokens, colors, cx).map(|mut section| {
            section.min_density = Density::Essential;
            section
        })
    }

    /// Branch, the git actions and Changes (`BranchToolbar` branch section + `GitActionsControl`).
    fn version_control_section_at(
        &self,
        density: Density,
        tokens: &Tokens,
        colors: &CardColors,
        cx: &mut Context<Self>,
    ) -> Option<Section> {
        self.vcs.cwd.as_ref()?;
        let status = self.vcs.status.as_ref();
        let is_repo = status.is_none_or(|status| status.local.is_repo);
        // `BranchToolbarBranchSelector` as a select: a draft's base ("From main (remote)") or the branch.
        let (branch, _, label) = self.branch_trigger();
        let mut rows: Vec<AnyElement> = Vec::new();
        if is_repo && self.details_item_shown("branch") && branch.is_some() {
            let trigger = select_row("details-branch", tokens, colors, cx)
                .disabled(!self.branch_picker_ready())
                .child(
                    h_flex()
                        .flex_1()
                        .min_w_0()
                        .gap(px(10.))
                        .child(icon("icons/git-branch.svg").with_size(px(16.)).text_color(tokens.muted_foreground))
                        .child(div().flex_1().min_w_0().truncate().text_left().child(label)),
                )
                .child(select_chevron(tokens));
            rows.push(
                div()
                    .w_full()
                    .child(self.branch_popover("details-branch-popover", trigger, true, tokens, cx))
                    .into_any_element(),
            );
        }
        let Some(status) = status else {
            return (!rows.is_empty()).then(|| Section {
                height: rows.len() as f32 * ROW_HEIGHT,
                element: v_flex().children(rows).into_any_element(),
                min_density: Density::Essential,
            });
        };
        let (actions_shown, changes_shown) =
            (self.details_item_shown("gitActions"), self.details_item_shown("changes"));
        if status.local.is_repo {
            let quick = git::quick_action(Some(status), self.vcs.running().is_some());
            let (insertions, deletions, _) = git::changes_totals(status);
            let quiet_empty_draft = matches!(self.target, Some(Target::Draft { .. }))
                && self.vcs.running().is_none()
                && quick.disabled()
                && insertions == 0
                && deletions == 0;
            if actions_shown && !quiet_empty_draft && (density == Density::Full || self.vcs.running().is_some()) {
                rows.push(self.git_actions_row(status, &quick, tokens, colors, cx));
            }
            if changes_shown && !quiet_empty_draft {
                rows.push(self.changes_row(status, tokens, colors, cx));
            }
        } else {
            if !actions_shown {
                return (!rows.is_empty()).then(|| Section {
                    height: rows.len() as f32 * ROW_HEIGHT,
                    element: v_flex().children(rows).into_any_element(),
                    min_density: Density::Essential,
                });
            }
            let running = self.vcs.running().is_some();
            let enabled = !running && self.can_write_source_control();
            rows.push(
                action_row("details-init-git", colors, enabled, cx)
                    .child(icon("icons/git-branch-plus.svg").with_size(px(14.)).text_color(tokens.muted_foreground))
                    .child(div().ml(px(2.)).child(if running { "Initializing..." } else { "Initialize Git" }))
                    .when(enabled, |el| {
                        el.on_click(
                            cx.listener(|this, _, _, cx| this.run_vcs_request("vcs.init", "Initializing...", cx)),
                        )
                    })
                    .into_any_element(),
            );
        }
        (!rows.is_empty()).then(|| Section {
            height: rows.len() as f32 * ROW_HEIGHT,
            element: v_flex().children(rows).into_any_element(),
            min_density: Density::Essential,
        })
    }

    /// The quick action with its menu, or the running action's progress.
    fn git_actions_row(
        &self,
        status: &git::VcsStatus,
        quick: &git::QuickAction,
        tokens: &Tokens,
        colors: &CardColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if let Some(progress) = self.vcs.running().cloned() {
            return row(colors)
                .bg(if tokens.dark { hsla(0., 0., 1., 0.055) } else { hsla(0., 0., 0., 0.035) })
                .child(icon("icons/refresh-cw.svg").with_size(px(16.)).text_color(tokens.muted_foreground))
                .child(div().flex_1().min_w_0().truncate().child(progress))
                .into_any_element();
        }
        let can_write = self.can_write_source_control();
        let disabled = quick.disabled() || !can_write;
        let kind = quick.kind.clone();
        // `quickActionDisabledReason`: the missing grant first, then the action's own hint.
        let hint = match &quick.kind {
            _ if !can_write => Some(SharedString::from(NO_SOURCE_CONTROL_WRITE)),
            QuickKind::Hint(hint) => Some(SharedString::from(hint.clone())),
            _ => None,
        };
        let view = cx.entity();
        let items = git::menu_actions(status, false);
        let can_publish = can_write && status.local.is_repo && !status.local.has_primary_remote;
        let build = move |mut menu: PopupMenu, window: &mut Window, cx: &mut Context<PopupMenu>| {
            // Built as the menu opens: refresh the status, like `requestVcsStatusRefresh` on open.
            view.update(cx, super::Workspace::refresh_vcs);
            for item in &items {
                let action = item.action;
                menu = menu.item(
                    PopupMenuItem::new(item.label.clone())
                        .icon(icon(item.icon))
                        .disabled(!can_write || !item.enabled)
                        .on_click(window.listener_for(&view, move |this, _, window, cx| match action {
                            "commit" => this.open_commit_dialog(window, cx),
                            action => this.request_git_action(GitRequest::new(action), window, cx),
                        })),
                );
            }
            if can_publish {
                menu =
                    menu.item(PopupMenuItem::new("Publish repository").icon(icon("icons/cloud-upload.svg")).on_click(
                        window.listener_for(&view, |this, _, window, cx| this.open_publish_dialog(window, cx)),
                    ));
            }
            menu
        };
        split_row("details-git", colors, cx)
            .child(
                split_primary("details-git-primary", colors, !disabled, cx)
                    // `aria-disabled` with its reason: still a tab stop.
                    .when(disabled, |el| keyboard_button(el, colors))
                    .child(icon(quick.icon()).with_size(px(16.)).text_color(tokens.muted_foreground))
                    .child(div().min_w_0().truncate().child(quick.label.clone()))
                    .when_some(hint, |el, hint| {
                        el.tooltip(move |window, cx| Tooltip::new(hint.clone()).build(window, cx))
                    })
                    .on_click(cx.listener(move |this, _, window, cx| match &kind {
                        _ if !this.can_write_source_control() => {}
                        QuickKind::Run(action) => this.request_git_action(GitRequest::new(action), window, cx),
                        QuickKind::Pull => this.run_vcs_request("vcs.pull", "Pulling latest changes...", cx),
                        QuickKind::Publish => this.open_publish_dialog(window, cx),
                        QuickKind::Hint(_) => {}
                    })),
            )
            .child(split_hairline("details-git", colors))
            .child(
                split_menu("details-git-menu", tokens, colors, cx)
                    .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, window, cx| {
                        build(menu.min_w(px(264.)), window, cx)
                    }),
            )
            .into_any_element()
    }

    /// The commit dialog: an optional message; blank lets the server write one.
    fn open_commit_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_kit::component::WindowExt as _;
        use gpui_kit::component::input::{Input, InputState};
        use gpui_kit::{AppContext as _, Focusable as _};
        let message = cx.new(|cx| InputState::new(window, cx).placeholder("Leave empty to auto-generate"));
        let branch =
            self.vcs.status.as_ref().and_then(|s| s.local.ref_name.clone()).unwrap_or("(detached HEAD)".into());
        let view = cx.entity();
        let focus = std::cell::Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            if focus.replace(false) {
                message.focus_handle(cx).focus(window, cx);
            }
            let (message, view) = (message.clone(), view.clone());
            let (can_write, can_branch) = {
                let this = view.read(cx);
                (this.can_write_source_control(), this.can_change_thread_branch())
            };
            let on_new_branch = {
                let (message, view) = (message.clone(), view.clone());
                move |_: &gpui_kit::ClickEvent, window: &mut Window, cx: &mut App| {
                    let text = message.read(cx).value().trim().to_owned();
                    view.update(cx, |this, cx| this.run_commit(text, true, cx));
                    window.close_dialog(cx);
                }
            };
            dialog
                .title("Commit changes")
                .w(px(512.))
                .child(
                    v_flex()
                        .gap_3()
                        .child(
                            div()
                                .text_sm()
                                .text_color(theme::tokens(cx).muted_foreground)
                                .child("Review and confirm your commit. Leave the message blank to auto-generate one."),
                        )
                        .child(field("Branch", div().text_sm().child(branch.clone())))
                        .child(field("Commit message (optional)", Input::new(&message))),
                )
                .footer({
                    use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
                    DialogFooter::new()
                        .child(DialogClose::new().trigger(|button| button.outline().label("Cancel")))
                        .child(
                            Button::new("commit-new-branch")
                                .outline()
                                .label("Commit on new branch")
                                .disabled(!can_branch)
                                .on_click(on_new_branch),
                        )
                        .child(
                            DialogAction::new()
                                .child(Button::new("commit-confirm").primary().label("Commit").disabled(!can_write)),
                        )
                })
                .on_ok(move |_, _, cx: &mut App| {
                    let text = message.read(cx).value().trim().to_owned();
                    view.update(cx, |this, cx| this.run_commit(text, false, cx));
                    true
                })
        });
    }

    /// `runDialogAction`, or `runDialogActionOnNewBranch` with `feature_branch`; neither asks
    /// about the default ref.
    fn run_commit(&mut self, message: String, feature_branch: bool, cx: &mut Context<Self>) {
        let commit_message = (!message.is_empty()).then_some(message);
        self.run_git_action(GitRequest { action: "commit", commit_message, feature_branch }, cx);
    }

    /// The default-ref confirmation: Abort, continue on `branch`, or continue on a feature branch.
    pub(super) fn confirm_default_ref(
        &mut self,
        request: GitRequest,
        branch: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use gpui_kit::component::WindowExt as _;
        use gpui_kit::component::dialog::{DialogClose, DialogFooter};
        let Some(status) = self.vcs.status.as_ref() else { return };
        let has_changes = status.local.has_working_tree_changes;
        let includes_commit = matches!(request.action, "commit" | "commit_push" | "commit_push_pr")
            && (request.action == "commit" || has_changes || request.feature_branch);
        let copy = git::default_ref_copy(request.action, &branch, includes_commit, status);
        let unavailable: Option<SharedString> = git::feature_branch_unavailable(&branch, has_changes).map(Into::into);
        let (title, description, continue_label): (SharedString, SharedString, SharedString) =
            (copy.title.into(), copy.description.into(), copy.continue_label.into());
        let view = cx.entity();
        window.open_dialog(cx, move |dialog, _, cx| {
            let muted = theme::tokens(cx).muted_foreground;
            let (can_write, can_branch) = {
                let this = view.read(cx);
                (this.can_write_source_control(), this.can_change_thread_branch() && unavailable.is_none())
            };
            let run = |feature_branch: bool| {
                let (view, request) = (view.clone(), request.clone());
                move |_: &gpui_kit::ClickEvent, window: &mut Window, cx: &mut App| {
                    let request = GitRequest { feature_branch, ..request.clone() };
                    view.update(cx, |this, cx| this.run_git_action(request, cx));
                    window.close_dialog(cx);
                }
            };
            dialog
                .title(title.clone())
                .w(px(576.))
                .child(
                    v_flex()
                        .gap_2()
                        .child(div().text_sm().text_color(muted).child(description.clone()))
                        .children(unavailable.clone().map(|reason| div().text_xs().text_color(muted).child(reason))),
                )
                .footer(
                    DialogFooter::new()
                        .child(
                            div().mr_auto().child(DialogClose::new().trigger(|button| button.outline().label("Abort"))),
                        )
                        .child(
                            Button::new("default-ref-continue")
                                .outline()
                                .label(continue_label.clone())
                                .disabled(!can_write)
                                .on_click(run(false)),
                        )
                        .child(
                            Button::new("default-ref-feature-branch")
                                .primary()
                                .label("Check out feature branch & continue")
                                .disabled(!can_branch)
                                .on_click(run(true)),
                        ),
                )
        });
    }

    /// `PublishRepositoryDialog`: the first signed-in source-control provider, `account/` prefilled.
    fn open_publish_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.client() else { return };
        cx.spawn_in(window, async move |this, cx| {
            let discovery = client.request("server.discoverSourceControl", json!({})).await;
            let _ = this.update_in(cx, |this, window, cx| match discovery {
                Ok(discovery) => match publish_provider(&discovery) {
                    Some((kind, label, account)) => this.show_publish_dialog(kind, label, account, window, cx),
                    None => {
                        this.error = Some("No signed-in source control provider. Set one up in Settings.".into());
                        cx.notify();
                    }
                },
                Err(error) => {
                    this.error = Some(error.to_string().into());
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn show_publish_dialog(
        &mut self,
        provider: String,
        label: String,
        account: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use gpui_kit::component::WindowExt as _;
        use gpui_kit::component::input::{Input, InputState};
        use gpui_kit::{AppContext as _, Focusable as _};
        let prefill = account.map(|account| format!("{account}/")).unwrap_or_default();
        let repository =
            cx.new(|cx| InputState::new(window, cx).default_value(prefill).placeholder("owner/repository"));
        let public = std::rc::Rc::new(std::cell::Cell::new(false));
        // Advanced: the remote to add (`origin`) and its protocol (SSH unless HTTPS is picked).
        let remote = cx.new(|cx| InputState::new(window, cx).default_value("origin").placeholder("origin"));
        let https = std::rc::Rc::new(std::cell::Cell::new(false));
        let advanced = std::rc::Rc::new(std::cell::Cell::new(false));
        let view = cx.entity();
        let focus = std::cell::Cell::new(true);
        let title: SharedString = format!("Publish to {label}").into();
        window.open_dialog(cx, move |dialog, window, cx| {
            if focus.replace(false) {
                repository.focus_handle(cx).focus(window, cx);
            }
            let tokens = theme::tokens(cx);
            let choice = |id: &'static str, text: &'static str, cell: &std::rc::Rc<std::cell::Cell<bool>>, value| {
                let cell = cell.clone();
                Button::new(id)
                    .label(text)
                    .small()
                    .map(|b| if cell.get() == value { b.primary() } else { b.outline() })
                    .on_click(move |_, window, _| {
                        cell.set(value);
                        window.refresh();
                    })
            };
            let small_field = |label: &'static str, input: AnyElement| {
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(6.))
                    .child(div().text_xs().font_weight(gpui_kit::FontWeight::MEDIUM).child(label))
                    .child(input)
            };
            let open = advanced.get();
            let toggle = {
                let advanced = advanced.clone();
                let hover = tokens.foreground;
                h_flex()
                    .id("publish-advanced")
                    .gap(px(4.))
                    .text_xs()
                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                    .text_color(tokens.muted_foreground)
                    .cursor_pointer()
                    .hover(move |style| style.text_color(hover))
                    .child(
                        icon(if open { "icons/chevron-down.svg" } else { "icons/chevron-right.svg" })
                            .with_size(px(14.)),
                    )
                    .child("Advanced")
                    .on_click(move |_, window, _| {
                        advanced.set(!advanced.get());
                        window.refresh();
                    })
            };
            let options = open.then(|| {
                h_flex()
                    .mt_3()
                    .gap_3()
                    .items_start()
                    .child(small_field("Remote", Input::new(&remote).into_any_element()))
                    .child(small_field(
                        "Protocol",
                        h_flex()
                            .gap_2()
                            .child(choice("publish-ssh", "SSH", &https, false))
                            .child(choice("publish-https", "HTTPS", &https, true))
                            .into_any_element(),
                    ))
            });
            let visibility = h_flex()
                .gap_2()
                .child(choice("publish-private", "Private", &public, false))
                .child(choice("publish-public", "Public", &public, true));
            let (repository, remote, public, https, view, provider) =
                (repository.clone(), remote.clone(), public.clone(), https.clone(), view.clone(), provider.clone());
            dialog
                .title(title.clone())
                .w(px(448.))
                .child(
                    v_flex()
                        .gap_3()
                        .child(field("Repository", Input::new(&repository)))
                        .child(field("Visibility", visibility))
                        .child(v_flex().child(toggle).children(options)),
                )
                .footer(dialog_footer("publish-confirm", "Publish"))
                .on_ok(move |_, _, cx: &mut App| {
                    let repository = repository.read(cx).value().trim().to_owned();
                    if repository.is_empty() || repository.ends_with('/') {
                        return false;
                    }
                    let options = PublishOptions {
                        visibility: if public.get() { "public" } else { "private" },
                        remote_name: remote.read(cx).value().trim().to_owned(),
                        protocol: if https.get() { "https" } else { "ssh" },
                    };
                    let provider = provider.clone();
                    view.update(cx, |this, cx| this.publish_repository(provider, repository, options, cx));
                    true
                })
        });
    }

    /// `sourceControl.publishRepository`; a blank remote name is `origin`, as on the web.
    fn publish_repository(
        &mut self,
        provider: String,
        repository: String,
        options: PublishOptions,
        cx: &mut Context<Self>,
    ) {
        if !self.can_write_source_control() {
            return;
        }
        let Some(client) = self.vcs_client() else { return };
        let Some(checkout) = self.vcs.start("Publishing repository...") else { return };
        let remote_name = if options.remote_name.is_empty() { "origin".to_owned() } else { options.remote_name };
        let input = json!({
            "cwd": checkout.1,
            "provider": provider,
            "repository": repository,
            "visibility": options.visibility,
            "remoteName": remote_name,
            "protocol": options.protocol,
        });
        cx.spawn(async move |this, cx| {
            let result = client.request("sourceControl.publishRepository", input).await;
            let _ =
                this.update(cx, |this, cx| this.finish_vcs(client, checkout, result.err().map(|e| e.to_string()), cx));
        })
        .detach();
        cx.notify();
    }

    /// `Changes  +N -M`: opens the diff in the right panel.
    fn changes_row(
        &self,
        status: &git::VcsStatus,
        tokens: &Tokens,
        colors: &CardColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (insertions, deletions, base) = git::changes_totals(status);
        let scheme = git::DiffColorScheme::from_settings(&self.client_settings.values);
        let addition = if insertions > 0 { git::addition_color(tokens, scheme) } else { tokens.muted_foreground };
        let deletion = if deletions > 0 { git::deletion_color(tokens, scheme) } else { tokens.muted_foreground };
        // `changesBaseLabel`: working-tree totals have no base.
        let base_label: SharedString = base.map_or_else(|| "Uncommitted".into(), |base| format!("vs {base}").into());
        action_row("details-changes", colors, true, cx)
            .child(icon("icons/file-diff.svg").with_size(px(16.)).text_color(tokens.muted_foreground))
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .items_baseline()
                    .gap(px(6.))
                    .child(div().flex_none().child("Changes"))
                    .child({
                        let tip = base_label.clone();
                        div()
                            .id("details-changes-base")
                            .min_w_0()
                            .truncate()
                            .font_weight(gpui_kit::FontWeight::NORMAL)
                            .text_color(colors.muted)
                            .child(base_label)
                            .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                    })
                    .child(
                        h_flex()
                            .ml_auto()
                            .flex_none()
                            .items_baseline()
                            .gap(px(8.))
                            .pl(px(8.))
                            .font_family(tokens.fonts.mono)
                            .child(div().text_color(addition).child(format!("+{}", git::group_thousands(insertions))))
                            .child(div().text_color(deletion).child(format!("-{}", git::group_thousands(deletions)))),
                    ),
            )
            .on_click(cx.listener(|this, _, window, cx| this.open_changes(window, cx)))
            .into_any_element()
    }

    /// `ThreadRelationshipsPanel` from the shell: the parent thread, running
    /// subagents, and settled ones folded under "Previous agents".
    fn lineage_section(&self, tokens: &Tokens, colors: &CardColors, cx: &mut Context<Self>) -> Option<Section> {
        let Some(Target::Thread { id, .. }) = &self.target else { return None };
        let thread = self.shell.threads.iter().find(|thread| &thread.id == id)?;
        let parent = thread.lineage.parent_thread_id.as_ref().and_then(|parent| self.thread_shell(parent));
        let (previous, active): (Vec<&ThreadShell>, Vec<&ThreadShell>) = self
            .shell
            .threads
            .iter()
            .filter(|child| {
                child.lineage.parent_thread_id.as_ref() == Some(id)
                    && child.lineage.relationship_to_parent.as_deref() == Some("subagent")
            })
            .partition(|child| is_settled_status(&child.status));
        let can_detach = self
            .open_projection()
            .and_then(|projection| projection.provider_session())
            .is_some_and(|session| !matches!(session.status.as_str(), "stopped" | "error"));
        let delegated = thread.delegated_from.as_ref();
        if parent.is_none() && active.is_empty() && previous.is_empty() && !can_detach && delegated.is_none() {
            return None;
        }
        let relationship_row = |thread: &ThreadShell, path: &'static str, cx: &mut Context<Self>| {
            let thread_id = thread.id.clone();
            row(colors)
                .id(ElementId::Name(format!("details-lineage-{thread_id}").into()))
                .cursor_pointer()
                .hover(|style| style.bg(colors.hover))
                .child(icon(path).with_size(px(16.)).text_color(tokens.muted_foreground))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .line_height(px(16.))
                        .text_color(tokens.foreground.opacity(0.85))
                        .child(thread.title.clone()),
                )
                .on_click(cx.listener(move |this, _, window, cx| this.select_thread(thread_id.clone(), window, cx)))
        };
        let heading: SharedString = match active.len() {
            0 => "Lineage".into(),
            running => format!("Lineage · {running} running").into(),
        };
        let mut rows = 0usize;
        let show_heading = parent.is_some() || !active.is_empty() || can_detach || delegated.is_some();
        let mut section = v_flex().when(show_heading, |el| {
            let actions = can_detach.then(|| self.lineage_menu(tokens, colors, cx));
            el.child(section_heading(heading, tokens).children(actions))
        });
        if let Some(from) = delegated {
            section = section.child(self.delegated_from_row(from, thread, tokens, colors, cx));
            rows += 1;
        }
        if let Some(parent) = parent {
            section = section.child(relationship_row(parent, "icons/corner-left-up.svg", cx));
            rows += 1;
        }
        for child in &active {
            section = section.child(relationship_row(child, "icons/bot.svg", cx));
            rows += 1;
        }
        if !previous.is_empty() {
            let expanded = self.lineage_expanded.as_ref() == Some(id);
            let thread_id = id.clone();
            rows += 1;
            section = section.child(
                row(colors)
                    .id("details-previous-agents")
                    .cursor_pointer()
                    .hover(|style| style.bg(colors.hover))
                    .child(icon("icons/bot.svg").with_size(px(16.)).text_color(tokens.muted_foreground))
                    .child(div().flex_1().min_w_0().truncate().child("Previous agents"))
                    .when(!expanded, |el| {
                        el.child(
                            div()
                                .flex_none()
                                .text_xs()
                                .font_weight(gpui_kit::FontWeight::NORMAL)
                                .text_color(tokens.muted_foreground)
                                .child(previous.len().to_string()),
                        )
                    })
                    .child(
                        icon("icons/chevron-down.svg")
                            .with_size(px(16.))
                            .text_color(tokens.muted_foreground)
                            .when(!expanded, |icon| icon.rotate(gpui_kit::radians(-std::f32::consts::FRAC_PI_2))),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.lineage_expanded =
                            (this.lineage_expanded.as_ref() != Some(&thread_id)).then(|| thread_id.clone());
                        cx.notify();
                    })),
            );
            if expanded {
                for child in &previous {
                    section = section.child(relationship_row(child, "icons/bot.svg", cx));
                    rows += 1;
                }
            }
        }
        Some(Section {
            height: rows as f32 * ROW_HEIGHT + if show_heading { HEADING_HEIGHT } else { 0. },
            element: section.into_any_element(),
            min_density: Density::Full,
        })
    }

    /// `DelegatedFromRow`: the thread on a linked environment whose agent delegated this one.
    /// It opens only when this client is connected to that environment.
    fn delegated_from_row(
        &self,
        from: &DelegatedFrom,
        thread: &ThreadShell,
        tokens: &Tokens,
        colors: &CardColors,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Stateful<gpui_kit::Div> {
        let parent = self
            .shell
            .threads
            .iter()
            .find(|parent| parent.id == from.thread_id && *parent.environment_id == *from.environment_id);
        let title = parent.map_or(&from.title, |parent| &parent.title).clone();
        let reachable = self.session(&from.environment_id).is_some();
        let label = self
            .env_info(&from.environment_id)
            .and_then(|info| info.environment_label.clone())
            .or_else(|| thread.link_origin.as_ref().map(|origin| origin.label.clone()));
        let hint: SharedString = if reachable {
            "Open parent agent".into()
        } else {
            format!(
                "Delegated from {}. Connect to it to open the parent thread.",
                label.as_deref().unwrap_or("a linked environment")
            )
            .into()
        };
        let (environment, thread_id) = (SharedString::from(from.environment_id.clone()), from.thread_id.clone());
        row(colors)
            .id("details-lineage-delegated-from")
            .when(reachable, |el| {
                el.cursor_pointer().hover(|style| style.bg(colors.hover)).on_click(cx.listener(
                    move |this, _, window, cx| {
                        this.select_thread_in(environment.clone(), thread_id.clone(), window, cx);
                    },
                ))
            })
            .tooltip(move |window, cx| Tooltip::new(hint.clone()).build(window, cx))
            .child(icon("icons/corner-left-up.svg").with_size(px(16.)).text_color(tokens.muted_foreground))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .line_height(px(16.))
                    .text_color(tokens.foreground.opacity(0.85))
                    .child(title),
            )
            .children(label.map(|label| {
                div()
                    .flex_none()
                    .text_size(px(11.))
                    .font_weight(gpui_kit::FontWeight::NORMAL)
                    .text_color(tokens.muted_foreground)
                    .child(format!("on {label}"))
            }))
    }

    /// The Lineage heading's "…" menu: Disconnect agent session (`provider-session.detach`).
    fn lineage_menu(&self, tokens: &Tokens, colors: &CardColors, cx: &mut Context<Self>) -> AnyElement {
        let view = cx.entity();
        Button::new("details-lineage-more")
            .custom(
                ButtonCustomVariant::new(cx)
                    .foreground(tokens.muted_foreground)
                    .hover(colors.hover)
                    .active(colors.hover),
            )
            .xsmall()
            .icon(icon("icons/ellipsis.svg"))
            .size(px(24.))
            .rounded(px(6.))
            .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, window, _| {
                menu.min_w(px(240.)).item(
                    PopupMenuItem::new("Disconnect agent session")
                        .icon(icon("icons/unplug.svg"))
                        .on_click(window.listener_for(&view, |this, _, _, cx| this.detach_provider_sessions(cx))),
                )
            })
            .into_any_element()
    }

    /// `stopThreadSession`: detaches every provider session of the open thread.
    fn detach_provider_sessions(&mut self, cx: &mut Context<Self>) {
        let Some(projection) = self.open_projection() else { return };
        let thread_id = projection.thread.id.clone();
        let command_id = new_id();
        let commands: Vec<Value> = projection
            .provider_sessions
            .iter()
            .map(|session| {
                json!({
                    "type": "provider-session.detach",
                    "commandId": format!("{command_id}:detach:{}", session.id),
                    "threadId": thread_id,
                    "providerSessionId": session.id,
                    "reason": "client-requested",
                })
            })
            .collect();
        for command in commands {
            self.dispatch(command, cx);
        }
    }

    fn thread_shell(&self, id: &str) -> Option<&ThreadShell> {
        self.shell.threads.iter().find(|thread| thread.id == id)
    }

    /// The selected provider's limit windows; each bar reads as room left.
    fn usage_section(&self, tokens: &Tokens, colors: &CardColors) -> Option<Section> {
        let selection = self.composer_model()?;
        let limits = self.server_info().provider(&selection.instance_id)?.usage_limits.as_ref()?;
        let meters = limit_meters(limits, tokens);
        Some(Section {
            height: meters.len() as f32 * METER_ROW_HEIGHT,
            element: meter_grid(meters, colors),
            min_density: Density::Essential,
        })
    }

    fn context_section(&self, tokens: &Tokens, colors: &CardColors) -> Option<Section> {
        let usage = self.open_projection()?.context_usage()?;
        Some(Section {
            height: METER_ROW_HEIGHT,
            element: meter_grid(vec![context_meter(usage, tokens)], colors),
            min_density: Density::Essential,
        })
    }

    pub(super) fn open_projection(&self) -> Option<&crate::model::ThreadProjection> {
        match &self.target {
            Some(Target::Thread { .. }) => self.thread.as_ref()?.state.projection.as_ref(),
            _ => None,
        }
    }

    pub(super) fn target_project(&self) -> Option<&crate::model::ProjectShell> {
        let project_id = match self.target.as_ref()? {
            Target::Thread { id, .. } => &self.shell.threads.iter().find(|thread| &thread.id == id)?.project_id,
            Target::Draft { project_id, .. } => project_id,
        };
        self.shell.projects.iter().find(|project| &project.id == project_id)
    }
}

/// `ThreadDetailsItemsMenuItems`: the card's context menu, its empty state and Settings share it.
pub(in crate::app) fn thread_details_items_menu(
    workspace: WeakEntity<Workspace>,
) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
    move |menu, window, cx| {
        let Some(entity) = workspace.upgrade() else { return menu };
        let settings = entity.read(cx).client_settings.values.clone();
        let mut menu = menu.min_w(px(220.)).label("Show in thread details");
        for (id, label, _) in thread_details::ITEMS {
            let shown = thread_details::visible(&settings, id);
            let patch = thread_details::set_item(&settings, id, !shown);
            let toggle = window
                .listener_for(&entity, move |this, _, window, cx| this.update_settings(patch.clone(), window, cx));
            menu = menu.item(PopupMenuItem::new(label).checked(shown).on_click(toggle));
        }
        let reset = window
            .listener_for(&entity, |this, _, window, cx| this.update_settings(thread_details::reset(), window, cx));
        menu.separator().item(
            PopupMenuItem::new("Reset to default").disabled(thread_details::is_default(&settings)).on_click(reset),
        )
    }
}

/// `getPublishProviderReadiness`: the first available, signed-in provider as (kind, label, account).
fn publish_provider(discovery: &Value) -> Option<(String, String, Option<String>)> {
    discovery["sourceControlProviders"].as_array()?.iter().find_map(|provider| {
        let auth = &provider["auth"];
        let ready =
            provider["status"].as_str() == Some("available") && auth["status"].as_str() != Some("unauthenticated");
        if !ready {
            return None;
        }
        let account = auth["account"]["value"].as_str().map(str::to_owned);
        Some((provider["kind"].as_str()?.to_owned(), provider["label"].as_str()?.to_owned(), account))
    })
}

/// The publish dialog's visibility and its Advanced remote and protocol.
struct PublishOptions {
    visibility: &'static str,
    remote_name: String,
    protocol: &'static str,
}

/// Cancel and the confirming action; the action runs the dialog's `on_ok`.
fn dialog_footer(id: &'static str, label: &'static str) -> impl IntoElement {
    use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
    DialogFooter::new()
        .child(DialogClose::new().trigger(|button| button.outline().label("Cancel")))
        .child(DialogAction::new().child(Button::new(id).primary().label(label)))
}

fn field(label: &'static str, input: impl IntoElement) -> impl IntoElement {
    v_flex().gap(px(6.)).child(div().text_sm().font_weight(gpui_kit::FontWeight::MEDIUM).child(label)).child(input)
}

/// The statuses `ThreadRelationshipsControl.tsx` files under "Previous agents".
fn is_settled_status(status: &str) -> bool {
    matches!(status, "completed" | "failed" | "error" | "cancelled" | "interrupted" | "idle")
}

/// `THREAD_DETAILS_PANEL_LOCKED_ROW_CLASS`: 32px, 10px inset and gap, 13px medium.
fn row(colors: &CardColors) -> gpui_kit::Div {
    h_flex()
        .h(px(ROW_HEIGHT))
        .w_full()
        .flex_none()
        .rounded(px(8.))
        .gap(px(10.))
        .px(px(10.))
        .text_size(px(13.))
        .font_weight(gpui_kit::FontWeight::MEDIUM)
        .text_color(colors.label)
}

/// `part="row"`: a whole-row button with the panel's hover fill; a disabled one is dimmed and
/// out of the tab order.
fn action_row(
    id: &'static str,
    colors: &CardColors,
    enabled: bool,
    _: &mut Context<Workspace>,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    let hover = colors.hover;
    let row = row(colors).id(id);
    if enabled {
        keyboard_button(row.cursor_pointer().hover(move |style| style.bg(hover)), colors)
    } else {
        row.opacity(0.64)
    }
}

/// `ThreadDetailsControl` as a keyboard button: a tab stop that Enter and Space click, with
/// `focus-visible:ring-2 ring-inset`. The ring takes 2px of the row's 10px inset, so nothing moves.
fn keyboard_button(
    element: gpui_kit::Stateful<gpui_kit::Div>,
    colors: &CardColors,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    let ring = colors.ring;
    element
        .tab_index(0)
        .px(px(8.))
        .border_2()
        .border_color(gpui_kit::transparent_black())
        .focus_visible(move |style| style.border_color(ring))
}

/// `THREAD_DETAILS_PANEL_SPLIT_GROUP_CLASS`: the pair shares one hover fill and shows its hairline.
fn split_row(id: &'static str, colors: &CardColors, _: &mut Context<Workspace>) -> gpui_kit::Stateful<gpui_kit::Div> {
    let hover = colors.hover;
    h_flex().id(id).group(id).h(px(ROW_HEIGHT)).w_full().flex_none().rounded(px(8.)).hover(move |style| style.bg(hover))
}

/// The split's primary half; each half stacks its own fill over the group's.
fn split_primary(
    id: &'static str,
    colors: &CardColors,
    enabled: bool,
    _: &mut Context<Workspace>,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    let hover = colors.hover;
    h_flex()
        .id(id)
        .flex_1()
        .min_w_0()
        .h_full()
        .gap(px(10.))
        .px(px(10.))
        .rounded_l(px(8.))
        .text_size(px(13.))
        .font_weight(gpui_kit::FontWeight::MEDIUM)
        .text_color(colors.label)
        .when(enabled, |el| keyboard_button(el.cursor_pointer().hover(move |style| style.bg(hover)), colors))
        .when(!enabled, |el| el.opacity(0.64))
}

/// The 1×16 `bg-border/65` hairline between the halves, shown while the pair is hovered.
fn split_hairline(group: &'static str, colors: &CardColors) -> impl IntoElement {
    let line = colors.divider;
    div().w(px(1.)).h(px(16.)).flex_none().group_hover(group, move |style| style.bg(line))
}

/// `ThreadDetailsControl part="select"`: one trigger holding the row's label and its chevron.
fn select_row(id: &'static str, tokens: &Tokens, colors: &CardColors, cx: &mut Context<Workspace>) -> Button {
    Button::new(id)
        .custom(
            ButtonCustomVariant::new(cx)
                .color(tokens.background.opacity(0.))
                .foreground(colors.label)
                .hover(colors.hover)
                .active(colors.hover),
        )
        .w_full()
        .h(px(ROW_HEIGHT))
        .pl(px(10.))
        .pr_0()
        .gap_0()
        .rounded(px(8.))
        .text_size(px(13.))
        .font_weight(gpui_kit::FontWeight::MEDIUM)
}

/// The select's 32px chevron box (`[data-slot=select-icon]`).
fn select_chevron(tokens: &Tokens) -> gpui_kit::Div {
    div()
        .w(px(32.))
        .h_full()
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .child(icon("icons/chevron-down.svg").with_size(px(16.)).text_color(tokens.muted_foreground))
}

/// The 32px chevron half that opens the row's menu.
fn split_menu(id: &'static str, tokens: &Tokens, colors: &CardColors, cx: &mut Context<Workspace>) -> Button {
    Button::new(id)
        .custom(
            ButtonCustomVariant::new(cx).foreground(tokens.muted_foreground).hover(colors.hover).active(colors.hover),
        )
        .w(px(32.))
        .h(px(ROW_HEIGHT))
        .rounded_r(px(8.))
        .rounded_l(px(0.))
        .child(icon("icons/chevron-down.svg").with_size(px(16.)))
}

fn section_heading(title: SharedString, tokens: &Tokens) -> gpui_kit::Div {
    h_flex()
        .mb_1()
        .min_h(px(32.))
        .px(px(10.))
        .gap_2()
        .justify_between()
        .text_size(px(11.))
        .line_height(px(16.))
        .font_weight(gpui_kit::FontWeight::MEDIUM)
        .text_color(tokens.muted_foreground)
        .child(title)
}

fn limit_meters(limits: &UsageLimits, tokens: &Tokens) -> Vec<Meter> {
    if limits.unavailable.is_some() || limits.windows.is_empty() {
        return vec![Meter {
            id: "usage-unavailable".into(),
            label: "Limits".into(),
            fill: None,
            fill_color: tokens.foreground,
            tick: None,
            value: "Unavailable".into(),
            value_color: tokens.muted_foreground,
            tooltip: "Could not read limits.".into(),
        }];
    }
    limits
        .windows
        .iter()
        .map(|window| {
            let left = window.remaining_percent();
            let (fill_color, value_color) = tone(left <= 10, left <= 20, tokens.foreground.opacity(0.8), tokens);
            Meter {
                id: format!("usage-{}", window.id).into(),
                label: window.label.clone().into(),
                fill: Some(left as f32),
                fill_color,
                tick: None,
                value: format!("{left}%").into(),
                value_color,
                tooltip: format!("{}: {left}% left", window.label).into(),
            }
        })
        .collect()
}

/// `ThreadDetailsContext`: the bar fills as context is used; the tick marks auto-compaction.
fn context_meter(usage: &TokenUsage, tokens: &Tokens) -> Meter {
    let max = usage.max_tokens.filter(|max| *max > 0);
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a percentage clamped to 0..=100 and rounded"
    )]
    let used = max.map(|max| (usage.used_tokens as f64 / max as f64 * 100.).clamp(0., 100.).round() as u32);
    let tick = max
        .zip(usage.auto_compact_threshold)
        .filter(|(max, threshold)| *threshold > 0 && threshold < max)
        .map(|(max, threshold)| threshold as f32 / max as f32 * 100.);
    let used_label = format_context_tokens(usage.used_tokens);
    let value = match max {
        Some(max) => format!("{used_label} / {}", format_context_tokens(max)),
        None => used_label.clone(),
    };
    let tooltip = match (used, max) {
        (Some(used), Some(max)) => format!(
            "Context: {used}% used · {} of {} tokens",
            git::group_thousands(usage.used_tokens),
            git::group_thousands(max)
        ),
        _ => format!("Context: {used_label} tokens used"),
    };
    let (fill_color, value_color) = match used {
        Some(used) => tone(used >= 90, used >= 80, tokens.muted_foreground, tokens),
        None => (tokens.muted_foreground, tokens.muted_foreground),
    };
    Meter {
        id: "usage-context".into(),
        label: "Context".into(),
        fill: used.map(|used| used as f32),
        fill_color,
        tick,
        value: value.into(),
        value_color,
        tooltip: tooltip.into(),
    }
}

/// Error and warning tones for a meter's (fill, value) colors.
fn tone(error: bool, warning: bool, fill: Hsla, tokens: &Tokens) -> (Hsla, Hsla) {
    if error {
        (tokens.error, tokens.error_foreground)
    } else if warning {
        (tokens.warning, tokens.warning_foreground)
    } else {
        (fill, tokens.foreground.opacity(0.8))
    }
}

/// The `grid-cols-[auto_minmax(3rem,1fr)_auto] gap-x-3` grid. Columns are
/// flex columns, so every row shares the widest label and value; hover rows
/// sit underneath and own the tooltips.
fn meter_grid(meters: Vec<Meter>, colors: &CardColors) -> AnyElement {
    let hover = colors.hover;
    let rows = meters.iter().enumerate().map(|(index, meter)| {
        let tooltip = meter.tooltip.clone();
        div()
            .id(ElementId::Name(meter.id.clone()))
            .absolute()
            .top(px(index as f32 * METER_ROW_HEIGHT))
            .left_0()
            .right_0()
            .h(px(METER_ROW_HEIGHT))
            .rounded(px(8.))
            .hover(|style| style.bg(hover))
            .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
    });
    let cell = || h_flex().h(px(METER_ROW_HEIGHT));
    let labels = meters.iter().map(|meter| {
        cell().max_w(px(112.)).child(div().min_w_0().truncate().text_color(colors.muted).child(meter.label.clone()))
    });
    let track = colors.track;
    let bars = meters.iter().map(|meter| {
        cell().w_full().children(meter.fill.map(|fill| {
            div()
                .relative()
                .w_full()
                .h(px(3.))
                .rounded_full()
                .bg(track)
                .when(fill > 0., |el| {
                    el.child(
                        div()
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .left_0()
                            .w(relative(fill / 100.))
                            .min_w(px(6.))
                            .rounded_full()
                            .bg(meter.fill_color),
                    )
                })
                .children(meter.tick.map(|tick| {
                    div()
                        .absolute()
                        .top(px(-3.))
                        .left(relative(tick / 100.))
                        .ml(px(-0.5))
                        .w(px(1.))
                        .h(px(9.))
                        .bg(colors.tick)
                }))
        }))
    });
    let values = meters.iter().map(|meter| {
        cell()
            .justify_end()
            .font_weight(gpui_kit::FontWeight::MEDIUM)
            .text_color(meter.value_color)
            .child(meter.value.clone())
    });
    div()
        .relative()
        .children(rows)
        .child(
            h_flex()
                .px(px(10.))
                .gap_3()
                .text_xs()
                .child(v_flex().flex_none().children(labels))
                .child(v_flex().flex_1().min_w(px(48.)).children(bars))
                .child(v_flex().flex_none().items_end().children(values)),
        )
        .into_any_element()
}

/// `formatContextWindowTokens` in `apps/web/src/lib/contextWindow.ts`.
pub(super) fn format_context_tokens(value: u64) -> String {
    let trim = |text: String| text.strip_suffix(".0").map(str::to_owned).unwrap_or(text);
    match value {
        0..1_000 => value.to_string(),
        1_000..10_000 => format!("{}k", trim(format!("{:.1}", value as f64 / 1_000.))),
        10_000..999_500 => format!("{}k", (value as f64 / 1_000.).round()),
        _ => format!("{}m", trim(format!("{:.1}", value as f64 / 1_000_000.))),
    }
}

/// A project script's run from the details card.
/// One saved script in the card's menu.
struct ScriptItem {
    label: SharedString,
    name: SharedString,
    icon: String,
    shortcut: Option<SharedString>,
    run: ScriptRun,
}

/// `projectScriptMenuLabel`: the name with its setup and settle roles.
fn project_script_menu_label(script: &ProjectScript) -> String {
    let roles: Vec<&str> =
        [(script.run_on_worktree_create, "setup"), (script.run_on_settle == Some(true), "on settle")]
            .into_iter()
            .filter_map(|(on, role)| on.then_some(role))
            .collect();
    if roles.is_empty() { script.name.clone() } else { format!("{} ({})", script.name, roles.join(", ")) }
}

/// A script's menu row: the label, its shortcut, and on hover the "Edit {name}" settings button
/// in the shortcut's place, which opens the script's editor instead of running it.
fn script_item_row(
    item: &ScriptItem,
    view: &gpui_kit::Entity<Workspace>,
    popup: &gpui_kit::Entity<PopupMenu>,
) -> impl Fn(&mut Window, &mut App) -> AnyElement + 'static {
    let (label, name, shortcut) = (item.label.clone(), item.name.clone(), item.shortcut.clone());
    let (project_id, script_id) = (item.run.project_id.clone(), item.run.id.clone());
    let (view, popup) = (view.clone(), popup.clone());
    move |_, cx| {
        let tokens = *theme::tokens(cx);
        let group: SharedString = format!("details-script-{script_id}").into();
        let (view, popup, project_id, script_id) = (view.clone(), popup.clone(), project_id.clone(), script_id.clone());
        let tooltip: SharedString = format!("Edit {name}").into();
        let edit = div()
            .id(ElementId::Name(format!("{group}-edit").into()))
            .absolute()
            .right_0()
            .size(px(24.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(6.))
            .invisible()
            .group_hover(group.clone(), gpui_kit::Styled::visible)
            .hover(move |style| style.bg(tokens.accent))
            .child(gpui_kit::component::Icon::new(gpui_kit::assets::IconName::Settings).size(px(14.)))
            .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
            .on_click(move |_, window, cx| {
                cx.stop_propagation();
                view.update(cx, |this, cx| this.open_project_script_editor(&project_id, Some(&script_id), window, cx));
                popup.update(cx, |_, cx| cx.emit(gpui_kit::DismissEvent));
            });
        h_flex()
            .w_full()
            .gap_2()
            .group(group.clone())
            .child(div().flex_1().min_w_0().truncate().child(label.clone()))
            .child(
                h_flex()
                    .relative()
                    .flex_none()
                    .h(px(24.))
                    .min_w(px(24.))
                    .justify_end()
                    .children(shortcut.clone().map(|shortcut| {
                        div()
                            .text_xs()
                            .text_color(tokens.muted_foreground)
                            .group_hover(group.clone(), |style| style.opacity(0.))
                            .child(shortcut)
                    }))
                    .child(edit),
            )
            .into_any_element()
    }
}

#[derive(Clone)]
struct ScriptRun {
    project_id: String,
    id: String,
    /// The whole script, so its run keeps `stopOnSettle` and its preview settings.
    script: std::rc::Rc<ProjectScript>,
}

impl ScriptRun {
    fn new(project_id: &str, script: &ProjectScript) -> Self {
        Self { project_id: project_id.to_owned(), id: script.id.clone(), script: std::rc::Rc::new(script.clone()) }
    }
}

/// `t3code:last-invoked-script-by-project`, kept with this device's client settings.
const LAST_INVOKED_SCRIPT_KEY: &str = "lastInvokedScriptByProject";

/// The card's run button: the script last run in the project, else `primaryProjectScript` (a
/// regular script, then a setup one; a settle-only script is never primary).
fn primary_script<'a>(scripts: &'a [ProjectScript], last_invoked: Option<&str>) -> Option<&'a ProjectScript> {
    let settles = |script: &ProjectScript| script.run_on_settle == Some(true);
    last_invoked
        .and_then(|id| scripts.iter().find(|script| script.id == id))
        .or_else(|| scripts.iter().find(|script| !script.run_on_worktree_create && !settles(script)))
        .or_else(|| scripts.iter().find(|script| !settles(script)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_tokens_format_like_the_web() {
        let cases = [
            (999, "999"),
            (1_000, "1k"),
            (1_234, "1.2k"),
            (9_960, "10k"),
            (10_000, "10k"),
            (345_400, "345k"),
            (999_499, "999k"),
            (999_500, "1m"),
            (1_000_000, "1m"),
            (1_500_000, "1.5m"),
        ];
        for (value, expected) in cases {
            assert_eq!(format_context_tokens(value), expected, "{value}");
        }
    }

    #[test]
    fn the_primary_script_is_the_last_run_else_a_regular_then_a_setup_one() {
        let script = |id: &str, setup: bool, settle: bool| ProjectScript {
            id: id.into(),
            name: id.into(),
            command: id.into(),
            icon: "play".into(),
            run_on_worktree_create: setup,
            run_on_settle: settle.then_some(true),
            stop_on_settle: None,
            run_async: None,
            preview_url: None,
            auto_open_preview: None,
        };
        let primary =
            |scripts: &[ProjectScript], last: Option<&str>| primary_script(scripts, last).map(|s| s.id.clone());
        let scripts = [script("setup", true, false), script("settle", false, true), script("dev", false, false)];
        assert_eq!(primary(&scripts, None).as_deref(), Some("dev"));
        assert_eq!(primary(&scripts, Some("settle")).as_deref(), Some("settle"), "the last run wins");
        assert_eq!(primary(&scripts, Some("deleted")).as_deref(), Some("dev"));
        assert_eq!(primary(&scripts[..2], None).as_deref(), Some("setup"));
        assert_eq!(primary(&scripts[1..2], None), None, "a settle-only script is never primary");

        // The run keeps the script's settle flag and the preview it opens automatically.
        let mut dev = script("dev", false, false);
        dev.stop_on_settle = Some(true);
        dev.preview_url = Some("http://localhost:5173".into());
        let run = |script: &ProjectScript| super::super::RunInTerminal::script("/repo".into(), script);
        assert!(run(&dev).stop_on_settle);
        assert_eq!(run(&dev).preview_url, None, "no autoOpenPreview, no preview");
        dev.auto_open_preview = Some(true);
        assert_eq!(run(&dev).preview_url.as_deref(), Some("http://localhost:5173"));
        dev.preview_url = Some("  ".into());
        assert_eq!(run(&dev).preview_url, None);
    }

    #[test]
    fn details_visibility_is_per_thread_and_keeps_only_collapsed_cards() {
        let mut visibility = DetailsVisibility::default();
        assert!(visibility.update("e:a", |v| v.inline_open = false), "collapsing is stored");
        assert!(!visibility.update("e:b", |v| v.popover_open = true), "a popover is not stored");
        assert_eq!(visibility.get("e:a"), Visibility { inline_open: false, popover_open: false });
        assert_eq!(visibility.get("e:b"), Visibility { inline_open: true, popover_open: true });
        assert_eq!(visibility.get("e:c"), DEFAULT_VISIBILITY, "other threads keep the default");
        assert_eq!(visibility.stored(), json!({"e:a": {"inlineOpen": false, "popoverOpen": false}}));

        assert!(!DetailsVisibility::from_stored(&visibility.stored()).get("e:a").inline_open, "it survives a restart");
        assert_eq!(DetailsVisibility::from_stored(&json!({"e:b": {"inlineOpen": true}})), DetailsVisibility::default());

        assert!(visibility.update("e:a", |v| v.inline_open = true));
        assert!(!visibility.update("e:b", |v| v.popover_open = false));
        assert_eq!(visibility, DetailsVisibility::default(), "back at the default, no entries remain");
    }

    #[test]
    fn density_steps_down_as_the_card_shrinks() {
        assert_eq!(resolve_density(400., 300., 200.), Density::Full);
        assert_eq!(resolve_density(250., 300., 200.), Density::Compact);
        assert_eq!(resolve_density(150., 300., 200.), Density::Essential);
    }

    #[test]
    fn publish_picks_the_first_signed_in_provider() {
        let discovery = json!({"sourceControlProviders": [
            {"kind": "gitlab", "label": "GitLab", "status": "missing", "auth": {"status": "unknown", "account": {"_tag": "None"}}},
            {"kind": "github", "label": "GitHub", "status": "available", "auth": {"status": "unauthenticated", "account": {"_tag": "None"}}},
            {"kind": "forgejo", "label": "Forgejo", "status": "available", "auth": {"status": "authenticated", "account": {"_tag": "Some", "value": "me"}}},
        ]});
        assert_eq!(publish_provider(&discovery), Some(("forgejo".into(), "Forgejo".into(), Some("me".into()))));
        assert_eq!(publish_provider(&json!({"sourceControlProviders": []})), None);
    }
}
