//! Terminals as a right-panel tab (`PersistentThreadTerminalPanel`): each tab is one split
//! group of the thread's terminals, kept out of the drawer.

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::button::ButtonVariant;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString, Styled as _, Window,
    div, px,
};

use super::layout::{self, Split};
use super::{DrawerKey, Workspace};
use crate::app::right_panel::Surface;
use crate::theme;

#[derive(Clone, Debug)]
pub(super) struct PanelGroup {
    /// The thread on its environment: same-id threads on two environments have their own tabs.
    pub key: DrawerKey,
    pub terminal_ids: Vec<String>,
    pub active: String,
    pub split: Split,
}

impl super::Terminals {
    /// Every panel tab's terminal ids for `key`'s thread.
    pub(super) fn panel_ids(&self, key: &DrawerKey) -> Vec<String> {
        self.panel.values().filter(|g| g.key == *key).flat_map(|g| g.terminal_ids.clone()).collect()
    }

    /// Makes `terminal_id` active in `key`'s tab holding it; whether a tab changed.
    pub(super) fn activate_in_panel(&mut self, key: &DrawerKey, terminal_id: &str) -> bool {
        let mut changed = false;
        for group in self.panel.values_mut().filter(|g| g.key == *key) {
            if group.terminal_ids.iter().any(|id| id == terminal_id) && group.active != terminal_id {
                group.active = terminal_id.to_owned();
                changed = true;
            }
        }
        changed
    }

    /// Drops a closed terminal from `key`'s tabs, handing a shown tab's focus on. Returns the tabs
    /// it emptied, which are removed.
    pub(super) fn remove_from_panel(&mut self, key: &DrawerKey, terminal_id: &str, shown: bool) -> Vec<String> {
        let mut emptied = Vec::new();
        for (surface, group) in self.panel.iter_mut().filter(|(_, g)| g.key == *key) {
            let Some(index) = group.terminal_ids.iter().position(|id| id == terminal_id) else { continue };
            group.terminal_ids.remove(index);
            match group.terminal_ids.get(index.min(group.terminal_ids.len().saturating_sub(1))) {
                Some(next) => {
                    if group.active == terminal_id {
                        group.active = next.clone();
                    }
                    if shown {
                        self.focus_pending = Some((key.clone(), group.active.clone()));
                    }
                }
                None => emptied.push(surface.clone()),
            }
        }
        for surface in &emptied {
            self.panel.remove(surface);
        }
        emptied
    }
}

impl Workspace {
    /// The open target's panel terminal tab, if it is the one on screen.
    fn active_panel_group(&self) -> Option<(&String, &PanelGroup)> {
        let Some(Surface::Terminal(id)) = self.active_surface() else { return None };
        self.terminals.panel.get_key_value(id)
    }

    /// The terminals the shown panel tab mounts.
    pub(super) fn shown_panel_terminals(&self, key: &DrawerKey) -> Vec<String> {
        self.active_panel_group()
            .filter(|(_, g)| g.key == *key)
            .map(|(_, g)| g.terminal_ids.clone())
            .unwrap_or_default()
    }

    /// The panel tab and terminal holding keyboard focus: terminal commands act there instead of
    /// on the drawer, as the web routes them by focus owner.
    fn focused_panel_terminal(&self, window: &Window, cx: &Context<Self>) -> Option<(String, String)> {
        // The mounted panes are the shown thread's; a same-id thread's tab may reuse their ids.
        let shown = self.shown_drawer_key()?;
        self.terminals.panel.iter().filter(|(_, group)| group.key == shown).find_map(|(surface, group)| {
            group.terminal_ids.iter().find_map(|id| {
                let pane = self.terminals.panes.get(id)?;
                pane.session.read(cx).focus.is_focused(window).then(|| (surface.clone(), id.clone()))
            })
        })
    }

