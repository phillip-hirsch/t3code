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
use super::{Target, Workspace};
use crate::app::right_panel::Surface;
use crate::theme;

#[derive(Clone, Debug)]
pub(in crate::app) struct PanelGroup {
    pub thread_id: String,
    pub terminal_ids: Vec<String>,
    pub active: String,
    pub split: Split,
}

impl super::Terminals {
    /// Every panel tab's terminal ids for `thread_id`.
    pub(super) fn panel_ids(&self, thread_id: &str) -> Vec<String> {
        self.panel.values().filter(|g| g.thread_id == thread_id).flat_map(|g| g.terminal_ids.clone()).collect()
    }
}

impl Workspace {
    /// The open target's panel terminal tab, if it is the one on screen.
    fn active_panel_group(&self) -> Option<(&String, &PanelGroup)> {
        let Some(Surface::Terminal(id)) = self.active_surface() else { return None };
        self.terminals.panel.get_key_value(id)
    }

    /// The terminals the shown panel tab mounts.
    pub(super) fn shown_panel_terminals(&self, thread_id: &str) -> Vec<String> {
        self.active_panel_group()
            .filter(|(_, g)| g.thread_id == thread_id)
            .map(|(_, g)| g.terminal_ids.clone())
            .unwrap_or_default()
    }

    /// The panel tab and terminal holding keyboard focus: terminal commands act there instead of
    /// on the drawer, as the web routes them by focus owner.
    fn focused_panel_terminal(&self, window: &Window, cx: &Context<Self>) -> Option<(String, String)> {
        self.terminals.panel.iter().find_map(|(surface, group)| {
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
        if self.client().is_none() {
            return;
        }
        let terminal_id = self.next_terminal_id(&thread_id);
        let surface = format!("terminal-{}", super::super::new_id());
        let group = PanelGroup {
            thread_id: thread_id.clone(),
            terminal_ids: vec![terminal_id.clone()],
            active: terminal_id.clone(),
            split: Split::Horizontal,
        };
        self.terminals.panel.insert(surface.clone(), group);
        self.open_surface(Surface::Terminal(surface), window, cx);
        self.open_terminal_session(thread_id, terminal_id, launch, None, cx);
    }

    /// `splitPanelTerminal`: another terminal in the tab's group, up to four.
    pub(super) fn split_panel_terminal(&mut self, surface: &str, split: Split, _: &mut Window, cx: &mut Context<Self>) {
        let Some(launch) = self.terminal_launch() else { return };
        let Some(thread_id) = self.terminals.panel.get(surface).map(|g| g.thread_id.clone()) else { return };
        if self.terminals.panel.get(surface).is_some_and(|g| g.terminal_ids.len() >= layout::MAX_PER_GROUP) {
            return;
        }
        let terminal_id = self.next_terminal_id(&thread_id);
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
        self.open_terminal_session(thread_id, terminal_id, launch, None, cx);
        cx.notify();
    }

    pub(super) fn activate_panel_terminal(&mut self, thread_id: &str, terminal_id: &str, cx: &mut Context<Self>) {
        for group in self.terminals.panel.values_mut().filter(|g| g.thread_id == thread_id) {
            if group.terminal_ids.iter().any(|id| id == terminal_id) && group.active != terminal_id {
                group.active = terminal_id.to_owned();
                cx.notify();
            }
        }
    }

    /// Drops a closed terminal from its tab; the tab closes with its last terminal.
    pub(super) fn close_panel_terminal(&mut self, thread_id: &str, terminal_id: &str) {
        let mut emptied = Vec::new();
        for (surface, group) in self.terminals.panel.iter_mut().filter(|(_, g)| g.thread_id == thread_id) {
            let Some(index) = group.terminal_ids.iter().position(|id| id == terminal_id) else { continue };
            group.terminal_ids.remove(index);
            match group.terminal_ids.get(index.min(group.terminal_ids.len().saturating_sub(1))) {
                Some(next) => {
                    if group.active == terminal_id {
                        group.active = next.clone();
                    }
                    self.terminals.focus_pending = Some(group.active.clone());
                }
                None => emptied.push(surface.clone()),
            }
        }
        for surface in emptied {
            self.terminals.panel.remove(&surface);
            self.drop_surface(&Target::Thread(thread_id.to_owned()), &Surface::Terminal(surface));
        }
    }

    /// The tab's label: its active terminal's.
    pub(in crate::app) fn terminal_surface_title(&self, surface: &str) -> SharedString {
        match self.terminals.panel.get(surface) {
            Some(group) => self.terminals.label(&group.thread_id, &group.active).into(),
            None => "Terminal".into(),
        }
    }

    /// Closing the tab asks first (`confirmTerminalClose`), then closes its terminals.
    pub(in crate::app) fn request_close_terminal_surface(
        &mut self,
        surface: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(group) = self.terminals.panel.get(&surface).cloned() else {
            if let Some(target) = self.target.clone() {
                self.drop_surface(&target, &Surface::Terminal(surface));
            }
            cx.notify();
            return;
        };
        let mut labels: Vec<String> = vec![self.terminals.label(&group.thread_id, &group.active)];
        labels.extend(
            group
                .terminal_ids
                .iter()
                .filter(|id| **id != group.active)
                .map(|id| self.terminals.label(&group.thread_id, id)),
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
        let workspace = cx.weak_entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let (workspace, group) = (workspace.clone(), group.clone());
            alert
                .title(title.clone())
                .description(description.clone())
                .show_cancel(true)
                .ok_text("Confirm")
                .ok_variant(ButtonVariant::Danger)
                .centered()
                .width(px(512.))
                .on_ok(move |_, window, cx| {
                    let (workspace, group) = (workspace.clone(), group.clone());
                    window.defer(cx, move |_, cx| {
                        let _ = workspace.update(cx, |this, cx| {
                            for id in &group.terminal_ids {
                                this.close_terminal(&group.thread_id, id, cx);
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
        div()
            .id("terminal-panel")
            .relative()
            .flex_1()
            .min_h_0()
            .flex()
            .bg(tokens.background)
            .child(self.render_pane_group(&group.terminal_ids, &group.active, group.split == Split::Vertical, &tokens))
            .when(!group.terminal_ids.is_empty(), |el| {
                el.child(self.render_floating_actions(
                    group.active.clone(),
                    limit,
                    Some(surface.to_owned()),
                    &tokens,
                    cx,
                ))
            })
            .into_any_element()
    }
}
