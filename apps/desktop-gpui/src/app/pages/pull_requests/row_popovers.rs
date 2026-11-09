//! A row's checks and stack popovers (`PullRequestChecksPopover`'s `LazyChecksBody`,
//! `PullRequestStackPopover`): each reads what it lists only when it opens, so the list asks
//! nothing per row.

use std::rc::Rc;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::{Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, ElementId, Entity, FontWeight, Hsla, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Task, div,
    prelude::FluentBuilder as _, px,
};

use super::{Workspace, reference_of, state_glyph};
use crate::pull_request_detail::{Check, Detail, Reference, Stack};
use crate::pull_requests::{self as logic, Entry, StackMembership};
use crate::theme::{self, Tokens};

/// What an open popover read, for the row `key`.
pub(super) struct RowPopover {
    key: String,
    read: Read,
    /// The checks' "Show all".
    show_all: bool,
    _task: Option<Task<()>>,
}

enum Read {
    Loading,
    Checks(Rc<[Check]>),
    /// `None`: the pull request is no longer in a stack.
    Stack(Option<Rc<Stack>>),
    Failed(SharedString),
}

/// Which popover a row has open; only one opens at a time.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Checks,
    Stack,
}

impl Workspace {
    fn row_popover(&self, key: &str, kind: Kind) -> Option<&RowPopover> {
        let (open, popover) = self.pull_requests.row_popover.as_ref()?;
        (*open == kind && popover.key == key).then_some(popover)
    }

    fn close_row_popover(&mut self, key: &str, kind: Kind, cx: &mut gpui_kit::Context<Self>) {
        if self.row_popover(key, kind).is_some() {
            self.pull_requests.row_popover = None;
            cx.notify();
        }
    }

    /// Opens `entry`'s popover and reads what it lists; open checks reuse a fresh prefetch.
    fn open_row_popover(&mut self, entry: &Entry, kind: Kind, cx: &mut gpui_kit::Context<Self>) {
        let key = entry.key();
        let prefetched = self
            .pull_requests
            .prefetched
            .as_ref()
            .filter(|held| kind == Kind::Checks && held.0 == reference_of(entry).key())
            .filter(|held| held.2.elapsed() < super::PREFETCH_FRESH)
            .map(|held| held.1.clone());
        let mut popover = RowPopover { key: key.clone(), read: Read::Loading, show_all: false, _task: None };
        if let Some(detail) = prefetched {
            popover.read = Read::Checks(detail.checks.clone().into());
        } else if let Some(client) = self.env_client(&entry.environment_id) {
            let input = entry.reference();
            let method = match kind {
                Kind::Checks => "pullRequests.detail",
                Kind::Stack => "pullRequests.stack",
            };
            popover._task = Some(cx.spawn(async move |this, cx| {
                let result = client.request(method, input).await;
                let read = match (kind, result) {
                    (_, Err(error)) => Read::Failed(logic::error_message(&error).into()),
                    (Kind::Checks, Ok(value)) => match serde_json::from_value::<Detail>(value) {
                        Ok(detail) => Read::Checks(detail.checks.into()),
                        Err(error) => Read::Failed(format!("Unreadable pull request: {error}").into()),
                    },
                    (Kind::Stack, Ok(value)) => match serde_json::from_value::<Option<Stack>>(value) {
                        Ok(stack) => Read::Stack(stack.map(Rc::new)),
                        Err(error) => Read::Failed(format!("Unreadable stack: {error}").into()),
                    },
                };
                let _ = this.update(cx, |this, cx| {
                    if let Some((open, popover)) = this.pull_requests.row_popover.as_mut()
                        && *open == kind
                        && popover.key == key
                    {
                        popover.read = read;
                        cx.notify();
                    }
                });
            }));
        } else {
            popover.read = Read::Failed("The environment request failed.".into());
        }
        self.pull_requests.row_popover = Some((kind, popover));
        cx.notify();
    }
}

/// A popover in a row: its trigger keeps the press from opening the row.
fn row_popover(
    entry: &Rc<Entry>,
    kind: Kind,
    trigger: Button,
    open: bool,
    view: &Entity<Workspace>,
    content: impl Fn(&Workspace, &Tokens, &Entity<Workspace>) -> AnyElement + 'static,
) -> AnyElement {
    let key = entry.key();
    let (opened, toggle) = (entry.clone(), view.clone());
    let content_view = view.clone();
    let id = match kind {
        Kind::Checks => "checks-popover",
        Kind::Stack => "stack-popover",
    };
    div()
        .flex_none()
        .flex()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(
            Popover::new(ElementId::Name(format!("pr-{key}-{id}").into()))
                .anchor(gpui_kit::Anchor::TopLeft)
                .trigger(trigger)
                .open(open)
                .on_open_change(move |open, _, cx| {
                    toggle.update(cx, |this, cx| {
                        if *open {
                            this.open_row_popover(&opened, kind, cx);
                        } else {
                            this.close_row_popover(&opened.key(), kind, cx);
                        }
                    });
                })
                .when(kind == Kind::Checks, |p| p.w(px(360.)).p(px(12.)))
                .when(kind == Kind::Stack, |p| p.w(px(288.)).p(px(4.)))
                .content(move |_, _, cx| {
                    let t = *theme::tokens(cx);
                    content(content_view.read(cx), &t, &content_view)
                }),
        )
        .into_any_element()
}

