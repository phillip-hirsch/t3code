//! The thread terminal drawer under the chat (`ThreadTerminalDrawer.tsx`):
//! per-thread terminals in split groups, the session sidebar, resizing, and
//! the `subscribeTerminalMetadata` labels and running indicators.

mod grid;
mod keys;
mod layout;
mod session;

use std::collections::HashMap;

use gpui_kit::component::button::Button;
use gpui_kit::component::button::ButtonVariant;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AnyView, App, AppContext as _, Context, Div, DragMoveEvent, EmptyView, Entity, InteractiveElement as _,
    IntoElement, KeyBinding, NoAction, ParentElement as _, SharedString, Stateful, StatefulInteractiveElement as _,
    StyleRefinement, Styled as _, Subscription, Task, Window, div, px, rgb,
};
use serde_json::{Value, json};

use self::layout::{DrawerLayout, Place, Split};
use self::session::{Launch, SessionEvent, TerminalSession};
use super::titlebar::icon;
use super::{Target, Workspace};
use crate::rpc::RpcClient;
use crate::theme::{self, Tokens};

gpui_kit::actions!(terminal, [ToggleTerminal, SplitTerminal, SplitTerminalVertical, NewTerminal, CloseTerminal]);

/// `DEFAULT_KEYBINDINGS` in `packages/shared/src/keybindings.ts`; the last four only while a terminal has focus.
/// The window root binds Ctrl+C (copy) and Tab (focus cycling); in a terminal those keys belong to the shell.
pub fn key_bindings() -> Vec<KeyBinding> {
    vec![
        KeyBinding::new("ctrl-c", NoAction, Some("Terminal")),
        KeyBinding::new("tab", NoAction, Some("Terminal")),
        KeyBinding::new("shift-tab", NoAction, Some("Terminal")),
        KeyBinding::new("ctrl-j", ToggleTerminal, None),
        KeyBinding::new("ctrl-d", SplitTerminal, Some("Terminal")),
        KeyBinding::new("ctrl-shift-d", SplitTerminalVertical, Some("Terminal")),
        KeyBinding::new("ctrl-n", NewTerminal, Some("Terminal")),
        KeyBinding::new("ctrl-w", CloseTerminal, Some("Terminal")),
    ]
}

const MIN_DRAWER_HEIGHT: f32 = 180.;
const MAX_DRAWER_HEIGHT_RATIO: f32 = 0.75;

/// A `TerminalSummary` from `subscribeTerminalMetadata`.
#[derive(Clone, Debug, PartialEq)]
struct Summary {
    terminal_id: String,
    cwd: String,
    worktree_path: Option<String>,
    running_subprocess: bool,
    label: String,
}

impl Summary {
    fn parse(value: &Value) -> Option<(String, Self)> {
        let text = |key: &str| value[key].as_str().map(str::to_owned);
        Some((
            text("threadId")?,
            Self {
                terminal_id: text("terminalId")?,
                cwd: text("cwd").unwrap_or_default(),
                worktree_path: text("worktreePath"),
                running_subprocess: value["hasRunningSubprocess"].as_bool().unwrap_or(false),
                label: text("label").unwrap_or_default(),
            },
        ))
    }
}

struct Pane {
    session: Entity<TerminalSession>,
    _events: Subscription,
}

#[derive(Default)]
pub(super) struct Terminals {
    drawers: HashMap<String, DrawerLayout>,
    /// Sessions of the shown group, by terminal id.
    panes: HashMap<String, Pane>,
    /// By thread, in server order; `None` until the first snapshot.
    summaries: Option<HashMap<String, Vec<Summary>>>,
    /// The id a draft's thread will launch with, so its terminals carry over.
    draft_threads: HashMap<String, String>,
    focus_pending: Option<String>,
    _metadata: Option<Task<()>>,
}

impl Terminals {
    /// The thread id the draft in `project_id` launches with, if its terminal drawer used one.
    pub(super) fn take_draft_thread(&mut self, project_id: &str) -> Option<String> {
        self.draft_threads.remove(project_id)
    }

    fn summaries(&self, thread_id: &str) -> &[Summary] {
        self.summaries.as_ref().and_then(|all| all.get(thread_id)).map_or(&[], Vec::as_slice)
    }

    fn running(&self, thread_id: &str) -> Vec<&str> {
        self.summaries(thread_id).iter().filter(|s| s.running_subprocess).map(|s| s.terminal_id.as_str()).collect()
    }

