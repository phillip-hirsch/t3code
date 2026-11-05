//! Subagents seen from their parent and from themselves: approvals a child thread waits
//! on, answered from the parent's composer (`ChildThreadApprovals.tsx`), and the bar that
//! stands in for the composer on a provider's own subagent (`ProviderSubagentBar.tsx`).

use std::collections::HashSet;
use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, Context, ElementId, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Task, div, prelude::FluentBuilder as _, px, svg,
};
use serde_json::{Value, json};

use super::{Target, Workspace, new_id};
use crate::model::{RuntimeRequest, ThreadProjection, TurnItem};
use crate::model_label;
use crate::theme::Tokens;
use crate::thread_now;

/// The default options when an approval item lists none (`DEFAULT_APPROVAL_OPTIONS`).
const DEFAULT_OPTIONS: [(&str, &str); 4] = [
    ("cancel", "Cancel"),
    ("decline", "Decline"),
    ("acceptForSession", "Always allow this session"),
    ("accept", "Approve"),
];

#[derive(Default)]
pub(super) struct SubagentUi {
    approvals: Vec<ChildApproval>,
    /// Children whose projection is being read, by fetch key.
    fetching: HashSet<String>,
    responding: Option<String>,
    _fetches: Vec<Task<()>>,
    _status_tick: Option<Task<()>>,
}

struct ChildApproval {
    thread_id: String,
    title: SharedString,
    /// The child's `updatedAt` the approvals were read at; a newer one reads them again.
    key: String,
    requests: Vec<(RuntimeRequest, Option<TurnItem>)>,
    error: Option<SharedString>,
}

impl Workspace {
    /// Reads the pending approvals of each child that asks for one; the shell only says that it does.
    pub(super) fn sync_child_approvals(&mut self, cx: &mut Context<Self>) {
        let parent = match &self.target {
            Some(Target::Thread(id)) => id.clone(),
            _ => {
                self.subagents.approvals.clear();
                return;
            }
        };
        let children: Vec<(String, String, String)> = crate::inbox::child_thread_inputs(&self.shell.threads, &parent)
            .into_iter()
            .filter(|child| thread_now::has_pending_approval(child))
            .map(|child| (child.id.clone(), child.title.clone(), format!("{}:{}", child.id, child.updated_at)))
            .collect();
        let before = self.subagents.approvals.len();
        self.subagents.approvals.retain(|entry| children.iter().any(|(id, ..)| *id == entry.thread_id));
        let mut changed = before != self.subagents.approvals.len();
        for (thread_id, title, key) in children {
            match self.subagents.approvals.iter_mut().find(|e| e.thread_id == thread_id) {
                Some(entry) if entry.key == key => {
                    if entry.title != title.as_str() {
                        entry.title = title.into();
                        changed = true;
                    }
                    continue;
                }
                _ => {}
            }
            if !self.subagents.fetching.insert(key.clone()) {
                continue;
            }
            let Some(client) = self.client() else { return };
            let input = json!({"threadId": thread_id});
            self.subagents._fetches.push(cx.spawn(async move |this, cx| {
                let result = client.request("orchestration.getThreadProjection", input).await;
                let _ = this.update(cx, |this, cx| {
                    this.subagents.fetching.remove(&key);
                    let Ok(projection) = result
                        .map_err(|e| e.to_string())
                        .and_then(|value| serde_json::from_value::<ThreadProjection>(value).map_err(|e| e.to_string()))
                    else {
                        return;
                    };
                    let requests: Vec<(RuntimeRequest, Option<TurnItem>)> = projection
                        .pending_approvals()
                        .into_iter()
                        .map(|(request, item)| (request.clone(), item.cloned()))
                        .collect();
                    let entry = ChildApproval { thread_id, title: title.into(), key, requests, error: None };
                    match this.subagents.approvals.iter_mut().find(|e| e.thread_id == entry.thread_id) {
                        Some(existing) => *existing = entry,
                        None => this.subagents.approvals.push(entry),
                    }
                    this.subagents._fetches.retain(|task| !task.is_ready());
                    cx.notify();
                });
            }));
        }
        if changed {
            cx.notify();
        }
    }

