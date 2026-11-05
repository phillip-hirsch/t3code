//! Side chats (`SideChatPane.tsx`, `useSideChat.ts`): a hidden fork of a thread's latest stable
//! point that takes a side question in its own right-panel tab while the thread keeps working.
//! The tab shows the side chat's timeline, its pending approvals and a plain-text composer;
//! Bring back hands its last reply to the main thread's next message.

use std::collections::HashMap;
use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::message_scroller::{MessageScroller, MessageScrollerState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, ElementId, Entity, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, PromptLevel, SharedString, StatefulInteractiveElement as _, StyleRefinement, Styled as _,
    Subscription, Task, Window, div, px,
};
use serde_json::{Value, json};

use super::right_panel::Surface;
use super::titlebar::icon;
use super::{Target, Workspace, new_id};
use crate::model::{Run, ThreadProjection, ThreadShell, ThreadState};
use crate::theme::{self, Tokens};
use crate::timeline::{self, Row};

/// The side chat tab on screen, subscribed while it shows.
pub(super) struct SideChatView {
    thread_id: String,
    state: ThreadState,
    pub(super) rows: Rc<Vec<Row>>,
    cache: timeline::EntryCache,
    pub(super) scroller: Entity<MessageScrollerState>,
    composer: Entity<TextareaState>,
    sending: bool,
    _subscription: Option<Task<()>>,
    _events: Vec<Subscription>,
}

#[derive(Default)]
pub(super) struct SideChats {
    pub(super) view: Option<SideChatView>,
    /// A side chat is being forked; another start waits for it (`pendingRef`).
    starting: bool,
    bringing_back: bool,
    deleting: bool,
    /// Unsent composer text of side chats not on screen, by thread id.
    drafts: HashMap<String, String>,
}

impl SideChats {
    /// Drops the side chat's stream with the socket it ran on; it subscribes again on reconnect.
    pub(super) fn disconnect(&mut self) {
        if let Some(view) = &mut self.view {
            view._subscription = None;
        }
    }

    /// Releases the view on screen, keeping its unsent text for when it shows again.
    fn release_view(&mut self, cx: &App) {
        let Some(view) = self.view.take() else { return };
        let text = view.composer.read(cx).value().to_string();
        if text.is_empty() {
            self.drafts.remove(&view.thread_id);
        } else {
            self.drafts.insert(view.thread_id, text);
        }
    }
}

/// What kind of thread would start the side chat.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Origin {
    SideChat,
    Subagent,
    /// A thread the server knows.
    Thread,
    /// A draft, which has no thread to fork yet.
    Draft,
}

/// `sideChatDisabledReason` in `ChatView.tsx`; `None` when one can start. `capability` is
/// `environment.capabilities.sideChats`, `None` before the server config arrived.
fn disabled_reason(origin: Origin, capability: Option<bool>, worktree_pending: bool) -> Option<&'static str> {
    match (origin, capability) {
        (Origin::SideChat, _) => Some("Side chats cannot start side chats."),
        (Origin::Subagent, _) => Some("Subagent threads cannot start side chats."),
        (_, None) => Some("Side chats are not ready yet. Try again in a moment."),
        (_, Some(false)) => Some("Update this environment's T3 Code server to use side chats."),
        (Origin::Draft, _) => Some("Side chats start after the first message."),
        (Origin::Thread, _) if worktree_pending => Some("Available once the thread's worktree is set up."),
        _ => None,
    }
}

/// `parseSideChatCommand`: `/side` or `/btw`, then the prompt.
pub(super) fn parse_command(text: &str) -> Option<&str> {
    let text = text.trim();
    let rest = ["/side", "/btw"].iter().find_map(|command| {
        let head = text.get(..command.len())?;
        head.eq_ignore_ascii_case(command).then(|| &text[command.len()..])
    })?;
    (rest.is_empty() || rest.starts_with(char::is_whitespace)).then(|| rest.trim())
}

