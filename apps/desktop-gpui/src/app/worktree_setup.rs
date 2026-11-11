//! The worktree setup card in the timeline (`WorktreeSetupCard.tsx`, `useWorktreeSetup.ts`).

use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, Context, Hsla, IntoElement, ParentElement as _, SharedString, Styled as _, Task, WeakEntity, div,
    px,
};
use serde_json::json;

use super::{Target, Workspace};
use crate::rpc::StreamEvent;
use crate::theme::Tokens;
use crate::worktree_setup::{self, Snapshot, Stage};

/// A thread on its environment: same-id threads on two environments have their own setups.
type Owner = (SharedString, String);

#[derive(Default)]
pub(super) struct SetupWatch {
    /// The thread the stream watches.
    watching: Option<Owner>,
    /// The card, with the environment that streamed it. Shared with the timeline row, so a
    /// refresh never copies the stage logs.
    snapshot: Option<(SharedString, Rc<Snapshot>)>,
    details_open: bool,
    _task: Option<Task<()>>,
}

impl SetupWatch {
    /// Drops the stream with its socket, keeping the card, so the next sync resubscribes.
    pub(super) fn disconnect(&mut self) {
        self.watching = None;
        self._task = None;
    }

    /// Watches `wanted` from scratch; a card it holds for that same thread stays up until the
    /// stream resends it, as after a reconnect.
    fn restart(&mut self, wanted: Option<Owner>) {
        let same = |(env, snapshot): &(SharedString, Rc<Snapshot>)| {
            wanted.as_ref().is_some_and(|(e, thread_id)| e == env && *thread_id == snapshot.thread_id)
        };
        let snapshot = self.snapshot.take().filter(same);
        *self = Self { watching: wanted, snapshot, ..Self::default() };
    }

    /// Takes `owner`'s streamed snapshot unless the watch moved on or it is older than the card;
    /// whether the card changed.
    fn apply(&mut self, owner: &Owner, next: Option<Rc<Snapshot>>) -> bool {
        let Some(next) = next.filter(|_| self.watching.as_ref() == Some(owner)) else { return false };
        // Keep the newer of the two: a late event never rolls the card back.
        let newer = self
            .snapshot
            .as_ref()
            .is_none_or(|(_, held)| next.thread_id != held.thread_id || next.sequence >= held.sequence);
        if newer {
            self.snapshot = Some((owner.0.clone(), next));
        }
        newer
    }

    /// The card of `thread_id` on `env`.
    fn card(&self, env: &str, thread_id: &str) -> Option<&Rc<Snapshot>> {
        let (_, snapshot) = self.snapshot.as_ref().filter(|(e, s)| e == env && s.thread_id == thread_id)?;
        Some(snapshot)
    }
}

impl Workspace {
    /// Keeps one `subscribeWorktreeSetup` stream on an open thread that has or is preparing a worktree.
    pub(super) fn sync_worktree_setup(&mut self, cx: &mut Context<Self>) {
        let wanted = match (&self.target, self.open_projection()) {
            (Some(Target::Thread { env, id }), Some(projection)) => {
                let preparing = projection.runs.last().is_some_and(|run| run.status == "preparing");
                (preparing || projection.thread.worktree_path.is_some()).then(|| (env.clone(), id.clone()))
            }
            _ => None,
        };
        if wanted == self.setup.watching {
            return;
        }
        // Without its environment's socket the watch waits, so it subscribes once one connects.
        let client = match &wanted {
            Some((env, _)) => match self.env_client(env) {
                Some(client) => Some(client),
                None => return,
            },
            None => None,
        };
        self.setup.restart(wanted.clone());
        let (Some(owner), Some(client)) = (wanted, client) else { return };
        let stream = client.stream("subscribeWorktreeSetup", json!({"threadId": owner.1}));
        self.setup._task = Some(cx.spawn(async move |this, cx| {
            while let Some(event) = stream.next().await {
                let ended = this.update(cx, |this, cx| match event {
                    StreamEvent::Item(value) => {
                        let next = serde_json::from_value::<Option<Rc<Snapshot>>>(value).ok().flatten();
                        if this.setup.apply(&owner, next) {
                            this.refresh_rows(cx);
                            cx.notify();
                        }
                        false
                    }
                    StreamEvent::End(_) => true,
                });
                if ended.unwrap_or(true) {
                    break;
                }
            }
        }));
    }

