//! One terminal pane outside any thread's drawer: the Welcome page's setup terminal
//! (`AgentInstallTerminal`'s `TerminalViewport`). The caller opens the terminal on the server
//! first; the pane only attaches to it, so it never starts a second shell.

use gpui_kit::{AnyView, AppContext as _, Context, Entity, Subscription, Window};

use super::session::{Launch, SessionEvent, TerminalSession};
use crate::app::Workspace;
use crate::rpc::RpcClient;

pub(in crate::app) struct StandaloneTerminal {
    session: Entity<TerminalSession>,
    /// The socket and grant it attached with, to notice a reconnect or a grant change.
    client: RpcClient,
    read_only: bool,
    _events: Subscription,
}

impl StandaloneTerminal {
    /// Attaches to `terminal_id` of `thread_id`; `on_exit` runs once the shell ends. It has no
    /// composer, so its menus offer no Add to chat.
    pub(in crate::app) fn new(
        thread_id: String,
        terminal_id: String,
        cwd: String,
        client: RpcClient,
        on_exit: impl Fn(&mut Workspace, &mut Window, &mut Context<Workspace>) + 'static,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Self {
        let launch = Launch { cwd: cwd.clone(), worktree_path: None, project_root: cwd, stop_on_settle: None };
        let session = cx.new(|cx| {
            let mut session = TerminalSession::new(thread_id, terminal_id, launch, None, false, window, cx);
            session.can_add_to_chat = false;
            session.attach(client.clone(), cx);
            session
        });
        let _events = cx.subscribe_in(&session, window, move |this, _, event: &SessionEvent, window, cx| match event {
            SessionEvent::Exited => on_exit(this, window, cx),
            SessionEvent::OpenUrl { url, .. } => cx.open_url(url),
            SessionEvent::Activated | SessionEvent::AddToChat(_) | SessionEvent::OpenPath(_) => {}
        });
        Self { session, client, read_only: false, _events }
    }

    /// Follows its environment like a drawer pane: a new socket reattaches, and without
    /// `terminal:operate` it only observes.
    pub(in crate::app) fn sync(&mut self, client: Option<&RpcClient>, read_only: bool, cx: &mut Context<Workspace>) {
        if let Some(client) = client.filter(|client| !client.same_socket(&self.client)) {
            self.client = client.clone();
            self.session.update(cx, |session, cx| session.attach(client.clone(), cx));
        }
        if std::mem::replace(&mut self.read_only, read_only) != read_only {
            self.session.update(cx, |session, cx| session.set_read_only(read_only, cx));
        }
    }

    pub(in crate::app) fn view(&self) -> AnyView {
        self.session.clone().into()
    }

    pub(in crate::app) fn focus(&self, window: &mut Window, cx: &mut Context<Workspace>) {
        let focus = self.session.read(cx).focus.clone();
        focus.focus(window, cx);
    }
}