    /// Runs a `terminal.*` command on the focused panel terminal; `false` leaves it to the drawer.
    pub(in crate::app) fn panel_terminal_command(
        &mut self,
        command: &str,
        held: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some((surface, terminal_id)) = self.focused_panel_terminal(window, cx) else { return false };
        match command {
            "terminal.split" => self.split_panel_terminal(&surface, Split::Horizontal, window, cx),
            "terminal.splitVertical" => self.split_panel_terminal(&surface, Split::Vertical, window, cx),
            "terminal.new" => self.add_terminal_surface(window, cx),
            "terminal.close" if !held => self.request_close_terminal(terminal_id, window, cx),
            _ => {}
        }
        true
    }

    /// `addTerminalSurface`: a new terminal in a tab of its own.
    pub(in crate::app) fn add_terminal_surface(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(thread_id), Some(launch)) = (self.terminal_thread(true), self.terminal_launch()) else { return };
        if self.terminal_client(&thread_id).is_none() || !self.terminal_operable(&thread_id) {
            return;
        }
        let key = self.drawer_key(&thread_id);
        let terminal_id = self.next_terminal_id(&key);
        let surface = format!("terminal-{}", super::super::new_id());
        let group = PanelGroup {
            key: key.clone(),
            terminal_ids: vec![terminal_id.clone()],
            active: terminal_id.clone(),
            split: Split::Horizontal,
        };
        self.terminals.panel.insert(surface.clone(), group);
        self.open_surface(Surface::Terminal(surface), window, cx);
        self.open_terminal_session(key, terminal_id, launch, None, cx).detach();
    }

    /// `splitPanelTerminal`: another terminal in the tab's group, up to four.
    pub(super) fn split_panel_terminal(&mut self, surface: &str, split: Split, _: &mut Window, cx: &mut Context<Self>) {
        let Some(launch) = self.terminal_launch() else { return };
        let Some(key) = self.terminals.panel.get(surface).map(|g| g.key.clone()) else { return };
        if !self.drawer_operable(&key)
            || self.terminals.panel.get(surface).is_some_and(|g| g.terminal_ids.len() >= layout::MAX_PER_GROUP)
        {
            return;
        }
        let terminal_id = self.next_terminal_id(&key);
        if let Some(group) = self.terminals.panel.get_mut(surface) {
            let at = group
                .terminal_ids
                .iter()
                .position(|id| *id == group.active)
                .map_or(group.terminal_ids.len(), |i| i + 1);
            group.terminal_ids.insert(at, terminal_id.clone());
            group.active = terminal_id.clone();
            group.split = split;
        }
        self.open_terminal_session(key, terminal_id, launch, None, cx).detach();
        cx.notify();
    }

    pub(super) fn activate_panel_terminal(&mut self, key: &DrawerKey, terminal_id: &str, cx: &mut Context<Self>) {
        if self.terminals.activate_in_panel(key, terminal_id) {
            cx.notify();
        }
    }

    /// Drops a closed terminal from its tab; the tab closes with its last terminal.
    pub(super) fn close_panel_terminal(&mut self, key: &DrawerKey, terminal_id: &str) {
        let shown = self.shown_drawer_key().as_ref() == Some(key);
        let emptied = self.terminals.remove_from_panel(key, terminal_id, shown);
        let Some(target) = self.drawer_target(key) else { return };
        for surface in emptied {
            self.drop_surface(&target, &Surface::Terminal(surface));
        }
    }

    /// The tab's label: its active terminal's.
    pub(in crate::app) fn terminal_surface_title(&self, surface: &str) -> SharedString {
        match self.terminals.panel.get(surface) {
            Some(group) => self.terminals.label(&group.key.thread_id, &group.active).into(),
            None => "Terminal".into(),
        }
    }

    /// Drops the shown target's tab alone; its terminals keep running on the server.
    fn drop_terminal_surface(&mut self, surface: String) {
        self.terminals.panel.remove(&surface);
        if let Some(target) = self.target.clone() {
            self.drop_surface(&target, &Surface::Terminal(surface));
        }
    }

    /// Closing the tab asks first (`confirmTerminalClose`), then closes its terminals. Without
    /// `terminal:operate` only the tab closes, as in `useRightPanelSurfaceClose`.
    pub(in crate::app) fn request_close_terminal_surface(
        &mut self,
        surface: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let group = self.terminals.panel.get(&surface).cloned();
        let Some(group) = group.filter(|group| self.drawer_operable(&group.key)) else {
            self.drop_terminal_surface(surface);
            cx.notify();
            return;
        };
        let mut labels: Vec<String> = vec![self.terminals.label(&group.key.thread_id, &group.active)];
        labels.extend(
            group
                .terminal_ids
                .iter()
                .filter(|id| **id != group.active)
                .map(|id| self.terminals.label(&group.key.thread_id, id)),
        );
        let (title, description) = if labels.len() == 1 {
            (
                format!("Close terminal \"{}\"?", labels[0]),
                "This stops the running process and clears its history.".to_owned(),
            )
        } else {
            let quoted: Vec<String> = labels.iter().map(|l| format!("\"{l}\"")).collect();
            (
                format!("Close {} terminals?", labels.len()),
                format!("This stops their running processes and clears their histories: {}.", quoted.join(", ")),
            )
        };
        let key = group.key.clone();
        let workspace = cx.weak_entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let (workspace, group, surface, key) = (workspace.clone(), group.clone(), surface.clone(), key.clone());
            alert
                .title(title.clone())
                .description(description.clone())
                .show_cancel(true)
                .ok_text("Confirm")
                .ok_variant(ButtonVariant::Danger)
                .centered()
                .width(px(512.))
                .on_ok(move |_, window, cx| {
                    let (workspace, group, surface, key) =
                        (workspace.clone(), group.clone(), surface.clone(), key.clone());
                    window.defer(cx, move |_, cx| {
                        // The grant is read again: it may have changed while the dialog was open.
                        let _ = workspace.update(cx, |this, cx| {
                            if this.drawer_operable(&key) {
                                for id in &group.terminal_ids {
                                    this.close_terminal(&key, id, cx);
                                }
                            } else {
                                this.drop_terminal_surface(surface);
                                cx.notify();
                            }
                        });
                    });
                    true
                })
        });
    }

    /// The tab's panes, with the split / new / close buttons over them.
    pub(in crate::app) fn render_terminal_surface(
        &mut self,
        surface: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.sync_terminal_panes(window, cx);
        let tokens = *theme::tokens(cx);
        let Some(group) = self.terminals.panel.get(surface).cloned() else {
            return div().flex_1().into_any_element();
        };
        let limit = group.terminal_ids.len() >= layout::MAX_PER_GROUP;
        // `ThreadTerminalDrawer` in panel mode: the session sidebar once the tab splits.
        let has_sidebar = group.terminal_ids.len() > 1;
        let panes = self.render_pane_group(&group.terminal_ids, &group.active, group.split == Split::Vertical, &tokens);
        let side = if has_sidebar {
            let sidebar = super::SidebarModel {
                key: &group.key,
                groups: vec![(surface, group.terminal_ids.as_slice(), group.split)],
                active: &group.active,
                limit,
                panel: Some(surface.to_owned()),
            };
            self.render_terminal_sidebar(&sidebar, &tokens, window, cx).into_any_element()
        } else if group.terminal_ids.is_empty() {
            div().into_any_element()
        } else {
            self.render_floating_actions(group.active.clone(), limit, Some(surface.to_owned()), &tokens, window, cx)
                .into_any_element()
        };
        div()
            .id("terminal-panel")
            .relative()
            .flex_1()
            .min_h_0()
            .flex()
            .bg(tokens.terminal.background)
            .children(crate::app::theme_inspect::probe(
                &["terminalBackground", "terminalForeground", "terminalCursor", "terminalSelection"],
                cx,
            ))
            .when(has_sidebar, |el| el.gap(px(6.)))
            .child(panes)
            .child(side)
            .into_any_element()
    }
}