    fn respond_to_child(&mut self, thread_id: String, request_id: String, decision: String, cx: &mut Context<Self>) {
        let Some(client) = self.client() else { return };
        if self.subagents.responding.is_some() {
            return;
        }
        self.subagents.responding = Some(request_id.clone());
        if let Some(entry) = self.subagents.approvals.iter_mut().find(|e| e.thread_id == thread_id) {
            entry.error = None;
        }
        cx.notify();
        let command = json!({"type": "runtime-request.respond", "commandId": new_id(), "threadId": thread_id,
            "requestId": request_id, "decision": decision});
        cx.spawn(async move |this, cx| {
            let result = client.request("orchestration.dispatchCommand", command).await;
            let _ = this.update(cx, |this, cx| {
                this.subagents.responding = None;
                if let Some(entry) = this.subagents.approvals.iter_mut().find(|e| e.thread_id == thread_id) {
                    match result {
                        Err(error) => entry.error = Some(error.to_string().into()),
                        Ok(_) => entry.requests.retain(|(request, _)| request.id != request_id),
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// `ChildThreadApprovals`: one card per approval a child thread waits on.
    pub(super) fn render_child_approvals(&self, t: &Tokens, cx: &mut Context<Self>) -> Option<AnyElement> {
        let cards: Vec<AnyElement> = self
            .subagents
            .approvals
            .iter()
            .flat_map(|entry| entry.requests.iter().map(move |(request, item)| (entry, request, item)))
            .map(|(entry, request, item)| self.child_approval_card(entry, request, item.as_ref(), t, cx))
            .collect();
        if cards.is_empty() {
            return None;
        }
        Some(
            v_flex()
                .id("child-approvals")
                .mb(px(8.))
                .max_h(px(320.))
                .overflow_y_scroll()
                .gap(px(8.))
                .children(cards)
                .into_any_element(),
        )
    }

    fn child_approval_card(
        &self,
        entry: &ChildApproval,
        request: &RuntimeRequest,
        item: Option<&TurnItem>,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let text = |key: &str| item.and_then(|i| i.str(key)).filter(|v| !v.is_empty()).map(str::to_owned);
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
            .unwrap_or_else(|| DEFAULT_OPTIONS.map(|(d, l)| (d.to_owned(), l.to_owned())).to_vec());
        let live = request.is_live();
        let enabled = live && self.subagents.responding.is_none();
        let mut actions = h_flex().flex_wrap().items_center().gap(px(8.)).text_xs();
        for (decision, label) in options.iter().filter(|(d, _)| d == "decline" || d == "accept") {
            let (thread_id, request_id, decision) = (entry.thread_id.clone(), request.id.clone(), decision.clone());
            let tone = if decision == "accept" { t.foreground } else { t.muted_foreground };
            actions = actions.child(
                div()
                    .id(ElementId::Name(format!("child-approval-{}-{decision}", request.id).into()))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(tone)
                    .child(label.clone())
                    .when(!enabled, |el| el.opacity(0.64))
                    .when(enabled, |el| {
                        el.cursor_pointer().hover(|s| s.underline().text_color(t.foreground)).on_click(cx.listener(
                            move |this, _, _, cx| {
                                this.respond_to_child(thread_id.clone(), request_id.clone(), decision.clone(), cx)
                            },
                        ))
                    }),
            );
        }
        let more: Vec<(String, String)> =
            options.iter().filter(|(d, _)| d != "decline" && d != "accept").cloned().collect();
        if !more.is_empty() {
            let view = cx.entity();
            let (thread_id, request_id) = (entry.thread_id.clone(), request.id.clone());
            actions = actions.child(
                Button::new(ElementId::Name(format!("child-approval-more-{}", request.id).into()))
                    .ghost()
                    .xsmall()
                    .label("More")
                    .h(px(16.))
                    .px(px(0.))
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(t.muted_foreground)
                    .disabled(!enabled)
                    .dropdown_menu(move |mut menu, window, _| {
                        for (decision, label) in &more {
                            let (thread_id, request_id, decision) =
                                (thread_id.clone(), request_id.clone(), decision.clone());
                            menu = menu.item(PopupMenuItem::new(label.clone()).on_click(window.listener_for(
                                &view,
                                move |this, _, _, cx| {
                                    this.respond_to_child(thread_id.clone(), request_id.clone(), decision.clone(), cx)
                                },
                            )));
                        }
                        menu
                    }),
            );
        }
        let note = |text: String| div().mt(px(8.)).text_color(t.muted_foreground).child(text);
        let full_access = text("fullAccessOverride").map(|kind| {
            if kind == "ask_rule" {
                "Full access skips approvals, but an ask rule in the agent's own settings matches this action, so it still asks. Remove or narrow that rule to stop these prompts."
            } else {
                "Full access skips approvals, but the agent still stops for a few safety checks of its own that no setting can skip, such as a delete whose path it can't verify. T3 Code can't turn these off, so check the request and approve it if it's what you expect."
            }
            .to_owned()
        });
        v_flex()
            .p(px(12.))
            .rounded(px(8.))
            .border_1()
            .border_color(t.border)
            .bg(t.background)
            .text_xs()
            .child(
                div().mb(px(8.)).font_weight(FontWeight::MEDIUM).child(format!("Approval needed from {}", entry.title)),
            )
            .child(
                div()
                    .id(ElementId::Name(format!("child-approval-detail-{}", request.id).into()))
                    .max_h(px(160.))
                    .overflow_y_scroll()
                    .font_family(t.fonts.mono)
                    .child(text("prompt").unwrap_or_else(|| request.kind.clone())),
            )
            .children(text("appName").map(note))
            .when(!live, |el| {
                el.child(
                    div()
                        .mt(px(8.))
                        .text_color(t.warning)
                        .child("Provider process is gone. Interrupt or restart the child thread to respond."),
                )
            })
            .children(text("reason").map(note))
            .children(full_access.map(note))
            .child(div().mt(px(8.)).child(actions))
            .children(entry.error.clone().map(|error| div().mt(px(8.)).text_color(t.error).child(error)))
            .into_any_element()
    }

    /// `isProviderNativeSubagentThread` for the open thread: the composer gives way to the bar.
    pub(super) fn open_provider_subagent(&self) -> Option<&ThreadProjection> {
        let projection = self.open_projection()?;
        let shell = self.shell.threads.iter().find(|t| t.id == projection.thread.id)?;
        (shell.lineage.relationship_to_parent.as_deref() == Some("subagent")
            && shell.creation_source.as_deref() == Some("provider"))
        .then_some(projection)
    }

    /// Repaints once a second while the subagent works, for its elapsed time.
    pub(super) fn sync_subagent_tick(&mut self, cx: &mut Context<Self>) {
        let live = self
            .open_provider_subagent()
            .and_then(provider_subagent_status)
            .is_some_and(|(status, ..)| matches!(status.as_str(), "pending" | "running" | "waiting"));
        if !live {
            self.subagents._status_tick = None;
        } else if self.subagents._status_tick.is_none() {
            self.subagents._status_tick = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(Duration::from_secs(1)).await;
                    if this.update(cx, |_, cx| cx.notify()).is_err() {
                        break;
                    }
                }
            }));
        }
    }

    /// `ProviderSubagentBar`: the model it runs on, its status, "Runs on its own", Open parent.
    pub(super) fn render_provider_subagent_bar(&self, t: &Tokens, cx: &mut Context<Self>) -> Option<AnyElement> {
        let projection = self.open_provider_subagent()?;
        let selection = projection.thread.model_selection.as_ref();
        let provider = selection.and_then(|s| self.server_info.provider(&s.instance_id));
        let model = selection.and_then(|s| self.server_info.model(s));
        let driver = provider.map(|p| p.driver.as_str()).unwrap_or_default();
        let name = match (selection, model) {
            (_, Some(model)) => model_label::trigger_model_name(model, driver),
            (Some(selection), None) => selection.model.clone(),
            (None, None) => String::new(),
        };
        let descriptors =
            model.and_then(|m| m.capabilities.as_ref()).map(|c| c.option_descriptors.as_slice()).unwrap_or(&[]);
        let effort = selection.and_then(|s| model_label::effort_label(descriptors, s));
        let status = format_status(provider_subagent_status(projection), super::sidebar::now_ms());
        let icon = provider.and_then(|p| crate::assets::provider_icon(&p.driver));
        let parent = self
            .shell
            .threads
            .iter()
            .find(|t| t.id == projection.thread.id)
            .and_then(|t| t.lineage.parent_thread_id.clone())
            .filter(|parent| self.shell.threads.iter().any(|t| &t.id == parent));
        Some(
            h_flex()
                .min_h(px(48.))
                .gap(px(12.))
                .py(px(8.))
                .pl(px(20.))
                .pr(px(8.))
                .rounded(px(16.))
                .text_sm()
                .child(
                    h_flex()
                        .flex_1()
                        .min_w_0()
                        .gap(px(12.))
                        .child(
                            h_flex()
                                .min_w_0()
                                .gap(px(8.))
                                .children(icon.map(|(path, tint)| {
                                    svg().path(path).size(px(16.)).flex_none().text_color(tint.unwrap_or(t.foreground))
                                }))
                                .child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(t.foreground)
                                        .child(name),
                                )
                                .children(effort.map(|e| div().flex_none().text_color(t.muted_foreground).child(e))),
                        )
                        .child(div().min_w_0().truncate().text_color(t.muted_foreground).child(status)),
                )
                .child(div().flex_none().text_color(t.muted_foreground).child("Runs on its own"))
                .children(parent.map(|parent| {
                    Button::new("open-parent")
                        .ghost()
                        .small()
                        .icon(Icon::new(IconName::ArrowUpLeft).size(px(16.)))
                        .label("Open parent")
                        .on_click(
                            cx.listener(move |this, _, window, cx| this.select_thread(parent.clone(), window, cx)),
                        )
                }))
                .into_any_element(),
        )
    }
}

/// `deriveProviderSubagentStatus`: the last runless root turn's status and times.
fn provider_subagent_status(projection: &ThreadProjection) -> Option<(String, Option<String>, Option<String>)> {
    let node = projection.nodes.iter().rev().find(|n| n.kind == "root_turn" && n.run_id.is_none())?;
    Some((node.status.clone(), node.started_at.clone(), node.completed_at.clone()))
}

/// `formatProviderSubagentStatus`: "Working 12s", "Completed in 3m 2s", or the plain label.
fn format_status(status: Option<(String, Option<String>, Option<String>)>, now_ms: i64) -> String {
    let Some((status, started, completed)) = status else { return "Starting".into() };
    let label = match status.as_str() {
        "idle" => "Idle",
        "pending" | "running" => "Working",
        "waiting" => "Waiting",
        "completed" => "Completed",
        "interrupted" => "Interrupted",
        "failed" => "Failed",
        _ => "Cancelled",
    };
    let live = matches!(status.as_str(), "pending" | "running" | "waiting");
    if !live && status != "completed" {
        return label.into();
    }
    let parse = |at: Option<&str>| at.and_then(crate::work_log::parse_iso_ms);
    let (Some(start), Some(end)) =
        (parse(started.as_deref()), if live { Some(now_ms) } else { parse(completed.as_deref()) })
    else {
        return label.into();
    };
    let elapsed = crate::work_log::format_duration(((end - start) / 1000 * 1000).max(1000));
    if live { format!("{label} {elapsed}") } else { format!("{label} in {elapsed}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(status: &str, completed: Option<&str>) -> Option<(String, Option<String>, Option<String>)> {
        Some((status.into(), Some("2026-01-01T00:00:00Z".into()), completed.map(Into::into)))
    }

    #[test]
    fn subagent_status_reads_like_the_web() {
        let start = crate::work_log::parse_iso_ms("2026-01-01T00:00:00Z").unwrap();
        assert_eq!(format_status(None, 0), "Starting");
        assert_eq!(format_status(status("running", None), start + 12_400), "Working 12s");
        assert_eq!(format_status(status("completed", Some("2026-01-01T00:03:02Z")), 0), "Completed in 3m 2s");
        assert_eq!(format_status(status("failed", None), 0), "Failed");
    }
}
