//! The hover actions on timeline messages (`TimelineUserMessageRow.tsx`,
//! `TimelineAssistantRows.tsx`, `useTimelineRewind.ts`): copy with a moment of
//! "Copied", copy prompt link, edit from here, and fork from a response.

use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::{DialogClose, DialogFooter};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, WindowExt as _, v_flex};
use gpui_kit::{
    App, ClipboardItem, Context, ElementId, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Task, WeakEntity, Window, div, prelude::FluentBuilder as _, px,
};
use serde_json::json;

use gpui_kit::assets::IconName;

use super::{Workspace, new_id};
use crate::theme::Tokens;
use crate::timeline::Checkpoint;

/// `ANCHORED_COPY_TOAST_TIMEOUT_MS`.
const COPIED_FOR: Duration = Duration::from_millis(1000);
/// `MarkdownCodeBlock`'s `useCopyToClipboard` timeout.
const CODE_COPIED_FOR: Duration = Duration::from_millis(1200);
/// `PROMPT_LINK_MAX_PROMPT_LENGTH`.
const PROMPT_LINK_MAX: usize = 8000;

/// The copy button that just copied, shown as a check until the timer clears it.
#[derive(Default)]
pub(super) struct CopyFeedback {
    id: Option<SharedString>,
    _reset: Option<Task<()>>,
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
    fn copied(&mut self, id: SharedString, duration: Duration, cx: &mut Context<Self>) {
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

    /// "Edit from here": rewinds the chat to before `text` and hands it back to the composer.
    fn rewind(
        &mut self,
        text: SharedString,
        checkpoint: Checkpoint,
        restore_files: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (Some(client), Some(thread)) = (self.client(), self.thread.as_ref()) else { return };
        let command = json!({
            "type": "checkpoint.rollback",
            "restoreFiles": restore_files,
            "commandId": new_id(),
            "threadId": thread.id,
            "scopeId": checkpoint.scope_id,
            "checkpointId": checkpoint.checkpoint_id,
        });
        self.error = None;
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("orchestration.dispatchCommand", command).await;
            let _ = this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(_) => {
                        let draft = this.composer.read(cx).value().trim_end().to_owned();
                        let text = if draft.is_empty() { text.to_string() } else { format!("{draft}\n\n{text}") };
                        this.set_composer_text(text, window, cx);
                        this.focus_composer(window, cx);
                    }
                    Err(error) => this.error = Some(error.to_string().into()),
                }
                cx.notify();
            });
        })
        .detach();
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
pub(super) fn timestamp(id: SharedString, iso: &str, t: &Tokens) -> Option<impl IntoElement> {
    let (text, tooltip) = crate::timestamp::now_local(iso)?;
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
        let working =
            workspace.thread.as_ref().and_then(|t| t.state.projection.as_ref()).is_some_and(|p| p.is_running());
        (workspace.supports_rollback(), working)
    });
    let edit = revert.filter(|_| can_rewind).map(|checkpoint| {
        let (this, text, checkpoint) = (this.clone(), text.clone(), checkpoint.clone());
        icon_button(format!("edit:{id}").into(), Icon::new(IconName::Pencil), "Edit from here", false, t)
            .when(working, |button| button.opacity(0.5).cursor_default())
            .on_click(move |_, window, cx| {
                if !working {
                    confirm_rewind(this.clone(), text.clone(), checkpoint.clone(), window, cx);
                }
            })
    });
    let link = prompt_link(text).map(|link| {
        copy_button(this, format!("link:{id}").into(), link.into(), "Copy prompt link", IconName::Link, t, cx)
    });
    gpui_kit::component::h_flex().gap(px(8.)).children(timestamp(format!("time:{id}").into(), created_at, t)).child(
        gpui_kit::component::h_flex()
            .gap(px(2.))
            .children(edit)
            .child(copy_button(this, format!("copy:{id}").into(), text.clone(), "Copy message", IconName::Copy, t, cx))
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
            .font_family(crate::theme::FONT_MONO)
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
        .children((!streaming).then(|| timestamp(format!("time:{id}").into(), updated_at, t)).flatten())
}

/// The "Edit from here?" confirmation (`ChatOverlayHosts`).
fn confirm_rewind(
    this: WeakEntity<Workspace>,
    text: SharedString,
    checkpoint: Checkpoint,
    window: &mut Window,
    cx: &mut App,
) {
    window.open_dialog(cx, move |dialog, _, _| {
        let action = |id: &'static str, label: &'static str, restore_files: bool| {
            let (this, text, checkpoint) = (this.clone(), text.clone(), checkpoint.clone());
            Button::new(id).label(label).on_click(move |_, window, cx| {
                window.close_dialog(cx);
                let _ = this.update(cx, |workspace, cx| {
                    workspace.rewind(text.clone(), checkpoint.clone(), restore_files, window, cx)
                });
            })
        };
        dialog
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
    fn prompt_links_encode_like_url_search_params() {
        assert_eq!(prompt_link("  fix the bug  ").unwrap(), "https://app.t3.codes/new?prompt=fix+the+bug");
        assert_eq!(prompt_link("a&b=c/é").unwrap(), "https://app.t3.codes/new?prompt=a%26b%3Dc%2F%C3%A9");
        assert_eq!(prompt_link("   "), None);
        assert_eq!(prompt_link(&"x".repeat(8001)), None);
    }
}
