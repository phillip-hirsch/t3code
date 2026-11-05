//! The `@` files, `$` skills and `/` commands menu above the composer
//! (`ComposerCommandMenu.tsx`, `ComposerCommandMenuLayer.tsx`,
//! `useComposerMenuItems.ts`).

use std::collections::HashSet;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::input::InlineToken;
use gpui_kit::component::{Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, Context, ElementId, FontWeight, InteractiveElement as _, IntoElement, Keystroke, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, Task, Window, div, prelude::FluentBuilder as _, px, relative,
};
use serde_json::json;

use super::{Target, Workspace};
use crate::composer_search::{self, MenuAction, MenuItem};
use crate::composer_text::{self, Chip, Trigger, TriggerKind};
use crate::theme::Tokens;

/// `useComposerPathSearch` waits this long after the last keystroke.
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(120);
const SEARCH_LIMIT: u32 = 80;
/// `--chat-composer-drawer-inset` and the 17px the composer covers.
const DRAWER_INSET: f32 = 22.;
const OVERLAP: f32 = 17.;

#[derive(Default)]
pub(super) struct MenuState {
    trigger: Option<Trigger>,
    /// Escape hid the trigger starting here, until the caret leaves it.
    dismissed: Option<(TriggerKind, usize)>,
    items: Rc<Vec<MenuItem>>,
    highlighted: usize,
    loading: bool,
    _search: Option<Task<()>>,
    /// Provider/checkout pairs already asked for a workspace skills snapshot.
    snapshots_requested: HashSet<(String, String)>,
}

impl Workspace {
    /// The checkout a thread or draft works in: its worktree, else the project root.
    pub(super) fn composer_cwd(&self) -> Option<String> {
        let worktree = match &self.target {
            Some(Target::Thread(_)) => {
                self.thread.as_ref()?.state.projection.as_ref().and_then(|p| p.thread.worktree_path.clone())
            }
            _ => None,
        };
        worktree.or_else(|| self.target_project().map(|p| p.workspace_root.clone()))
    }

    /// Re-reads the trigger under the caret; called on every edit and caret move.
    pub(super) fn sync_trigger(&mut self, cx: &mut Context<Self>) {
        let state = self.composer.read(cx);
        let cursor = state.cursor();
        let in_chip = |trigger: &Trigger| {
            state
                .tokens()
                .iter()
                .any(|span| span.range().start < trigger.range.end && trigger.range.start <= span.range().end)
        };
        let trigger = (self.question.is_none())
            .then(|| composer_text::detect(&state.value(), cursor))
            .flatten()
            .filter(|t| !in_chip(t));
        if trigger.is_none() || trigger.as_ref().map(|t| (t.kind, t.range.start)) != self.menu.dismissed {
            self.menu.dismissed = None;
        }
        let trigger = trigger.filter(|t| self.menu.dismissed != Some((t.kind, t.range.start)));
        if trigger == self.menu.trigger {
            return;
        }
        let same_query = trigger.as_ref().map(|t| (t.kind, t.query.trim().to_lowercase()))
            == self.menu.trigger.as_ref().map(|t| (t.kind, t.query.trim().to_lowercase()));
        self.menu.trigger = trigger;
        if !same_query {
            self.menu.highlighted = 0;
            self.refresh_menu_items(cx);
        }
        cx.notify();
    }

