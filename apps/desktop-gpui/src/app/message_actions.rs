//! The hover actions on timeline messages (`TimelineUserMessageRow.tsx`,
//! `TimelineAssistantRows.tsx`, `useTimelineRewind.ts`): copy with a moment of
//! "Copied", copy prompt link, edit from here, and fork from a response.

use std::time::Duration;

use futures::future::Either;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::{DialogClose, DialogEntrance, DialogFooter};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, WindowExt as _, v_flex};
use gpui_kit::{
    App, ClipboardItem, Context, ElementId, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Task, WeakEntity, Window, div, prelude::FluentBuilder as _, px,
};
use serde_json::{Value, json};

use gpui_kit::assets::IconName;

use super::attachments::{Attachment, MAX_ATTACHMENTS};
use super::composer::ComposerDraft;
use super::{Target, Workspace, new_id};
use crate::composer_text;
use crate::model::{Run, ThreadProjection, ThreadState};
use crate::rpc::{RpcClient, RpcStream, StreamEvent};
use crate::theme::Tokens;
use crate::timeline::Checkpoint;

/// `ANCHORED_COPY_TOAST_TIMEOUT_MS`.
const COPIED_FOR: Duration = Duration::from_millis(1000);
/// `MarkdownCodeBlock`'s `useCopyToClipboard` timeout.
const CODE_COPIED_FOR: Duration = Duration::from_millis(1200);
/// `waitForRevertedMessage`'s limit.
const REWIND_TIMEOUT: Duration = Duration::from_secs(120);
/// `prepareRevertedMessageAttachments`' fetch limit.
const ATTACHMENT_TIMEOUT: Duration = Duration::from_secs(30);
/// `PROMPT_LINK_MAX_PROMPT_LENGTH`.
const PROMPT_LINK_MAX: usize = 8000;

/// The copy button that just copied, shown as a check until the timer clears it.
#[derive(Default)]
pub(super) struct CopyFeedback {
    pub(super) id: Option<SharedString>,
    _reset: Option<Task<()>>,
}

impl CopyFeedback {
    pub(super) fn is(&self, id: &SharedString) -> bool {
        self.id.as_ref() == Some(id)
    }
}

/// `promptLinkFor`: a link that starts a new thread with this prompt.
pub(super) fn prompt_link(prompt: &str) -> Option<String> {
    let prompt = prompt.trim();
    if prompt.is_empty() || prompt.encode_utf16().count() > PROMPT_LINK_MAX {
        return None;
    }
    let mut url = String::from("https://app.t3.codes/new?prompt=");
    for byte in prompt.bytes() {
        match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'*' | b'-' | b'.' | b'_' => url.push(byte as char),
            b' ' => url.push('+'),
            _ => url.push_str(&format!("%{byte:02X}")),
        }
    }
    Some(url)
}