    /// The card's row, when it shows.
    pub(super) fn insert_worktree_setup(&self, rows: &mut Vec<crate::timeline::Row>) {
        let (Some(Target::Thread { env, .. }), Some(projection)) = (&self.target, self.open_projection()) else {
            return;
        };
        let run_started = projection.runs.last().is_some_and(|run| run.started_at.is_some());
        let snapshot = self.setup.card(env, &projection.thread.id);
        if let Some(snapshot) = worktree_setup::visible(snapshot, run_started) {
            worktree_setup::insert_row(rows, snapshot, run_started);
        }
    }

    fn cancel_worktree_setup(&mut self, cx: &mut Context<Self>) {
        let Some((env, snapshot)) = &self.setup.snapshot else { return };
        let Some(client) = self.env_client(env).filter(|_| snapshot.phase == "running") else { return };
        let input = json!({"threadId": snapshot.thread_id});
        cx.spawn(async move |this, cx| {
            if let Err(error) = client.request("worktreeSetup.cancel", input).await {
                let _ = this.update(cx, |this, cx| {
                    this.error = Some(error.to_string().into());
                    cx.notify();
                });
            }
        })
        .detach();
    }
}

fn elapsed(start: Option<&str>, end: Option<&str>, live: bool) -> Option<String> {
    let start = crate::timestamp::parse_ms(start?)?;
    let end = match end {
        Some(end) => crate::timestamp::parse_ms(end)?,
        None if live => super::sidebar::now_ms(),
        None => return None,
    };
    Some(crate::work_log::format_duration((end - start).max(0)))
}

fn stage_icon(status: &str) -> IconName {
    match status {
        "done" => IconName::Check,
        // A still spinner: a looping one would repaint the timeline every frame.
        "running" => IconName::LoaderCircle,
        "failed" => IconName::X,
        "warning" => IconName::CircleAlert,
        "skipped" => IconName::Minus,
        _ => IconName::Circle,
    }
}

fn stage_color(status: &str, t: &Tokens) -> Hsla {
    match status {
        "failed" => t.error_foreground,
        "warning" => t.warning_foreground,
        "pending" => t.muted_foreground.opacity(0.4),
        _ => t.muted_foreground,
    }
}