    /// Rebuilds the open menu's rows, e.g. after the provider's skills arrive.
    pub(super) fn refresh_menu_items(&mut self, cx: &mut Context<Self>) {
        let Some(trigger) = self.menu.trigger.clone() else {
            self.menu.items = Rc::default();
            self.menu._search = None;
            return;
        };
        let cwd = self.composer_cwd();
        if trigger.kind == TriggerKind::Path {
            self.search_files(trigger.query.trim().to_owned(), cwd, cx);
            return;
        }
        self.menu._search = None;
        let provider = self.composer_model().and_then(|s| self.server_info.provider(&s.instance_id).cloned());
        let Some(provider) = provider else {
            self.menu.items = Rc::default();
            self.menu.loading = false;
            return;
        };
        let has_snapshot = provider.workspace_snapshots.iter().any(|s| Some(&s.cwd) == cwd.as_ref());
        if let (false, Some(cwd)) = (has_snapshot, &cwd) {
            self.request_workspace_snapshot(&provider.instance_id, cwd, cx);
        }
        self.menu.loading = !has_snapshot && cwd.is_some();
        let (skills, commands) = composer_search::provider_lists(&provider, cwd.as_deref());
        let items = match trigger.kind {
            TriggerKind::Skill => composer_search::skill_items(skills, &trigger.query),
            // `getProviderSkillsForSlashMenu`: skills join `/` only when the setting allows.
            _ if !self.client_settings.values.bool("showSkillsInSlashMenu") => {
                composer_search::slash_items(&[], commands, &trigger.query, trigger.range.start == 0)
            }
            _ => composer_search::slash_items(skills, commands, &trigger.query, trigger.range.start == 0),
        };
        self.menu.highlighted = self.menu.highlighted.min(items.len().saturating_sub(1));
        self.menu.items = Rc::new(items);
    }

    /// `server.refreshProviders` for one checkout, so `$` and `/` list its skills.
    fn request_workspace_snapshot(&mut self, instance_id: &str, cwd: &str, cx: &mut Context<Self>) {
        let key = (instance_id.to_owned(), cwd.to_owned());
        let Some(client) = self.client().filter(|_| self.menu.snapshots_requested.insert(key)) else { return };
        let input = json!({"instanceId": instance_id, "cwd": cwd, "workspaceSnapshotsOnDemand": true});
        cx.spawn(async move |this, cx| {
            let result = client.request("server.refreshProviders", input).await;
            let _ = this.update(cx, |this, cx| {
                if let Err(error) = result {
                    log::warn!("could not load workspace skills: {error}");
                }
                // The new snapshot arrives on the config stream; without one, stop loading.
                this.refresh_menu_items(cx);
                this.menu.loading = false;
                cx.notify();
            });
        })
        .detach();
    }