impl Workspace {
    pub(super) fn copied(&mut self, id: SharedString, duration: Duration, cx: &mut Context<Self>) {
        self.copy_feedback.id = Some(id);
        self.copy_feedback._reset = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(duration).await;
            let _ = this.update(cx, |this, cx| {
                this.copy_feedback.id = None;
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// "Fork from this response": a new thread from this run, then opens it.
    fn fork_from(
        &mut self,
        source_thread_id: SharedString,
        run_id: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.client() else { return };
        let title = self.thread.as_ref().and_then(|t| t.state.projection.as_ref()).map(|p| p.thread.title.clone());
        let target = new_id();
        let command = json!({
            "type": "thread.fork",
            "commandId": new_id(),
            "createdBy": "user",
            "creationSource": "web",
            "sourceThreadId": source_thread_id,
            "targetThreadId": target,
            "sourcePoint": {"type": "run", "runId": run_id},
            "title": format!("{} fork", title.unwrap_or_default()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("orchestration.dispatchCommand", command).await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok(_) => this.select_thread(target, window, cx),
                Err(_) => {
                    this.error = Some("Failed to fork this response.".into());
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// "Edit from here" (`rewindToMessage`): rolls the thread back to before the message,
    /// waits until the server has finished, then returns the message's text and
    /// attachments to that thread's composer.
    fn rewind(
        &mut self,
        message_id: SharedString,
        checkpoint: Checkpoint,
        restore_files: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (Some(client), Some(http_base)) = (self.client(), self.http_base().map(str::to_owned)) else { return };
        let Some(thread) = self.thread.as_ref() else { return };
        let Some(projection) = thread.state.projection.as_ref() else { return };
        let Some(message) = projection.visible_turn_items.iter().map(|row| &row.item).find(|i| *i.id == *message_id)
        else {
            return;
        };
        if self.rewinding.contains(&thread.id) {
            return;
        }
        let files: Vec<Value> = message.raw["attachments"].as_array().cloned().unwrap_or_default();
        let blocked = if self.attachments_busy() {
            Some("Wait for attachments to finish preparing before rewinding.")
        } else if self.attachments.len() + files.len() > MAX_ATTACHMENTS {
            Some("Make room for this message's attachments in the composer before rewinding.")
        } else {
            None
        };
        let witness = rewind_witness(&projection.runs, checkpoint.turn_count);
        let Some(witness) = witness.filter(|_| blocked.is_none()) else {
            let reason = blocked.unwrap_or("The thread has changed. Select a message to rewind again.");
            self.error = Some(reason.into());
            cx.notify();
            return;
        };
        let text = composer_text::recallable_prompt(message.str("text").unwrap_or_default());
        let (thread_id, command_id) = (thread.id.clone(), new_id());
        let command = json!({
            "type": "checkpoint.rollback",
            "restoreFiles": restore_files,
            "commandId": command_id,
            "threadId": thread_id,
            "scopeId": checkpoint.scope_id,
            "checkpointId": checkpoint.checkpoint_id,
        });
        self.rewinding.insert(thread_id.clone());
        self.error = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let timeout = cx.background_executor().timer(REWIND_TIMEOUT);
            let rewound = async {
                let files = download_attachments(&client, &http_base, files).await?;
                let stream = client.stream("orchestration.subscribeThread", json!({"threadId": thread_id}));
                client.request("orchestration.dispatchCommand", command).await.map_err(|e| e.to_string())?;
                wait_for_rollback(&stream, &command_id, &witness).await?;
                Ok(files)
            };
            let result = match futures::future::select(std::pin::pin!(rewound), timeout).await {
                Either::Left((result, _)) => result,
                Either::Right(_) => Err("Timed out waiting for the thread to rewind.".to_owned()),
            };
            let _ = this.update_in(cx, |this, window, cx| {
                this.rewinding.remove(&thread_id);
                match result {
                    Ok(files) => this.restore_rewound(Target::Thread(thread_id), text, files, window, cx),
                    Err(error) => this.error = Some(error.into()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Puts a rewound message back in its thread's draft, after anything typed there,
    /// and focuses it only when that thread is still on screen.
    fn restore_rewound(
        &mut self,
        origin: Target,
        text: String,
        files: Vec<Attachment>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let here = self.target.as_ref() == Some(&origin);
        if here {
            self.stash_composer(cx);
        }
        let merge = |current: &mut String| {
            if current.is_empty() || text.is_empty() {
                current.push_str(&text);
            } else {
                *current = format!("{}\n\n{text}", current.trim_end());
            }
        };
        let uploads: Vec<String> = files.iter().map(|file| file.local_id().to_owned()).collect();
        match self.composer_drafts.entry(origin).or_insert_with(|| ComposerDraft::Prompt(String::new(), Vec::new())) {
            ComposerDraft::Prompt(prompt, attachments) => {
                merge(prompt);
                attachments.extend(files);
            }
            // An open question holds the editor; its prompt waits for it to close.
            ComposerDraft::Question(question) => merge(&mut question.prompt),
        }
        if here {
            self.restore_composer(window, cx);
            self.focus_composer(window, cx);
        }
        for local_id in uploads {
            self.upload(local_id, cx);
        }
    }

    /// Whether the open thread's provider can rewind its conversation.
    pub(super) fn supports_rollback(&self) -> bool {
        let Some(projection) = self.thread.as_ref().and_then(|t| t.state.projection.as_ref()) else { return false };
        let instance = projection.thread.model_selection.as_ref().map(|m| m.instance_id.as_str());
        self.server_info
            .providers
            .iter()
            .find(|provider| Some(provider.instance_id.as_str()) == instance)
            .is_some_and(|provider| provider.supports_conversation_rollback != Some(false))
    }
}

/// The latest settled run after the target. The rollback marks it `rolled_back` in the
/// same write that ends it, after the provider and the files rewound.
fn rewind_witness(runs: &[Run], turn_count: i64) -> Option<String> {
    runs.iter()
        .filter(|run| run.ordinal > turn_count)
        .filter(|run| matches!(run.status.as_str(), "completed" | "interrupted" | "failed" | "cancelled"))
        .max_by_key(|run| run.ordinal)
        .map(|run| run.id.clone())
}

/// Whether the rollback `command_id` finished in this projection: `None` while it runs.
fn rollback_outcome(projection: &ThreadProjection, command_id: &str, witness: &str) -> Option<Result<(), String>> {
    if let Some(failure) = projection.thread.rollback_failure.as_ref().filter(|f| f.request_id == command_id) {
        return Some(Err(failure.message.clone()));
    }
    projection.runs.iter().any(|run| run.id == witness && run.status == "rolled_back").then_some(Ok(()))
}

/// Follows the thread until the rollback lands or fails (`waitForRevertedMessage`).
async fn wait_for_rollback(stream: &RpcStream, command_id: &str, witness: &str) -> Result<(), String> {
    let mut state = ThreadState::default();
    while let Some(StreamEvent::Item(event)) = stream.next().await {
        if state.apply(&event).is_err() {
            continue;
        }
        if let Some(outcome) = state.projection.as_ref().and_then(|p| rollback_outcome(p, command_id, witness)) {
            return outcome;
        }
    }
    Err("Lost the thread while it rewound.".into())
}

/// `prepareRevertedMessageAttachments`: each attachment's bytes, through a signed URL.
async fn download_attachments(
    client: &RpcClient,
    http_base: &str,
    files: Vec<Value>,
) -> Result<Vec<Attachment>, String> {
    let mut restored = Vec::with_capacity(files.len());
    for file in files {
        let (kind, id, name, mime) = (file["type"].as_str(), &file["id"], &file["name"], &file["mimeType"]);
        let (Some(kind @ ("image" | "file")), Some(name), Some(mime)) = (kind, name.as_str(), mime.as_str()) else {
            return Err("This message has an attachment that cannot be restored.".into());
        };
        let resource = json!({"_tag": "attachment", "attachmentId": id, "fileName": name, "mimeType": mime});
        let failed = || format!("Could not restore attachment: {name}");
        let signed = client.request("assets.createUrl", json!({"resource": resource})).await.map_err(|_| failed())?;
        let url = format!("{http_base}{}", signed["relativeUrl"].as_str().ok_or_else(failed)?);
        let bytes = crate::runtime()
            .spawn(async move {
                let response = reqwest::Client::new().get(url).timeout(ATTACHMENT_TIMEOUT).send().await?;
                response.error_for_status()?.bytes().await
            })
            .await;
        let Ok(Ok(bytes)) = bytes else { return Err(failed()) };
        restored.push(Attachment::restored(kind, name.to_owned(), mime.to_owned(), bytes.to_vec()));
    }
    Ok(restored)
}

/// A 24px `icon-xs` button: `ghost` (foreground) or `ghost-muted`.
fn icon_button(
    id: SharedString,
    icon: Icon,
    label: &'static str,
    muted: bool,
    t: &Tokens,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    div()
        .id(ElementId::Name(id))
        .size(px(24.))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .rounded(px(8.))
        .cursor_pointer()
        .text_color(if muted { t.muted_foreground } else { t.foreground })
        .hover(|style| style.bg(t.accent).text_color(t.foreground))
        .child(icon.size(px(12.)))
        .tooltip(move |window, cx| Tooltip::new(label).build(window, cx))
}

/// Copies `text`; its icon turns into a primary check for a second.
pub(super) fn copy_button(
    this: &WeakEntity<Workspace>,
    id: SharedString,
    text: SharedString,
    label: &'static str,
    icon: IconName,
    t: &Tokens,
    cx: &App,
) -> impl IntoElement {
    let copied = this.upgrade().is_some_and(|w| w.read(cx).copy_feedback.id.as_ref() == Some(&id));
    let glyph = if copied { Icon::new(IconName::Check).text_color(t.primary) } else { Icon::new(icon) };
    let this = this.clone();
    icon_button(id.clone(), glyph, if copied { "Copied!" } else { label }, true, t).on_click(move |_, _, cx| {
        cx.write_to_clipboard(ClipboardItem::new_string(text.to_string()));
        let _ = this.update(cx, |workspace, cx| workspace.copied(id.clone(), COPIED_FOR, cx));
    })
}

/// A code block's copy button: "Copy code", then a check and "Copied".
pub(super) fn code_copy_button(
    this: &WeakEntity<Workspace>,
    id: SharedString,
    code: SharedString,
    t: &Tokens,
    cx: &App,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    let copied = this.upgrade().is_some_and(|w| w.read(cx).copy_feedback.id.as_ref() == Some(&id));
    let glyph = Icon::new(if copied { IconName::Check } else { IconName::Copy });
    let this = this.clone();
    icon_button("copy".into(), glyph, if copied { "Copied" } else { "Copy code" }, true, t)
        .text_color(gpui_kit::Hsla { a: t.foreground.a * 0.72, ..t.foreground })
        .on_click(move |_, _, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(code.to_string()));
            let _ = this.update(cx, |workspace, cx| workspace.copied(id.clone(), CODE_COPIED_FOR, cx));
        })
}

/// `TimelineRowTimestamp` text: "3:04 PM", with the full date as its tooltip.
pub(super) fn timestamp(
    this: &WeakEntity<Workspace>,
    id: SharedString,
    iso: &str,
    t: &Tokens,
    cx: &App,
) -> Option<impl IntoElement> {
    let format = this.upgrade().map(|w| w.read(cx).client_settings.values.get("timestampFormat").clone());
    let hour12 = crate::timestamp::hour12(format.as_ref().and_then(Value::as_str).unwrap_or_default());
    let (text, tooltip) = crate::timestamp::now_local(iso, hour12)?;
    let tooltip: SharedString = tooltip.into();
    Some(
        div()
            .id(ElementId::Name(id))
            .text_xs()
            .text_color(t.muted_foreground)
            .child(text)
            .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx)),
    )
}

/// The row under a user message: time, then edit, copy and prompt link.
pub(super) fn user_actions(
    this: &WeakEntity<Workspace>,
    id: &SharedString,
    text: &SharedString,
    created_at: &str,
    revert: Option<&Checkpoint>,
    t: &Tokens,
    cx: &App,
) -> gpui_kit::Div {
    let (can_rewind, working) = this.upgrade().map_or((false, false), |workspace| {
        let workspace = workspace.read(cx);
        let working = workspace.thread.as_ref().is_some_and(|thread| {
            workspace.rewinding.contains(&thread.id) || thread.state.projection.as_ref().is_some_and(|p| p.is_running())
        });
        (workspace.supports_rollback(), working)
    });
    let edit = revert.filter(|_| can_rewind).map(|checkpoint| {
        let (this, id, checkpoint) = (this.clone(), id.clone(), checkpoint.clone());
        icon_button(format!("edit:{id}").into(), Icon::new(IconName::Pencil), "Edit from here", false, t)
            .when(working, |button| button.opacity(0.5).cursor_default())
            .on_click(move |_, window, cx| {
                if !working {
                    confirm_rewind(this.clone(), id.clone(), checkpoint.clone(), window, cx);
                }
            })
    });
    let link = prompt_link(text).map(|link| {
        copy_button(this, format!("link:{id}").into(), link.into(), "Copy prompt link", IconName::Link, t, cx)
    });
    gpui_kit::component::h_flex()
        .gap(px(8.))
        .children(timestamp(this, format!("time:{id}").into(), created_at, t, cx))
        .child(
            gpui_kit::component::h_flex()
                .gap(px(2.))
                .children(edit)
                .child(copy_button(
                    this,
                    format!("copy:{id}").into(),
                    text.clone(),
                    "Copy message",
                    IconName::Copy,
                    t,
                    cx,
                ))
                .children(link),
        )
}

/// `AssistantMessageMeta`: status, copy, fork, then the time.
pub(super) fn assistant_meta(
    this: &WeakEntity<Workspace>,
    id: &SharedString,
    text: &SharedString,
    streaming: bool,
    updated_at: &str,
    status: &str,
    fork: Option<&(SharedString, SharedString)>,
    t: &Tokens,
    cx: &App,
) -> gpui_kit::Div {
    let pill = (status != "completed").then(|| {
        div()
            .px(px(6.))
            .py(px(2.))
            .rounded_full()
            .border_1()
            .border_color(gpui_kit::Hsla { a: t.border.a * 0.7, ..t.border })
            .font_family(t.fonts.mono)
            .text_size(px(10.))
            .text_color(t.muted_foreground)
            .child(status.to_owned())
    });
    let copy = (!streaming && !text.trim().is_empty())
        .then(|| copy_button(this, format!("copy:{id}").into(), text.clone(), "Copy message", IconName::Copy, t, cx));
    let fork = fork.map(|(thread, run)| {
        let (this, thread, run) = (this.clone(), thread.clone(), run.clone());
        icon_button(format!("fork:{id}").into(), Icon::new(IconName::GitFork), "Fork from this response", false, t)
            .on_click(move |_, window, cx| {
                let _ = this.update(cx, |workspace, cx| workspace.fork_from(thread.clone(), run.clone(), window, cx));
            })
    });
    gpui_kit::component::h_flex()
        .mt(px(6.))
        .gap(px(8.))
        .children(pill)
        .children(copy)
        .children(fork)
        .children((!streaming).then(|| timestamp(this, format!("time:{id}").into(), updated_at, t, cx)).flatten())
}

/// The "Edit from here?" confirmation (`ChatOverlayHosts`).
fn confirm_rewind(
    this: WeakEntity<Workspace>,
    message_id: SharedString,
    checkpoint: Checkpoint,
    window: &mut Window,
    cx: &mut App,
) {
    window.open_dialog(cx, move |dialog, _, _| {
        let action = |id: &'static str, label: &'static str, restore_files: bool| {
            let (this, message_id, checkpoint) = (this.clone(), message_id.clone(), checkpoint.clone());
            Button::new(id).label(label).on_click(move |_, window, cx| {
                window.close_dialog(cx);
                let _ = this.update(cx, |workspace, cx| {
                    workspace.rewind(message_id.clone(), checkpoint.clone(), restore_files, window, cx)
                });
            })
        };
        dialog
            .entrance(DialogEntrance::None)
            .title("Edit from here?")
            .child(v_flex().child(
                "Rewind chat to before this message. Your prompt and attachments return to the composer. \
                 \u{201c}Revert files too\u{201d} also undoes your own edits.",
            ))
            .footer(
                DialogFooter::new()
                    .child(DialogClose::new().child(Button::new("rewind-cancel").label("Cancel").outline()))
                    .child(action("rewind-files", "Revert files too", true).danger().outline())
                    .child(action("rewind-keep", "Revert and keep changes", false).primary()),
            )
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rewind_finishes_when_its_witness_run_rolls_back_or_its_failure_lands() {
        let thread = |runs: Value, failure: Value| {
            let projection = json!({"thread": {"id": "t", "title": "t", "rollbackFailure": failure}, "runs": runs});
            let mut state = ThreadState::default();
            state.apply(&json!({"kind": "snapshot", "snapshotSequence": 1, "projection": projection})).unwrap();
            state.projection.unwrap()
        };
        let runs = json!([
            {"id": "r1", "status": "completed", "ordinal": 1},
            {"id": "r2", "status": "completed", "ordinal": 2},
            {"id": "r3", "status": "running", "ordinal": 3},
        ]);
        let runs: Vec<Run> = serde_json::from_value(runs).unwrap();
        assert_eq!(rewind_witness(&runs, 1).as_deref(), Some("r2"), "the latest settled run after the target");
        assert_eq!(rewind_witness(&runs, 2), None);

        let pending = thread(json!([{"id": "r2", "status": "completed", "ordinal": 2}]), Value::Null);
        assert_eq!(rollback_outcome(&pending, "c1", "r2"), None, "accepting the command is not the rollback");
        let done = thread(json!([{"id": "r2", "status": "rolled_back", "ordinal": 2}]), Value::Null);
        assert_eq!(rollback_outcome(&done, "c1", "r2"), Some(Ok(())));
        let failed = thread(json!([]), json!({"requestId": "c1", "message": "Provider refused"}));
        assert_eq!(rollback_outcome(&failed, "c1", "r2"), Some(Err("Provider refused".into())));
        assert_eq!(rollback_outcome(&failed, "older", "r2"), None, "another rewind's failure");
    }

    #[test]
    fn prompt_links_encode_like_url_search_params() {
        assert_eq!(prompt_link("  fix the bug  ").unwrap(), "https://app.t3.codes/new?prompt=fix+the+bug");
        assert_eq!(prompt_link("a&b=c/é").unwrap(), "https://app.t3.codes/new?prompt=a%26b%3Dc%2F%C3%A9");
        assert_eq!(prompt_link("   "), None);
        assert_eq!(prompt_link(&"x".repeat(8001)), None);
    }
}
