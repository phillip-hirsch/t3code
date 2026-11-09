//! The thread terminal drawer under the chat (`ThreadTerminalDrawer.tsx`):
//! per-thread terminals in split groups, the session sidebar, resizing, and
//! the `subscribeTerminalMetadata` labels and running indicators.

mod grid;
mod keys;
mod layout;
mod links;
mod mouse;
mod panel;
mod scrollbar;
mod session;

pub(in crate::app) use self::grid::TerminalTheme;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui_kit::component::button::Button;
use gpui_kit::component::button::ButtonVariant;
use gpui_kit::component::input::InlineToken;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AnyView, App, AppContext as _, Context, Div, DragMoveEvent, EmptyView, Entity, FocusHandle,
    InteractiveElement as _, IntoElement, KeyBinding, NoAction, ParentElement as _, SharedString, Stateful,
    StatefulInteractiveElement as _, StyleRefinement, Styled as _, Subscription, Task, Window, div, px, rgb,
};
use serde_json::{Value, json};

use self::layout::{DrawerLayout, Place, Split};
use self::session::{Launch, SessionEvent, TerminalSession};
use super::draft_sessions::DraftId;
use super::titlebar::icon;
use super::{Target, Workspace};
use crate::composer_text::Chip;
use crate::rpc::RpcClient;
use crate::theme::{self, Tokens};

/// A row count as an alacritty `Line` offset; grids hold far fewer than `i32::MAX` rows.
fn line_count(rows: usize) -> i32 {
    i32::try_from(rows).unwrap_or(i32::MAX)
}

gpui_kit::actions!(terminal, [ToggleTerminal, SplitTerminal, SplitTerminalVertical, NewTerminal, CloseTerminal]);

/// The window root binds Ctrl+C (copy) and Tab (focus cycling); in a terminal those keys belong to
/// the shell. The `terminal.*` commands run from the keymap (`app::keymap`), so they are rebindable.
pub fn key_bindings() -> Vec<KeyBinding> {
    vec![
        KeyBinding::new("ctrl-c", NoAction, Some("Terminal")),
        KeyBinding::new("tab", NoAction, Some("Terminal")),
        KeyBinding::new("shift-tab", NoAction, Some("Terminal")),
    ]
}

const MIN_DRAWER_HEIGHT: f32 = 180.;
/// `MAX_HIDDEN_MOUNTED_TERMINAL_THREADS`: hidden threads whose open drawers keep their sessions.
const MAX_HIDDEN_THREADS: usize = 10;
/// Drawer drags change the height every frame; the file is written once they settle.
const PERSIST_DEBOUNCE: Duration = Duration::from_millis(300);
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
    /// By thread; changed through [`Terminals::drawer`] and [`Terminals::drawer_mut`].
    drawers: HashMap<String, DrawerLayout>,
    drawers_changed: bool,
    /// The drawers as last saved (`t3code:terminal-state:v1`).
    persisted: Value,
    _persist: Option<Task<()>>,
    /// Sessions of the shown group, by terminal id.
    panes: HashMap<String, Pane>,
    /// Hidden threads' open drawer sessions, kept so switching back finds them as they were;
    /// least recently shown first.
    parked: Vec<(String, HashMap<String, Pane>)>,
    /// By thread, in server order; `None` until the first snapshot.
    summaries: Option<HashMap<String, Vec<Summary>>>,
    /// The environment whose metadata stream reported each thread's terminals.
    summary_environments: HashMap<String, SharedString>,
    /// The id a draft's thread will launch with, so its terminals carry over.
    draft_threads: HashMap<DraftId, String>,
    /// The terminal to focus once it mounts, by thread: another thread's request never lands.
    focus_pending: Option<(String, String)>,
    /// Selections added to chat, by context id: the `TerminalContextRecord` each chip sends.
    pub(super) contexts: HashMap<String, Value>,
    /// Right-panel terminal tabs, by surface id.
    panel: HashMap<String, panel::PanelGroup>,
}

/// `COMPOSER_CONTEXT_TERMINAL_TEXT_MAX_CHARS` and `clampContextText`'s marker.
const CONTEXT_TEXT_MAX: usize = 64_000;
const TRUNCATION_MARKER: &str = "\n… truncated …";

/// `formatTerminalContextLabel`.
fn context_label(terminal_label: &str, line_start: usize, line_end: usize) -> String {
    if line_start == line_end {
        format!("{terminal_label} line {line_start}")
    } else {
        format!("{terminal_label} lines {line_start}-{line_end}")
    }
}

/// `terminalContextRecord`: the text clamped to the server's limit in UTF-16 units.
fn context_record(id: &str, terminal_id: &str, terminal_label: &str, selected: &session::Selected) -> Value {
    let mut text = selected.text.clone();
    if text.encode_utf16().count() > CONTEXT_TEXT_MAX {
        let budget = CONTEXT_TEXT_MAX - TRUNCATION_MARKER.encode_utf16().count();
        let mut units = 0;
        let end = text.char_indices().find(|(_, c)| {
            units += c.len_utf16();
            units > budget
        });
        text.truncate(end.map_or(text.len(), |(i, _)| i));
        text.push_str(TRUNCATION_MARKER);
    }
    let label = context_label(terminal_label, selected.line_start, selected.line_end);
    json!({
        "version": 1,
        "contextId": id,
        "kind": "terminal",
        "label": label,
        "terminalId": terminal_id,
        "terminalLabel": terminal_label,
        "lineStart": selected.line_start,
        "lineEnd": selected.line_end,
        "text": text,
    })
}

/// Where the drawers are kept between launches.
fn drawers_path() -> Option<PathBuf> {
    Some(crate::settings::userdata_dir()?.join("terminal-state.json"))
}

/// The stored drawers, normalized; a missing or unreadable file restores none.
fn read_drawers(path: &Path) -> HashMap<String, DrawerLayout> {
    let stored: HashMap<String, DrawerLayout> =
        std::fs::read(path).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default();
    stored.into_iter().map(|(thread_id, drawer)| (thread_id, drawer.restored())).collect()
}

/// The drawers that differ from the default, by thread id.
fn stored_drawers(drawers: &HashMap<String, DrawerLayout>) -> Value {
    let mut stored: Vec<(&String, &DrawerLayout)> =
        drawers.iter().filter_map(|(thread_id, drawer)| Some((thread_id, drawer.stored()?))).collect();
    stored.sort_by_key(|(thread_id, _)| *thread_id);
    stored
        .into_iter()
        .map(|(thread_id, drawer)| (thread_id.clone(), json!(drawer)))
        .collect::<serde_json::Map<_, _>>()
        .into()
}