    fn search_files(&mut self, query: String, cwd: Option<String>, cx: &mut Context<Self>) {
        let (Some(cwd), Some(client), false) = (cwd, self.client(), query.is_empty()) else {
            self.menu._search = None;
            self.menu.loading = false;
            self.menu.items = Rc::default();
            return;
        };
        self.menu.loading = true;
        self.menu._search = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SEARCH_DEBOUNCE).await;
            let input = json!({"cwd": cwd, "query": query, "limit": SEARCH_LIMIT});
            let result = client.request("projects.searchEntries", input).await;
            let _ = this.update(cx, |this, cx| {
                // A newer query replaced this one.
                if this.menu.trigger.as_ref().map(|t| t.query.trim()) != Some(query.as_str()) {
                    return;
                }
                this.menu.loading = false;
                this.menu.items = Rc::new(match result {
                    Ok(value) => composer_search::file_items(&value),
                    Err(error) => {
                        log::warn!("file search failed: {error}");
                        Vec::new()
                    }
                });
                this.menu.highlighted = 0;
                cx.notify();
            });
        }));
    }

    /// Hides the menu for the trigger now under the caret, as Escape does.
    pub(super) fn hide_menu(&mut self, cx: &mut Context<Self>) {
        if let Some(trigger) = self.menu.trigger.take() {
            self.menu.dismissed = Some((trigger.kind, trigger.range.start));
            self.refresh_menu_items(cx);
            cx.notify();
        }
    }

    /// Arrows, Enter/Tab and Escape while the menu is open; `true` when handled.
    pub(super) fn menu_key(&mut self, keystroke: &Keystroke, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(trigger) = &self.menu.trigger else { return false };
        let modifiers = keystroke.modifiers;
        let count = self.menu.items.len();
        match keystroke.key.as_str() {
            "escape" => {
                let _ = trigger;
                self.hide_menu(cx);
                true
            }
            "up" | "down" if count > 0 && !modifiers.control && !modifiers.alt && !modifiers.platform => {
                let step = if keystroke.key == "up" { count - 1 } else { 1 };
                self.menu.highlighted = (self.menu.highlighted + step) % count;
                cx.notify();
                true
            }
            "enter" | "tab" if count > 0 && !modifiers.control && !modifiers.platform => {
                self.pick_menu_item(self.menu.highlighted, window, cx);
                true
            }
            _ => false,
        }
    }

    pub(super) fn pick_menu_item(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(trigger), Some(item)) = (self.menu.trigger.clone(), self.menu.items.get(index).cloned()) else {
            return;
        };
        let (replacement, chip) = match &item.action {
            MenuAction::File { path, .. } => (composer_text::file_link(path), Some(Chip::File { path: path.clone() })),
            MenuAction::Skill { name, .. } => (format!("${name}"), Some(Chip::Skill { name: name.clone() })),
            MenuAction::Command { name } => (format!("/{name} "), None),
            MenuAction::Model => (String::new(), None),
        };
        self.menu.trigger = None;
        self.composer.update(cx, |state, cx| {
            let text = state.value();
            let typed = if chip.is_some() { format!("{replacement} ") } else { replacement.clone() };
            let range = composer_text::extend_for_trailing_space(&text, trigger.range.clone(), &typed);
            match chip {
                Some(chip) => {
                    let token = InlineToken::new(chip.id(), replacement).with_label(chip.label().to_owned());
                    if state.replace_range_with_token(range, token, window, cx).is_ok() {
                        state.insert(" ", window, cx);
                    }
                }
                None => {
                    state.set_selected_range(range, cx);
                    state.replace(replacement, window, cx);
                }
            }
        });
        if item.action == MenuAction::Model {
            self.open_model_picker(window, cx);
        }
        self.sync_composer_empty(cx);
        self.sync_trigger(cx);
        cx.notify();
    }

    /// The menu, drawn above the composer and tucked 17px under its top edge.
    pub(super) fn render_command_menu(&self, t: &Tokens, cx: &mut Context<Self>) -> Option<AnyElement> {
        let trigger = self.menu.trigger.as_ref()?;
        let items = self.menu.items.clone();
        let outline = if t.dark { gpui_kit::hsla(0., 0., 1., 0.05) } else { gpui_kit::hsla(0., 0., 0., 0.08) };
        let body: AnyElement = if items.is_empty() {
            let text = match (self.menu.loading, trigger.kind) {
                (true, TriggerKind::Path) => "Searching workspace files...",
                (true, _) => "Searching workspace skills...",
                (false, TriggerKind::Path) => "No matching files or folders.",
                (false, TriggerKind::Skill) => "No skills found. Try / to browse provider commands.",
                (false, TriggerKind::SlashCommand) => "No matching command.",
            };
            div()
                .px(px(20.))
                .pt(px(14.))
                .pb(px(28.))
                .text_xs()
                .text_color(t.muted_foreground)
                .child(text)
                .into_any_element()
        } else {
            let highlighted = self.menu.highlighted;
            let kind = trigger.kind;
            v_flex()
                .id("composer-menu-list")
                .max_h(px(288.))
                .overflow_y_scroll()
                .p(px(8.))
                .children(items.iter().enumerate().map(|(index, item)| {
                    menu_row(item, kind, index == highlighted, t)
                        .on_mouse_move(cx.listener(move |this, _, _, cx| {
                            if this.menu.highlighted != index {
                                this.menu.highlighted = index;
                                cx.notify();
                            }
                        }))
                        .on_mouse_down(gpui_kit::MouseButton::Left, |_, window, _| window.prevent_default())
                        .on_click(cx.listener(move |this, _, window, cx| this.pick_menu_item(index, window, cx)))
                        .into_any_element()
                }))
                .into_any_element()
        };
        Some(
            div()
                .absolute()
                .left(px(DRAWER_INSET))
                .right(px(DRAWER_INSET))
                .bottom(relative(1.))
                .mb(px(-OVERLAP))
                .child(
                    v_flex()
                        .id("composer-menu")
                        .occlude()
                        .pb(px(OVERLAP))
                        .rounded_t(px(16.))
                        .border_1()
                        .border_color(outline)
                        .bg(t.background.blend(t.card.opacity(t.glass)))
                        .child(body),
                )
                .into_any_element(),
        )
    }
}

