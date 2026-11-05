//! The main window: sidebar of projects and threads, the open thread's
//! timeline, pending approvals, and the composer.

use std::rc::Rc;
use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::message_scroller::{MessageScroller, MessageScrollerState};
use gpui_kit::component::text::TextView;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, ElementId, Entity, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, StyleRefinement, Styled as _,
    Subscription, Task, Window, div, prelude::FluentBuilder as _, px,
};
use serde_json::{Value, json};

use crate::backend::{self, ServerHandle};
use crate::model::{ModelSelection, ProjectShell, ServerInfo, ShellState, ThreadPill, ThreadShell, ThreadState};
use crate::rpc::{ConnectOptions, RpcClient, RpcError, RpcStream, StreamEvent};
use crate::runtime;
use crate::timeline::{self, Row, WorkKind};

const RECONNECT_DELAY: Duration = Duration::from_secs(1);
const CONTENT_WIDTH: f32 = 820.;

enum Connection {
    Starting,
    Connecting,
    Connected(RpcClient),
    Disconnected(SharedString),
    Failed(SharedString),
}

/// Where the composer sends.
#[derive(Clone, PartialEq)]
enum Target {
    Thread(String),
    /// A new thread in this project, created by the first message.
    Draft {
        project_id: String,
    },
}

struct OpenThread {
    id: String,
    state: ThreadState,
    _subscription: Option<Task<()>>,
}