impl Terminals {
    /// The drawers the last session left.
    pub(super) fn restored() -> Self {
        let drawers = drawers_path().map(|path| read_drawers(&path)).unwrap_or_default();
        Self { persisted: stored_drawers(&drawers), drawers, ..Self::default() }
    }

    fn focus_later(&mut self, thread_id: &str, terminal_id: &str) {
        self.focus_pending = Some((thread_id.to_owned(), terminal_id.to_owned()));
    }

    /// The pending focus when it is `thread_id`'s; another thread's is dropped.
    fn take_focus(&mut self, thread_id: &str) -> Option<String> {
        self.focus_pending.take().filter(|(pending, _)| pending == thread_id).map(|(_, terminal_id)| terminal_id)
    }

    fn drawer(&mut self, thread_id: String) -> &mut DrawerLayout {
        self.drawers_changed = true;
        self.drawers.entry(thread_id).or_default()
    }

    fn drawer_mut(&mut self, thread_id: &str) -> Option<&mut DrawerLayout> {
        self.drawers_changed = true;
        self.drawers.get_mut(thread_id)
    }

    /// Saves the drawers once they settle, if they changed since the last save.
    fn persist(&mut self, cx: &App) {
        if !std::mem::take(&mut self.drawers_changed) {
            return;
        }
        let stored = stored_drawers(&self.drawers);
        if stored == self.persisted {
            return;
        }
        self.persisted = stored.clone();
        let Some(path) = drawers_path() else { return };
        let executor = cx.background_executor().clone();
        self._persist = Some(cx.background_spawn(async move {
            executor.timer(PERSIST_DEBOUNCE).await;
            if let Err(error) = crate::settings::write_atomically(&path, &stored) {
                log::warn!("could not save {}: {error}", path.display());
            }
        }));
    }

    /// The thread id `draft` launches with, if its terminal drawer used one.
    pub(super) fn take_draft_thread(&mut self, draft: &DraftId) -> Option<String> {
        self.draft_threads.remove(draft)
    }

    /// The unsent draft that launches with `thread_id`.
    fn draft_of(&self, thread_id: &str) -> Option<&DraftId> {
        self.draft_threads.iter().find(|(_, id)| *id == thread_id).map(|(draft, _)| draft)
    }

    fn summaries(&self, thread_id: &str) -> &[Summary] {
        self.summaries.as_ref().and_then(|all| all.get(thread_id)).map_or(&[], Vec::as_slice)
    }

    fn running(&self, thread_id: &str) -> Vec<&str> {
        self.summaries(thread_id).iter().filter(|s| s.running_subprocess).map(|s| s.terminal_id.as_str()).collect()
    }

    /// The server terminal an empty drawer opens on: a running one first, never a panel tab's.
    fn drawer_reuse(&self, thread_id: &str) -> Option<String> {
        let panel = self.panel_ids(thread_id);
        let free = |id: &&str| !panel.iter().any(|p| p == id);
        let known = self.summaries(thread_id).iter().map(|s| s.terminal_id.as_str());
        self.running(thread_id).into_iter().find(free).or_else(|| known.into_iter().find(free)).map(str::to_owned)
    }

    fn label(&self, thread_id: &str, terminal_id: &str) -> String {
        let summary = self.summaries(thread_id).iter().find(|s| s.terminal_id == terminal_id);
        layout::terminal_label(terminal_id, summary.map(|s| s.label.as_str()))
    }

