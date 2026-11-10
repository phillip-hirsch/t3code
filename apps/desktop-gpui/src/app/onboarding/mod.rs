//! First run (`FirstRunGate.tsx`, `WelcomePage.tsx`, `routes/welcome.tsx`): the launch waits
//! until it can tell a fresh workspace from one in use, then opens the Welcome page or lands
//! in the app. Finishing setup records `onboardingCompletedAt` in this device's settings.

mod agents;
mod first_run;
mod import;
mod projects;
mod welcome;

pub(in crate::app) use self::welcome::{AgentState, agent_name, onboarding_state};

use std::time::Duration;

use gpui_kit::component::notification::Notification;
use gpui_kit::component::{WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, FontWeight, InteractiveElement as _, IntoElement as _, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Task, Window, div, px,
};
use serde_json::Value;

use self::first_run::{Decision, Evidence, GateState, Welcome};
use self::welcome::ImportOutcome;
use super::Workspace;
use super::pages::settings::connections::ui::{Size, Variant, WebButton};
use super::pages::{Location, Page};
use crate::rpc::RpcClient;

/// `FIRST_RUN_DECISION_TIMEOUT_MS`.
const DECISION_TIMEOUT: Duration = Duration::from_secs(4);
const PRIVACY_POLICY_URL: &str = "https://t3.codes/privacy-policy";
const COMPLETED_KEY: &str = "onboardingCompletedAt";

/// The first-run gate's state and the evidence only it reads.
pub(super) struct Onboarding {
    gate: GateState,
    /// The primary server's last `welcome` lifecycle event.
    welcome: Option<Welcome>,
    _timeout: Option<Task<()>>,
    agents: agents::AgentsStage,
    projects: projects::ProjectsStage,
}

impl Onboarding {
    pub(super) fn new(settings: &crate::settings::ClientSettings) -> Self {
        Self {
            gate: GateState::initial(completed(settings)),
            welcome: None,
            _timeout: None,
            agents: agents::AgentsStage::default(),
            projects: projects::ProjectsStage::default(),
        }
    }

    /// The launch can't yet tell whether to open setup or the app.
    pub(super) fn pending(&self) -> bool {
        self.gate.decision == Decision::Pending
    }

    /// The decision stayed pending past the timeout (`FirstRunRecovery reason="connection"`).
    pub(super) fn stalled(&self) -> bool {
        self.gate.stalled
    }

    fn wizard(&self) -> bool {
        self.gate.decision == Decision::Wizard
    }
}

fn completed(settings: &crate::settings::ClientSettings) -> bool {
    !settings.values.get(COMPLETED_KEY).is_null()
}

struct FinishFailed;

impl Workspace {
    /// `subscribeServerLifecycle` on the primary server, for its `welcome`.
    pub(super) fn subscribe_lifecycle(
        &mut self,
        client: &RpcClient,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        let stream = client.stream("subscribeServerLifecycle", serde_json::json!({}));
        Self::pump(
            stream,
            window,
            cx,
            |this, event| {
                if event["type"] != "welcome" {
                    return;
                }
                match serde_json::from_value(event["payload"].clone()) {
                    Ok(welcome) => this.onboarding.welcome = Some(welcome),
                    Err(error) => log::warn!("bad welcome event: {error}"),
                }
            },
            Self::land,
        )
    }

    /// `FirstRunGate`'s inputs, read from the primary session, every environment's shell, the
    /// saved environments and this device's settings.
    fn first_run_evidence(&self, cx: &gpui_kit::App) -> Evidence {
        let hydrated = !self.client_settings.read_failed;
        let completed = completed(&self.client_settings);
        let primary = self.primary_environment().map(SharedString::as_ref);
        let enabled = primary.is_some_and(|id| self.env_client(id).is_some());
        if completed || !enabled {
            return Evidence { enabled, hydrated, completed, ..Evidence::default() };
        }
        let live = |id: &str| self.env_client(id).is_some() && self.shell.is_live(id);
        let (projects, threads) = (&self.shell.projects, &self.shell.threads);
        let info = self.primary_info();
        let workspace_fresh = threads.len() <= 1
            && first_run::is_fresh_workspace(
                primary,
                info.config["cwd"].as_str(),
                self.onboarding.welcome.as_ref(),
                projects,
                threads,
            );
        Evidence {
            enabled,
            hydrated,
            completed,
            bootstrapped: self.shells_bootstrapped(),
            authoritative: primary.is_some_and(live),
            workspace_authoritative: projects
                .iter()
                .map(|p| &p.environment_id)
                .chain(threads.iter().map(|t| &t.environment_id))
                .all(|id| live(id)),
            workspace_provenance_authoritative: Welcome::provenance_authoritative(self.onboarding.welcome.as_ref()),
            // The saved environments are read off the UI thread; until then nothing is decided.
            catalog_ready: !self.saved_environments.read(cx).loading(),
            server_config_available: !info.config.is_null(),
            workspace_fresh,
            project_count: projects.len(),
            thread_count: threads.len(),
        }
    }