pub struct Workspace {
    connection: Connection,
    server: Option<ServerHandle>,
    server_info: ServerInfo,
    shell: ShellState,
    target: Option<Target>,
    thread: Option<OpenThread>,
    rows: Rc<Vec<Row>>,
    scroller: Entity<MessageScrollerState>,
    composer: Entity<TextareaState>,
    sending: bool,
    /// Model picked in the composer; for an open thread it applies to the next message.
    model_override: Option<ModelSelection>,
    error: Option<SharedString>,
    _connection_task: Task<()>,
    _stream_tasks: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl Workspace {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 12)
                .submit_on_enter(true)
                .placeholder("Ask anything. Enter to send, Shift+Enter for a new line")
        });
        let scroller = cx.new(|cx| MessageScrollerState::new(0, cx));
        let subscriptions = vec![
            cx.subscribe_in(&composer, window, |this, _, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { shift: false, .. } = event {
                    this.submit(window, cx);
                }
            }),
            cx.observe(&scroller, |_, _, cx| cx.notify()),
        ];
        Self {
            connection: Connection::Starting,
            server: None,
            server_info: ServerInfo::default(),
            shell: ShellState::default(),
            target: None,
            thread: None,
            rows: Rc::default(),
            scroller,
            composer,
            sending: false,
            model_override: None,
            error: None,
            _connection_task: Self::connect(cx),
            _stream_tasks: Vec::new(),
            _subscriptions: subscriptions,
        }
    }

    /// Starts or finds the server, then keeps one socket open, reconnecting after drops.
    fn connect(cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            let resolved = runtime().spawn(backend::resolve_endpoint()).await;
            let endpoint = match resolved {
                Ok(Ok((endpoint, server))) => {
                    log::info!("server ready at {}", endpoint.http_base);
                    let handle = server.map(|server| server.supervise(runtime().handle()));
                    let _ = this.update(cx, |this, cx| {
                        if let Some(handle) = handle {
                            this.watch_server(handle, cx);
                        }
                    });
                    endpoint
                }
                Ok(Err(error)) => {
                    let _ = this.update(cx, |this, cx| {
                        this.set_connection(Connection::Failed(format!("{error:#}").into()), cx)
                    });
                    return;
                }
                Err(error) => {
                    let _ = this
                        .update(cx, |this, cx| this.set_connection(Connection::Failed(error.to_string().into()), cx));
                    return;
                }
            };
            loop {
                if this.update(cx, |this, cx| this.set_connection(Connection::Connecting, cx)).is_err() {
                    return;
                }
                let options = ConnectOptions { ws_url: endpoint.ws_url(), bearer_token: endpoint.bearer_token.clone() };
                match RpcClient::connect(runtime().handle(), options).await {
                    Ok(client) => {
                        log::info!("connected");
                        if this.update(cx, |this, cx| this.on_connected(client.clone(), cx)).is_err() {
                            return;
                        }
                        let reason = client.closed().await;
                        if this
                            .update(cx, |this, cx| this.set_connection(Connection::Disconnected(reason.into()), cx))
                            .is_err()
                        {
                            return;
                        }
                    }
                    Err(error) => {
                        let message: SharedString = format!("{error:#}").into();
                        if this
                            .update(cx, |this, cx| this.set_connection(Connection::Disconnected(message), cx))
                            .is_err()
                        {
                            return;
                        }
                    }
                }
                cx.background_executor().timer(RECONNECT_DELAY).await;
            }
        })
    }

    fn watch_server(&mut self, mut handle: ServerHandle, cx: &mut Context<Self>) {
        let exited = std::mem::replace(&mut handle.exited, futures::channel::oneshot::channel().1);
        self.server = Some(handle);
        cx.spawn(async move |this, cx| {
            if let Ok(reason) = exited.await {
                log::warn!("{reason}");
                let _ = this.update(cx, |this, cx| this.set_connection(Connection::Failed(reason.into()), cx));
            }
        })
        .detach();
    }

    fn set_connection(&mut self, connection: Connection, cx: &mut Context<Self>) {
        // A stopped server is final; a late socket close must not hide why.
        if matches!(self.connection, Connection::Failed(_)) && !matches!(connection, Connection::Connected(_)) {
            return;
        }
        if !matches!(connection, Connection::Connected(_)) {
            self._stream_tasks.clear();
            if let Some(thread) = &mut self.thread {
                thread._subscription = None;
            }
        }
        self.connection = connection;
        cx.notify();
    }

    fn client(&self) -> Option<RpcClient> {
        match &self.connection {
            Connection::Connected(client) => Some(client.clone()),
            _ => None,
        }
    }

    fn on_connected(&mut self, client: RpcClient, cx: &mut Context<Self>) {
        self.connection = Connection::Connected(client.clone());
        let config = client.stream("subscribeServerConfig", json!({"workspaceSnapshotsOnDemand": true}));
        let mut shell_input = json!({"requestCompletionMarker": true});
        if self.shell.sequence > 0 {
            shell_input["afterSequence"] = self.shell.sequence.into();
        }
        let shell = client.stream("orchestration.subscribeShell", shell_input);
        self._stream_tasks = vec![
            Self::pump(config, cx, |this, value, _| {
                if let Err(error) = this.server_info.apply(value) {
                    log::warn!("bad server config event: {error}");
                }
            }),
            Self::pump(shell, cx, |this, value, _| {
                if let Err(error) = this.shell.apply(value) {
                    log::warn!("bad shell event: {error}");
                }
            }),
        ];
        self.subscribe_thread(cx);
        cx.notify();
    }

    /// Applies every event already queued on `stream` before one re-render.
    fn pump(
        stream: RpcStream,
        cx: &mut Context<Self>,
        apply: impl Fn(&mut Self, &Value, &mut Context<Self>) + 'static,
    ) -> Task<()> {
        cx.spawn(async move |this, cx| {
            while let Some(first) = stream.next().await {
                let mut events = vec![first];
                events.extend(std::iter::from_fn(|| stream.try_next()));
                let ended = this.update(cx, |this, cx| {
                    let mut ended = false;
                    for event in events {
                        match event {
                            StreamEvent::Item(value) => apply(this, &value, cx),
                            StreamEvent::End(result) => {
                                ended = true;
                                if let Err(error) = result {
                                    if !matches!(error, RpcError::Disconnected(_)) {
                                        this.error = Some(error.to_string().into());
                                    }
                                }
                            }
                        }
                    }
                    cx.notify();
                    ended
                });
                if ended.unwrap_or(true) {
                    break;
                }
            }
        })
    }

    fn subscribe_thread(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client() else { return };
        let Some(thread) = &mut self.thread else { return };
        let mut input = json!({"threadId": thread.id, "requestCompletionMarker": true});
        if thread.state.sequence > 0 {
            input["afterSequence"] = thread.state.sequence.into();
        }
        let stream = client.stream("orchestration.subscribeThread", input);
        let thread_id = thread.id.clone();
        thread._subscription = Some(Self::pump(stream, cx, move |this, value, cx| {
            let Some(thread) = this.thread.as_mut().filter(|t| t.id == thread_id) else { return };
            if let Err(error) = thread.state.apply(value) {
                log::warn!("bad thread event: {error}");
            }
            this.refresh_rows(cx);
        }));
    }

    /// Rebuilds the timeline and tells the scroller which rows changed.
    fn refresh_rows(&mut self, cx: &mut Context<Self>) {
        let new_rows = match self.thread.as_ref().and_then(|t| t.state.projection.as_ref()) {
            Some(projection) => timeline::rows(projection),
            None => Vec::new(),
        };
        let was_empty = self.rows.is_empty();
        let Some((range, count)) = timeline::changed_range(&self.rows, &new_rows) else { return };
        let total = new_rows.len();
        self.rows = Rc::new(new_rows);
        self.scroller.update(cx, |scroller, cx| {
            if was_empty {
                scroller.reset(total, cx);
            } else {
                scroller.splice(range, count, cx);
            }
        });
    }

    fn select_thread(&mut self, thread_id: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.target.as_ref() == Some(&Target::Thread(thread_id.clone())) {
            return;
        }
        self.target = Some(Target::Thread(thread_id.clone()));
        self.thread = Some(OpenThread { id: thread_id, state: ThreadState::default(), _subscription: None });
        self.model_override = None;
        self.reset_timeline(cx);
        self.subscribe_thread(cx);
        self.focus_composer(window, cx);
        cx.notify();
    }

    fn start_draft(&mut self, project_id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.target = Some(Target::Draft { project_id });
        self.thread = None;
        self.model_override = None;
        self.reset_timeline(cx);
        self.focus_composer(window, cx);
        cx.notify();
    }

    fn reset_timeline(&mut self, cx: &mut Context<Self>) {
        self.rows = Rc::default();
        self.scroller.update(cx, |scroller, cx| scroller.reset(0, cx));
    }

    fn focus_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.composer.update(cx, |composer, cx| composer.focus(window, cx));
    }

    /// The model the next message runs on.
    fn composer_model(&self) -> Option<ModelSelection> {
        if let Some(selection) = &self.model_override {
            return Some(selection.clone());
        }
        match &self.target {
            Some(Target::Thread(_)) => self.thread.as_ref()?.state.projection.as_ref()?.thread.model_selection.clone(),
            Some(Target::Draft { project_id }) => {
                self.server_info.new_thread_model(self.shell.projects.iter().find(|p| &p.id == project_id))
            }
            None => None,
        }
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.composer.read(cx).value().trim().to_owned();
        if text.is_empty() || self.sending {
            return;
        }
        let (Some(client), Some(target)) = (self.client(), self.target.clone()) else { return };
        let (method, payload, new_thread_id) = match target {
            Target::Thread(thread_id) => {
                let mut command = json!({
                    "type": "message.dispatch",
                    "commandId": new_id(),
                    "createdBy": "user",
                    "creationSource": "web",
                    "threadId": thread_id,
                    "messageId": new_id(),
                    "text": text,
                    "attachments": [],
                    "deliveryIntent": "auto",
                    "dispatchMode": {"type": "start_immediately"},
                });
                // Omitted, the server keeps the thread's own model.
                if let Some(model) = &self.model_override {
                    command["modelSelection"] = model.to_json();
                }
                ("orchestration.dispatchCommand", command, None)
            }
            Target::Draft { project_id } => {
                let Some(model) = self.composer_model() else {
                    self.error = Some("No provider is ready. Set one up in the T3 server first.".into());
                    cx.notify();
                    return;
                };
                let thread_id = new_id();
                let input = json!({
                    "commandId": new_id(),
                    "creationSource": "web",
                    "threadId": thread_id,
                    "projectId": project_id,
                    "title": draft_title(&text),
                    "generateTitle": true,
                    "modelSelection": model.to_json(),
                    "runtimeMode": "full-access",
                    "interactionMode": "default",
                    "workspaceStrategy": {"type": "root"},
                    "initialMessage": {"messageId": new_id(), "text": text, "attachments": []},
                });
                ("orchestration.launchThread", input, Some(thread_id))
            }
        };
        self.sending = true;
        self.composer.update(cx, |composer, cx| composer.set_value("", window, cx));
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request(method, payload).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.sending = false;
                match result {
                    Ok(_) => {
                        if let Some(thread_id) = new_thread_id {
                            this.select_thread(thread_id, window, cx);
                        }
                    }
                    Err(error) => {
                        this.error = Some(error.to_string().into());
                        // Give the text back so a failed send loses nothing.
                        if this.composer.read(cx).value().is_empty() {
                            this.composer.update(cx, |composer, cx| composer.set_value(text, window, cx));
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        let Some(projection) = self.thread.as_ref().and_then(|t| t.state.projection.as_ref()) else { return };
        let thread_id = projection.thread.id.clone();
        let command = if self.server_info.thread_stop {
            json!({"type": "thread.stop", "commandId": new_id(), "threadId": thread_id})
        } else {
            let Some(run) = projection.interruptible_run() else { return };
            json!({"type": "run.interrupt", "commandId": new_id(), "threadId": thread_id, "runId": run.id, "holdQueue": true})
        };
        self.dispatch(command, cx);
    }

    fn respond(&mut self, request_id: String, decision: String, cx: &mut Context<Self>) {
        let Some(thread) = &self.thread else { return };
        let command = json!({
            "type": "runtime-request.respond",
            "commandId": new_id(),
            "threadId": thread.id,
            "requestId": request_id,
            "decision": decision,
        });
        self.dispatch(command, cx);
    }

    fn dispatch(&mut self, command: Value, cx: &mut Context<Self>) {
        let Some(client) = self.client() else { return };
        cx.spawn(async move |this, cx| {
            if let Err(error) = client.request("orchestration.dispatchCommand", command).await {
                let _ = this.update(cx, |this, cx| {
                    this.error = Some(error.to_string().into());
                    cx.notify();
                });
            }
        })
        .detach();
    }
}

fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn draft_title(text: &str) -> String {
    let line = text.lines().next().unwrap_or_default().trim();
    match line.char_indices().nth(60) {
        Some((end, _)) => format!("{}…", &line[..end]),
        None => line.to_owned(),
    }
}

fn project_name(project: &ProjectShell) -> SharedString {
    project
        .repository_identity
        .as_ref()
        .and_then(|identity| identity.display_name.clone())
        .unwrap_or_else(|| project.title.clone())
        .into()
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        h_flex()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .text_sm()
            .child(self.render_sidebar(cx))
            .child(
                v_flex()
                    .flex_1()
                    .h_full()
                    .min_w_0()
                    .child(self.render_header(cx))
                    .children(self.render_error(cx))
                    .child(self.render_body(window, cx))
                    .children(self.render_approvals(cx))
                    .child(self.render_composer(cx)),
            )
    }
}

impl Workspace {
    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (sidebar, sidebar_foreground, sidebar_border) = {
            let theme = cx.theme();
            (theme.sidebar, theme.sidebar_foreground, theme.sidebar_border)
        };
        let selected = match &self.target {
            Some(Target::Thread(id)) => Some(id.as_str()),
            _ => None,
        };
        let draft_project = match &self.target {
            Some(Target::Draft { project_id }) => Some(project_id.as_str()),
            _ => None,
        };
        let mut projects: Vec<&ProjectShell> = self.shell.projects.iter().filter(|p| p.archived_at.is_none()).collect();
        projects.sort_by(|a, b| b.pinned_at.is_some().cmp(&a.pinned_at.is_some()).then_with(|| a.title.cmp(&b.title)));

        let mut list = v_flex().gap_3().p_2();
        for project in projects {
            let project_id = project.id.clone();
            let mut group =
                v_flex().gap_0p5().child(
                    h_flex()
                        .justify_between()
                        .px_2()
                        .py_1()
                        .text_xs()
                        .text_color(sidebar_foreground.opacity(0.7))
                        .child(div().truncate().child(project_name(project)))
                        .child(
                            Button::new(SharedString::from(format!("new-thread-{project_id}")))
                                .ghost()
                                .xsmall()
                                .icon(IconName::Plus)
                                .tooltip("New thread")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.start_draft(project_id.clone(), window, cx)
                                })),
                        ),
                );
            if draft_project == Some(project.id.as_str()) {
                group = group.child(thread_row_frame("draft".into(), true, cx).child("New thread"));
            }
            for thread in self.shell.project_threads(&project.id) {
                group = group.child(self.render_thread_row(thread, selected == Some(thread.id.as_str()), cx));
            }
            list = list.child(group);
        }

        v_flex()
            .w(px(280.))
            .h_full()
            .flex_none()
            .bg(sidebar)
            .text_color(sidebar_foreground)
            .border_r_1()
            .border_color(sidebar_border)
            .child(h_flex().h(px(44.)).px_4().font_semibold().child("T3 Code"))
            .child(div().id("sidebar-scroll").flex_1().min_h_0().overflow_y_scroll().child(list))
            .child(self.render_connection(cx))
    }

    fn render_thread_row(&self, thread: &ThreadShell, selected: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let thread_id = thread.id.clone();
        let theme = cx.theme();
        let pill = match thread.pill() {
            ThreadPill::Approval => Some(("Approval", theme.warning)),
            ThreadPill::Input => Some(("Input", theme.info)),
            ThreadPill::Waiting => Some(("Waiting", theme.muted_foreground)),
            ThreadPill::Working => Some(("Working", theme.primary)),
            ThreadPill::Failed => Some(("Failed", theme.danger)),
            ThreadPill::Limited => Some(("Limited", theme.warning)),
            ThreadPill::PlanReady => Some(("Plan ready", theme.info)),
            ThreadPill::Idle => None,
        };
        thread_row_frame(SharedString::from(thread.id.clone()), selected, cx)
            .on_click(cx.listener(move |this, _, window, cx| this.select_thread(thread_id.clone(), window, cx)))
            .child(div().flex_1().min_w_0().truncate().child(thread.title.clone()))
            .children(pill.map(|(label, color)| status_badge(label, color)))
    }

    fn render_connection(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (label, color): (SharedString, Hsla) = match &self.connection {
            Connection::Starting => ("Starting server…".into(), theme.muted_foreground),
            Connection::Connecting => ("Connecting…".into(), theme.muted_foreground),
            Connection::Connected(_) if !self.shell.live => ("Syncing…".into(), theme.muted_foreground),
            Connection::Connected(_) => ("Connected".into(), theme.success),
            Connection::Disconnected(reason) => (format!("Reconnecting: {reason}").into(), theme.warning),
            Connection::Failed(reason) => (reason.clone(), theme.danger),
        };
        h_flex()
            .gap_2()
            .px_4()
            .py_2()
            .border_t_1()
            .border_color(theme.sidebar_border)
            .text_xs()
            .child(div().size(px(6.)).flex_none().rounded_full().bg(color))
            .child(div().min_w_0().truncate().text_color(theme.muted_foreground).child(label))
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (title, subtitle): (SharedString, Option<SharedString>) = match (&self.target, &self.thread) {
            (Some(Target::Thread(_)), Some(thread)) => match &thread.state.projection {
                Some(projection) => {
                    let branch = projection.thread.branch.clone();
                    (projection.thread.title.clone().into(), branch.map(Into::into))
                }
                None => ("Loading…".into(), None),
            },
            (Some(Target::Draft { project_id }), _) => {
                let project = self.shell.projects.iter().find(|p| &p.id == project_id);
                (format!("New thread in {}", project.map(project_name).unwrap_or_default()).into(), None)
            }
            _ => ("".into(), None),
        };
        h_flex()
            .h(px(44.))
            .flex_none()
            .px_4()
            .gap_3()
            .border_b_1()
            .border_color(theme.border)
            .child(div().min_w_0().truncate().font_medium().child(title))
            .children(subtitle.map(|s| div().flex_none().text_xs().text_color(theme.muted_foreground).child(s)))
    }

    fn render_error(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let error = self.error.clone()?;
        let theme = cx.theme();
        Some(
            h_flex()
                .mx_4()
                .mt_2()
                .px_3()
                .py_2()
                .gap_2()
                .rounded_md()
                .bg(theme.danger.opacity(0.12))
                .text_color(theme.danger)
                .child(div().flex_1().min_w_0().child(error))
                .child(Button::new("dismiss-error").ghost().xsmall().icon(IconName::Close).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.error = None;
                        cx.notify();
                    },
                ))),
        )
    }

    fn render_body(&self, _: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let empty = |text: &'static str| {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_color(theme.muted_foreground)
                .child(text)
                .into_any_element()
        };
        match (&self.target, &self.thread) {
            (None, _) => empty("Pick a thread, or press + next to a project to start one."),
            (Some(Target::Draft { .. }), _) => empty("Describe the task to start a new thread."),
            (Some(Target::Thread(_)), Some(thread)) if thread.state.projection.is_none() => empty("Loading thread…"),
            (Some(Target::Thread(_)), _) => {
                let rows = self.rows.clone();
                div()
                    .flex_1()
                    .min_h_0()
                    .child(
                        MessageScroller::new("timeline", self.scroller.clone(), move |index, _, cx| {
                            match rows.get(index) {
                                Some(row) => render_row(row, cx),
                                None => div().into_any_element(),
                            }
                        })
                        .with_list_style(StyleRefinement::default().py_4())
                        // Rows pad themselves; the scroller's default 32px gap is too loose.
                        .with_row_style(StyleRefinement::default().pb(px(0.)))
                        .with_bottom_fade(theme.background),
                    )
                    .into_any_element()
            }
        }
    }

    fn render_approvals(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let projection = self.thread.as_ref()?.state.projection.as_ref()?;
        let approvals = projection.pending_approvals();
        if approvals.is_empty() {
            return None;
        }
        let theme = cx.theme();
        let mut column = v_flex().gap_2().w_full().max_w(px(CONTENT_WIDTH)).mx_auto().px_4().pb_2();
        for (request, item) in approvals {
            let prompt = item.and_then(|i| i.str("prompt").or(i.str("reason"))).unwrap_or_default().to_owned();
            let options: Vec<(String, String)> = item
                .and_then(|i| i.raw.get("options"))
                .and_then(Value::as_array)
                .map(|options| {
                    options
                        .iter()
                        .filter_map(|o| {
                            Some((o.get("decision")?.as_str()?.to_owned(), o.get("label")?.as_str()?.to_owned()))
                        })
                        .collect()
                })
                .filter(|options: &Vec<_>| !options.is_empty())
                .unwrap_or_else(|| {
                    vec![
                        ("accept".into(), "Approve".into()),
                        ("acceptForSession".into(), "Approve for session".into()),
                        ("decline".into(), "Decline".into()),
                    ]
                });
            let live = request.is_live();
            let mut buttons = h_flex().gap_2().flex_wrap();
            for (decision, label) in options {
                let request_id = request.id.clone();
                let id = SharedString::from(format!("approval-{}-{decision}", request.id));
                let button = Button::new(id).small().label(label).disabled(!live);
                let button = if decision.starts_with("accept") { button.primary() } else { button.outline() };
                buttons = buttons.child(button.on_click(
                    cx.listener(move |this, _, _, cx| this.respond(request_id.clone(), decision.clone(), cx)),
                ));
            }
            column = column.child(
                v_flex()
                    .gap_2()
                    .p_3()
                    .rounded_lg()
                    .border_1()
                    .border_color(theme.warning.opacity(0.5))
                    .bg(theme.warning.opacity(0.08))
                    .child(div().font_medium().child(format!("Approval needed: {}", request.kind)))
                    .when(!prompt.is_empty(), |el| {
                        el.child(
                            div().font_family("monospace").text_xs().max_h(px(160.)).overflow_hidden().child(prompt),
                        )
                    })
                    .when(!live, |el| {
                        el.child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child("This request can no longer be answered here."),
                        )
                    })
                    .child(buttons),
            );
        }
        Some(column)
    }

    fn render_model_picker(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let label: SharedString = match self.composer_model() {
            Some(model) => self.server_info.model_name(&model).map(str::to_owned).unwrap_or(model.model).into(),
            None => "Pick a model".into(),
        };
        let choices: Rc<Vec<(SharedString, ModelSelection)>> = Rc::new(self.server_info.model_choices());
        let current = self.composer_model();
        let view = cx.entity();
        Button::new("model-picker")
            .ghost()
            .small()
            .label(label)
            .disabled(self.target.is_none() || choices.is_empty())
            .dropdown_menu(move |mut menu, window, _| {
                for (label, selection) in choices.iter() {
                    let picked = selection.clone();
                    menu = menu.item(
                        PopupMenuItem::new(label.clone()).checked(current.as_ref() == Some(selection)).on_click(
                            window.listener_for(&view, move |this, _, _, cx| {
                                this.model_override = Some(picked.clone());
                                cx.notify();
                            }),
                        ),
                    );
                }
                menu.max_h(px(360.))
            })
    }

    fn render_composer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let running = self.thread.as_ref().and_then(|t| t.state.projection.as_ref()).is_some_and(|p| p.is_running());
        let can_send = self.target.is_some() && matches!(self.connection, Connection::Connected(_));
        let action = if running {
            Button::new("stop")
                .small()
                .danger()
                .icon(IconName::Square)
                .label("Stop")
                .on_click(cx.listener(|this, _, _, cx| this.stop(cx)))
        } else {
            Button::new("send")
                .small()
                .primary()
                .icon(IconName::ArrowUp)
                .label("Send")
                .loading(self.sending)
                .disabled(!can_send)
                .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx)))
        };
        div().w_full().flex_none().px_4().pb_4().child(
            v_flex()
                .w_full()
                .max_w(px(CONTENT_WIDTH))
                .mx_auto()
                .gap_1()
                .p_2()
                .rounded_xl()
                .border_1()
                .border_color(theme.border)
                .bg(theme.secondary)
                .child(Textarea::new(&self.composer).appearance(false).disabled(self.target.is_none()))
                .child(h_flex().justify_between().child(self.render_model_picker(cx)).child(action)),
        )
    }
}