    /// Applies one element of `environment`'s `subscribeTerminalMetadata`; a snapshot replaces
    /// only that environment's threads.
    fn apply_metadata(&mut self, environment: &SharedString, event: &Value) {
        let all = self.summaries.get_or_insert_default();
        let owners = &mut self.summary_environments;
        match event["type"].as_str() {
            Some("snapshot") => {
                all.retain(|thread_id, _| owners.get(thread_id) != Some(environment));
                owners.retain(|_, owner| owner != environment);
                for (thread_id, summary) in
                    event["terminals"].as_array().into_iter().flatten().filter_map(Summary::parse)
                {
                    owners.insert(thread_id.clone(), environment.clone());
                    all.entry(thread_id).or_default().push(summary);
                }
                for list in all.values_mut() {
                    sort_summaries(list);
                }
            }
            Some("upsert") => {
                let Some((thread_id, summary)) = Summary::parse(&event["terminal"]) else { return };
                owners.insert(thread_id.clone(), environment.clone());
                let list = all.entry(thread_id).or_default();
                match list.iter_mut().find(|s| s.terminal_id == summary.terminal_id) {
                    Some(existing) => *existing = summary,
                    None => {
                        list.push(summary);
                        sort_summaries(list);
                    }
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
            _ => {}
        }
    }
}

/// Keeps `value` as `thread_id`'s, the most recent of at most `max`.
fn park<T>(parked: &mut Vec<(String, T)>, thread_id: String, value: T, max: usize) {
    parked.retain(|(id, _)| *id != thread_id);
    parked.push((thread_id, value));
    let excess = parked.len().saturating_sub(max);
    parked.drain(..excess);
}

fn sort_summaries(list: &mut [Summary]) {
    list.sort_by(|a, b| layout::numeric_order(&a.terminal_id, &b.terminal_id));
}

struct DrawerResize;

fn tooltip(text: SharedString) -> impl Fn(&mut Window, &mut App) -> AnyView {
    move |window, cx| Tooltip::new(text.clone()).build(window, cx)
}

impl Workspace {
    /// The thread terminals belong to now: the open thread, or the id the
    /// draft's thread will launch with (created on first use).
    pub(in crate::app) fn terminal_thread(&mut self, create: bool) -> Option<String> {
        match self.target.as_ref()? {
            Target::Thread { id, .. } => Some(id.clone()),
            Target::Draft { draft, .. } => match self.terminals.draft_threads.get(draft) {
                Some(id) => Some(id.clone()),
                None if create => {
                    let id = super::new_id();
                    self.terminals.draft_threads.insert(draft.clone(), id.clone());
                    Some(id)
                }
                None => None,
            },
        }
    }

    fn shown_terminal_thread(&self) -> Option<&str> {
        match self.target.as_ref()? {
            Target::Thread { id, .. } => Some(id),
            Target::Draft { draft, .. } => self.terminals.draft_threads.get(draft).map(String::as_str),
        }
    }

    /// Where new terminals start: the thread's worktree, else its project root.
    fn terminal_launch(&self) -> Option<Launch> {
        let (project_id, worktree_path) = match self.target.as_ref()? {
            Target::Thread { id, .. } => {
                let thread = self.shell.threads.iter().find(|t| &t.id == id)?;
                (thread.project_id.as_str(), thread.worktree_path.clone())
            }
            Target::Draft { project_id, .. } => (project_id.as_str(), None),
        };
        let project = self.shell.projects.iter().find(|p| p.id == project_id)?;
        Some(Launch {
            cwd: worktree_path.clone().unwrap_or_else(|| project.workspace_root.clone()),
            worktree_path,
            project_root: project.workspace_root.clone(),
            stop_on_settle: None,
        })
    }

    /// `canOperateTerminal`: the thread's environment lets this client start, type into, resize
    /// and close its terminals.
    pub(super) fn terminal_operable(&self, thread_id: &str) -> bool {
        let session = self.thread_environment(thread_id).and_then(|env| self.session(&env));
        session.is_none_or(super::sessions::EnvironmentSession::operates_terminals)
    }

    /// `terminalAvailable`: the chat has a project to start a shell in.
    pub(super) fn terminal_available(&self) -> bool {
        self.terminal_launch().is_some()
    }

    pub(super) fn terminal_open(&self) -> bool {
        self.shown_terminal_thread().and_then(|id| self.terminals.drawers.get(id)).is_some_and(|d| d.open)
    }

    /// Streams one environment's terminal list and reattaches the shown terminals that run there.
    pub(super) fn subscribe_terminal_metadata(
        &mut self,
        environment: SharedString,
        client: &RpcClient,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        let stream = client.stream("subscribeTerminalMetadata", json!({}));
        let apply_environment = environment.clone();
        let task = Self::pump(
            stream,
            window,
            cx,
            move |this, value| this.terminals.apply_metadata(&apply_environment, value),
            |this, _, cx| {
                this.reconcile_terminals();
                this.notify_sidebar(cx);
            },
        );
        let parked = self.terminals.parked.iter().flat_map(|(_, panes)| panes.values());
        for pane in self.terminals.panes.values().chain(parked) {
            let thread_id = &pane.session.read(cx).thread_id;
            if self.thread_environment(thread_id).as_ref() == Some(&environment) {
                pane.session.update(cx, |session, cx| session.attach(client.clone(), cx));
            }
        }
        task
    }

    /// Mounted drawers follow the server's terminal list (`PersistentThreadTerminalDrawer`).
    fn reconcile_terminals(&mut self) {
        let shown = self.shown_terminal_thread().map(str::to_owned);
        let Some(all) = &self.terminals.summaries else { return };
        for (thread_id, drawer) in &mut self.terminals.drawers {
            if !drawer.open && shown.as_ref() != Some(thread_id) {
                continue;
            }
            // A panel tab's terminals stay out of the drawer.
            let in_panel = |id: &str| {
                self.terminals
                    .panel
                    .values()
                    .any(|g| g.thread_id == *thread_id && g.terminal_ids.iter().any(|t| t == id))
            };
            let ids: Vec<String> = all
                .get(thread_id)
                .into_iter()
                .flatten()
                .filter(|s| !in_panel(&s.terminal_id))
                .map(|s| s.terminal_id.clone())
                .collect();
            drawer.reconcile(&ids);
        }
        self.terminals.drawers_changed = true;
    }

    /// `toggleTerminalVisibility`.
    pub(super) fn toggle_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread_id) = self.terminal_thread(true) else { return };
        let drawer = self.terminals.drawer(thread_id.clone());
        if drawer.open {
            drawer.open = false;
            self.focus_composer(window, cx);
        } else if drawer.terminal_ids.is_empty() {
            // A setup or agent-started shell already runs: show it instead of starting another.
            match self.terminals.drawer_reuse(&thread_id) {
                Some(id) => {
                    self.terminals.drawer(thread_id.clone()).ensure(&id);
                    self.terminals.focus_later(&thread_id, &id);
                }
                // Without the grant the drawer opens on its empty state.
                None if !self.terminal_operable(&thread_id) => {
                    self.terminals.drawer(thread_id).open = true;
                }
                None => self.launch_terminal(thread_id, Place::New, cx),
            }
        } else {
            drawer.open = true;
            let active = drawer.active_id.clone();
            self.terminals.focus_later(&thread_id, &active);
        }
        cx.notify();
    }

    fn open_drawer_for_action(&mut self) -> Option<String> {
        let thread_id = self.terminal_thread(true)?;
        self.terminals.drawer(thread_id.clone()).open = true;
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
        self.launch_terminal_with(thread_id, place, launch, None, cx).detach();
    }

    /// `runProjectScript`: types the command into an idle drawer terminal, or a new one opened in
    /// its cwd. A script that stops when the thread settles always gets a terminal of its own.
    /// Once the command is written, a script with `autoOpenPreview` opens its preview URL.
    fn run_in_terminal(&mut self, run: &super::RunInTerminal, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread_id) = self.open_drawer_for_action() else { return };
        let Some(mut launch) = self.terminal_launch() else { return };
        launch.cwd.clone_from(&run.cwd);
        launch.stop_on_settle = Some(run.stop_on_settle);
        let input = format!(" {}\r", run.command);
        let running: Vec<String> = self.terminals.running(&thread_id).into_iter().map(str::to_owned).collect();
        let drawer = self.terminals.drawer(thread_id.clone());
        let idle: Vec<&String> = drawer.terminal_ids.iter().filter(|id| !running.contains(id)).collect();
        let reuse = idle.iter().find(|id| ***id == drawer.active_id).or(idle.first()).map(|id| (*id).clone());
        let written = match reuse.filter(|_| !run.stop_on_settle) {
            // Reopening a live terminal only updates its launch flags, as the web's launch does.
            Some(terminal_id) => {
                drawer.set_active(&terminal_id);
                self.open_terminal_session(thread_id.clone(), terminal_id, launch, Some(input), cx)
            }
            None => self.launch_terminal_with(thread_id.clone(), Place::New, launch, Some(input), cx),
        };
        let environment = self.thread_environment(&thread_id);
        match run.preview_url.clone().map(|url| self.resolve_server_url(environment.as_deref(), url)) {
            Some(url) => cx
                .spawn_in(window, async move |this, cx| {
                    if written.await {
                        let _ = this.update_in(cx, |this, window, cx| {
                            this.open_preview_tab(thread_id, Some(url), window, cx);
                        });
                    }
                })
                .detach(),
            None => written.detach(),
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
    ) -> Task<bool> {
        if self.thread_client(&thread_id).is_none() || !self.terminal_operable(&thread_id) {
            return Task::ready(false);
        }
        let terminal_id = self.next_terminal_id(&thread_id);
        if !self.terminals.drawer(thread_id.clone()).place(&terminal_id, place) {
            return Task::ready(false);
        }
        self.open_terminal_session(thread_id, terminal_id, launch, then_write, cx)
    }