    fn label(&self, thread_id: &str, terminal_id: &str) -> String {
        let summary = self.summaries(thread_id).iter().find(|s| s.terminal_id == terminal_id);
        layout::terminal_label(terminal_id, summary.map(|s| s.label.as_str()))
    }

    fn apply_metadata(&mut self, event: &Value) {
        let all = self.summaries.get_or_insert_default();
        match event["type"].as_str() {
            Some("snapshot") => {
                all.clear();
                for (thread_id, summary) in
                    event["terminals"].as_array().into_iter().flatten().filter_map(Summary::parse)
                {
                    all.entry(thread_id).or_default().push(summary);
                }
            }
            Some("upsert") => {
                let Some((thread_id, summary)) = Summary::parse(&event["terminal"]) else { return };
                let list = all.entry(thread_id).or_default();
                match list.iter_mut().find(|s| s.terminal_id == summary.terminal_id) {
                    Some(existing) => *existing = summary,
                    None => list.push(summary),
                }
            }
            Some("remove") => {
                let (Some(thread_id), Some(terminal_id)) = (event["threadId"].as_str(), event["terminalId"].as_str())
                else {
                    return;
                };
                if let Some(list) = all.get_mut(thread_id) {
                    list.retain(|s| s.terminal_id != terminal_id);
                }
            }
            _ => return,
        }
        for list in all.values_mut() {
            list.sort_by(|a, b| layout::numeric_order(&a.terminal_id, &b.terminal_id));
        }
    }
}

struct DrawerResize;

fn tooltip(text: SharedString) -> impl Fn(&mut Window, &mut App) -> AnyView {
    move |window, cx| Tooltip::new(text.clone()).build(window, cx)
}

impl Workspace {
    /// The thread terminals belong to now: the open thread, or the id the
    /// draft's thread will launch with (created on first use).
    fn terminal_thread(&mut self, create: bool) -> Option<String> {
        match self.target.as_ref()? {
            Target::Thread(id) => Some(id.clone()),
            Target::Draft { project_id } => match self.terminals.draft_threads.get(project_id) {
                Some(id) => Some(id.clone()),
                None if create => {
                    let id = super::new_id();
                    self.terminals.draft_threads.insert(project_id.clone(), id.clone());
                    Some(id)
                }
                None => None,
            },
        }
    }

    fn shown_terminal_thread(&self) -> Option<&str> {
        match self.target.as_ref()? {
            Target::Thread(id) => Some(id),
            Target::Draft { project_id } => self.terminals.draft_threads.get(project_id).map(String::as_str),
        }
    }

    /// Where new terminals start: the thread's worktree, else its project root.
    fn terminal_launch(&self) -> Option<Launch> {
        let (project_id, worktree_path) = match self.target.as_ref()? {
            Target::Thread(id) => {
                let thread = self.shell.threads.iter().find(|t| &t.id == id)?;
                (thread.project_id.as_str(), thread.worktree_path.clone())
            }
            Target::Draft { project_id } => (project_id.as_str(), None),
        };
        let project = self.shell.projects.iter().find(|p| p.id == project_id)?;
        Some(Launch {
            cwd: worktree_path.clone().unwrap_or_else(|| project.workspace_root.clone()),
            worktree_path,
            project_root: project.workspace_root.clone(),
        })
    }

    /// `terminalAvailable`: the chat has a project to start a shell in.
    pub(super) fn terminal_available(&self) -> bool {
        self.terminal_launch().is_some()
    }

    pub(super) fn terminal_open(&self) -> bool {
        self.shown_terminal_thread().and_then(|id| self.terminals.drawers.get(id)).is_some_and(|d| d.open)
    }

    pub(super) fn subscribe_terminal_metadata(
        &mut self,
        client: &RpcClient,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let stream = client.stream("subscribeTerminalMetadata", json!({}));
        self.terminals._metadata = Some(Self::pump(
            stream,
            window,
            cx,
            |this, value| this.terminals.apply_metadata(value),
            |this, _, cx| {
                this.reconcile_terminals();
                this.notify_sidebar(cx);
            },
        ));
        for pane in self.terminals.panes.values() {
            pane.session.update(cx, |session, cx| session.attach(client.clone(), cx));
        }
    }

    /// Mounted drawers follow the server's terminal list (`PersistentThreadTerminalDrawer`).
    fn reconcile_terminals(&mut self) {
        let shown = self.shown_terminal_thread().map(str::to_owned);
        let Some(all) = &self.terminals.summaries else { return };
        for (thread_id, drawer) in &mut self.terminals.drawers {
            if !drawer.open && shown.as_ref() != Some(thread_id) {
                continue;
            }
            let ids: Vec<String> = all.get(thread_id).into_iter().flatten().map(|s| s.terminal_id.clone()).collect();
            drawer.reconcile(&ids);
        }
    }