    /// Moves the gate with the latest evidence, records setup as finished once a used workspace
    /// is confirmed, and runs the timeout while the decision waits on a connected server.
    pub(super) fn follow_first_run(&mut self, cx: &mut Context<Self>) {
        let evidence = self.first_run_evidence(cx);
        if !evidence.hydrated {
            return;
        }
        // The web opens the app without a primary server; this app's primary is its own server,
        // so until it answers the decision waits, without the timeout.
        if !evidence.enabled && !evidence.completed {
            self.onboarding._timeout = None;
            return;
        }
        let (decision, persist) = first_run::resolve(&evidence);
        let gate = self.onboarding.gate.evidence(decision);
        self.onboarding.gate = gate;
        if gate.decision != Decision::Wizard && persist && !evidence.completed {
            // As the web's `completeOnboarding().catch(() => undefined)`: a failed write is only logged.
            self.client_settings.apply(&completion_patch());
        }
        if !(evidence.enabled && gate.decision == Decision::Pending && !gate.stalled) {
            self.onboarding._timeout = None;
        } else if self.onboarding._timeout.is_none() {
            self.onboarding._timeout = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(DECISION_TIMEOUT).await;
                let _ = this.update(cx, |this, cx| {
                    this.onboarding.gate = this.onboarding.gate.timeout();
                    cx.notify();
                });
            }));
        }
    }

    /// Until setup is finished, home is the Welcome page, unless a link is waiting to open.
    pub(super) fn welcome_is_home(&self) -> bool {
        self.onboarding.wizard() && !self.has_pending_links()
    }

    /// Setup opens by itself only from home, so a link or a page the user opened never gives way to it.
    pub(super) fn welcome_owns_home(&self) -> bool {
        self.welcome_is_home() && self.target.is_none() && self.page.is_none() && self.settings.is_none()
    }

    /// `navigate({ to: "/welcome", replace: true })`.
    pub(super) fn show_welcome(&mut self, cx: &mut Context<Self>) {
        self.settings = None;
        self.page = Some(Page::Welcome);
        self.history.replace(Location::Page(Page::Welcome));
        self.notify_sidebar(cx);
        cx.notify();
    }

    /// Settings › About › Welcome page.
    pub(super) fn open_welcome(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_settings(window, cx);
        self.open_page(Page::Welcome, cx);
    }

    pub(super) fn welcome_open(&self) -> bool {
        self.page == Some(Page::Welcome)
    }

    /// Follows the Welcome page's bands as the page shows or goes.
    pub(super) fn sync_welcome(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_welcome_projects(window, cx);
        self.sync_welcome_agents(cx);
    }

    /// "Reload" on the stalled gate: the primary's subscriptions start over and the wait restarts.
    pub(super) fn reload_first_run(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.onboarding.gate.stalled = false;
        self.onboarding._timeout = None;
        if let Some(primary) = self.primary_environment().cloned() {
            self.start_session(primary, window, cx);
        }
        self.follow_first_run(cx);
        cx.notify();
    }

    /// `WelcomePage`'s `finish`: records setup as finished, then lands in `landing` (a project
    /// the import brought in, on its computer) or at home, and reports the import. False when
    /// setup could not be saved.
    pub(super) fn finish_welcome(
        &mut self,
        landing: Option<(SharedString, String)>,
        outcome: &ImportOutcome,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        window.remove_notification::<FinishFailed>(cx);
        if let Err(error) = self.client_settings.try_apply(&completion_patch()) {
            log::warn!("could not finish setup: {error}");
            let toast = Notification::error("Your settings could not be saved. Try again.")
                .title("Could not finish setup")
                .id::<FinishFailed>();
            window.push_notification(toast, cx);
            return false;
        }
        self.follow_first_run(cx);
        match landing {
            // The shell may not list a project created moments ago yet.
            Some((environment, project_id)) => self.start_draft_in(environment, project_id, window, cx),
            None => self.go_home(window, cx),
        }
        match welcome::finish_toast(outcome) {
            Some(welcome::FinishToast::PartialHistory(warning)) => window.push_notification(
                Notification::warning(warning).title("Some history was not imported").autohide(false),
                cx,
            ),
            Some(welcome::FinishToast::Imported(title)) => {
                window.push_notification(Notification::success("").title(title), cx);
            }
            None => {}
        }
        true
    }

    /// `WelcomePage` for the primary computer.
    pub(super) fn render_welcome(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = *crate::theme::tokens(cx);
        let info = self.primary_info();
        let label: SharedString = info.environment_label.clone().unwrap_or_else(|| "this computer".into()).into();
        let agent_ready = welcome::ready_agent_count(&info.providers, &info.settings, &info.managed_policy) > 0;
        let notice = welcome::telemetry_notice(&info.managed_policy);
        let start = |this: &mut Self, _: &gpui_kit::ClickEvent, window: &mut Window, cx: &mut Context<Self>| {
            this.start_welcome(window, cx);
        };
        // `pt-12 sm:pt-20`, and the bands' `sm:` grid.
        let wide = window.viewport_size().width >= px(640.);
        let primary = self.primary_environment().cloned();
        let agents = primary.clone().map(|environment| self.render_agents(environment, &label, wide, window, cx));
        let (projects, started) = match primary {
            Some(environment) => self.render_projects(environment, wide, cx),
            None => (None, projects::StartState { importing: false, label: "Start".into() }),
        };
        let start_button = WebButton::new("welcome-start", started.label)
            .variant(Variant::Primary)
            .size(Size::Default)
            .disabled(!agent_ready || started.importing);
        let footer = h_flex()
            .mt(px(24.))
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap(px(12.))
            .child(div().when(!agent_ready, |el| {
                el.child(
                    WebButton::new("welcome-start-without-agent", "Start without an agent")
                        .variant(Variant::GhostMuted)
                        .size(Size::Default)
                        .disabled(started.importing)
                        .on_click(cx.listener(start)),
                )
            }))
            .child(
                h_flex()
                    .items_center()
                    .gap(px(12.))
                    .when(!agent_ready, |el| {
                        el.child(
                            div()
                                .text_xs()
                                .line_height(px(16.))
                                .text_color(t.muted_foreground)
                                .child("Set up one agent to start."),
                        )
                    })
                    .child(if agent_ready { start_button.on_click(cx.listener(start)) } else { start_button }),
            );
        let top = if wide { 80. } else { 48. };
        div()
            .id("welcome-scroll")
            .size_full()
            .min_h_0()
            .overflow_y_scroll()
            .bg(t.background)
            .child(
                v_flex()
                    .mx_auto()
                    .min_h_full()
                    .w_full()
                    .max_w(px(768.))
                    .px(px(24.))
                    .pt(px(top))
                    .pb(px(32.))
                    .child(
                        div()
                            .text_size(px(24.))
                            .line_height(px(32.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(t.foreground)
                            .child("Welcome to T3 Code"),
                    )
                    .child(
                        h_flex()
                            .mt(px(6.))
                            .flex_wrap()
                            .text_sm()
                            .line_height(px(20.))
                            .text_color(t.muted_foreground)
                            .child("Setting up\u{a0}")
                            .child(div().text_color(t.foreground).child(label))
                            .child("."),
                    )
                    // The page's sections are bands under this rule.
                    .child(
                        div()
                            .mt(px(40.))
                            .border_t_1()
                            .border_color(t.border.opacity(0.6))
                            .children(agents)
                            .children(projects),
                    )
                    .child(footer)
                    .child(telemetry_notice(&notice, &t)),
            )
            .into_any_element()
    }
}

/// `{ onboardingCompletedAt: new Date().toISOString() }`.
fn completion_patch() -> serde_json::Map<String, Value> {
    let at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    serde_json::Map::from_iter([(COMPLETED_KEY.to_owned(), Value::String(at))])
}

/// `TelemetryNotice`: word by word so the line wraps, with the privacy policy as a tab stop.
fn telemetry_notice(lead: &str, t: &crate::theme::Tokens) -> AnyElement {
    let (size, line) = (px(11.), px(17.875));
    let color = t.muted_foreground.opacity(0.7);
    let hover = t.muted_foreground;
    let ring = t.ring;
    let link = div()
        .id("welcome-privacy-policy")
        .tab_index(0)
        .cursor_pointer()
        .underline()
        .hover(move |style| style.text_color(hover))
        .focus_visible(move |style| {
            style.rounded(px(2.)).shadow(vec![gpui_kit::BoxShadow::new(px(0.), px(0.), ring).spread_radius(px(2.))])
        })
        .on_click(|_, _, cx| cx.open_url(PRIVACY_POLICY_URL))
        .child("privacy policy");
    h_flex()
        .mt_auto()
        .pt(px(48.))
        .flex_wrap()
        .text_size(size)
        .line_height(line)
        .text_color(color)
        .children(lead.split_inclusive(' ').map(|word| div().child(word.to_owned())))
        .child(div().child(" "))
        .child(h_flex().child(link).child("."))
        .into_any_element()
}