fn menu_row(item: &MenuItem, kind: TriggerKind, active: bool, t: &Tokens) -> gpui_kit::Stateful<gpui_kit::Div> {
    let icon = match &item.action {
        MenuAction::File { directory: true, .. } => Some(Lucide::Folder),
        MenuAction::File { .. } => Some(Lucide::File),
        _ => None,
    };
    let badge = match (&item.action, kind) {
        (MenuAction::Skill { scope, path, .. }, TriggerKind::Skill) => Some(skill_badge(scope.as_deref(), path)),
        (MenuAction::Skill { scope, path, .. }, _) => Some((skill_badge(scope.as_deref(), path).0, None)),
        _ => None,
    };
    let label: AnyElement = match item.label.strip_prefix("/skill:") {
        Some(name) => h_flex()
            .child(div().text_color(t.muted_foreground).child("/skill:"))
            .child(name.to_owned())
            .into_any_element(),
        None => div().child(item.label.clone()).into_any_element(),
    };
    h_flex()
        .id(ElementId::Name(item.id.clone().into()))
        .min_h(px(28.))
        .px(px(8.))
        .py(px(6.))
        .gap(px(8.))
        .rounded(px(8.))
        .cursor_pointer()
        .when(active, |el| el.bg(t.accent))
        .children(icon.map(|icon| Icon::new(icon).with_size(px(16.)).flex_none().text_color(t.muted_foreground)))
        .child(
            h_flex()
                .flex_1()
                .min_w_0()
                .gap(px(8.))
                .text_xs()
                .child(
                    div()
                        .flex_none()
                        .max_w(relative(0.45))
                        .truncate()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(t.foreground)
                        .child(label),
                )
                .child(
                    div().flex_1().min_w_0().truncate().text_color(t.muted_foreground).child(item.description.clone()),
                ),
        )
        .children(badge.map(|(source, suffix)| {
            let (icon, word) = source;
            h_flex()
                .flex_none()
                .ml_auto()
                .h(px(18.))
                .px(px(3.))
                .gap(px(4.))
                .rounded(px(4.))
                .bg(t.secondary)
                .text_xs()
                .text_color(t.foreground)
                .child(Icon::new(icon).with_size(px(12.)).opacity(0.8))
                .child(format!("{word}{}", suffix.unwrap_or_default()))
        }))
}

/// `resolveProviderSkillSourceKind`: where a skill comes from, as its badge shows it.
fn skill_badge(scope: Option<&str>, path: &str) -> ((Lucide, &'static str), Option<&'static str>) {
    let path = path.replace('\\', "/");
    let kind = if path.contains("/.codex/plugins/") || path.contains("/.agents/plugins/") {
        (Lucide::Blocks, "App")
    } else {
        match scope.map(str::to_lowercase).as_deref() {
            Some("repo" | "repository") => (Lucide::Folder, "Repo"),
            Some("project" | "workspace" | "local") => (Lucide::Folder, "Project"),
            Some("user" | "personal") => (Lucide::UserRound, "Personal"),
            Some("system") => (Lucide::Settings, "System"),
            _ => (Lucide::Package, "Provider"),
        }
    };
    (kind, Some(" Skill"))
}
