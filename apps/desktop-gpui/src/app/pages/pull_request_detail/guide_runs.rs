//! Review guide runs, watched by environment and pull request whichever panel shows them, and
//! announced when they settle (`guideRunStore.ts`, `PullRequestGuideRunCoordinator.tsx`).

use super::*;
use crate::app::main_window::MainWindow;
use crate::app::right_panel::Surface;
use crate::notifications::{Kind, Mode};

/// `RUN_POLL_MS`.
const RUN_POLL: Duration = Duration::from_secs(5);
/// `UNSEEN_RUN_MS`: a run never seen running settles quietly after this long.
const UNSEEN_RUN_MS: i64 = 60_000;

/// "Open guide", shared by the toast and the OS notification.
type OpenGuide = Rc<dyn Fn(&mut Window, &mut App)>;

/// OS notifications of settled guide runs, one per run (`tag: guide:{runKey}`).
struct GuideSystemNotification;

/// What one `pullRequests.reviewGuide` answer means for a watched run (`onGuide`).
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Running,
    /// Not running and nothing new yet; the run may not have started.
    Waiting,
    /// Over with nothing to announce.
    Settled,
    Ready,
    Failed(String),
}

/// `GuideRunWatch` without its pull request.
#[derive(Clone, Debug)]
struct Watch {
    previous_guide_at: Option<String>,
    previous_failed_at: Option<String>,
    since: i64,
    saw_running: bool,
}

fn is_running(answer: &Value) -> bool {
    answer["run"]["_tag"] == "Running"
}

fn generated_at(answer: &Value) -> Option<String> {
    answer["guide"]["generatedAt"].as_str().map(str::to_owned)
}

fn failed_at(answer: &Value) -> Option<String> {
    (answer["run"]["_tag"] == "Failed").then(|| answer["run"]["failedAt"].as_str().map(str::to_owned)).flatten()
}

impl Watch {
    /// A run this client starts, against the answer on screen before it (`useReviewGuideWriter`).
    fn started(current: Option<&Value>, now: i64) -> Self {
        Self {
            previous_guide_at: current.and_then(generated_at),
            previous_failed_at: current.and_then(failed_at),
            since: now,
            saw_running: false,
        }
    }

    /// A run an answer reports running, started here or elsewhere (`answeredRunning`).
    fn answered_running(answer: &Value, now: i64) -> Self {
        Self { previous_guide_at: generated_at(answer), previous_failed_at: None, since: now, saw_running: true }
    }

    fn observe(&mut self, answer: &Value, now: i64) -> Outcome {
        if is_running(answer) {
            self.saw_running = true;
            return Outcome::Running;
        }
        if answer["run"]["_tag"] == "Failed" && failed_at(answer) != self.previous_failed_at {
            return Outcome::Failed(answer["run"]["detail"].as_str().unwrap_or_default().to_owned());
        }
        if !answer["guide"].is_null() && generated_at(answer) != self.previous_guide_at {
            return Outcome::Ready;
        }
        if self.saw_running || now - self.since > UNSEEN_RUN_MS { Outcome::Settled } else { Outcome::Waiting }
    }
}

/// One watched run; dropping it stops its poll.
pub(in crate::app) struct GuideRun {
    reference: Reference,
    title: Option<String>,
    watch: Watch,
    _poll: Task<()>,
}

/// A settled run to announce.
struct Announcement {
    reference: Reference,
    title: Option<String>,
    failure: Option<String>,
}

impl Workspace {
    /// `useGuideRunStore.watch`: a run already watched keeps its first watch.
    fn watch_pr_guide_run(
        &mut self,
        reference: &Reference,
        title: Option<String>,
        watch: Watch,
        cx: &mut Context<Self>,
    ) {
        let key = reference.key();
        if self.pull_requests.guide_runs.contains_key(&key) {
            return;
        }
        let poll = self.poll_pr_guide_run(key.clone(), cx);
        self.pull_requests.guide_runs.insert(key, GuideRun { reference: reference.clone(), title, watch, _poll: poll });
    }