/// `WorkLogRow`: icon, label, then the trailing detail and time.
fn log_row(
    icon: IconName,
    color: Hsla,
    label_color: Hsla,
    label: SharedString,
    trailing: Vec<String>,
    t: &Tokens,
) -> AnyElement {
    h_flex()
        .min_h(px(24.))
        .w_full()
        .min_w_0()
        .gap(px(6.))
        .px(px(2.))
        .text_sm()
        .child(
            div()
                .size(px(24.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .child(Icon::new(icon).size(px(16.)).text_color(color)),
        )
        .child(div().flex_1().min_w_0().truncate().text_color(label_color).child(label))
        .children(trailing.into_iter().map(|text| {
            div().flex_none().max_w(px(240.)).truncate().text_xs().text_color(t.muted_foreground).child(text)
        }))
        .into_any_element()
}

fn stage_row(stage: &Stage, snapshot: &Snapshot, t: &Tokens) -> AnyElement {
    let running = snapshot.phase == "running";
    let label = worktree_setup::stage_label(stage, snapshot.setup_script.as_ref()).into_owned();
    let mut trailing: Vec<String> = worktree_setup::stage_trailing(stage).into_iter().collect();
    if !matches!(stage.status.as_str(), "skipped" | "pending") {
        // A stage interrupted by the setup ending stops with it.
        let end = stage.ended_at.as_deref().or(snapshot.ended_at.as_deref());
        trailing.extend(elapsed(stage.started_at.as_deref(), end, running));
    }
    let icon_color = if stage.status == "pending" { t.muted_foreground.opacity(0.4) } else { t.muted_foreground };
    let row = log_row(stage_icon(&stage.status), icon_color, stage_color(&stage.status, t), label.into(), trailing, t);
    let tail = stage.id == "setup-script"
        && (stage.status == "failed" || (stage.status == "running" && !stage.tail.is_empty()));
    if !tail {
        return row;
    }
    let failed = stage.status == "failed";
    let lines: Vec<String> = (0..4)
        .map(|slot| stage.tail.len().checked_sub(4 - slot).and_then(|i| stage.tail.get(i)).cloned().unwrap_or_default())
        .collect();
    v_flex()
        .child(row)
        .child(
            v_flex()
                .mb(px(4.))
                .ml(px(32.))
                .px(px(10.))
                .py(px(6.))
                .rounded(px(6.))
                .border_1()
                .border_color(if failed { t.error.opacity(0.2) } else { t.border })
                .bg(if failed { t.error_surface } else { t.code_background })
                .font_family(t.fonts.mono)
                .text_size(px(11.))
                .text_color(if failed { t.error_foreground } else { t.muted_foreground })
                .children(
                    lines
                        .into_iter()
                        .map(|line| div().truncate().child(if line.is_empty() { "\u{a0}".to_owned() } else { line })),
                ),
        )
        .into_any_element()
}

pub(super) fn setup_row(
    snapshot: &Rc<Snapshot>,
    embedded: bool,
    this: &WeakEntity<Workspace>,
    t: &Tokens,
    cx: &App,
) -> AnyElement {
    let running = snapshot.phase == "running";
    let details_open = this.upgrade().is_some_and(|w| w.read(cx).setup.details_open);
    let collapsed = embedded && !running;
    let header = !embedded && !running && snapshot.phase != "done";
    let script_stage = snapshot.stages.iter().find(|s| s.id == "setup-script");
    let terminal = snapshot
        .setup_script
        .as_ref()
        .filter(|_| script_stage.is_some_and(|s| s.status != "pending"))
        .map(|script| script.terminal_id.clone());
    let tone = if snapshot.phase == "failed" {
        t.error_foreground
    } else if snapshot.phase == "done" && snapshot.summary_failed() {
        t.warning_foreground
    } else {
        t.muted_foreground
    };
    let mut body = v_flex().w_full().min_w_0();
    if header {
        body = body.child(
            h_flex()
                .h(px(24.))
                .mb(px(6.))
                .px(px(4.))
                .pb(px(8.))
                .pt(px(4.))
                .gap(px(8.))
                .border_b_1()
                .border_color(t.border.opacity(0.6))
                .text_sm()
                .text_color(tone)
                .child(div().min_w_0().truncate().child(snapshot.header_label()))
                .children(
                    elapsed(Some(&snapshot.started_at), snapshot.ended_at.as_deref(), running)
                        .map(|e| div().ml_auto().text_xs().text_color(t.muted_foreground).child(e)),
                ),
        );
    }
    body = if collapsed {
        let failed = snapshot.summary_failed();
        let status = if failed { "failed" } else { "done" };
        body.child(log_row(
            stage_icon(status),
            t.muted_foreground,
            stage_color(status, t),
            snapshot.header_label().into(),
            elapsed(Some(&snapshot.started_at), snapshot.ended_at.as_deref(), false).into_iter().collect(),
            t,
        ))
    } else {
        body.children(snapshot.stages.iter().map(|stage| stage_row(stage, snapshot, t)))
    };
    if let (Some(error), "failed") = (&snapshot.error, snapshot.phase.as_str()) {
        body = body.child(div().mt(px(4.)).ml(px(32.)).text_xs().text_color(t.muted_foreground).child(error.clone()));
    }
    let button = |id: &'static str, icon: IconName, label: &'static str| {
        Button::new(id).ghost().xsmall().text_color(t.muted_foreground).icon(Icon::new(icon).size(px(14.))).label(label)
    };
    let toggle = this.clone();
    let mut actions = h_flex().mt(px(2.)).ml(px(19.)).flex_wrap().gap(px(2.)).child(
        button("setup-details", if details_open { IconName::ChevronDown } else { IconName::ChevronRight }, "Details")
            .on_click(move |_, _, cx| {
                let _ = toggle.update(cx, |w, cx| {
                    w.setup.details_open = !w.setup.details_open;
                    w.refresh_rows(cx);
                    cx.notify();
                });
            }),
    );
    if let Some(terminal_id) = terminal {
        let (this, thread_id) = (this.clone(), snapshot.thread_id.clone());
        actions =
            actions.child(button("setup-terminal", IconName::Terminal, "Open terminal").on_click(move |_, _, cx| {
                let (thread_id, terminal_id) = (thread_id.clone(), terminal_id.clone());
                let _ = this.update(cx, |w, cx| {
                    // The card shows the open thread's setup; its terminal runs on that thread's environment.
                    let Some(Target::Thread { env, id }) = w.target.clone() else { return };
                    if id == thread_id && w.setup.card(&env, &id).is_some() {
                        w.open_worktree_setup_terminal(&env, &id, &terminal_id, cx);
                    }
                });
            }));
    }
    if running {
        let this = this.clone();
        actions = actions.child(button("setup-cancel", IconName::X, "Cancel").on_click(move |_, _, cx| {
            let _ = this.update(cx, super::Workspace::cancel_worktree_setup);
        }));
    }
    let details = details_open.then(|| {
        let field = |label: &'static str, value: &Option<String>| {
            value.as_ref().map(|value| {
                h_flex()
                    .min_w_0()
                    .gap(px(12.))
                    .child(div().w(px(48.)).flex_none().text_color(t.foreground.opacity(0.8)).child(label))
                    .child(div().min_w_0().truncate().font_family(t.fonts.mono).child(value.clone()))
            })
        };
        v_flex()
            .mt(px(4.))
            .mb(px(6.))
            .ml(px(32.))
            .gap(px(2.))
            .text_xs()
            .text_color(t.muted_foreground)
            .children(field("Branch", &snapshot.branch))
            .children(field("Base", &snapshot.base_ref))
            .children(field("Path", &snapshot.worktree_path))
            .children(field("Setup", &snapshot.setup_script.as_ref().map(|s| s.command.clone())))
    });
    super::chat::lane(8., body.child(actions).children(details)).into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(thread_id: &str, sequence: u64) -> Option<Rc<Snapshot>> {
        serde_json::from_value(json!({"threadId": thread_id, "phase": "running", "startedAt": "2026-01-01T00:00:00Z",
            "sequence": sequence}))
        .ok()
    }

    fn owner(env: &str, thread_id: &str) -> Owner {
        (env.into(), thread_id.to_owned())
    }

    #[test]
    fn a_same_id_thread_on_another_environment_never_shows_the_setup_card() {
        let (a, b) = (owner("a", "t"), owner("b", "t"));
        let mut watch = SetupWatch::default();
        watch.restart(Some(a.clone()));
        assert!(watch.apply(&a, snapshot("t", 2)));
        assert!(watch.card("a", "t").is_some());
        assert!(watch.card("b", "t").is_none(), "B's thread has no setup of its own");

        // A reconnect resubscribes the same thread and keeps its card until the stream resends it.
        watch.disconnect();
        watch.restart(Some(a.clone()));
        assert!(watch.card("a", "t").is_some());
        assert!(!watch.apply(&a, snapshot("t", 1)), "a late event never rolls the card back");

        watch.restart(Some(b.clone()));
        assert!(watch.card("a", "t").is_none() && watch.card("b", "t").is_none(), "A's card is dropped");
        assert!(!watch.apply(&a, snapshot("t", 3)), "A's stream lands after the switch");
        assert!(watch.card("b", "t").is_none());
        assert!(watch.apply(&b, snapshot("t", 1)));
        assert_eq!(watch.card("b", "t").map(|s| s.sequence), Some(1));
    }
}