fn thread_row_frame(id: SharedString, selected: bool, cx: &App) -> gpui_kit::Stateful<gpui_kit::Div> {
    let theme = cx.theme();
    h_flex()
        .id(ElementId::Name(id))
        .gap_2()
        .px_2()
        .py_1()
        .rounded_md()
        .cursor_pointer()
        .when(selected, |el| el.bg(theme.sidebar_accent).text_color(theme.sidebar_accent_foreground))
        .when(!selected, |el| el.hover(|style| style.bg(theme.sidebar_accent.opacity(0.5))))
}

fn status_badge(label: &'static str, color: Hsla) -> impl IntoElement {
    h_flex()
        .flex_none()
        .gap_1()
        .text_xs()
        .text_color(color)
        .child(div().size(px(6.)).rounded_full().bg(color))
        .child(label)
}

fn render_row(row: &Row, cx: &mut App) -> AnyElement {
    let theme = cx.theme();
    let frame = || div().w_full().max_w(px(CONTENT_WIDTH)).mx_auto().px_4().py_1p5();
    match row {
        Row::User { text, .. } => frame()
            .child(h_flex().justify_end().child(
                div().max_w(px(CONTENT_WIDTH * 0.8)).px_3().py_2().rounded_lg().bg(theme.secondary).child(text.clone()),
            ))
            .into_any_element(),
        Row::Assistant { id, text, .. } => frame()
            .child(TextView::markdown(ElementId::Name(id.clone()), text.clone()).selectable(true))
            .into_any_element(),
        Row::Plan { id, markdown } => frame()
            .child(
                v_flex()
                    .gap_2()
                    .p_3()
                    .rounded_lg()
                    .border_1()
                    .border_color(theme.border)
                    .child(div().text_xs().font_medium().text_color(theme.muted_foreground).child("Proposed plan"))
                    .child(TextView::markdown(ElementId::Name(id.clone()), markdown.clone()).selectable(true)),
            )
            .into_any_element(),
        Row::Work { kind, label, detail, status, .. } => {
            let (icon, color) = work_icon(*kind, status, cx);
            frame()
                .child(
                    h_flex()
                        .gap_2()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(Icon::new(icon).xsmall().text_color(color))
                        .child(div().min_w_0().truncate().child(label.clone()))
                        .children(detail.clone().map(|d| div().flex_none().child(d))),
                )
                .into_any_element()
        }
    }
}

fn work_icon(kind: WorkKind, status: &str, cx: &App) -> (IconName, Hsla) {
    let theme = cx.theme();
    if status == "failed" {
        return (IconName::CircleX, theme.danger);
    }
    let icon = match kind {
        WorkKind::Thinking => IconName::Bot,
        WorkKind::Command => IconName::SquareTerminal,
        WorkKind::Edit => IconName::FileText,
        WorkKind::Search => IconName::Search,
        WorkKind::Tool => IconName::Settings2,
        WorkKind::Approval => IconName::TriangleAlert,
        WorkKind::Question => IconName::Info,
        WorkKind::Error => return (IconName::CircleAlert, theme.danger),
        WorkKind::Notice => IconName::Info,
        WorkKind::Agent => IconName::Bot,
    };
    (icon, theme.muted_foreground)
}