    /// A terminal id no drawer, panel tab or server session of the thread uses yet.
    fn next_terminal_id(&self, thread_id: &str) -> String {
        let drawer_ids = self.terminals.drawers.get(thread_id).map(|d| d.terminal_ids.clone()).unwrap_or_default();
        let panel_ids = self.terminals.panel_ids(thread_id);
        let known = self.terminals.summaries(thread_id).iter().map(|s| s.terminal_id.as_str());
        let suffix = self.terminals.summaries.is_none().then(super::new_id);
        let taken = known.chain(drawer_ids.iter().map(String::as_str)).chain(panel_ids.iter().map(String::as_str));
        layout::next_terminal_id(taken, suffix.as_deref())
    }

    /// `terminal.open` (then an optional write) for a terminal already placed; resolves to
    /// whether both succeeded.
    fn open_terminal_session(
        &mut self,
        thread_id: String,
        terminal_id: String,
        launch: Launch,
        then_write: Option<String>,
        cx: &mut Context<Self>,
    ) -> Task<bool> {
        // A terminal runs on its thread's environment (a draft's is the open one).
        let Some(client) = self.thread_client(&thread_id).filter(|_| self.terminal_operable(&thread_id)) else {
            return Task::ready(false);
        };
        self.terminals.focus_later(&thread_id, &terminal_id);
        let payload = launch.payload(&thread_id, &terminal_id);
        let origin = match self.terminals.draft_of(&thread_id) {
            Some(draft) => self.drafts.target(draft),
            None => self.thread_target(&thread_id),
        }
        .or_else(|| self.target.clone());
        cx.spawn(async move |this, cx| {
            let mut result = client.request("terminal.open", payload).await.map(drop);
            if let (Ok(()), Some(data)) = (&result, then_write) {
                let write = json!({"threadId": thread_id, "terminalId": terminal_id, "data": data});
                result = client.request("terminal.write", write).await.map(drop);
            }
            match result {
                Ok(()) => true,
                Err(error) => {
                    let _ = this.update(cx, |this, cx| {
                        this.set_error_from(origin.as_ref(), error.to_string().into());
                        cx.notify();
                    });
                    false
                }
            }
        })
    }

    fn activate_terminal(&mut self, terminal_id: &str, cx: &mut Context<Self>) {
        let Some(thread_id) = self.shown_terminal_thread().map(str::to_owned) else { return };
        if let Some(drawer) = self.terminals.drawer_mut(&thread_id) {
            drawer.set_active(terminal_id);
            self.terminals.focus_later(&thread_id, terminal_id);
            cx.notify();
        }
    }