/// The checks glyph, opening the row's checks.
pub(super) fn checks_popover(
    entry: &Rc<Entry>,
    icon: Lucide,
    color: Hsla,
    label: &'static str,
    workspace: &Workspace,
    view: &Entity<Workspace>,
) -> AnyElement {
    let key = entry.key();
    let open = workspace.row_popover(&key, Kind::Checks).is_some();
    let trigger = Button::new(ElementId::Name(format!("pr-{key}-checks").into()))
        .ghost()
        .xsmall()
        .size(px(14.))
        .p_0()
        .child(Icon::new(icon).with_size(px(14.)).text_color(color));
    row_popover(entry, Kind::Checks, trigger, open, view, move |this, t, view| {
        let muted = |text: SharedString| div().text_xs().text_color(t.muted_foreground).child(text).into_any_element();
        let body = match this.row_popover(&key, Kind::Checks).map(|p| (&p.read, p.show_all)) {
            Some((Read::Checks(checks), show_all)) => {
                let (view, key) = (view.clone(), key.clone());
                super::super::pull_request_detail::checks_body(
                    checks,
                    show_all,
                    move |cx| {
                        view.update(cx, |this, cx| {
                            if let Some((Kind::Checks, popover)) = this.pull_requests.row_popover.as_mut()
                                && popover.key == key
                            {
                                popover.show_all = !popover.show_all;
                                cx.notify();
                            }
                        });
                    },
                    t,
                )
            }
            Some((Read::Failed(error), _)) => muted(error.clone()),
            _ => muted("Loading checks…".into()),
        };
        v_flex()
            .child(div().mb(px(8.)).text_sm().font_weight(FontWeight::MEDIUM).child(label))
            .child(body)
            .into_any_element()
    })
}

/// `PullRequestStackPopover`: the row's place in its stack, listing the layers to open.
pub(super) fn stack_popover(
    entry: &Rc<Entry>,
    membership: &StackMembership,
    workspace: &Workspace,
    view: &Entity<Workspace>,
    t: &Tokens,
) -> AnyElement {
    let key = entry.key();
    let open = workspace.row_popover(&key, Kind::Stack).is_some();
    let (number, position, size) = (membership.number, membership.position, membership.size);
    let trigger = Button::new(ElementId::Name(format!("pr-{key}-stack").into()))
        .ghost()
        .xsmall()
        .h(px(16.))
        .px(px(2.))
        .gap(px(4.))
        .text_xs()
        .font_weight(FontWeight::NORMAL)
        .text_color(t.muted_foreground)
        .tooltip(format!("View stack #{number}, layer {position} of {size}"))
        .child(Icon::new(Lucide::Layers).with_size(px(12.)))
        .child(format!("{position}/{size}"));
    let entry_for_layers = entry.clone();
    row_popover(entry, Kind::Stack, trigger, open, view, move |this, t, view| {
        let group_label = |text: SharedString| {
            div()
                .px(px(8.))
                .py(px(6.))
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(t.muted_foreground)
                .child(text)
                .into_any_element()
        };
        let read = this.row_popover(&key, Kind::Stack).map(|p| &p.read);
        let Some(Read::Stack(Some(stack))) = read else {
            let message = match read {
                Some(Read::Failed(error)) => error.clone(),
                Some(Read::Stack(None)) => "This pull request is no longer in a stack.".into(),
                _ => "Loading stack…".into(),
            };
            return v_flex()
                .child(group_label(format!("Stack #{number}").into()))
                .child(group_label(message))
                .into_any_element();
        };
        let current = entry_for_layers.number;
        let layers = stack.layers.iter().rev().map(|layer| {
            let reference = Reference { number: layer.number, ..reference_of(&entry_for_layers) };
            let (icon, color, state) = state_glyph(layer.state, layer.is_draft, t);
            let (view, row_key) = (view.clone(), key.clone());
            let branch = layer.head_branch.clone();
            h_flex()
                .id(ElementId::Name(format!("pr-stack-layer-{}", layer.number).into()))
                .min_h(px(32.))
                .px(px(8.))
                .py(px(4.))
                .gap(px(8.))
                .rounded(px(4.))
                .cursor_pointer()
                .text_sm()
                .hover(|s| s.bg(t.accent))
                .child(Icon::new(icon).with_size(px(16.)).text_color(color))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(div().truncate().child(layer.title.clone().unwrap_or_else(|| branch.clone())))
                        .child(
                            div()
                                .truncate()
                                .text_xs()
                                .text_color(t.muted_foreground)
                                .child(format!("#{} · {branch} · {state}", layer.number)),
                        ),
                )
                .when(layer.number == current, |el| el.child(Icon::new(Lucide::Check).with_size(px(14.))))
                .on_click(move |_, _, cx| {
                    view.update(cx, |this, cx| {
                        this.close_row_popover(&row_key, Kind::Stack, cx);
                        let seed = this.pull_requests.entry_for(&reference);
                        this.open_pull_request_panel(reference.clone(), seed, cx);
                    });
                })
        });
        v_flex()
            .child(group_label(format!("Stack #{}", stack.number).into()))
            .child(v_flex().id("pr-stack-layers").max_h(px(320.)).overflow_y_scroll().children(layers))
            .child(group_label(format!("↳ {}", stack.base).into()))
            .into_any_element()
    })
}
