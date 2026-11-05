//! A workflow coordinator's thread opens with its script, shown as the workflow's plan
//! (`WorkflowOpeningBlock.tsx`); its members live on the parent thread's subagent row.

use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, Context, FontWeight, IntoElement, ParentElement as _, SharedString, Styled as _, Task, WeakEntity,
    div, prelude::FluentBuilder as _, px,
};
use serde_json::{Value, json};

use super::Workspace;
use crate::theme::Tokens;
use crate::timeline::{self, Row, SubagentCard};
use crate::workflow::{self, Member, WorkflowRow};

/// The parent's subagent rows are read again at most this often while the thread updates.
const PARENT_REFRESH: Duration = Duration::from_secs(5);

#[derive(Default)]
pub(super) struct WorkflowUi {
    parent: Option<(String, Instant)>,
    subagents: Option<Rc<Vec<Value>>>,
    _fetch: Option<Task<()>>,
}

fn iso(ms: Option<i64>) -> Option<SharedString> {
    jiff::Timestamp::from_millisecond(ms?).ok().map(|at| at.to_string().into())
}

fn card(member: &Member, driver: &str) -> Rc<SubagentCard> {
    let (status_label, dot) = timeline::subagent_status(&member.status);
    Rc::new(SubagentCard {
        id: member.id.clone().into(),
        title: member.title.clone().into(),
        status_label,
        dot,
        detail: member.detail.clone().map(Into::into),
        failed: member.status == "failed",
        status_beside_detail: false,
        live: matches!(member.status.as_str(), "pending" | "running" | "waiting"),
        started_at: iso(member.started_ms),
        completed_at: iso(member.completed_ms),
        child_thread_id: member.child_thread_id.clone().map(Into::into),
        driver: driver.to_owned().into(),
        status: member.status.clone().into(),
        run_id: None,
        provider_turn_id: None,
    })
}

impl Workspace {
    /// Swaps a provider-native workflow thread's first message for its opening block.
    pub(super) fn apply_workflow_opening(&mut self, rows: &mut [Row], cx: &mut Context<Self>) {
        let Some(projection) = self.open_projection() else { return };
        let thread_id = projection.thread.id.clone();
        let Some(shell) = self.shell.threads.iter().find(|t| t.id == thread_id) else { return };
        let native = shell.lineage.relationship_to_parent.as_deref() == Some("subagent")
            && shell.creation_source.as_deref() == Some("provider");
        let Some(parent) = shell.lineage.parent_thread_id.clone().filter(|_| native) else { return };
        let Some(index) = rows.iter().position(|row| matches!(row, Row::User { .. })) else { return };
        let Row::User { id, text, .. } = &rows[index] else { return };
        let mark = projection
            .turn_items
            .iter()
            .find(|item| item.id == id.as_ref())
            .and_then(|item| item.raw.get("workflowScript").and_then(Value::as_bool));
        if !workflow::is_workflow_script(text, mark) {
            return;
        }
        let (id, text) = (id.to_string(), text.to_string());
        self.load_workflow_parent(parent, cx);
        let opening = workflow::opening(&thread_id, &id, &text, self.workflow.subagents.as_deref().map(Vec::as_slice));
        let driver = opening.driver.clone().unwrap_or_default();
        let phases = opening
            .phases
            .iter()
            .map(|p| (p.title.clone(), p.settled, p.members.iter().map(|m| card(m, &driver)).collect()))
            .collect();
        let unphased = opening.unphased.iter().map(|m| card(m, &driver)).collect();
        let key = timeline::entry_key(&format!("workflow-script:{id}"));
        rows[index] = Row::Workflow {
            row: Rc::new(WorkflowRow {
                headline: opening.headline(),
                script_markdown: workflow::script_markdown(&opening.script).into(),
                foldable: super::chat::should_collapse(&opening.script),
                opening,
                phases,
                unphased,
            }),
            expanded: self.expanded.contains(&key),
        };
    }