    /// `confirmTerminalClose`, then [`Self::close_terminal`].
    fn request_close_terminal(&mut self, terminal_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread_id) = self.shown_terminal_thread().map(str::to_owned) else { return };
        if !self.terminal_operable(&thread_id) {
            return;
        }
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
                        // The grant is read again: it may have changed while the dialog was open.
                        let _ = workspace.update(cx, |this, cx| {
                            if this.terminal_operable(&thread_id) {
                                this.close_terminal(&thread_id, &terminal_id, cx);
                            }
                        });
                    });
                    true
                })
        });
    }

    /// Ends the session and deletes its history; a failed close types `exit`.
    fn close_terminal(&mut self, thread_id: &str, terminal_id: &str, cx: &mut Context<Self>) {
        self.close_panel_terminal(thread_id, terminal_id);
        // Only a shown drawer hands focus on, as the web's `visible` guard does.
        let shown = self.shown_terminal_thread() == Some(thread_id);
        if let Some(drawer) = self.terminals.drawer_mut(thread_id) {
            drawer.close(terminal_id);
            if drawer.open && shown {
                let active = drawer.active_id.clone();
                self.terminals.focus_later(thread_id, &active);
            }
        }
        if let Some(client) = self.thread_client(thread_id) {
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

    /// Parks the sessions of a thread that stopped showing while its drawer is open, unparks the
    /// shown thread's, and drops parked threads whose drawers closed.
    fn park_terminal_panes(&mut self, shown_thread: Option<&str>, cx: &App) {
        let terminals = &mut self.terminals;
        let previous = terminals.panes.values().next().map(|pane| pane.session.read(cx).thread_id.clone());
        if let Some(previous) = previous.filter(|previous| Some(previous.as_str()) != shown_thread) {
            let group = terminals.drawers.get(&previous).filter(|d| d.open).and_then(|d| d.active_group());
            let ids = group.map(|group| group.terminal_ids.clone()).unwrap_or_default();
            let mut panes = std::mem::take(&mut terminals.panes);
            panes.retain(|id, _| ids.contains(id));
            if !panes.is_empty() {
                park(&mut terminals.parked, previous, panes, MAX_HIDDEN_THREADS);
            }
        }
        if let Some(shown) = shown_thread
            && let Some(index) = terminals.parked.iter().position(|(thread_id, _)| thread_id == shown)
        {
            terminals.panes.extend(terminals.parked.remove(index).1);
        }
        let drawers = &terminals.drawers;
        terminals.parked.retain(|(thread_id, _)| drawers.get(thread_id).is_some_and(|d| d.open));
    }

    /// Mounts sessions for the shown group and unmounts the rest (hidden threads' open drawers are
    /// parked). Unmounted terminals keep running on the server and replay their history on return.
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
                .unwrap_or_default()
                .into_iter()
                .chain(self.shown_panel_terminals(thread_id))
                .collect(),
            _ => Vec::new(),
        };
        self.terminals.persist(cx);
        self.park_terminal_panes(thread_id.as_deref().filter(|_| self.page.is_none()), cx);
        let thread_id = thread_id.unwrap_or_default();
        self.terminals.panes.retain(|id, pane| shown.contains(id) && pane.session.read(cx).thread_id == thread_id);
        for terminal_id in &shown {
            if self.terminals.panes.contains_key(terminal_id) {
                continue;
            }
            let Some(mut launch) = self.terminal_launch() else { continue };
            // A known terminal reattaches where it was started.
            if let Some(summary) = self.terminals.summaries(&thread_id).iter().find(|s| &s.terminal_id == terminal_id)
                && !summary.cwd.is_empty()
            {
                launch.cwd = summary.cwd.clone();
                launch.worktree_path = summary.worktree_path.clone();
            }
            let client = self.thread_client(&thread_id);
            let (thread, terminal) = (thread_id.clone(), terminal_id.clone());
            let read_only = !self.terminal_operable(&thread_id);
            let session = cx.new(|cx| TerminalSession::new(thread, terminal, launch, client, read_only, window, cx));
            let _events = cx.subscribe_in(&session, window, |this, session, event: &SessionEvent, window, cx| {
                let (thread_id, terminal_id) = {
                    let session = session.read(cx);
                    (session.thread_id.clone(), session.terminal_id.clone())
                };
                this.on_session_event(&thread_id, &terminal_id, event, window, cx);
            });
            self.terminals.panes.insert(terminal_id.clone(), Pane { session, _events });
        }
        let read_only = !self.terminal_operable(&thread_id);
        for pane in self.terminals.panes.values() {
            pane.session.update(cx, |session, cx| session.set_read_only(read_only, cx));
        }
        if let Some(pending) = self.terminals.take_focus(&thread_id) {
            match self.terminals.panes.get(&pending) {
                Some(pane) => {
                    let focus = pane.session.read(cx).focus.clone();
                    focus.focus(window, cx);
                }
                // Not mounted yet (another group or a closed drawer): keep the request.
                None if shown.is_empty() => {}
                None => self.terminals.focus_later(&thread_id, &pending),
            }
        }
    }

    /// Focuses the open drawer's active terminal: the terminal stop of the F6 cycle.
    pub(super) fn focus_terminal_drawer(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(thread_id) = self.shown_terminal_thread() else { return false };
        let Some(drawer) = self.terminals.drawers.get(thread_id).filter(|d| d.open) else { return false };
        let Some(pane) = self.terminals.panes.get(&drawer.active_id) else { return false };
        let focus = pane.session.read(cx).focus.clone();
        focus.focus(window, cx);
        true
    }

    fn on_session_event(
        &mut self,
        thread_id: &str,
        terminal_id: &str,
        event: &SessionEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            // `handleSessionExited`: only a client that may operate the terminal cleans it up.
            SessionEvent::Exited if self.terminal_operable(thread_id) => {
                self.close_terminal(thread_id, terminal_id, cx);
            }
            SessionEvent::Exited => {}
            SessionEvent::Activated => {
                self.activate_panel_terminal(thread_id, terminal_id, cx);
                if let Some(drawer) = self.terminals.drawer_mut(thread_id)
                    && drawer.active_id != terminal_id
                {
                    drawer.set_active(terminal_id);
                    cx.notify();
                }
            }
            SessionEvent::AddToChat(selected) => {
                self.add_terminal_context(thread_id, terminal_id, selected, window, cx);
            }
            // `openTerminalLinkInPreview`: the in-app browser when links open there, else the system's.
            SessionEvent::OpenUrl { url, force_browser } => {
                let thread = self.preview_thread_id().filter(|id| *id == thread_id).map(str::to_owned);
                let in_app = crate::preview::link_opens_in_app(
                    url,
                    *force_browser,
                    &self.client_settings.values,
                    thread.is_some(),
                );
                match thread.filter(|_| in_app) {
                    Some(thread) => self.request_preview_tab(thread, Some(url.clone()), true, window, cx),
                    None => cx.open_url(url),
                }
            }
            SessionEvent::OpenPath(path) => match self.preferred_editor_id() {
                Some(editor) => {
                    self.open_path_in_editor(path.clone(), editor, false, "Unable to open path".into(), window, cx);
                }
                None => {
                    self.error = Some("No editor is available to open this path.".into());
                    cx.notify();
                }
            },
        }
    }

    /// `addTerminalContext`: a chip for the selection at the composer's cursor; its record
    /// goes with the message that keeps the chip.
    fn add_terminal_context(
        &mut self,
        thread_id: &str,
        terminal_id: &str,
        selected: &session::Selected,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.question.is_some() {
            return;
        }
        let terminal_label = self.terminals.label(thread_id, terminal_id);
        let id = format!("terminal_{}", super::new_id());
        let record = context_record(&id, terminal_id, &terminal_label, selected);
        let label = record["label"].as_str().unwrap_or_default().to_owned();
        let chip = Chip::Context { kind: "terminal".into(), id: id.clone(), label: label.clone() };
        let link = crate::composer_text::context_link("terminal", &id, &label);
        self.terminals.contexts.insert(id, record);
        self.composer.update(cx, |state, cx| {
            let at = state.cursor();
            if at > 0 && !state.value()[..at].ends_with(char::is_whitespace) {
                state.insert(" ", window, cx);
            }
            let at = state.cursor();
            let token = InlineToken::new(chip.id(), link).with_label(label);
            if state.replace_range_with_token(at..at, token, window, cx).is_ok() {
                state.insert(" ", window, cx);
            }
        });
        self.sync_composer_empty(cx);
        self.focus_composer(window, cx);
        cx.notify();
    }

    /// The terminal records the prompt's chips name (`buildMessageContext` puts them first).
    pub(super) fn terminal_context_records(&self, text: &str) -> Vec<Value> {
        let mut found: Vec<(usize, &Value)> = self
            .terminals
            .contexts
            .iter()
            .filter_map(|(id, record)| Some((text.find(&format!("/terminal/{id})"))?, record)))
            .collect();
        found.sort_by_key(|(at, _)| *at);
        found.into_iter().map(|(_, record)| record.clone()).collect()
    }

    /// Wires the terminal actions onto the workspace root.
    pub(super) fn on_terminal_actions(&self, root: Div, cx: &mut Context<Self>) -> Div {
        root.on_action(cx.listener(|this, _: &ToggleTerminal, window, cx| this.toggle_terminal(window, cx)))
            .on_action(cx.listener(|this, _: &SplitTerminal, _, cx| this.split_terminal(Split::Horizontal, cx)))
            .on_action(cx.listener(|this, _: &SplitTerminalVertical, _, cx| this.split_terminal(Split::Vertical, cx)))
            .on_action(cx.listener(|this, _: &NewTerminal, _, cx| this.new_terminal(cx)))
            .on_action(
                cx.listener(|this, action: &super::RunInTerminal, window, cx| this.run_in_terminal(action, window, cx)),
            )
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
                        .label(self.with_terminal_shortcut("New Terminal", "terminal.new"))
                        .disabled(!self.terminal_operable(&thread_id))
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
                .when(has_sidebar, |el| {
                    let sidebar = SidebarModel {
                        thread_id: &thread_id,
                        groups: drawer
                            .sorted_groups()
                            .into_iter()
                            .map(|g| (g.id.as_str(), g.terminal_ids.as_slice(), g.split))
                            .collect(),
                        active: &drawer.active_id,
                        limit: drawer.split_limit_reached(),
                        panel: None,
                    };
                    el.child(self.render_terminal_sidebar(&sidebar, &tokens, window, cx))
                })
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
                    if let Some(drawer) = this.terminals.drawer_mut(&thread_id) {
                        drawer.height = height.clamp(MIN_DRAWER_HEIGHT, max);
                        cx.notify();
                    }
                }))
                .child(body)
                .when(!has_sidebar && !drawer.terminal_ids.is_empty(), |el| {
                    el.child(self.render_floating_actions(drawer.active_id.clone(), limit, None, &tokens, window, cx))
                })
                .child(handle)
                .into_any_element(),
        )
    }

    fn render_terminal_panes(&self, drawer: &DrawerLayout, tokens: &Tokens) -> impl IntoElement {
        let group = drawer.active_group();
        let vertical = group.is_some_and(|g| g.split == Split::Vertical);
        let ids = group.map(|g| g.terminal_ids.as_slice()).unwrap_or_default();
        self.render_pane_group(ids, &drawer.active_id, vertical, tokens)
    }

    /// A group's mounted panes side by side, or stacked when split vertically.
    fn render_pane_group(&self, ids: &[String], active: &str, vertical: bool, tokens: &Tokens) -> Div {
        let panes = ids.iter().enumerate().filter_map(|(index, id)| {
            let pane = self.terminals.panes.get(id)?;
            let color = if *id == active { tokens.border } else { tokens.border.opacity(0.7) };
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
        div()
            .flex()
            .when(vertical, gpui_kit::Styled::flex_col)
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_hidden()
            .children(panes)
    }

    /// The split/new/close buttons over a lone terminal's top-right corner.
    pub(super) fn render_floating_actions(
        &self,
        active: String,
        limit: bool,
        panel: Option<String>,
        tokens: &Tokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let border = tokens.border.opacity(0.8);
        let separator = || div().h(px(16.)).w(px(1.)).bg(border);
        let operable = self.shown_terminal_operable();
        let scope = panel.clone().unwrap_or_else(|| "drawer".into());
        let mut button = |id: &'static str, path: &'static str, disabled: bool| {
            let el = action_button(id, path, disabled || !operable, tokens)
                .p(px(4.))
                .when(!disabled && operable, |el| el.hover(|s| s.bg(tokens.accent)));
            keyboard_button(el, format!("{scope}-{id}"), !disabled && operable, tokens, window, cx)
        };
        let buttons = [
            button("terminal-split", "icons/square-split-horizontal.svg", limit),
            button("terminal-split-vertical", "icons/square-split-vertical.svg", limit),
            button("terminal-new", "icons/plus.svg", false),
            button("terminal-close", "icons/trash-2.svg", false),
        ];
        let actions = self.wire_actions(buttons, active, limit, operable, panel, cx);
        let mut row = h_flex()
            .id("terminal-floating-actions")
            .occlude()
            .items_center()
            .overflow_hidden()
            .rounded(px(6.))
            .border_1()
            .border_color(border)
            .bg(tokens.background)
            .shadow_xs();
        for (index, action) in actions.into_iter().enumerate() {
            row = row.when(index > 0, |el| el.child(separator())).child(action);
        }
        div().absolute().top(px(8.)).right(px(8.)).child(row)
    }

    /// Split, split vertically, new and close, in that order.
    fn wire_actions(
        &self,
        [split, split_vertical, new, close]: [Stateful<Div>; 4],
        active: String,
        limit: bool,
        operable: bool,
        panel: Option<String>,
        cx: &mut Context<Self>,
    ) -> [Stateful<Div>; 4] {
        [
            self.wire_split(split, Split::Horizontal, limit, operable, panel.clone(), cx),
            self.wire_split(split_vertical, Split::Vertical, limit, operable, panel.clone(), cx),
            self.wire_new(new, panel, operable, cx),
            self.wire_close(close, active, operable, cx),
        ]
    }

    fn wire_split(
        &self,
        button: Stateful<Div>,
        split: Split,
        limit: bool,
        operable: bool,
        panel: Option<String>,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let (name, command) = match split {
            Split::Horizontal => ("Split Terminal Horizontally", "terminal.split"),
            Split::Vertical => ("Split Terminal Vertically", "terminal.splitVertical"),
        };
        let label = if limit {
            format!("{name} (max {} per group)", layout::MAX_PER_GROUP)
        } else {
            self.with_terminal_shortcut(name, command)
        };
        button.tooltip(tooltip(label.into())).when(!limit && operable, |el| {
            el.on_click(cx.listener(move |this, _, window, cx| match &panel {
                Some(surface) => this.split_panel_terminal(surface, split, window, cx),
                None => this.split_terminal(split, cx),
            }))
        })
    }

    fn wire_new(
        &self,
        button: Stateful<Div>,
        panel: Option<String>,
        operable: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        button.tooltip(tooltip(self.with_terminal_shortcut("New Terminal", "terminal.new").into())).when(
            operable,
            |el| {
                el.on_click(cx.listener(move |this, _, window, cx| match panel {
                    Some(_) => this.add_terminal_surface(window, cx),
                    None => this.new_terminal(cx),
                }))
            },
        )
    }

    fn wire_close(
        &self,
        button: Stateful<Div>,
        active: String,
        operable: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        button.tooltip(tooltip(self.with_terminal_shortcut("Close Terminal", "terminal.close").into())).when(
            operable,
            |el| {
                el.on_click(
                    cx.listener(move |this, _, window, cx| this.request_close_terminal(active.clone(), window, cx)),
                )
            },
        )
    }

    fn shown_terminal_operable(&self) -> bool {
        self.shown_terminal_thread().is_none_or(|thread_id| self.terminal_operable(thread_id))
    }

    /// Shows `terminal_id` in the drawer, or in its panel tab when the sidebar is a tab's.
    fn activate_sidebar_terminal(
        &mut self,
        thread_id: &str,
        terminal_id: &str,
        panel: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        if panel.is_none() {
            self.activate_terminal(terminal_id, cx);
            return;
        }
        self.activate_panel_terminal(thread_id, terminal_id, cx);
        self.terminals.focus_later(thread_id, terminal_id);
        cx.notify();
    }

    /// The session list beside split panes once a drawer or panel tab has several terminals.
    pub(super) fn render_terminal_sidebar(
        &self,
        sidebar: &SidebarModel<'_>,
        tokens: &Tokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let SidebarModel { thread_id, groups, active, limit, panel } = sidebar;
        let (thread_id, active, limit) = (*thread_id, *active, *limit);
        let border = tokens.border.opacity(0.7);
        let operable = self.terminal_operable(thread_id);
        let scope = panel.clone().unwrap_or_else(|| "drawer".into());
        let mut header_button = |id: &'static str, path: &'static str, disabled: bool, first: bool| {
            let el = action_button(id, path, disabled || !operable, tokens)
                .h_full()
                .px(px(4.))
                .when(!first, |el| el.border_l_1().border_color(border))
                .when(!disabled && operable, |el| el.hover(|s| s.bg(tokens.accent.opacity(0.7))));
            keyboard_button(el, format!("{scope}-sidebar-{id}"), !disabled && operable, tokens, window, cx)
        };
        let buttons = [
            header_button("terminal-split", "icons/square-split-horizontal.svg", limit, true),
            header_button("terminal-split-vertical", "icons/square-split-vertical.svg", limit, false),
            header_button("terminal-new", "icons/plus.svg", false, false),
            header_button("terminal-close", "icons/trash-2.svg", false, false),
        ];
        let actions = self.wire_actions(buttons, active.to_owned(), limit, operable, panel.clone(), cx);
        let header = h_flex()
            .h(px(22.))
            .flex_none()
            .justify_end()
            .border_b_1()
            .border_color(border)
            .child(h_flex().h_full().children(actions));
        let show_headers = groups.len() > 1 || groups.iter().any(|(_, ids, _)| ids.len() > 1);
        let mut list_groups = Vec::with_capacity(groups.len());
        for (group_id, ids, split) in groups {
            let group_active = ids.iter().any(|id| id == active);
            let header = show_headers.then(|| {
                let (path, label) = match (ids.len() > 1, split) {
                    (false, _) => ("icons/square.svg", "Single"),
                    (true, Split::Vertical) => ("icons/square-split-vertical.svg", "Stacked"),
                    (true, Split::Horizontal) => ("icons/square-split-horizontal.svg", "Side by side"),
                };
                let target = if group_active { active.to_owned() } else { ids[0].clone() };
                let color = if group_active { tokens.foreground } else { tokens.muted_foreground };
                let (thread, panel) = (thread_id.to_owned(), panel.clone());
                let el = h_flex()
                    .id(SharedString::from(format!("terminal-group-{group_id}")))
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
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.activate_sidebar_terminal(&thread, &target, panel.as_deref(), cx);
                    }))
                    .child(icon(path).with_size(px(12.)).flex_none())
                    .child(div().flex_1().min_w_0().truncate().child(label))
                    .child(
                        div()
                            .text_size(px(10.))
                            .line_height(px(14.))
                            .text_color(tokens.muted_foreground.opacity(0.7))
                            .child(ids.len().to_string()),
                    );
                keyboard_button(el, format!("{scope}-group-{group_id}"), true, tokens, window, cx)
            });
            let rows: Vec<_> = ids
                .iter()
                .map(|terminal_id| {
                    let row = RowModel { thread_id, terminal_id, active: terminal_id == active, scope: &scope };
                    self.render_terminal_row(&row, panel.as_deref(), tokens, window, cx)
                })
                .collect();
            list_groups.push(v_flex().pb(px(2.)).children(header).child(v_flex().gap(px(2.)).children(rows)));
        }
        let list =
            v_flex().id("terminal-session-list").flex_1().min_h_0().overflow_y_scroll().p(px(4.)).children(list_groups);
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
        row: &RowModel<'_>,
        panel: Option<&str>,
        tokens: &Tokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let RowModel { thread_id, terminal_id, active, scope } = *row;
        let label = self.terminals.label(thread_id, terminal_id);
        let close_label: SharedString = if active {
            self.with_terminal_shortcut(&format!("Close {label}"), "terminal.close")
        } else {
            format!("Close {label}")
        }
        .into();
        let group = SharedString::from(format!("terminal-row-{terminal_id}"));
        let (close_id, activate_id) = (terminal_id.to_owned(), terminal_id.to_owned());
        let (thread, panel) = (thread_id.to_owned(), panel.map(str::to_owned));
        let operable = self.terminal_operable(thread_id);
        let glyph = |path: &'static str| {
            div().absolute().inset_0().flex().items_center().justify_center().child(icon(path).with_size(px(12.)))
        };
        let close = div()
            .id(SharedString::from(format!("terminal-row-close-{terminal_id}")))
            .relative()
            .size(px(16.))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(4.));
        // Without the grant the row shows only its terminal icon.
        let close = if operable {
            let focus = control_focus(format!("{scope}-row-close-{terminal_id}"), window, cx);
            // `group-focus-visible/close`: the keyboard-focused close button shows its X.
            let focused = focus.is_focused(window);
            let close = close
                .cursor_pointer()
                .hover(|s| s.bg(tokens.muted))
                .tooltip(tooltip(close_label))
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.request_close_terminal(close_id.clone(), window, cx);
                }))
                .map(|el| {
                    if focused {
                        el.child(glyph("icons/x.svg"))
                    } else {
                        el.child(
                            glyph("icons/square-terminal.svg").group_hover(group.clone(), gpui_kit::Styled::invisible),
                        )
                        .child(glyph("icons/x.svg").invisible().group_hover(group.clone(), gpui_kit::Styled::visible))
                    }
                });
            super::titlebar::keyboard_control(close, &focus, tokens.ring)
        } else {
            close.child(glyph("icons/square-terminal.svg"))
        };
        let activate = div()
            .id(SharedString::from(format!("terminal-row-label-{terminal_id}")))
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .items_center()
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                this.activate_sidebar_terminal(&thread, &activate_id, panel.as_deref(), cx);
            }))
            .child(div().truncate().child(label));
        let activate = keyboard_button(activate, format!("{scope}-row-label-{terminal_id}"), true, tokens, window, cx);
        h_flex()
            .id(group.clone())
            .group(group)
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
            .child(close)
            .child(activate)
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
        self.terminals.drawer(thread_id.clone()).ensure(&terminal_id);
        self.terminals.focus_later(&thread_id, &terminal_id);
        cx.notify();
    }
}

