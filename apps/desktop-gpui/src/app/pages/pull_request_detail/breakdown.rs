//! `PullRequestChangeBreakdownStat`: a diff stat whose hover splits its lines into code, tests,
//! mocks, generated files and docs, read from `pullRequests.changeBreakdown` on first hover (or
//! at once where a view shows one pull request).

use std::rc::Rc;

use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::{
    AnyElement, Context, ElementId, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, div, prelude::FluentBuilder as _, px, rgb,
};
use serde_json::Value;

use super::super::super::Workspace;
use crate::app::git::group_thousands;
use crate::pull_request_detail::ChangeBreakdown;
use crate::theme::{self, Tokens};

/// One pull request's split, as read so far.
pub(in crate::app) enum Breakdown {
    Loading,
    /// `None`: the host can't split this change.
    Ready(Option<Rc<ChangeBreakdown>>),
    Failed,
}

/// The pull request and totals a split belongs to: new totals read it again.
pub(in crate::app) struct Subject {
    pub(in crate::app) environment_id: String,
    pub(in crate::app) reference: Value,
    pub(in crate::app) additions: u64,
    pub(in crate::app) deletions: u64,
}

impl Subject {
    pub(in crate::app) fn key(&self) -> String {
        format!("{}:{}:{}:{}", self.environment_id, self.reference, self.additions, self.deletions)
    }
}

impl Workspace {
    /// Reads a split once; the answer stays while the totals do.
    pub(in crate::app) fn read_pr_breakdown(&mut self, subject: &Subject, cx: &mut Context<Self>) {
        let key = subject.key();
        if self.pull_requests.breakdowns.contains_key(&key) {
            return;
        }
        let Some(client) = self.env_client(&subject.environment_id) else { return };
        self.pull_requests.breakdowns.insert(key.clone(), Breakdown::Loading);
        let reference = subject.reference.clone();
        cx.spawn(async move |this, cx| {
            let answer = client.request("pullRequests.changeBreakdown", reference).await;
            let _ = this.update(cx, |this, cx| {
                let state = match answer.map(serde_json::from_value::<Option<ChangeBreakdown>>) {
                    Ok(Ok(split)) => Breakdown::Ready(split.map(Rc::new)),
                    _ => Breakdown::Failed,
                };
                this.pull_requests.breakdowns.insert(key, state);
                cx.notify();
            });
        })
        .detach();
    }
}

/// `stat` with the split on hover. A view of one pull request reads it first (`eager`) with
/// [`Workspace::read_pr_breakdown`].
pub(in crate::app) fn breakdown_stat(
    id: impl Into<ElementId>,
    stat: impl IntoElement,
    subject: Subject,
    view: &Entity<Workspace>,
) -> AnyElement {
    let totals = (subject.additions, subject.deletions);
    let key = subject.key();
    let subject = Rc::new(subject);
    let (hover_view, tip_view) = (view.clone(), view.clone());
    div()
        .id(id.into())
        .flex_none()
        .on_hover(move |hovered, _, cx| {
            if *hovered {
                hover_view.update(cx, |this, cx| this.read_pr_breakdown(&subject, cx));
            }
        })
        .tooltip(move |window, cx| {
            let (view, key) = (tip_view.clone(), key.clone());
            Tooltip::element(move |_, cx| {
                let t = *theme::tokens(cx);
                match view.read(cx).pull_requests.breakdowns.get(&key) {
                    None | Some(Breakdown::Loading) => div().child("Loading breakdown").into_any_element(),
                    Some(Breakdown::Ready(Some(split))) => grid(split, Some(totals), &t),
                    Some(Breakdown::Ready(None) | Breakdown::Failed) => div().into_any_element(),
                }
            })
            .build(window, cx)
        })
        .child(stat)
        .into_any_element()
}

/// `ChangeBreakdownGrid`: each kind with lines, then any total under a rule.
pub(in crate::app) fn grid(split: &ChangeBreakdown, total: Option<(u64, u64)>, t: &Tokens) -> AnyElement {
    let addition: gpui_kit::Hsla = if t.dark { rgb(0x00d492).into() } else { rgb(0x007a55).into() };
    let row = |label: &str, additions: u64, deletions: u64| {
        [
            div().font_family(t.fonts.sans).child(label.to_owned()),
            div().text_right().text_color(addition).child(format!("+{}", group_thousands(additions))),
            div().text_right().text_color(t.error).child(format!("-{}", group_thousands(deletions))),
        ]
    };
    div()
        .grid()
        .grid_cols_max_content(3)
        .justify_start()
        .gap_x(px(16.))
        .gap_y(px(4.))
        .py(px(2.))
        .font_family(t.fonts.mono)
        .children(split.rows().into_iter().flat_map(|(label, count)| row(label, count.additions, count.deletions)))
        .when_some(total, |el, (additions, deletions)| {
            el.child(div().col_span(3).border_t_1().border_color(t.border)).children(row("Total", additions, deletions))
        })
        .into_any_element()
}
