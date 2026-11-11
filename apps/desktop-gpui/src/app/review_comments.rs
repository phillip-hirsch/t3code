//! Review comments on diff and file lines (`DiffCommentAnnotation`): the draft form a gutter
//! pick opens and a comment the open draft holds. Both the Diff surface and the file editor
//! show them; a comment is the `review-comment` record behind a composer chip.

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, ElementId, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, Styled as _, Subscription, Window, div, px, rems,
};

use super::Workspace;
use crate::theme::Tokens;

/// What a draft's buttons and shortcuts do, run on the workspace.
pub(super) type CommentAction = fn(&mut Workspace, &mut Window, &mut Context<Workspace>);

/// A comment being written: its input, and the subscription that sends it on Mod+Enter.
pub(super) struct CommentDraft {
    input: Entity<TextareaState>,
    _subscription: Subscription,
}

impl CommentDraft {
    /// A focused, empty draft; Mod+Enter runs `submit` once it holds text.
    pub(super) fn new(submit: CommentAction, window: &mut Window, cx: &mut Context<Workspace>) -> Self {
        let input = cx.new(|cx| TextareaState::new(window, cx).rows(3).placeholder("Add a comment…"));
        input.update(cx, |input, cx| input.focus(window, cx));
        let empty = Rc::new(Cell::new(true));
        let subscription =
            cx.subscribe_in(&input, window, move |this, input, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    let now = input.read(cx).value().trim().is_empty();
                    if empty.replace(now) != now {
                        cx.notify();
                    }
                }
                InputEvent::PressEnter { secondary: true, .. } if !empty.get() => submit(this, window, cx),
                _ => {}
            });
        Self { input, _subscription: subscription }
    }

    pub(super) fn text(&self, cx: &App) -> String {
        self.input.read(cx).value().trim().to_owned()
    }
}

/// `DiffCommentAnnotation kind="draft"`: the input, the shortcut hint, Cancel and Comment.
/// Escape cancels.
pub(super) fn render_draft(
    draft: &CommentDraft,
    id: &str,
    cancel: CommentAction,
    submit: CommentAction,
    t: &Tokens,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let empty = draft.text(cx).is_empty();
    v_flex()
        .id(ElementId::Name(format!("{id}-draft").into()))
        .px(px(12.))
        .py(px(8.))
        .text_color(t.foreground)
        .capture_key_down(cx.listener(move |this, event: &gpui_kit::KeyDownEvent, window, cx| {
            if event.keystroke.key == "escape" {
                cx.stop_propagation();
                cancel(this, window, cx);
            }
        }))
        .child(Textarea::new(&draft.input).small())
        .child(
            h_flex()
                .mt(px(6.))
                .flex_wrap()
                .items_center()
                .gap(px(4.))
                .child(
                    div()
                        .mr_auto()
                        .text_size(rems(0.625))
                        .text_color(t.muted_foreground.opacity(0.7))
                        .child("⌘/Ctrl Enter to send"),
                )
                .child(
                    Button::new(ElementId::Name(format!("{id}-cancel").into()))
                        .ghost()
                        .xsmall()
                        .text_color(t.muted_foreground)
                        .child("Cancel")
                        .on_click(cx.listener(move |this, _, window, cx| cancel(this, window, cx))),
                )
                .child(
                    Button::new(ElementId::Name(format!("{id}-submit").into()))
                        .primary()
                        .xsmall()
                        .disabled(empty)
                        .child("Comment")
                        .on_click(cx.listener(move |this, _, window, cx| submit(this, window, cx))),
                ),
        )
        .into_any_element()
}

/// `DiffCommentAnnotation kind="comment"`: the comment, an optional label for where it sits,
/// and Delete.
pub(super) fn render_comment(
    id: &str,
    text: &str,
    place: Option<AnyElement>,
    delete: impl Fn(&mut Workspace, &mut Window, &mut Context<Workspace>) + 'static,
    t: &Tokens,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let text: SharedString = text.to_owned().into();
    h_flex()
        .min_w_0()
        .items_start()
        .gap(px(10.))
        .border_l_2()
        .border_color(t.primary.opacity(0.55))
        .bg(t.primary.opacity(0.045))
        .px(px(12.))
        .py(px(10.))
        .text_color(t.foreground)
        .child(
            Icon::new(Lucide::MessageCircle)
                .with_size(px(14.))
                .h(px(20.))
                .flex_none()
                .text_color(t.primary.opacity(0.7)),
        )
        .child(v_flex().min_w_0().flex_1().children(place).child(div().text_sm().line_height(px(20.)).child(text)))
        .child(
            Button::new(ElementId::Name(format!("{id}-delete").into()))
                .ghost()
                .xsmall()
                .flex_none()
                .my(px(-4.))
                .mr(px(-4.))
                .text_color(t.muted_foreground)
                .tooltip("Delete comment")
                .child(Icon::new(Lucide::Trash).with_size(px(12.)))
                .on_click(cx.listener(move |this, _, window, cx| delete(this, window, cx))),
        )
        .into_any_element()
}