/// What a session sidebar lists: the drawer's groups, or a panel tab's one group.
pub(super) struct SidebarModel<'a> {
    thread_id: &'a str,
    /// Each group's id, terminals and split, in the sidebar's order.
    groups: Vec<(&'a str, &'a [String], Split)>,
    active: &'a str,
    limit: bool,
    /// The panel tab's surface; `None` for the drawer.
    panel: Option<String>,
}

#[derive(Clone, Copy)]
struct RowModel<'a> {
    thread_id: &'a str,
    terminal_id: &'a str,
    active: bool,
    /// Keeps the drawer's and each panel tab's focus handles apart.
    scope: &'a str,
}

/// A control's focus handle, kept while it renders.
fn control_focus(key: String, window: &mut Window, cx: &mut App) -> FocusHandle {
    window.use_keyed_state(SharedString::from(key), cx, |_, cx| cx.focus_handle().tab_stop(true)).read(cx).clone()
}

/// A drawer control as the web's `<button>`: a tab stop that Enter and Space press.
fn keyboard_button(
    el: Stateful<Div>,
    key: String,
    enabled: bool,
    tokens: &Tokens,
    window: &mut Window,
    cx: &mut App,
) -> Stateful<Div> {
    if !enabled {
        return el;
    }
    let focus = control_focus(key, window, cx);
    super::titlebar::keyboard_control(el, &focus, tokens.ring)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_records_label_the_lines_and_clamp_the_text() {
        let selected = session::Selected { line_start: 4, line_end: 4, text: "ls".into() };
        let record = context_record("terminal_a", "default", "Terminal 1", &selected);
        assert_eq!(record["label"], "Terminal 1 line 4");
        assert_eq!(record["contextId"], "terminal_a");
        let long = session::Selected { line_start: 2, line_end: 9, text: "x".repeat(CONTEXT_TEXT_MAX + 5) };
        let record = context_record("terminal_b", "default", "Terminal 1", &long);
        assert_eq!(record["label"], "Terminal 1 lines 2-9");
        let text = record["text"].as_str().unwrap();
        assert_eq!(text.encode_utf16().count(), CONTEXT_TEXT_MAX);
        assert!(text.ends_with(TRUNCATION_MARKER));
    }

    #[test]
    fn a_drafts_future_thread_maps_back_to_its_draft_until_sent() {
        let mut terminals = Terminals::default();
        let draft = DraftId::for_test("draft");
        terminals.draft_threads.insert(draft.clone(), "future".into());
        assert_eq!(terminals.draft_of("future"), Some(&draft));
        assert_eq!(terminals.draft_of("other"), None);
        terminals.take_draft_thread(&draft);
        assert_eq!(terminals.draft_of("future"), None, "a sent draft's thread is listed");
    }

    #[test]
    fn a_hidden_threads_focus_request_never_lands_on_the_shown_one() {
        let mut terminals = Terminals::default();
        terminals.focus_later("hidden", "term-2");
        assert_eq!(terminals.take_focus("shown"), None);
        assert_eq!(terminals.take_focus("hidden"), None, "dropped, not kept for later");
        terminals.focus_later("shown", "term-2");
        assert_eq!(terminals.take_focus("shown").as_deref(), Some("term-2"));
    }

    #[test]
    fn drawers_survive_a_relaunch_and_default_ones_are_not_stored() {
        let mut drawers = HashMap::new();
        drawers.insert("closed".to_owned(), DrawerLayout::default());
        let mut open = DrawerLayout::default();
        open.place("term-1", Place::New);
        drawers.insert("thread".to_owned(), open.clone());
        let stored = stored_drawers(&drawers);
        assert_eq!(stored.as_object().unwrap().keys().collect::<Vec<_>>(), ["thread"]);

        let dir = std::env::temp_dir().join(format!("t3-gpui-terminal-state-{}", std::process::id()));
        let path = dir.join("terminal-state.json");
        assert!(read_drawers(&path).is_empty());
        crate::settings::write_atomically(&path, &stored).unwrap();
        assert_eq!(read_drawers(&path), HashMap::from([("thread".to_owned(), open)]));
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir(&dir).unwrap();
    }

    #[test]
    fn hidden_threads_keep_their_sessions_up_to_the_limit() {
        let mut parked = Vec::new();
        for thread in ["a", "b", "c"] {
            park(&mut parked, thread.to_owned(), thread, 2);
        }
        assert_eq!(parked, [("b".to_owned(), "b"), ("c".to_owned(), "c")], "the oldest is dropped");
        park(&mut parked, "b".to_owned(), "b2", 2);
        assert_eq!(parked, [("c".to_owned(), "c"), ("b".to_owned(), "b2")]);
    }

    #[test]
    fn an_empty_drawer_reuses_a_server_terminal_no_panel_tab_shows() {
        let mut terminals = Terminals::default();
        let summary = |id: &str, running: bool| json!({"threadId": "t", "terminalId": id, "hasRunningSubprocess": running, "label": ""});
        let snapshot = json!({"type": "snapshot", "terminals": [summary("default", false), summary("term-2", true), summary("term-3", false)]});
        terminals.apply_metadata(&"env".into(), &snapshot);
        assert_eq!(terminals.drawer_reuse("t").as_deref(), Some("term-2"), "a running terminal first");
        let group = |ids: &[&str]| panel::PanelGroup {
            thread_id: "t".into(),
            terminal_ids: ids.iter().map(|id| (*id).to_owned()).collect(),
            active: ids[0].into(),
            split: Split::Horizontal,
        };
        terminals.panel.insert("surface".into(), group(&["term-2"]));
        assert_eq!(terminals.drawer_reuse("t").as_deref(), Some("default"));
        terminals.panel.insert("surface".into(), group(&["term-2", "default", "term-3"]));
        assert_eq!(terminals.drawer_reuse("t"), None);
    }
}