    /// `toggleTerminalVisibility`.
    pub(super) fn toggle_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread_id) = self.terminal_thread(true) else { return };
        let drawer = self.terminals.drawers.entry(thread_id.clone()).or_default();
        if drawer.open {
            drawer.open = false;
            self.focus_composer(window, cx);
        } else if drawer.terminal_ids.is_empty() {
            // A setup or agent-started shell already runs: show it instead of starting another.
            let running = self.terminals.running(&thread_id).first().map(|id| id.to_string());
            let existing =
                running.or_else(|| self.terminals.summaries(&thread_id).first().map(|s| s.terminal_id.clone()));
            match existing {
                Some(id) => {
                    self.terminals.drawers.entry(thread_id).or_default().ensure(&id);
                    self.terminals.focus_pending = Some(id);
                }
                None => self.launch_terminal(thread_id, Place::New, cx),
            }
        } else {
            drawer.open = true;
            self.terminals.focus_pending = Some(drawer.active_id.clone());
        }
        cx.notify();
    }

    fn open_drawer_for_action(&mut self) -> Option<String> {
        let thread_id = self.terminal_thread(true)?;
        self.terminals.drawers.entry(thread_id.clone()).or_default().open = true;
        Some(thread_id)
    }

    fn split_terminal(&mut self, split: Split, cx: &mut Context<Self>) {
        let Some(thread_id) = self.open_drawer_for_action() else { return };
        if self.terminals.drawers.get(&thread_id).is_some_and(DrawerLayout::split_limit_reached) {
            return;
        }
        self.launch_terminal(thread_id, Place::Split(split), cx);
        cx.notify();
    }

    fn new_terminal(&mut self, cx: &mut Context<Self>) {
        let Some(thread_id) = self.open_drawer_for_action() else { return };
        self.launch_terminal(thread_id, Place::New, cx);
        cx.notify();
    }

    /// Places a new terminal, then opens its shell (`launchTerminal`).
    fn launch_terminal(&mut self, thread_id: String, place: Place, cx: &mut Context<Self>) {
        let Some(launch) = self.terminal_launch() else { return };
        self.launch_terminal_with(thread_id, place, launch, None, cx);
    }

    /// `runProjectScript`: types `command` into an idle drawer terminal, or a new one opened in `cwd`.
    fn run_in_terminal(&mut self, cwd: String, command: String, cx: &mut Context<Self>) {
        let Some(thread_id) = self.open_drawer_for_action() else { return };
        let Some(mut launch) = self.terminal_launch() else { return };
        let input = format!(" {command}\r");
        let running: Vec<String> = self.terminals.running(&thread_id).into_iter().map(str::to_owned).collect();
        let drawer = self.terminals.drawers.entry(thread_id.clone()).or_default();
        let idle: Vec<&String> = drawer.terminal_ids.iter().filter(|id| !running.contains(id)).collect();
        let reuse = idle.iter().find(|id| ***id == drawer.active_id).or(idle.first()).map(|id| (*id).clone());
        match reuse {
            Some(terminal_id) => {
                drawer.set_active(&terminal_id);
                self.terminals.focus_pending = Some(terminal_id.clone());
                if let Some(client) = self.client() {
                    let payload = json!({"threadId": thread_id, "terminalId": terminal_id, "data": input});
                    cx.spawn(async move |this, cx| {
                        if let Err(error) = client.request("terminal.write", payload).await {
                            let _ = this.update(cx, |this, cx| {
                                this.error = Some(error.to_string().into());
                                cx.notify();
                            });
                        }
                    })
                    .detach();
                }
            }
            None => {
                launch.cwd = cwd;
                self.launch_terminal_with(thread_id, Place::New, launch, Some(input), cx);
            }
        }
        cx.notify();
    }

    fn launch_terminal_with(
        &mut self,
        thread_id: String,
        place: Place,
        launch: Launch,
        then_write: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.client() else { return };
        let drawer_ids = self.terminals.drawers.get(&thread_id).map(|d| d.terminal_ids.clone()).unwrap_or_default();
        let known = self.terminals.summaries(&thread_id).iter().map(|s| s.terminal_id.as_str());
        let suffix = self.terminals.summaries.is_none().then(super::new_id);
        let terminal_id =
            layout::next_terminal_id(known.chain(drawer_ids.iter().map(String::as_str)), suffix.as_deref());
        if !self.terminals.drawers.entry(thread_id.clone()).or_default().place(&terminal_id, place) {
            return;
        }
        self.terminals.focus_pending = Some(terminal_id.clone());
        let payload = launch.payload(&thread_id, &terminal_id);
        cx.spawn(async move |this, cx| {
            let mut result = client.request("terminal.open", payload).await.map(drop);
            if let (Ok(()), Some(data)) = (&result, then_write) {
                let write = json!({"threadId": thread_id, "terminalId": terminal_id, "data": data});
                result = client.request("terminal.write", write).await.map(drop);
            }
            if let Err(error) = result {
                let _ = this.update(cx, |this, cx| {
                    this.error = Some(error.to_string().into());
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn activate_terminal(&mut self, terminal_id: &str, cx: &mut Context<Self>) {
        let Some(thread_id) = self.shown_terminal_thread().map(str::to_owned) else { return };
        if let Some(drawer) = self.terminals.drawers.get_mut(&thread_id) {
            drawer.set_active(terminal_id);
            self.terminals.focus_pending = Some(terminal_id.to_owned());
            cx.notify();
        }
    }

    /// `confirmTerminalClose`, then [`Self::close_terminal`].
    fn request_close_terminal(&mut self, terminal_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread_id) = self.shown_terminal_thread().map(str::to_owned) else { return };
        let label = self.terminals.label(&thread_id, &terminal_id);
        let workspace = cx.weak_entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let (workspace, thread_id, terminal_id) = (workspace.clone(), thread_id.clone(), terminal_id.clone());
            alert
                .title(format!("Close terminal \"{label}\"?"))
                .description("This stops the running process and clears its history.")
                .show_cancel(true)
                .ok_text("Confirm")
                .ok_variant(ButtonVariant::Danger)
                .centered()
                .width(px(512.))
                .on_ok(move |_, window, cx| {
                    // After the dialog hands focus back, so the next terminal can take it.
                    let (workspace, thread_id, terminal_id) =
                        (workspace.clone(), thread_id.clone(), terminal_id.clone());
                    window.defer(cx, move |_, cx| {
                        let _ = workspace.update(cx, |this, cx| this.close_terminal(&thread_id, &terminal_id, cx));
                    });
                    true
                })
        });
    }

    /// Ends the session and deletes its history; a failed close types `exit`.
    fn close_terminal(&mut self, thread_id: &str, terminal_id: &str, cx: &mut Context<Self>) {
        if let Some(drawer) = self.terminals.drawers.get_mut(thread_id) {
            drawer.close(terminal_id);
            if drawer.open {
                self.terminals.focus_pending = Some(drawer.active_id.clone());
            }
        }
        if let Some(client) = self.client() {
            let (thread_id, terminal_id) = (thread_id.to_owned(), terminal_id.to_owned());
            cx.background_spawn(async move {
                let close = json!({"threadId": thread_id, "terminalId": terminal_id, "deleteHistory": true});
                if client.request("terminal.close", close).await.is_err() {
                    let exit = json!({"threadId": thread_id, "terminalId": terminal_id, "data": "exit\n"});
                    let _ = client.request("terminal.write", exit).await;
                }
            })
            .detach();
        }
        cx.notify();
    }

    /// Mounts sessions for the shown group and unmounts the rest. Hidden
    /// terminals keep running on the server and replay their history on return.
    fn sync_terminal_panes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let thread_id = self.shown_terminal_thread().map(str::to_owned);
        let shown: Vec<String> = match (&thread_id, self.page) {
            (Some(thread_id), None) => self
                .terminals
                .drawers
                .get(thread_id)
                .filter(|d| d.open)
                .and_then(|d| d.active_group())
                .map(|group| group.terminal_ids.clone())
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        let thread_id = thread_id.unwrap_or_default();
        self.terminals.panes.retain(|id, pane| shown.contains(id) && pane.session.read(cx).thread_id == thread_id);
        for terminal_id in &shown {
            if self.terminals.panes.contains_key(terminal_id) {
                continue;
            }
            let Some(mut launch) = self.terminal_launch() else { continue };
            // A known terminal reattaches where it was started.
            if let Some(summary) = self.terminals.summaries(&thread_id).iter().find(|s| &s.terminal_id == terminal_id) {
                if !summary.cwd.is_empty() {
                    launch.cwd = summary.cwd.clone();
                    launch.worktree_path = summary.worktree_path.clone();
                }
            }
            let client = self.client();
            let (thread, terminal) = (thread_id.clone(), terminal_id.clone());
            let session = cx.new(|cx| TerminalSession::new(thread, terminal, launch, client, window, cx));
            let _events = cx.subscribe(&session, |this, session, event: &SessionEvent, cx| {
                let (thread_id, terminal_id) = {
                    let session = session.read(cx);
                    (session.thread_id.clone(), session.terminal_id.clone())
                };
                match event {
                    SessionEvent::Exited => this.close_terminal(&thread_id, &terminal_id, cx),
                    SessionEvent::Activated => {
                        if let Some(drawer) = this.terminals.drawers.get_mut(&thread_id) {
                            if drawer.active_id != terminal_id {
                                drawer.set_active(&terminal_id);
                                cx.notify();
                            }
                        }
                    }
                }
            });
            self.terminals.panes.insert(terminal_id.clone(), Pane { session, _events });
        }
        if let Some(pending) = self.terminals.focus_pending.take() {
            match self.terminals.panes.get(&pending) {
                Some(pane) => {
                    let focus = pane.session.read(cx).focus.clone();
                    focus.focus(window, cx);
                }
                // Not mounted yet (another group or a closed drawer): keep the request.
                None if shown.is_empty() => {}
                None => self.terminals.focus_pending = Some(pending),
            }
        }
    }

    /// Wires the terminal actions onto the workspace root.
    pub(super) fn on_terminal_actions(&self, root: Div, cx: &mut Context<Self>) -> Div {
        root.on_action(cx.listener(|this, _: &ToggleTerminal, window, cx| this.toggle_terminal(window, cx)))
            .on_action(cx.listener(|this, _: &SplitTerminal, _, cx| this.split_terminal(Split::Horizontal, cx)))
            .on_action(cx.listener(|this, _: &SplitTerminalVertical, _, cx| this.split_terminal(Split::Vertical, cx)))
            .on_action(cx.listener(|this, _: &NewTerminal, _, cx| this.new_terminal(cx)))
            .on_action(cx.listener(|this, action: &super::RunInTerminal, _, cx| {
                this.run_in_terminal(action.cwd.clone(), action.command.clone(), cx)
            }))
            .on_action(cx.listener(|this, _: &CloseTerminal, window, cx| {
                let active =
                    this.shown_terminal_thread().and_then(|id| this.terminals.drawers.get(id)).filter(|d| d.open);
                if let Some(active) = active.map(|d| d.active_id.clone()).filter(|id| !id.is_empty()) {
                    this.request_close_terminal(active, window, cx);
                }
            }))
    }

    /// The drawer under the chat while the shown thread's drawer is open.
    pub(super) fn render_terminal_drawer(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.sync_terminal_panes(window, cx);
        let thread_id = self.shown_terminal_thread()?.to_owned();
        let drawer = self.terminals.drawers.get(&thread_id).filter(|d| d.open)?;
        let tokens = *theme::tokens(cx);
        let border = tokens.border.opacity(0.8);
        let viewport = window.viewport_size().height;
        // `resolveTerminalDrawerMaxHeight`, without the composer's measured height.
        let max = (f32::from(viewport) * MAX_DRAWER_HEIGHT_RATIO).floor().max(MIN_DRAWER_HEIGHT);
        let max = max.min(f32::from(viewport - super::titlebar::TITLEBAR_HEIGHT) - 160.).max(MIN_DRAWER_HEIGHT);
        let height = px(drawer.height.round().clamp(MIN_DRAWER_HEIGHT, max));
        let has_sidebar = drawer.terminal_ids.len() > 1;
        let handle = div()
            .id("terminal-drawer-resize")
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .h(px(6.))
            .occlude()
            .cursor_row_resize()
            .on_drag(DrawerResize, |_, _, _, cx| cx.new(|_| EmptyView));
        let body: AnyElement = if drawer.terminal_ids.is_empty() {
            v_flex()
                .flex_1()
                .min_h_0()
                .items_center()
                .justify_center()
                .gap(px(12.))
                .px(px(16.))
                .py(px(24.))
                .text_sm()
                .text_color(tokens.muted_foreground)
                .child("No terminal sessions for this thread yet.")
                .child(
                    Button::new("terminal-empty-new")
                        .outline()
                        .xsmall()
                        .label("New Terminal (Ctrl+N)")
                        .on_click(cx.listener(|this, _, _, cx| this.new_terminal(cx))),
                )
                .into_any_element()
        } else {
            h_flex()
                .flex_1()
                .min_h_0()
                .w_full()
                .bg(tokens.background)
                .when(has_sidebar, |el| el.gap(px(6.)))
                .child(self.render_terminal_panes(drawer, &tokens))
                .when(has_sidebar, |el| el.child(self.render_terminal_sidebar(&thread_id, drawer, &tokens, cx)))
                .into_any_element()
        };
        let limit = drawer.split_limit_reached();
        Some(
            v_flex()
                .id("terminal-drawer")
                .relative()
                .flex_none()
                .h(height)
                .min_w_0()
                .overflow_hidden()
                .bg(tokens.background)
                .border_t_1()
                .border_color(border)
                .on_drag_move(cx.listener(move |this, event: &DragMoveEvent<DrawerResize>, _, cx| {
                    let Some(thread_id) = this.shown_terminal_thread().map(str::to_owned) else { return };
                    let height = f32::from(event.bounds.bottom() - event.event.position.y).round();
                    if let Some(drawer) = this.terminals.drawers.get_mut(&thread_id) {
                        drawer.height = height.clamp(MIN_DRAWER_HEIGHT, max);
                        cx.notify();
                    }
                }))
                .child(body)
                .when(!has_sidebar && !drawer.terminal_ids.is_empty(), |el| {
                    el.child(self.render_floating_actions(drawer.active_id.clone(), limit, &tokens, cx))
                })
                .child(handle)
                .into_any_element(),
        )
    }

    fn render_terminal_panes(&self, drawer: &DrawerLayout, tokens: &Tokens) -> impl IntoElement {
        let group = drawer.active_group();
        let vertical = group.is_some_and(|g| g.split == Split::Vertical);
        let ids = group.map(|g| g.terminal_ids.as_slice()).unwrap_or_default();
        let panes = ids.iter().enumerate().filter_map(|(index, id)| {
            let pane = self.terminals.panes.get(id)?;
            let color = if *id == drawer.active_id { tokens.border } else { tokens.border.opacity(0.7) };
            let view = AnyView::from(pane.session.clone()).cached(StyleRefinement::default().size_full());
            Some(
                div()
                    .id(SharedString::from(format!("terminal-pane-{id}")))
                    .flex_1()
                    .flex_basis(px(0.))
                    .min_w_0()
                    .min_h_0()
                    .border_color(color)
                    .when(index > 0, |el| if vertical { el.border_t_1() } else { el.border_l_1() })
                    .child(view),
            )
        });
        div().flex().when(vertical, |el| el.flex_col()).flex_1().min_w_0().h_full().overflow_hidden().children(panes)
    }

    /// The split/new/close buttons over a lone terminal's top-right corner.
    fn render_floating_actions(
        &self,
        active: String,
        limit: bool,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let border = tokens.border.opacity(0.8);
        let separator = || div().h(px(16.)).w(px(1.)).bg(border);
        let button = |id: &'static str, path: &'static str, disabled: bool| {
            action_button(id, path, disabled, tokens).p(px(4.)).when(!disabled, |el| el.hover(|s| s.bg(tokens.accent)))
        };
        div().absolute().top(px(8.)).right(px(8.)).child(
            h_flex()
                .id("terminal-floating-actions")
                .occlude()
                .items_center()
                .overflow_hidden()
                .rounded(px(6.))
                .border_1()
                .border_color(border)
                .bg(tokens.background)
                .shadow_xs()
                .child(self.wire_split(
                    button("terminal-split", "icons/square-split-horizontal.svg", limit),
                    Split::Horizontal,
                    limit,
                    cx,
                ))
                .child(separator())
                .child(self.wire_split(
                    button("terminal-split-vertical", "icons/square-split-vertical.svg", limit),
                    Split::Vertical,
                    limit,
                    cx,
                ))
                .child(separator())
                .child(self.wire_new(button("terminal-new", "icons/plus.svg", false), cx))
                .child(separator())
                .child(self.wire_close(button("terminal-close", "icons/trash-2.svg", false), active, cx)),
        )
    }

    fn wire_split(&self, button: Stateful<Div>, split: Split, limit: bool, cx: &mut Context<Self>) -> Stateful<Div> {
        let (name, shortcut) = match split {
            Split::Horizontal => ("Split Terminal Horizontally", "Ctrl+D"),
            Split::Vertical => ("Split Terminal Vertically", "Ctrl+Shift+D"),
        };
        let label = if limit {
            format!("{name} (max {} per group)", layout::MAX_PER_GROUP)
        } else {
            format!("{name} ({shortcut})")
        };
        button
            .tooltip(tooltip(label.into()))
            .when(!limit, |el| el.on_click(cx.listener(move |this, _, _, cx| this.split_terminal(split, cx))))
    }

    fn wire_new(&self, button: Stateful<Div>, cx: &mut Context<Self>) -> Stateful<Div> {
        button
            .tooltip(tooltip("New Terminal (Ctrl+N)".into()))
            .on_click(cx.listener(|this, _, _, cx| this.new_terminal(cx)))
    }

    fn wire_close(&self, button: Stateful<Div>, active: String, cx: &mut Context<Self>) -> Stateful<Div> {
        button
            .tooltip(tooltip("Close Terminal (Ctrl+W)".into()))
            .on_click(cx.listener(move |this, _, window, cx| this.request_close_terminal(active.clone(), window, cx)))
    }

    /// The session list beside split panes once a thread has several terminals.
    fn render_terminal_sidebar(
        &self,
        thread_id: &str,
        drawer: &DrawerLayout,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let border = tokens.border.opacity(0.7);
        let limit = drawer.split_limit_reached();
        let header_button = |id: &'static str, path: &'static str, disabled: bool, first: bool| {
            action_button(id, path, disabled, tokens)
                .h_full()
                .px(px(4.))
                .when(!first, |el| el.border_l_1().border_color(border))
                .when(!disabled, |el| el.hover(|s| s.bg(tokens.accent.opacity(0.7))))
        };
        let active = drawer.active_id.clone();
        let header = h_flex().h(px(22.)).flex_none().justify_end().border_b_1().border_color(border).child(
            h_flex()
                .h_full()
                .child(self.wire_split(
                    header_button("terminal-split", "icons/square-split-horizontal.svg", limit, true),
                    Split::Horizontal,
                    limit,
                    cx,
                ))
                .child(self.wire_split(
                    header_button("terminal-split-vertical", "icons/square-split-vertical.svg", limit, false),
                    Split::Vertical,
                    limit,
                    cx,
                ))
                .child(self.wire_new(header_button("terminal-new", "icons/plus.svg", false, false), cx))
                .child(self.wire_close(header_button("terminal-close", "icons/trash-2.svg", false, false), active, cx)),
        );
        let groups = drawer.sorted_groups();
        let show_headers = groups.len() > 1 || groups.iter().any(|g| g.terminal_ids.len() > 1);
        let list = v_flex().id("terminal-session-list").flex_1().min_h_0().overflow_y_scroll().p(px(4.)).children(
            groups.into_iter().map(|group| {
                let group_active = group.terminal_ids.contains(&drawer.active_id);
                let header = show_headers.then(|| {
                    let (path, label) = match (group.terminal_ids.len() > 1, group.split) {
                        (false, _) => ("icons/square.svg", "Single"),
                        (true, Split::Vertical) => ("icons/square-split-vertical.svg", "Stacked"),
                        (true, Split::Horizontal) => ("icons/square-split-horizontal.svg", "Side by side"),
                    };
                    let target = if group_active { drawer.active_id.clone() } else { group.terminal_ids[0].clone() };
                    let color = if group_active { tokens.foreground } else { tokens.muted_foreground };
                    h_flex()
                        .id(SharedString::from(format!("terminal-group-{}", group.id)))
                        .h(px(22.))
                        .w_full()
                        .gap(px(4.))
                        .px(px(6.))
                        .rounded(px(4.))
                        .cursor_pointer()
                        .text_size(px(11.))
                        .line_height(px(16.))
                        .text_color(color)
                        .map(|el| {
                            if group_active {
                                el.bg(tokens.accent.opacity(0.5))
                            } else {
                                el.hover(|s| s.bg(tokens.accent.opacity(0.4)).text_color(tokens.foreground))
                            }
                        })
                        .on_click(cx.listener(move |this, _, _, cx| this.activate_terminal(&target, cx)))
                        .child(icon(path).with_size(px(12.)).flex_none())
                        .child(div().flex_1().min_w_0().truncate().child(label))
                        .child(
                            div()
                                .text_size(px(10.))
                                .line_height(px(14.))
                                .text_color(tokens.muted_foreground.opacity(0.7))
                                .child(group.terminal_ids.len().to_string()),
                        )
                });
                let rows: Vec<_> = group
                    .terminal_ids
                    .iter()
                    .map(|terminal_id| {
                        self.render_terminal_row(thread_id, terminal_id, terminal_id == &drawer.active_id, tokens, cx)
                    })
                    .collect();
                v_flex().pb(px(2.)).children(header).child(v_flex().gap(px(2.)).children(rows))
            }),
        );
        v_flex()
            .w(px(144.))
            .flex_none()
            .h_full()
            .border_1()
            .border_color(border)
            .bg(tokens.muted.opacity(0.1))
            .child(header)
            .child(list)
    }

    fn render_terminal_row(
        &self,
        thread_id: &str,
        terminal_id: &str,
        active: bool,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let label = self.terminals.label(thread_id, terminal_id);
        let close_label: SharedString =
            if active { format!("Close {label} (Ctrl+W)") } else { format!("Close {label}") }.into();
        let group = SharedString::from(format!("terminal-row-{terminal_id}"));
        let (close_id, activate_id) = (terminal_id.to_owned(), terminal_id.to_owned());
        let glyph = |path: &'static str| {
            div().absolute().inset_0().flex().items_center().justify_center().child(icon(path).with_size(px(12.)))
        };
        h_flex()
            .id(group.clone())
            .group(group.clone())
            .h(px(24.))
            .w_full()
            .gap(px(2.))
            .pl(px(6.))
            .pr(px(8.))
            .rounded(px(6.))
            .text_xs()
            .map(|el| {
                if active {
                    el.bg(tokens.accent).text_color(tokens.foreground)
                } else {
                    el.text_color(tokens.muted_foreground)
                        .hover(|s| s.bg(tokens.accent.opacity(0.6)).text_color(tokens.foreground))
                }
            })
            .child(
                div()
                    .id(SharedString::from(format!("terminal-row-close-{terminal_id}")))
                    .relative()
                    .size(px(16.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(4.))
                    .cursor_pointer()
                    .hover(|s| s.bg(tokens.muted))
                    .tooltip(tooltip(close_label))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.request_close_terminal(close_id.clone(), window, cx);
                    }))
                    .child(glyph("icons/square-terminal.svg").group_hover(group.clone(), |s| s.invisible()))
                    .child(glyph("icons/x.svg").invisible().group_hover(group, |s| s.visible())),
            )
            .child(
                div()
                    .id(SharedString::from(format!("terminal-row-label-{terminal_id}")))
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| this.activate_terminal(&activate_id, cx)))
                    .child(div().truncate().child(label)),
            )
    }

    /// The sidebar row's indicator: teal while a terminal process runs. It
    /// does not pulse; a looping animation would repaint the sidebar forever.
    pub(super) fn render_running_terminals(&self, thread_id: &str, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let running = self.terminals.running(thread_id);
        let first = running.first()?.to_string();
        let tokens = theme::tokens(cx);
        let color = if tokens.dark { rgb(0x46ecd5).into() } else { gpui_kit::Hsla::from(rgb(0x009689)) };
        let color = if tokens.dark { gpui_kit::Hsla { a: 0.9, ..color } } else { color };
        let label: SharedString = if running.len() == 1 {
            "1 terminal process running. Open terminal".into()
        } else {
            format!("{} terminal processes running. Open terminal", running.len()).into()
        };
        let thread_id = thread_id.to_owned();
        Some(
            div()
                .id(SharedString::from(format!("terminal-status-{thread_id}")))
                .flex_none()
                .flex()
                .items_center()
                .rounded(px(4.))
                .cursor_pointer()
                .hover(|s| s.opacity(0.8))
                .tooltip(tooltip(label))
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.reveal_thread_terminal(thread_id.clone(), first.clone(), window, cx);
                }))
                .child(icon("icons/terminal.svg").with_size(px(14.)).text_color(color)),
        )
    }

    /// `revealThreadTerminal`: open the thread with that terminal shown.
    pub(super) fn reveal_thread_terminal(
        &mut self,
        thread_id: String,
        terminal_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_thread(thread_id.clone(), window, cx);
        self.terminals.drawers.entry(thread_id).or_default().ensure(&terminal_id);
        self.terminals.focus_pending = Some(terminal_id);
        cx.notify();
    }
}

/// A drawer action button: a 13px icon in `text-foreground/90`, dimmed when disabled.
fn action_button(id: &'static str, path: &'static str, disabled: bool, tokens: &Tokens) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .text_color(tokens.foreground.opacity(0.9))
        .map(|el| if disabled { el.opacity(0.64).cursor_not_allowed() } else { el.cursor_pointer() })
        .child(icon(path).with_size(px(13.)))
}
