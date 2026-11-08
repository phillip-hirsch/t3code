//! A workflow coordinator's thread opens with its script, shown as the workflow's plan
//! (`WorkflowOpeningBlock.tsx`); its members live on the parent thread's subagent row.

use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, Context, FontWeight, IntoElement, ParentElement as _, SharedString, Styled as _, Task, WeakEntity,
    div, prelude::FluentBuilder as _, px,
};
use serde_json::{Value, json};

use super::Workspace;
use crate::rpc::StreamEvent;
use crate::theme::Tokens;
use crate::timeline::{self, Row, SubagentCard};
use crate::workflow::{self, Member, WorkflowRow};

/// The open workflow thread's parent, whose subagent rows hold the workflow's members
/// (`useQuietThreadProjection` on the parent).
#[derive(Default)]
pub(super) struct WorkflowUi {
    parent: Option<String>,
    subagents: Option<Rc<[Value]>>,
    /// The parent's `orchestration.subscribeThread`; dropping it unsubscribes.
    _stream: Option<Task<()>>,
}

/// Applies one `orchestration.subscribeThread` item to the parent's subagents, as
/// `orchestrationV2Projection` does; returns whether they changed.
fn apply_parent_item(subagents: &mut Option<Vec<Value>>, item: &Value) -> bool {
    match item["kind"].as_str() {
        Some("snapshot") => {
            *subagents = Some(item["projection"]["subagents"].as_array().cloned().unwrap_or_default());
            true
        }
        Some("event") if item["event"]["type"] == "subagent.updated" => {
            let Some(list) = subagents.as_mut() else { return false };
            let mut next = item["event"]["payload"].clone();
            let Some(id) = next["id"].as_str().map(str::to_owned) else { return false };
            match list.iter().position(|subagent| subagent["id"].as_str() == Some(id.as_str())) {
                Some(index) => {
                    restore_subagent_bodies(&mut next, &list[index]);
                    list[index] = next;
                }
                None => list.push(next),
            }
            true
        }
        _ => false,
    }
}

/// `restoreSubagentBodies`: a body the stream left out keeps the previous record's.
fn restore_subagent_bodies(next: &mut Value, previous: &Value) {
    for (body, marker) in [("prompt", "promptOmitted"), ("result", "resultOmitted")] {
        if next[marker] == true && previous[marker] != true {
            next[body] = previous[body].clone();
            if let Some(record) = next.as_object_mut() {
                record.remove(marker);
            }
        }
    }
}

fn iso(ms: Option<i64>) -> Option<SharedString> {
    crate::timestamp::iso(ms?).map(Into::into)
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
        remote: None,
        driver: driver.to_owned().into(),
        status: member.status.clone().into(),
        run_id: None,
        provider_turn_id: None,
    })
}

impl Workspace {
    /// Swaps a provider-native workflow thread's first message for its opening block; any other
    /// thread drops the parent's subscription.
    pub(super) fn apply_workflow_opening(&mut self, rows: &mut [Row], cx: &mut Context<Self>) {
        if !self.try_workflow_opening(rows, cx) {
            self.workflow = WorkflowUi::default();
        }
    }

    fn try_workflow_opening(&mut self, rows: &mut [Row], cx: &mut Context<Self>) -> bool {
        let Some(projection) = self.open_projection() else { return false };
        let thread_id = projection.thread.id.clone();
        let Some(shell) = self.shell.threads.iter().find(|t| t.id == thread_id) else { return false };
        let native = shell.lineage.relationship_to_parent.as_deref() == Some("subagent")
            && shell.creation_source.as_deref() == Some("provider");
        let Some(parent) = shell.lineage.parent_thread_id.clone().filter(|_| native) else { return false };
        let Some(index) = rows.iter().position(|row| matches!(row, Row::User { .. })) else { return false };
        let Row::User { id, text, .. } = &rows[index] else { return false };
        let mark = projection
            .turn_items
            .iter()
            .find(|item| item.id == id.as_ref())
            .and_then(|item| item.raw.get("workflowScript").and_then(Value::as_bool));
        if !workflow::is_workflow_script(text, mark) {
            return false;
        }
        let (id, text) = (id.to_string(), text.to_string());
        self.load_workflow_parent(parent, cx);
        let opening = workflow::opening(&thread_id, &id, &text, self.workflow.subagents.as_deref());
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
        true
    }

    /// Subscribes to the parent on the thread's environment, so a member that changes only on
    /// the parent updates the opening block; a stream that ends is opened again on the next rebuild.
    fn load_workflow_parent(&mut self, parent: String, cx: &mut Context<Self>) {
        if self.workflow.parent.as_deref() == Some(parent.as_str()) {
            return;
        }
        let Some(client) = self.client() else { return };
        self.workflow = WorkflowUi { parent: Some(parent.clone()), subagents: None, _stream: None };
        let stream = client.stream("orchestration.subscribeThread", json!({"threadId": parent}));
        self.workflow._stream = Some(cx.spawn(async move |this, cx| {
            let mut subagents: Option<Vec<Value>> = None;
            while let Some(first) = stream.next().await {
                let mut changed = false;
                let mut ended = false;
                for event in std::iter::once(first).chain(std::iter::from_fn(|| stream.try_next())) {
                    match event {
                        StreamEvent::Item(item) => changed |= apply_parent_item(&mut subagents, &item),
                        StreamEvent::End(_) => ended = true,
                    }
                }
                let current = subagents.clone().filter(|_| changed);
                let alive = this.update(cx, |this, cx| {
                    // A late batch for a parent the view has moved past changes nothing.
                    if this.workflow.parent.as_deref() != Some(parent.as_str()) {
                        return false;
                    }
                    if let Some(current) = current {
                        this.workflow.subagents = Some(Rc::from(current));
                        this.refresh_rows(cx);
                        cx.notify();
                    }
                    if ended {
                        this.workflow.parent = None;
                    }
                    !ended
                });
                if !alive.unwrap_or(false) {
                    return;
                }
            }
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
    let phases: Vec<_> = row
        .phases
        .iter()
        .map(|(title, settled, cards)| {
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
                            div()
                                .flex_none()
                                .font_weight(FontWeight::NORMAL)
                                .child(format!("{settled}/{}", cards.len())),
                        ),
                )
                .children(cards.iter().map(|card| super::chat::subagent_card(card, this, t, cx)))
        })
        .collect();
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
            el.child(v_flex().children(row.unphased.iter().map(|card| super::chat::subagent_card(card, this, t, cx))))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_parent_stream_keeps_its_subagents_current() {
        let mut subagents = None;
        let event =
            |payload: Value| json!({"kind": "event", "event": {"type": "subagent.updated", "payload": payload}});
        assert!(!apply_parent_item(&mut subagents, &event(json!({"id": "a"}))), "nothing before the snapshot");
        let snapshot = json!({"kind": "snapshot", "projection": {"subagents": [
            {"id": "a", "status": "running", "prompt": "Review it", "result": null},
        ]}});
        assert!(apply_parent_item(&mut subagents, &snapshot));
        let update = json!({"id": "a", "status": "completed", "promptOmitted": true, "result": "Done"});
        assert!(apply_parent_item(&mut subagents, &event(update)));
        assert!(apply_parent_item(&mut subagents, &event(json!({"id": "b", "status": "pending"}))));
        assert!(!apply_parent_item(&mut subagents, &json!({"kind": "synchronized"})));
        assert_eq!(
            subagents,
            Some(vec![
                json!({"id": "a", "status": "completed", "prompt": "Review it", "result": "Done"}),
                json!({"id": "b", "status": "pending"}),
            ])
        );
    }
}