/// `resolveLatestMergeBackRun`: the newest finished reply, unless a newer run is under way.
fn merge_back_run(projection: &ThreadProjection) -> Option<&Run> {
    let latest = projection
        .runs
        .iter()
        .filter(|run| matches!(run.status.as_str(), "waiting" | "completed"))
        .max_by_key(|run| run.ordinal)?;
    let newer_active = projection
        .runs
        .iter()
        .any(|run| run.ordinal > latest.ordinal && matches!(run.status.as_str(), "preparing" | "starting" | "running"));
    (!newer_active).then_some(latest)
}

/// `sideChatTabState`: what the tab's dot shows.
fn tab_state(shell: &ThreadShell, facts: crate::inbox::Facts) -> Option<&'static str> {
    match shell.pending_runtime_request.as_ref().map(|r| r.kind.as_str()) {
        Some("user_input") => return Some("needs input"),
        Some("auth_refresh") | None => {}
        Some(_) => return Some("needs approval"),
    }
    match facts.child_request {
        Some(crate::inbox::ChildRequest::Approval) => Some("needs approval"),
        Some(crate::inbox::ChildRequest::Input) => Some("needs input"),
        None => facts.runtime_status().filter(|s| crate::model::is_active_run_status(s)).map(|_| "running"),
    }
}

/// `orchestrationV2WorktreePending`: the thread's worktree is still being prepared, or failed to.
fn worktree_pending(worktree_path: Option<&str>, runs: &[Run]) -> bool {
    worktree_path.is_none()
        && runs.iter().any(|run| {
            matches!(run.status.as_str(), "preparing" | "failed" | "interrupted")
                && run.workspace_preparation.as_ref().and_then(|p| p["type"].as_str()) == Some("worktree")
        })
}

impl Workspace {
    fn shell_thread(&self, thread_id: &str) -> Option<&ThreadShell> {
        self.shell.threads.iter().find(|t| t.id == thread_id)
    }

    /// The open thread as a side chat source, if any.
    fn side_chat_origin(&self) -> Option<Origin> {
        Some(match self.target.as_ref()? {
            Target::Draft { .. } => Origin::Draft,
            Target::Thread { id, .. } => match self.shell_thread(id) {
                Some(shell) if shell.lineage.side_chat == Some(true) => Origin::SideChat,
                Some(shell) if shell.lineage.relationship_to_parent.as_deref() == Some("subagent") => Origin::Subagent,
                _ => Origin::Thread,
            },
        })
    }

    /// `sideChatsOffered`: the launcher lists Side chat; whether it can start is
    /// [`Self::side_chat_disabled_reason`].
    pub(super) fn side_chats_offered(&self) -> bool {
        matches!(self.side_chat_origin(), Some(Origin::Thread | Origin::Draft))
            && self.server_info().capability("sideChats")
    }

