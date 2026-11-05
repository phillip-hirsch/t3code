//! The floating thread details card at the canvas' top right
//! (`chat/ThreadDetailsCard.tsx`, `ThreadDetailsPanel.tsx`): the environment,
//! Open in, project scripts, branch, git actions, Changes, lineage, usage limits
//! and context, at the density the card's height allows.

use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants as _};
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Anchor, AnyElement, App, Context, ElementId, Hsla, InteractiveElement as _, IntoElement, KeyDownEvent,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, WeakEntity, Window, anchored,
    deferred, div, hsla, img, point, px, relative, rgb, svg,
};
use serde_json::{Value, json};

use super::git::{self, QuickKind};
use super::titlebar::{RIGHT_RESIZE_INSET, TITLEBAR_HEIGHT, icon};
use super::{Target, Workspace, new_id};
use crate::model::{ThreadShell, TokenUsage, UsageLimits};
use crate::theme::{self, Tokens};
use crate::thread_details;

const CARD_WIDTH: f32 = 280.;
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
fn editor_label(id: &str) -> &str {
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
fn editor_icon(id: &str, tokens: &Tokens) -> AnyElement {
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

impl Workspace {
    /// Whether the card is on screen, inline or as the popover.
    pub(super) fn details_shown(&self, window: &Window) -> bool {
        self.target.is_some() && if self.details_inline(window) { self.details_open } else { self.details_popover }
    }

    /// The toggle drives whichever presentation the canvas allows.
    pub(super) fn toggle_details(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.details_inline(window) {
            self.details_open = !self.details_open;
        } else {
            self.details_popover = !self.details_popover;
        }
        cx.notify();
    }

    pub(super) fn escape_details_popover(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.details_popover && event.keystroke.key == "escape" {
            self.details_popover = false;
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
        let sections: Vec<Section> = [
            self.workspace_section(&tokens, &colors, cx),
            self.version_control_section(&tokens, &colors, cx),
            self.lineage_section(&tokens, &colors, cx).filter(|_| self.lineage_shown()),
            self.usage_section(&tokens, &colors).filter(|_| self.details_item_shown("usage")),
            self.context_section(&tokens, &colors).filter(|_| self.details_item_shown("context")),
        ]
        .into_iter()
        .flatten()
        .collect();
        let height_at = |density: Density| {
            let heights = sections.iter().filter(|s| s.min_density <= density).map(|s| s.height + SECTION_PADDING);
            heights.sum::<f32>() + 2.
        };
        let density = resolve_density(available, height_at(Density::Full), height_at(Density::Compact));
        // Re-rendered at the chosen density, so hidden rows never build.
        let sections: Vec<AnyElement> = [
            self.workspace_section_at(density, &tokens, &colors, cx),
            self.version_control_section_at(density, &tokens, &colors, cx),
            (density == Density::Full && self.lineage_shown())
                .then(|| self.lineage_section(&tokens, &colors, cx))
                .flatten(),
            self.usage_section(&tokens, &colors).filter(|_| self.details_item_shown("usage")),
            self.context_section(&tokens, &colors).filter(|_| self.details_item_shown("context")),
        ]
        .into_iter()
        .flatten()
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
                            this.details_popover = false;
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
                .then(|| self.environment_row(tokens, colors))
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
    fn environment_row(&self, tokens: &Tokens, colors: &CardColors) -> Option<AnyElement> {
        let label = self.server_info.environment_label.clone()?;
        let machine = match self.server_info.machine_kind() {
            "desktop" | "mac-studio" | "mac-mini" => "icons/monitor.svg",
            "laptop" => "icons/laptop.svg",
            "cloud" => "icons/cloud.svg",
            _ => "icons/server.svg",
        };
        let worktree = self.open_projection().and_then(|p| p.thread.worktree_path.clone());
        let workspace_root = self.target_project().map(|project| project.workspace_root.clone());
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
    fn preferred_editor(&self) -> Option<&str> {
        let available = &self.server_info.available_editors;
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
        let editors = self.server_info.available_editors.clone();
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

    /// `ProjectScriptsControl` in panel mode: run the first script, pick or add one from the menu,
    /// or "Add project script" while the project has none.
    fn scripts_row(&self, tokens: &Tokens, colors: &CardColors, cx: &mut Context<Self>) -> Option<AnyElement> {
        let project = self.target_project()?;
        let project_id = project.id.clone();
        let scripts: Vec<(SharedString, SharedString)> =
            super::pages::settings::code::projects::scripts::resolve_project_scripts(
                &self.server_info.settings,
                project,
            )
            .into_iter()
            .map(|script| (script.name.into(), script.command.into()))
            .collect();
        let Some((name, command)) = scripts.first().cloned() else {
            return Some(
                action_row("details-add-script", colors, cx)
                    .child(icon("icons/plus.svg").with_size(px(14.)).text_color(tokens.muted_foreground))
                    .child(div().ml(px(2.)).truncate().child("Add project script"))
                    .tooltip(|window, cx| Tooltip::new("Add project script").build(window, cx))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_project_script_editor(&project_id, None, window, cx)
                    }))
                    .into_any_element(),
            );
        };
        let view = cx.entity();
        let build = move |mut menu: PopupMenu, window: &mut Window, _: &mut Context<PopupMenu>| {
            for (name, command) in &scripts {
                let command = command.clone();
                menu = menu.item(PopupMenuItem::new(name.clone()).icon(icon("icons/play.svg")).on_click(
                    window.listener_for(&view, move |this, _, window, cx| {
                        this.run_project_script(command.clone(), window, cx)
                    }),
                ));
            }
            let project_id = project_id.clone();
            menu.separator().item(PopupMenuItem::new("Add action").icon(icon("icons/plus.svg")).on_click(
                window.listener_for(&view, move |this, _, window, cx| {
                    this.open_project_script_editor(&project_id, None, window, cx)
                }),
            ))
        };
        let tooltip: SharedString = format!("Run {name}").into();
        Some(
            split_row("details-scripts", colors, cx)
                .child(
                    split_primary("details-scripts-primary", colors, true, cx)
                        .child(icon("icons/play.svg").with_size(px(16.)).text_color(tokens.muted_foreground))
                        .child(div().min_w_0().truncate().child(name))
                        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                        .on_click(
                            cx.listener(move |this, _, window, cx| {
                                this.run_project_script(command.clone(), window, cx)
                            }),
                        ),
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
    fn run_project_script(&mut self, command: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        let Some(cwd) = self.git_cwd() else { return };
        window.dispatch_action(Box::new(super::RunInTerminal { cwd, command: command.to_string() }), cx);
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
        let branch = status
            .and_then(|status| status.local.ref_name.clone())
            .or_else(|| self.open_projection().and_then(|p| p.thread.branch.clone()));
        let mut rows: Vec<AnyElement> = Vec::new();
        if is_repo && self.details_item_shown("branch") {
            if let Some(branch) = branch {
                rows.push(
                    row(colors)
                        .child(icon("icons/git-branch.svg").with_size(px(16.)).text_color(tokens.muted_foreground))
                        .child(div().flex_1().min_w_0().truncate().child(branch))
                        .into_any_element(),
                );
            }
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
        if !status.local.is_repo {
            if !actions_shown {
                return (!rows.is_empty()).then(|| Section {
                    height: rows.len() as f32 * ROW_HEIGHT,
                    element: v_flex().children(rows).into_any_element(),
                    min_density: Density::Essential,
                });
            }
            let running = self.vcs.running.is_some();
            rows.push(
                action_row("details-init-git", colors, cx)
                    .when(running, |el| el.opacity(0.64))
                    .child(icon("icons/git-branch-plus.svg").with_size(px(14.)).text_color(tokens.muted_foreground))
                    .child(div().ml(px(2.)).child(if running { "Initializing..." } else { "Initialize Git" }))
                    .when(!running, |el| {
                        el.on_click(
                            cx.listener(|this, _, _, cx| this.run_vcs_request("vcs.init", "Initializing...", cx)),
                        )
                    })
                    .into_any_element(),
            );
        } else {
            let quick = git::quick_action(Some(status), self.vcs.running.is_some());
            let (insertions, deletions, _) = git::changes_totals(status);
            let quiet_empty_draft = matches!(self.target, Some(Target::Draft { .. }))
                && self.vcs.running.is_none()
                && quick.disabled()
                && insertions == 0
                && deletions == 0;
            if actions_shown && !quiet_empty_draft && (density == Density::Full || self.vcs.running.is_some()) {
                rows.push(self.git_actions_row(status, &quick, tokens, colors, cx));
            }
            if changes_shown && !quiet_empty_draft {
                rows.push(self.changes_row(status, tokens, colors, cx));
            }
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
        if let Some(progress) = self.vcs.running.clone() {
            return row(colors)
                .bg(if tokens.dark { hsla(0., 0., 1., 0.055) } else { hsla(0., 0., 0., 0.035) })
                .child(icon("icons/refresh-cw.svg").with_size(px(16.)).text_color(tokens.muted_foreground))
                .child(div().flex_1().min_w_0().truncate().child(progress))
                .into_any_element();
        }
        let disabled = quick.disabled();
        let kind = quick.kind.clone();
        let hint = match &quick.kind {
            QuickKind::Hint(hint) => Some(SharedString::from(hint.clone())),
            _ => None,
        };
        let view = cx.entity();
        let items = git::menu_actions(status, false);
        let can_publish = status.local.is_repo && !status.local.has_primary_remote;
        let build = move |mut menu: PopupMenu, window: &mut Window, cx: &mut Context<PopupMenu>| {
            // Built as the menu opens: refresh the status, like `requestVcsStatusRefresh` on open.
            view.update(cx, |this, cx| this.refresh_vcs(cx));
            for item in &items {
                let action = item.action;
                menu = menu.item(
                    PopupMenuItem::new(item.label.clone()).icon(icon(item.icon)).disabled(!item.enabled).on_click(
                        window.listener_for(&view, move |this, _, window, cx| match action {
                            "commit" => this.open_commit_dialog(window, cx),
                            action => this.run_git_action(action, None, cx),
                        }),
                    ),
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
                    .child(icon(quick.icon()).with_size(px(16.)).text_color(tokens.muted_foreground))
                    .child(div().min_w_0().truncate().child(quick.label.clone()))
                    .when_some(hint, |el, hint| {
                        el.tooltip(move |window, cx| Tooltip::new(hint.clone()).build(window, cx))
                    })
                    .on_click(cx.listener(move |this, _, window, cx| match &kind {
                        QuickKind::Run(action) => this.run_git_action(action, None, cx),
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
                .footer(dialog_footer("commit-confirm", "Commit"))
                .on_ok(move |_, _, cx: &mut App| {
                    let text = message.read(cx).value().trim().to_owned();
                    view.update(cx, |this, cx| this.run_commit(text, cx));
                    true
                })
        });
    }

    fn run_commit(&mut self, message: String, cx: &mut Context<Self>) {
        self.run_git_action("commit", (!message.is_empty()).then_some(message), cx);
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
        let view = cx.entity();
        let focus = std::cell::Cell::new(true);
        let title: SharedString = format!("Publish to {label}").into();
        window.open_dialog(cx, move |dialog, window, cx| {
            if focus.replace(false) {
                repository.focus_handle(cx).focus(window, cx);
            }
            let visibility = |id: &'static str, text: &'static str, value: bool| {
                let public = public.clone();
                Button::new(id)
                    .label(text)
                    .small()
                    .map(|b| if public.get() == value { b.primary() } else { b.outline() })
                    .on_click(move |_, window, _| {
                        public.set(value);
                        window.refresh();
                    })
            };
            let (repository, public, view, provider) =
                (repository.clone(), public.clone(), view.clone(), provider.clone());
            dialog
                .title(title.clone())
                .w(px(448.))
                .child(v_flex().gap_3().child(field("Repository", Input::new(&repository))).child(field(
                    "Visibility",
                    h_flex().gap_2().child(visibility("publish-private", "Private", false)).child(visibility(
                        "publish-public",
                        "Public",
                        true,
                    )),
                )))
                .footer(dialog_footer("publish-confirm", "Publish"))
                .on_ok(move |_, _, cx: &mut App| {
                    let repository = repository.read(cx).value().trim().to_owned();
                    if repository.is_empty() || repository.ends_with('/') {
                        return false;
                    }
                    let visibility = if public.get() { "public" } else { "private" };
                    let provider = provider.clone();
                    view.update(cx, |this, cx| this.publish_repository(provider, repository, visibility, cx));
                    true
                })
        });
    }

    /// `sourceControl.publishRepository` with the web's defaults: `origin` over SSH.
    fn publish_repository(&mut self, provider: String, repository: String, visibility: &str, cx: &mut Context<Self>) {
        let (Some(client), Some(cwd)) = (self.client(), self.vcs.cwd.clone()) else { return };
        let input = json!({
            "cwd": cwd,
            "provider": provider,
            "repository": repository,
            "visibility": visibility,
            "remoteName": "origin",
            "protocol": "ssh",
        });
        self.vcs.running = Some("Publishing repository...".into());
        cx.spawn(async move |this, cx| {
            let result = client.request("sourceControl.publishRepository", input).await;
            let _ = this.update(cx, |this, cx| {
                this.vcs.running = None;
                if let Err(error) = result {
                    this.error = Some(error.to_string().into());
                }
                this.refresh_vcs(cx);
                cx.notify();
            });
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
        let base_label =
            base.map(|base| SharedString::from(format!("vs {}", base.strip_prefix("origin/").unwrap_or(base))));
        action_row("details-changes", colors, cx)
            .child(icon("icons/file-diff.svg").with_size(px(16.)).text_color(tokens.muted_foreground))
            .child(div().flex_none().child("Changes"))
            .children(base_label.map(|label| {
                div()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .font_weight(gpui_kit::FontWeight::NORMAL)
                    .text_color(colors.muted)
                    .child(label)
            }))
            .child(
                h_flex()
                    .ml_auto()
                    .flex_none()
                    .gap(px(6.))
                    .font_family(tokens.fonts.mono)
                    .text_xs()
                    .child(div().text_color(addition).child(format!("+{}", git::group_thousands(insertions))))
                    .child(div().text_color(deletion).child(format!("-{}", git::group_thousands(deletions)))),
            )
            .on_click(cx.listener(|this, _, window, cx| this.open_changes(window, cx)))
            .into_any_element()
    }

    /// `ThreadRelationshipsPanel` from the shell: the parent thread, running
    /// subagents, and settled ones folded under "Previous agents".
    fn lineage_section(&self, tokens: &Tokens, colors: &CardColors, cx: &mut Context<Self>) -> Option<Section> {
        let Some(Target::Thread(id)) = &self.target else { return None };
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
        if parent.is_none() && active.is_empty() && previous.is_empty() && !can_detach {
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
        let show_heading = parent.is_some() || !active.is_empty() || can_detach;
        let mut section = v_flex().when(show_heading, |el| {
            let actions = can_detach.then(|| self.lineage_menu(tokens, colors, cx));
            el.child(section_heading(heading, tokens).children(actions))
        });
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
        let limits = self.server_info.provider(&selection.instance_id)?.usage_limits.as_ref()?;
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
            Some(Target::Thread(_)) => self.thread.as_ref()?.state.projection.as_ref(),
            _ => None,
        }
    }

    pub(super) fn target_project(&self) -> Option<&crate::model::ProjectShell> {
        let project_id = match self.target.as_ref()? {
            Target::Thread(id) => &self.shell.threads.iter().find(|thread| &thread.id == id)?.project_id,
            Target::Draft { project_id } => project_id,
        };
        self.shell.projects.iter().find(|project| &project.id == project_id)
    }
}

/// `getPublishProviderReadiness`: the first available, signed-in provider as (kind, label, account).
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

/// `part="row"`: a whole-row button with the panel's hover fill.
fn action_row(id: &'static str, colors: &CardColors, _: &mut Context<Workspace>) -> gpui_kit::Stateful<gpui_kit::Div> {
    let hover = colors.hover;
    row(colors).id(id).cursor_pointer().hover(move |style| style.bg(hover))
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
        .when(enabled, |el| el.cursor_pointer().hover(move |style| style.bg(hover)))
        .when(!enabled, |el| el.opacity(0.64))
}

/// The 1×16 `bg-border/65` hairline between the halves, shown while the pair is hovered.
fn split_hairline(group: &'static str, colors: &CardColors) -> impl IntoElement {
    let line = colors.divider;
    div().w(px(1.)).h(px(16.)).flex_none().group_hover(group, move |style| style.bg(line))
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
        1_000..10_000 => trim(format!("{:.1}", value as f64 / 1_000.)) + "k",
        10_000..999_500 => format!("{}k", (value as f64 / 1_000.).round()),
        _ => trim(format!("{:.1}", value as f64 / 1_000_000.)) + "m",
    }
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