    /// Reads the parent's subagent rows, again after [`PARENT_REFRESH`] while the thread updates.
    fn load_workflow_parent(&mut self, parent: String, cx: &mut Context<Self>) {
        let fresh = self
            .workflow
            .parent
            .as_ref()
            .is_some_and(|(loaded, at)| *loaded == parent && at.elapsed() < PARENT_REFRESH);
        let Some(client) = self.client().filter(|_| !fresh) else { return };
        if self.workflow.parent.as_ref().is_some_and(|(loaded, _)| *loaded != parent) {
            self.workflow.subagents = None;
        }
        self.workflow.parent = Some((parent.clone(), Instant::now()));
        self.workflow._fetch = Some(cx.spawn(async move |this, cx| {
            let Ok(value) = client.request("orchestration.getThreadProjection", json!({"threadId": parent})).await
            else {
                return;
            };
            let subagents = value["subagents"].as_array().cloned().unwrap_or_default();
            let _ = this.update(cx, |this, cx| {
                this.workflow.subagents = Some(Rc::new(subagents));
                this.refresh_rows(cx);
                cx.notify();
            });
        }));
    }
}

pub(super) fn workflow_row(
    row: &Rc<WorkflowRow>,
    expanded: bool,
    this: &WeakEntity<Workspace>,
    lane_width: f32,
    t: &Tokens,
    cx: &mut App,
) -> AnyElement {
    let opening = &row.opening;
    let (title, description, agents) = row.headline.clone();
    let muted = t.muted_foreground;
    let header = h_flex()
        .min_w_0()
        .items_start()
        .gap(px(8.))
        .child(div().mt(px(2.)).child(Icon::new(IconName::Workflow).size(px(14.)).text_color(muted)))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap(px(2.))
                .child(
                    div()
                        .text_size(px(10.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(muted)
                        .child("Workflow started"),
                )
                .child(div().truncate().text_sm().font_weight(FontWeight::MEDIUM).text_color(t.foreground).child(title))
                .children(description.map(|d| div().text_xs().text_color(muted).child(d))),
        )
        .children(agents.map(|a| div().flex_none().text_size(px(10.)).text_color(muted).child(a)));
    let phases = row.phases.iter().map(|(title, settled, cards)| {
        v_flex()
            .child(
                h_flex()
                    .gap(px(8.))
                    .px(px(8.))
                    .pb(px(2.))
                    .text_size(px(10.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(muted)
                    .child(div().min_w_0().truncate().child(title.clone()))
                    .child(
                        div().flex_none().font_weight(FontWeight::NORMAL).child(format!("{settled}/{}", cards.len())),
                    ),
            )
            .children(cards.iter().map(|card| super::chat::subagent_card(card, this, t)))
    });
    let key = timeline::entry_key(&format!("workflow-script:{}", opening.message_id));
    let markdown_key: SharedString = format!("workflow-script:{}", opening.message_id).into();
    let script = super::chat::markdown(this, markdown_key, &row.script_markdown, false, t, cx);
    let foldable = row.foldable;
    let folded = foldable && !expanded;
    let toggle_this = this.clone();
    v_flex()
        .w_full()
        .min_w_0()
        .gap(px(12.))
        .px(px(4.))
        .py(px(2.))
        .child(header)
        .children(phases)
        .when(!row.unphased.is_empty(), |el| {
            el.child(v_flex().children(row.unphased.iter().map(|card| super::chat::subagent_card(card, this, t))))
        })
        .child(
            v_flex()
                .child(
                    div().w(px(lane_width - 8.)).when(folded, |el| el.max_h(px(176.)).overflow_hidden()).child(script),
                )
                .when(foldable, |el| {
                    el.child(
                        div().ml(px(-4.)).child(
                            Button::new("workflow-script-toggle")
                                .ghost()
                                .xsmall()
                                .text_color(muted)
                                .label(if expanded { "Show less" } else { "Show full script" })
                                .on_click(move |_, _, cx| {
                                    let _ = toggle_this.update(cx, |w, cx| w.toggle_expanded(key.clone(), cx));
                                }),
                        ),
                    )
                }),
        )
        .into_any_element()
}