    pub(super) fn side_chat_disabled_reason(&self) -> Option<&'static str> {
        let origin = self.side_chat_origin()?;
        let capability = self.client().map(|_| self.server_info().capability("sideChats"));
        let pending = match &self.target {
            Some(Target::Thread { id, .. }) => worktree_pending(
                self.shell_thread(id).and_then(|t| t.worktree_path.as_deref()),
                self.open_projection().map_or(&[], |p| p.runs.as_slice()),
            ),
            _ => false,
        };
        disabled_reason(origin, capability, pending)
    }

    /// `useSideChat().open`: forks the open thread's latest stable point, sends `prompt` there,
    /// then shows the side chat's tab. `on_started` runs once it did.
    pub(super) fn start_side_chat(
        &mut self,
        prompt: Option<String>,
        on_started: impl FnOnce(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.side_chat_disabled_reason().is_some() || self.side_chat.starting {
            return;
        }
        let (Some(client), Some(Target::Thread { env, id: source })) = (self.client(), self.target.clone()) else {
            return;
        };
        let origin = Target::Thread { env, id: source.clone() };
        self.side_chat.starting = true;
        let target = new_id();
        let fork = json!({
            "type": "thread.fork",
            "commandId": new_id(),
            "createdBy": "user",
            "creationSource": "web",
            "sourceThreadId": source,
            "targetThreadId": target,
            "sourcePoint": {"type": "latest_stable"},
            "sideChat": true,
            "title": "Side chat",
        });
        let text = prompt.map(|p| p.trim().to_owned()).filter(|p| !p.is_empty());
        cx.spawn_in(window, async move |this, cx| {
            let forked = client.request("orchestration.dispatchCommand", fork).await;
            let sent = match (&forked, text) {
                (Err(_), _) | (_, None) => None,
                // Sent before the tab shows: closing a tab whose side chat never ran deletes it.
                (Ok(_), Some(text)) => Some(
                    client.request("orchestration.dispatchCommand", turn_command(&target, &text, false, "steer")).await,
                ),
            };
            let _ = this.update_in(cx, |this, window, cx| {
                this.side_chat.starting = false;
                // On the socket it was forked on: the shell may not list the side chat yet.
                let delete_unused = |cx: &mut Context<Self>| {
                    let (client, command) = (client.clone(), delete_command(&target, true));
                    cx.spawn(async move |_, _| client.request("orchestration.dispatchCommand", command).await).detach();
                };
                let failure = match (forked, sent) {
                    (Err(error), _) => Some(error.to_string()),
                    (Ok(_), Some(Err(error))) => {
                        // Drops the empty side chat the failed send left behind.
                        delete_unused(cx);
                        Some(error.to_string())
                    }
                    _ => None,
                };
                match failure {
                    Some(error) => this.error = Some(error.into()),
                    // The source thread left the screen meanwhile; its tab would open on another
                    // thread's panel, so a side chat that never ran goes.
                    None if this.target.as_ref() != Some(&origin) => delete_unused(cx),
                    None => {
                        this.open_surface(Surface::SideChat(target), window, cx);
                        on_started(this, window, cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The launcher's Side chat: one with no question yet.
    pub(super) fn add_side_chat_surface(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.start_side_chat(None, |_, _, _| {}, window, cx);
    }

    /// `/side <question>` from the composer (`useComposerSend`): side chats take text only.
    /// `true` when the text was the command, sent or refused.
    pub(super) fn submit_side_chat_command(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(prompt) = parse_command(text) else { return false };
        let warn = |title: &'static str, description: &'static str, window: &mut Window, cx: &mut Context<Self>| {
            window.push_notification(Notification::warning(description).title(title), cx);
        };
        if let Some(reason) = self.side_chat_disabled_reason() {
            warn("Side chat not started", reason, window, cx);
            return true;
        }
        if self.has_attachments() {
            warn("Side chats take text only", "Remove the attachments and context, then send it again.", window, cx);
            return true;
        }
        let sent = text.to_owned();
        self.start_side_chat(
            Some(prompt.to_owned()),
            move |this, window, cx| {
                // Clear only what was sent; the draft may have changed meanwhile.
                if this.composer.read(cx).value().trim() == sent.trim() {
                    this.set_composer_text("", window, cx);
                }
            },
            window,
            cx,
        );
        true
    }

    /// A side chat's tab: its shell title, and the dot for a run or a request.
    pub(super) fn side_chat_tab(&self, thread_id: &str) -> (SharedString, Option<&'static str>) {
        let Some(shell) = self.shell_thread(thread_id) else { return ("Side chat".into(), None) };
        let requests = crate::inbox::child_requests(&self.shell.threads);
        let state = tab_state(shell, crate::inbox::Facts::new(shell, &requests));
        (shell.title.clone().into(), state)
    }

    /// `releaseSurfaceResources` for a side chat: the server keeps one that ran or got a message.
    pub(super) fn release_side_chat(&mut self, thread_id: &str, cx: &mut Context<Self>) {
        if self.shell_thread(thread_id).is_some() {
            self.dispatch(delete_command(thread_id, true), cx);
        }
    }

    /// Subscribes the side chat on screen and drops the one that left it.
    pub(super) fn sync_side_chat(&mut self, shown: Option<&str>, window: &mut Window, cx: &mut Context<Self>) {
        let client = self.client();
        // While the socket is down the view keeps what it last showed.
        if let Some(view) = &self.side_chat.view
            && Some(view.thread_id.as_str()) == shown
            && (view._subscription.is_some() || client.is_none())
        {
            return;
        }
        self.side_chat.release_view(cx);
        let (Some(thread_id), Some(client)) = (shown, client) else { return };
        let draft = self.side_chat.drafts.remove(thread_id).unwrap_or_default();
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 8)
                .submit_on_enter(true)
                .placeholder("Ask a side question")
                .default_value(draft)
        });
        let events = vec![cx.subscribe_in(&composer, window, |this, _, event: &InputEvent, window, cx| {
            if let InputEvent::PressEnter { secondary: false, .. } = event {
                this.send_side_chat(window, cx);
            }
        })];
        let scroller = cx.new(|cx| MessageScrollerState::new(0, cx));
        let mut events = events;
        events.push(cx.observe(&scroller, |_, _, cx| cx.notify()));
        let stream = client
            .stream("orchestration.subscribeThread", json!({"threadId": thread_id, "requestCompletionMarker": true}));
        let (apply_id, batch_id) = (thread_id.to_owned(), thread_id.to_owned());
        let subscription = Self::pump(
            stream,
            window,
            cx,
            move |this, value| {
                let Some(view) = this.side_chat.view.as_mut().filter(|v| v.thread_id == apply_id) else { return };
                if let Err(error) = view.state.apply(value) {
                    log::warn!("bad side chat event: {error}");
                }
            },
            move |this, _, cx| {
                if this.side_chat.view.as_ref().is_some_and(|v| v.thread_id == batch_id) {
                    this.refresh_side_chat_rows(cx);
                }
            },
        );
        self.side_chat.view = Some(SideChatView {
            thread_id: thread_id.to_owned(),
            state: ThreadState::default(),
            rows: Rc::default(),
            cache: timeline::EntryCache::default(),
            scroller,
            composer,
            sending: false,
            _subscription: Some(subscription),
            _events: events,
        });
    }

    /// Rebuilds the side chat's rows. Its edit-from-here would rewind the main thread, so it is off.
    pub(super) fn refresh_side_chat_rows(&mut self, cx: &mut Context<Self>) {
        let Some(view) = self.side_chat.view.as_mut() else { return };
        let mut rows = match view.state.projection.as_ref() {
            Some(projection) => timeline::rows(projection, &self.expanded, &mut view.cache),
            None => Vec::new(),
        };
        for row in &mut rows {
            if let Row::User { revert, .. } = row {
                *revert = None;
            }
        }
        let was_empty = view.rows.is_empty();
        let Some((range, count)) = timeline::changed_range(&view.rows, &rows) else { return };
        let total = rows.len();
        view.rows = Rc::new(rows);
        view.scroller.update(cx, |scroller, cx| {
            if was_empty {
                scroller.reset(total, cx);
            } else {
                scroller.splice(range, count, cx);
            }
        });
        cx.notify();
    }

    fn side_chat_projection(&self) -> Option<&ThreadProjection> {
        self.side_chat.view.as_ref()?.state.projection.as_ref()
    }

    fn send_side_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(view) = self.side_chat.view.as_ref() else { return };
        let text = view.composer.read(cx).value().trim().to_owned();
        if text.is_empty() || view.sending {
            return;
        }
        let projection = view.state.projection.as_ref();
        let running = projection.is_some_and(ThreadProjection::is_running);
        // A send declines a waiting approval, so it steers.
        let follow_up = if projection.is_some_and(|p| !p.pending_approvals().is_empty()) {
            "steer"
        } else {
            self.client_settings.values.get("followUpBehavior").as_str().unwrap_or("steer")
        };
        let command = turn_command(&view.thread_id, &text, running, follow_up);
        let (Some(client), composer) = (self.client(), view.composer.clone()) else { return };
        let thread_id = view.thread_id.clone();
        if let Some(view) = self.side_chat.view.as_mut() {
            view.sending = true;
        }
        composer.update(cx, |state, cx| state.set_value("", window, cx));
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("orchestration.dispatchCommand", command).await;
            let _ = this.update_in(cx, |this, window, cx| {
                let Some(view) = this.side_chat.view.as_mut().filter(|v| v.thread_id == thread_id) else { return };
                view.sending = false;
                if let Err(error) = result {
                    // Gives the text back rather than lose it.
                    if view.composer.read(cx).value().is_empty() {
                        view.composer.update(cx, |state, cx| state.set_value(text.clone(), window, cx));
                    }
                    this.error = Some(error.to_string().into());
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn stop_side_chat(&mut self, cx: &mut Context<Self>) {
        let Some(projection) = self.side_chat_projection() else { return };
        let thread_id = projection.thread.id.clone();
        let command = if self.server_info().capability("threadStop") {
            json!({"type": "thread.stop", "commandId": new_id(), "threadId": thread_id})
        } else {
            let Some(run) = projection.interruptible_run() else { return };
            json!({"type": "run.interrupt", "commandId": new_id(), "threadId": thread_id, "runId": run.id, "holdQueue": true})
        };
        self.dispatch(command, cx);
    }

    fn respond_side_chat(&mut self, request_id: &str, decision: &str, cx: &mut Context<Self>) {
        let Some(view) = self.side_chat.view.as_ref() else { return };
        let command = json!({
            "type": "runtime-request.respond",
            "commandId": new_id(),
            "threadId": view.thread_id,
            "requestId": request_id,
            "decision": decision,
        });
        self.dispatch(command, cx);
    }

    /// Why Bring back is unavailable (`bringBackBlockedReason`), else the run it merges.
    fn bring_back_blocked(&self, parent_id: &str) -> Result<String, &'static str> {
        let parent_active = self.shell_thread(parent_id).is_some_and(|shell| {
            let requests = crate::inbox::child_requests(&self.shell.threads);
            crate::inbox::Facts::new(shell, &requests).runtime_status().is_some_and(crate::model::is_active_run_status)
        });
        if parent_active {
            return Err("Available when the main thread is idle.");
        }
        self.side_chat_projection()
            .and_then(merge_back_run)
            .map(|run| run.id.clone())
            .ok_or("Available after a finished reply.")
    }

    fn bring_back(&mut self, parent_id: String, thread_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let (Ok(run_id), Some(client)) = (self.bring_back_blocked(&parent_id), self.client()) else { return };
        if self.side_chat.bringing_back {
            return;
        }
        self.side_chat.bringing_back = true;
        let command = json!({
            "type": "thread.merge_back",
            "commandId": new_id(),
            "createdBy": "user",
            "creationSource": "web",
            "sourceThreadId": thread_id,
            "targetThreadId": parent_id,
            "sourcePoint": {"type": "run", "runId": run_id},
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("orchestration.dispatchCommand", command).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.side_chat.bringing_back = false;
                let notification = match result {
                    Ok(_) => Notification::success("The main thread gets this side chat with your next message there.")
                        .title("Brought back"),
                    Err(error) => Notification::error(error.to_string()).title("Could not bring back the side chat"),
                };
                window.push_notification(notification, cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn delete_side_chat(&mut self, thread_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(title) = self.shell_thread(&thread_id).map(|t| t.title.clone()) else { return };
        if self.side_chat.deleting {
            return;
        }
        let confirm = self.client_settings.values.get("confirmThreadDelete").as_bool() == Some(true);
        let answer = confirm.then(|| {
            window.prompt(
                PromptLevel::Critical,
                &format!("Delete side chat \"{title}\"?"),
                Some("This permanently clears its conversation history."),
                &["Delete", "Cancel"],
                cx,
            )
        });
        let client = self.client();
        cx.spawn_in(window, async move |this, cx| {
            if let Some(answer) = answer {
                if answer.await != Ok(0) {
                    return;
                }
            }
            let Some(client) = client else { return };
            let _ = this.update(cx, |this, cx| {
                this.side_chat.deleting = true;
                cx.notify();
            });
            let result = client.request("orchestration.dispatchCommand", delete_command(&thread_id, false)).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.side_chat.deleting = false;
                match result {
                    Ok(_) => this.close_side_chat_tab(&thread_id, window, cx),
                    Err(error) => window.push_notification(
                        Notification::error(error.to_string()).title("Could not delete the side chat"),
                        cx,
                    ),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The tab of a side chat, as the right panel renders it.
    pub(super) fn render_side_chat_surface(
        &mut self,
        thread_id: &str,
        width: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = *theme::tokens(cx);
        let Some(Target::Thread { id: parent_id, .. }) = self.target.clone() else { return div().into_any_element() };
        let shell = self.shell_thread(thread_id);
        let title: SharedString = shell.map_or_else(|| "Side chat".into(), |s| s.title.clone().into());
        let blocked = self.bring_back_blocked(&parent_id);
        let tooltip: SharedString =
            blocked.as_ref().err().copied().unwrap_or("Adds this side chat to the main thread's next message.").into();
        let can_bring_back = blocked.is_ok() && !self.side_chat.bringing_back;
        let (bring_parent, bring_thread, delete_thread, close_thread) =
            (parent_id.clone(), thread_id.to_owned(), thread_id.to_owned(), thread_id.to_owned());
        let header =
            h_flex()
                .h(px(36.))
                .flex_none()
                .gap_1()
                .pl_3()
                .pr_1()
                .border_b_1()
                .border_color(t.border)
                .child(div().min_w_0().flex_1().truncate().text_xs().font_weight(FontWeight::MEDIUM).child(title))
                .child(
                    div()
                        .id("side-chat-bring-back")
                        .child(
                            Button::new("side-chat-bring-back-button")
                                .ghost()
                                .xsmall()
                                .label("Bring back")
                                .when(!can_bring_back, |b| b.opacity(0.64))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.bring_back(bring_parent.clone(), bring_thread.clone(), window, cx)
                                })),
                        )
                        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx)),
                )
                .child(
                    Button::new("side-chat-delete")
                        .ghost()
                        .xsmall()
                        .icon(icon("icons/trash-2.svg"))
                        .disabled(shell.is_none() || self.side_chat.deleting)
                        .tooltip("Delete side chat")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.delete_side_chat(delete_thread.clone(), window, cx)
                        })),
                )
                .child(
                    Button::new("side-chat-close")
                        .ghost()
                        .xsmall()
                        .icon(Icon::new(IconName::X))
                        .tooltip("Close side chat")
                        .on_click(
                            cx.listener(move |this, _, window, cx| this.close_side_chat_tab(&close_thread, window, cx)),
                        ),
                );
        let body = match (shell, self.side_chat.view.as_ref()) {
            (None, _) if self.active_environment().is_some_and(|id| self.shell.sequence(id) > 0) => div()
                .p_3()
                .text_xs()
                .text_color(t.muted_foreground)
                .child("This side chat was deleted.")
                .into_any_element(),
            (Some(_), Some(view)) => self.render_side_chat_body(view, width, &t, window, cx),
            _ => div().flex_1().into_any_element(),
        };
        v_flex().flex_1().min_h_0().child(header).child(body).into_any_element()
    }

    fn close_side_chat_tab(&mut self, thread_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.close_surface(Surface::SideChat(thread_id.to_owned()), window, cx);
    }

    fn render_side_chat_body(
        &self,
        view: &SideChatView,
        width: f32,
        t: &Tokens,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let projection = view.state.projection.as_ref();
        let timeline = match projection {
            None => quiet("Loading side chat…", t),
            Some(_) if view.rows.is_empty() => quiet("Ask a side question while this thread keeps working.", t),
            Some(_) => {
                let rows = view.rows.clone();
                let this = cx.weak_entity();
                let lane = (width - 32.).max(120.);
                let background = t.background;
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .child(
                        MessageScroller::new("side-chat-timeline", view.scroller.clone(), move |index, window, cx| {
                            match rows.get(index) {
                                Some(row) => super::chat::render_row(row, &this, lane, window, cx),
                                None => div().into_any_element(),
                            }
                        })
                        .with_list_style(StyleRefinement::default().pt(px(12.)).pb(px(12.)).pl(px(16.)).pr(px(16.)))
                        .with_row_style(StyleRefinement::default().pb(px(0.)))
                        .with_bottom_fade(background),
                    )
                    .into_any_element()
            }
        };
        let approvals: Vec<AnyElement> = projection
            .map(|p| p.pending_approvals())
            .unwrap_or_default()
            .into_iter()
            .filter(|(request, _)| request.is_live())
            .map(|(request, item)| {
                let label =
                    item.and_then(|i| i.str("prompt")).filter(|p| !p.is_empty()).unwrap_or("Approval requested");
                let choice = |decision: &'static str, text: &'static str, cx: &mut Context<Self>| {
                    let request_id = request.id.clone();
                    Button::new(ElementId::Name(format!("side-chat-{decision}-{}", request.id).into()))
                        .ghost()
                        .xsmall()
                        .label(text)
                        .on_click(cx.listener(move |this, _, _, cx| this.respond_side_chat(&request_id, decision, cx)))
                };
                h_flex()
                    .gap_2()
                    .px_3()
                    .py_1()
                    .text_xs()
                    .child(Icon::new(IconName::Shield).size(px(12.)).text_color(t.warning))
                    .child(div().flex_1().min_w_0().truncate().text_color(t.muted_foreground).child(label.to_owned()))
                    .child(choice("decline", "Decline", cx))
                    .child(choice("accept", "Approve", cx))
                    .into_any_element()
            })
            .collect();
        let running = projection.is_some_and(ThreadProjection::is_running);
        let composer = h_flex()
            .m_2()
            .p_2()
            .gap_2()
            .items_end()
            .rounded(px(12.))
            .border_1()
            .border_color(t.border)
            .bg(t.card)
            .child(
                Textarea::new(&view.composer)
                    .appearance(false)
                    .xsmall()
                    .flex_1()
                    .font_family(t.fonts.composer)
                    .text_size(px(t.fonts.prompt_size)),
            )
            .child(if running {
                Button::new("side-chat-stop")
                    .xsmall()
                    .icon(Icon::new(IconName::Square))
                    .tooltip("Stop")
                    .on_click(cx.listener(|this, _, _, cx| this.stop_side_chat(cx)))
            } else {
                Button::new("side-chat-send")
                    .primary()
                    .xsmall()
                    .icon(Icon::new(IconName::ArrowUp))
                    .tooltip("Send")
                    .disabled(view.sending)
                    .on_click(cx.listener(|this, _, window, cx| this.send_side_chat(window, cx)))
            });
        v_flex().flex_1().min_h_0().child(timeline).children(approvals).child(composer).into_any_element()
    }
}

fn quiet(text: &'static str, t: &Tokens) -> AnyElement {
    div()
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .px_5()
        .text_center()
        .text_xs()
        .text_color(t.muted_foreground.opacity(0.7))
        .child(text)
        .into_any_element()
}

/// `startThreadTurn` as a side chat sends it: `message.dispatch` with the follow-up delivery.
fn turn_command(thread_id: &str, text: &str, running: bool, follow_up: &str) -> Value {
    let queue = running && follow_up == "queue";
    let mut command = json!({
        "type": "message.dispatch",
        "commandId": new_id(),
        "createdBy": "user",
        "creationSource": "web",
        "threadId": thread_id,
        "messageId": new_id(),
        "text": text,
        "attachments": [],
        "dispatchMode": {"type": if queue { "queue_after_active" } else { "start_immediately" }},
    });
    if !queue {
        command["deliveryIntent"] = (if running { "steer" } else { "auto" }).into();
    }
    command
}

fn delete_command(thread_id: &str, only_if_unused: bool) -> Value {
    let mut command = json!({"type": "thread.delete", "commandId": new_id(), "threadId": thread_id});
    if only_if_unused {
        command["onlyIfUnused"] = true.into();
    }
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn side_command_takes_the_rest_as_its_prompt() {
        assert_eq!(parse_command("/side why is this slow?"), Some("why is this slow?"));
        assert_eq!(parse_command("  /BTW\n  and this "), Some("and this"));
        assert_eq!(parse_command("/side"), Some(""));
        assert_eq!(parse_command("/sidebar"), None);
        assert_eq!(parse_command("ask /side"), None);
    }

    #[test]
    fn disabled_reasons_follow_the_web_order() {
        assert_eq!(disabled_reason(Origin::Thread, Some(true), false), None);
        assert_eq!(disabled_reason(Origin::SideChat, None, false), Some("Side chats cannot start side chats."));
        assert_eq!(
            disabled_reason(Origin::Subagent, Some(true), false),
            Some("Subagent threads cannot start side chats.")
        );
        assert_eq!(
            disabled_reason(Origin::Thread, None, false),
            Some("Side chats are not ready yet. Try again in a moment.")
        );
        assert_eq!(
            disabled_reason(Origin::Draft, Some(false), false),
            Some("Update this environment's T3 Code server to use side chats.")
        );
        assert_eq!(
            disabled_reason(Origin::Draft, Some(true), false),
            Some("Side chats start after the first message.")
        );
        assert_eq!(
            disabled_reason(Origin::Thread, Some(true), true),
            Some("Available once the thread's worktree is set up.")
        );
    }

    fn projection(runs: Value) -> ThreadProjection {
        serde_json::from_value(json!({"thread": {"id": "t", "title": "T"}, "runs": runs})).unwrap()
    }

    #[test]
    fn bring_back_merges_the_newest_finished_reply() {
        let run = |id: &str, ordinal: i64, status: &str| json!({"id": id, "ordinal": ordinal, "status": status});
        let done = projection(json!([run("a", 1, "completed"), run("b", 2, "waiting"), run("c", 3, "failed")]));
        assert_eq!(merge_back_run(&done).map(|r| r.id.as_str()), Some("b"));
        let busy = projection(json!([run("a", 1, "completed"), run("b", 2, "running")]));
        assert!(merge_back_run(&busy).is_none(), "a newer run is still working");
        assert!(merge_back_run(&projection(json!([run("a", 1, "running")]))).is_none());
    }

    #[test]
    fn worktree_pending_needs_a_worktree_run_and_no_path() {
        let runs = projection(json!([
            {"id": "a", "ordinal": 1, "status": "preparing", "workspacePreparation": {"type": "worktree"}}
        ]))
        .runs;
        assert!(worktree_pending(None, &runs));
        assert!(!worktree_pending(Some("/w"), &runs));
        let root = projection(
            json!([{"id": "a", "ordinal": 1, "status": "preparing", "workspacePreparation": {"type": "root"}}]),
        );
        assert!(!worktree_pending(None, &root.runs));
    }

    #[test]
    fn turns_steer_or_queue_behind_a_running_reply() {
        let idle = turn_command("t", "hi", false, "queue");
        assert_eq!(
            (idle["dispatchMode"]["type"].as_str(), idle["deliveryIntent"].as_str()),
            (Some("start_immediately"), Some("auto"))
        );
        let queued = turn_command("t", "hi", true, "queue");
        assert_eq!(queued["dispatchMode"]["type"], "queue_after_active");
        assert!(queued.get("deliveryIntent").is_none());
        assert_eq!(turn_command("t", "hi", true, "steer")["deliveryIntent"], "steer");
    }
}