    /// Reads the run's guide on its own environment while it is watched. The socket is looked up
    /// each time, so a dropped connection skips reads and a reconnected one resumes them.
    fn poll_pr_guide_run(&self, key: String, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(RUN_POLL).await;
                let next = this.update(cx, |this, _| {
                    let run = this.pull_requests.guide_runs.get(&key)?;
                    let environment = run.reference.environment_id.clone();
                    if this.session(&environment).is_none() {
                        this.pull_requests.guide_runs.remove(&key);
                        return None;
                    }
                    Some((this.env_client(&environment), run.reference.clone()))
                });
                let Ok(Some((client, reference))) = next else { return };
                let Some(client) = client else { continue };
                let Ok(answer) = client.request("pullRequests.reviewGuide", reference.to_json()).await else {
                    continue;
                };
                if this.update(cx, |this, cx| this.land_pr_guide(&reference, answer, cx)).is_err() {
                    return;
                }
            }
        })
    }

    /// `settle`: forgets a run without announcing it.
    fn settle_pr_guide_run(&mut self, key: &str) {
        self.pull_requests.guide_runs.remove(key);
    }

    /// A review guide answer, from any read: it lands in the pull request's panel, shown or
    /// parked, and moves its run along; one reporting a run nobody watches starts a watch.
    pub(super) fn land_pr_guide(&mut self, reference: &Reference, answer: Value, cx: &mut Context<Self>) {
        let key = reference.key();
        let now = now_ms();
        let outcome = match self.pull_requests.guide_runs.get_mut(&key) {
            Some(run) => Some(run.watch.observe(&answer, now)),
            None => {
                if is_running(&answer) {
                    self.watch_pr_guide_run(reference, None, Watch::answered_running(&answer, now), cx);
                }
                None
            }
        };
        if let Some(panel) = self.pr_panel(&key) {
            panel.guide_error = None;
            panel.set_guide(answer);
        }
        cx.notify();
        let failure = match outcome {
            None | Some(Outcome::Running | Outcome::Waiting) => return,
            Some(Outcome::Settled) => {
                self.settle_pr_guide_run(&key);
                return;
            }
            Some(Outcome::Ready) => None,
            Some(Outcome::Failed(detail)) => Some(detail),
        };
        let Some(run) = self.pull_requests.guide_runs.remove(&key) else { return };
        let announcement = Announcement { reference: run.reference, title: run.title, failure };
        // Announcing needs the window; the read that landed this may not hold it.
        let workspace = cx.weak_entity();
        cx.defer(move |cx| {
            let Some(window) = cx.try_global::<MainWindow>().map(|main| main.window) else { return };
            let _ = window.update(cx, |_, window, cx| {
                let _ = workspace.update(cx, |this, cx| this.announce_pr_guide_run(announcement, window, cx));
            });
        });
    }

    /// The guide tab of the pull request `key` names is on screen, on the page or in the thread's panel.
    fn pr_guide_on_screen(&self, key: &str) -> bool {
        let Some(panel) = self.pull_requests.panel.as_ref().filter(|p| p.reference.key() == key) else {
            return false;
        };
        if panel.tab != Tab::Guide
            || panel.detail.is_none()
            || self.settings.is_some()
            || !self.review_guides_enabled(&panel.reference)
        {
            return false;
        }
        let on_page = self.page == Some(super::super::Page::PullRequests) && self.pull_request_panel_shown();
        let in_thread = matches!(self.active_surface(), Some(Surface::PullRequest(r)) if *r == panel.reference);
        on_page || in_thread
    }

    /// The pull request's title as last read anywhere (`observedPullRequestTitle`).
    fn observed_pr_title(&self, reference: &Reference) -> Option<String> {
        let panel = self.pr_panel_ref(&reference.key());
        let from_panel =
            panel.and_then(|p| p.detail.as_ref().map(|d| d.title.clone()).or(p.seed.as_ref().map(|s| s.title.clone())));
        from_panel.or_else(|| self.pull_requests.entry_for(reference).map(|e| e.title.clone()))
    }

    /// A toast while the window is in front, else an OS notification; either opens the guide.
    /// Nothing while its guide is already on screen in front of the user.
    fn announce_pr_guide_run(&mut self, announcement: Announcement, window: &mut Window, cx: &mut Context<Self>) {
        let Announcement { reference, title, failure } = announcement;
        let key = reference.key();
        let looking = window.is_window_active();
        if looking && self.pr_guide_on_screen(&key) {
            return;
        }
        let heading = if failure.is_none() { "Review guide ready" } else { "Review guide failed" };
        let body = title
            .or_else(|| self.observed_pr_title(&reference))
            .unwrap_or_else(|| format!("{} #{}", reference.repository, reference.number));
        let mode = Mode::parse(self.client_settings.values.get("notificationMode"));
        if mode.sound() {
            crate::app::alerts::play_sound(if failure.is_none() { Kind::Completion } else { Kind::Input });
        }
        let workspace = cx.weak_entity();
        let open: OpenGuide = Rc::new(move |window, cx| {
            let reference = reference.clone();
            let _ = workspace.update(cx, |this, cx| this.open_pr_guide(reference, window, cx));
        });
        if looking {
            let t = *theme::tokens(cx);
            let (icon, color) = match failure {
                None => (Lucide::CircleCheck, t.success_foreground),
                Some(_) => (Lucide::CircleAlert, t.error_foreground),
            };
            let message = match &failure {
                None => body,
                Some(detail) => format!("{body}: {detail}"),
            };
            let toast = Notification::new()
                .icon(Icon::new(icon).text_color(color))
                .title(heading)
                .message(message)
                .action(move |_, _, cx| {
                    let open = open.clone();
                    Button::new("open-guide").label("Open guide").on_click(cx.listener(move |toast, _, window, cx| {
                        toast.dismiss(window, cx);
                        open(window, cx);
                    }))
                })
                .autohide(true);
            window.push_notification(toast, cx);
            return;
        }
        if !mode.desktop() {
            return;
        }
        let notification = Notification::new()
            .id1::<GuideSystemNotification>(SharedString::from(format!("guide:{key}")))
            .title(heading)
            .message(body)
            .system()
            .on_click(move |_, window, cx| {
                window.activate_window();
                open(window, cx);
            });
        window.push_notification(notification, cx);
    }

    /// "Open guide": the pull request on the Pull requests page, on its Guide tab.
    fn open_pr_guide(&mut self, reference: Reference, window: &mut Window, cx: &mut Context<Self>) {
        self.close_settings(window, cx);
        if self.page != Some(super::super::Page::PullRequests) {
            self.open_page(super::super::Page::PullRequests, cx);
        }
        let seed = self.pull_requests.entry_for(&reference);
        self.open_pull_request_panel(reference, seed, cx);
        self.select_pr_tab(Tab::Guide, cx);
    }

    /// `pullRequests.reviewGuide` for the pull request `key` names, shown or parked.
    pub(super) fn read_pr_guide_of(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some(client) = self.pull_request_client_of(key) else { return };
        let Some(panel) = self.pr_panel(key) else { return };
        panel.guide_loading = true;
        let reference = panel.reference.clone();
        let key = key.to_owned();
        cx.spawn(async move |this, cx| {
            let result = client.request("pullRequests.reviewGuide", reference.to_json()).await;
            let _ = this.update(cx, |this, cx| {
                if let Some(panel) = this.pr_panel(&key) {
                    panel.guide_loading = false;
                }
                match result {
                    Ok(answer) => this.land_pr_guide(&reference, answer, cx),
                    Err(error) => {
                        if let Some(panel) = this.pr_panel(&key) {
                            panel.guide_error = Some(list_logic::error_message(&error).into());
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// `useReviewGuideWriter.write`: starts an agent run on the server (`pullRequests.generateReviewGuide`),
    /// watched from before it is sent.
    pub(super) fn write_pr_guide(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.shown_pr_key() else { return };
        let Some(client) = self.pull_request_client_of(&key) else { return };
        let now = now_ms();
        let Some(panel) = self.pr_panel(&key) else { return };
        if panel.guide_starting || panel.guide.as_deref().is_some_and(is_running) {
            return;
        }
        panel.guide_starting = true;
        if panel.guide.as_ref().is_none_or(|g| g["guide"].is_null()) {
            panel.tab = Tab::Guide;
        }
        let reference = panel.reference.clone();
        let title = panel.detail.as_ref().map(|d| d.title.clone());
        let watch = Watch::started(panel.guide.as_deref(), now);
        self.watch_pr_guide_run(&reference, title, watch, cx);
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("pullRequests.generateReviewGuide", reference.to_json()).await;
            let _ = this.update_in(cx, |this, window, cx| {
                if let Some(panel) = this.pr_panel(&key) {
                    panel.guide_starting = false;
                }
                match result {
                    Ok(answer) => this.land_pr_guide(&reference, answer, cx),
                    Err(error) => {
                        this.settle_pr_guide_run(&key);
                        toast_error(
                            window,
                            cx,
                            "Could not start a review guide",
                            logic::readable_failure(
                                &list_logic::error_message(&error),
                                "Check the review guide model in Settings → Source control.",
                            ),
                        );
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const START: i64 = 1_000_000;

    /// A case's name, its answers with how long after the start each lands, and the last outcome.
    type Case<'a> = (&'a str, &'a [(Value, i64)], Outcome);

    fn answer(guide_at: Option<&str>, run: Value) -> Value {
        json!({ "guide": guide_at.map(|at| json!({ "generatedAt": at })), "run": run, "currentHeadSha": "abc" })
    }

    fn running() -> Value {
        json!({ "_tag": "Running", "headSha": "abc", "startedAt": "2026-01-01T00:00:00Z" })
    }

    fn failed(at: &str) -> Value {
        json!({ "_tag": "Failed", "headSha": "abc", "failedAt": at, "detail": "model refused" })
    }

    #[test]
    fn a_started_run_settles_by_what_its_answers_show() {
        let before = answer(Some("t1"), failed("f1"));
        let cases: &[Case] = &[
            ("the old answer, early", &[(answer(Some("t1"), failed("f1")), 5_000)], Outcome::Waiting),
            ("the old answer, never seen running", &[(answer(Some("t1"), Value::Null), 61_000)], Outcome::Settled),
            ("running", &[(answer(Some("t1"), running()), 5_000)], Outcome::Running),
            (
                "seen running, then nothing new",
                &[(answer(Some("t1"), running()), 5_000), (answer(Some("t1"), Value::Null), 10_000)],
                Outcome::Settled,
            ),
            ("a new guide", &[(answer(Some("t2"), Value::Null), 5_000)], Outcome::Ready),
            ("a new failure", &[(answer(Some("t1"), failed("f2")), 5_000)], Outcome::Failed("model refused".into())),
        ];
        for (name, answers, expected) in cases {
            let mut watch = Watch::started(Some(&before), START);
            let outcome = answers.iter().map(|(a, after)| watch.observe(a, START + after)).last().unwrap();
            assert_eq!(&outcome, expected, "{name}");
        }
    }

    #[test]
    fn a_run_first_seen_running_announces_any_failure_and_settles_on_an_old_guide() {
        let seen = answer(Some("t1"), running());
        let mut watch = Watch::answered_running(&seen, START);
        assert_eq!(
            watch.observe(&answer(Some("t1"), failed("f0")), START + 5_000),
            Outcome::Failed("model refused".into())
        );
        let mut watch = Watch::answered_running(&seen, START);
        assert_eq!(watch.observe(&answer(Some("t1"), Value::Null), START + 5_000), Outcome::Settled);
        let mut watch = Watch::answered_running(&answer(None, running()), START);
        assert_eq!(watch.observe(&answer(Some("t1"), Value::Null), START + 5_000), Outcome::Ready);
    }
}
