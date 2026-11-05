//! Failure banners: the thread error and provider status pinned over the timeline
//! (`ChatBannerLane.tsx`, `ThreadErrorBanner.tsx`, `ProviderStatusBanner.tsx`), and the
//! composer's usage-limit recovery and offline notices (`UsageLimitRecoveryBanner.tsx`,
//! `useSystemComposerBanners.tsx`).

use std::collections::HashSet;
use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, ClipboardItem, Context, FontWeight, Hsla, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, Task, Window, div, prelude::FluentBuilder as _, px,
    rems,
};
use serde_json::{Value, json};

use super::{Connection, Target, Workspace, new_id};
use crate::model::{Provider, ThreadShell};
use crate::theme::Tokens;

/// `CHATGPT_USAGE_LIMIT_CODE` and friends (`packages/shared/src/usageLimits.ts`).
const CHATGPT_USAGE_LIMIT_CODE: &str = "subscription_sharing_usage_limit_exceeded";
const CHATGPT_WORKSPACE_USAGE_LIMIT_CODE: &str = "subscription_sharing_workplace_individual_usage_limit_exceeded";
const CHATGPT_WORKSPACE_ADMIN_REQUEST: &str = "I've reached the limit for using my ChatGPT plan in T3 Code. Could you increase our workspace's usage limits so I can continue?\n\nGo to admin.openai.com → Workspace → Usage limits. Thank you!";
/// `ENVIRONMENT_RECONNECT_WARNING_GRACE_MS`.
const RECONNECT_GRACE: Duration = Duration::from_secs(2);

#[derive(Default)]
pub(super) struct BannerUi {
    /// `sessionDismissedThreadErrorBannerKeys`.
    dismissed_errors: HashSet<String>,
    dismissed_provider: Option<String>,
    expanded_error: bool,
    refreshing_provider: bool,
    recovery_pending: bool,
    /// The socket dropped after it had connected once.
    was_connected: bool,
    /// The reconnect grace has passed; `_grace` flips it.
    reconnect_warning: bool,
    _grace: Option<Task<()>>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Variant {
    Error,
    Warning,
}

/// `getThreadErrorBannerMessage`: a provider's JSON error reads as its `error.message`.
pub(crate) fn thread_error_message(error: &str) -> String {
    serde_json::from_str::<Value>(error)
        .ok()
        .and_then(|payload| payload.pointer("/error/message")?.as_str().map(str::to_owned))
        .filter(|message| !message.trim().is_empty())
        .unwrap_or_else(|| error.to_owned())
}

/// `formatProviderDriverKindLabel`: `claudeAgent` reads "Claude Agent".
pub(crate) fn driver_label(driver: &str) -> String {
    let mut spaced = String::with_capacity(driver.len() + 4);
    let mut previous: Option<char> = None;
    for c in driver.chars() {
        if c.is_ascii_uppercase() && previous.is_some_and(|p| p.is_ascii_lowercase()) {
            spaced.push(' ');
        }
        spaced.push(if c == '_' || c == '-' { ' ' } else { c });
        previous = Some(c);
    }
    let words: Vec<String> = spaced
        .split_whitespace()
        .map(|word| {
            let mut chars = word.chars();
            chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
        })
        .collect();
    words.join(" ")
}

fn advisory<'a>(provider: &'a Provider) -> (Option<&'a str>, Option<&'a str>) {
    let advisory = provider.compatibility_advisory.as_ref();
    let field = |key: &str| advisory.and_then(|a| a.get(key)).and_then(Value::as_str);
    (field("status"), field("message"))
}

/// `getIncompatibleVersion`: the advisory's status, when the banner should name it.
fn incompatible(provider: &Provider) -> Option<&str> {
    if provider.status == "error" && provider.auth.status == "unauthenticated" {
        return None;
    }
    let (status, _) = advisory(provider);
    status.filter(|s| *s == "broken" || (provider.status == "ready" && *s == "unsupported"))
}

/// `getProviderStatusBannerKey`: what the banner is about, `None` when there's nothing to say.
pub(crate) fn provider_banner_key(provider: &Provider) -> Option<String> {
    if provider.status == "disabled" {
        return None;
    }
    if let Some(status) = incompatible(provider) {
        let message = advisory(provider).1.unwrap_or_default();
        return Some(
            [provider.instance_id.as_str(), status, provider.version.as_deref().unwrap_or_default(), message]
                .join("\0"),
        );
    }
    if provider.status == "ready" {
        return None;
    }
    // Antigravity validates credentials at session start; unknown restart auth is not failure.
    if provider.driver == "antigravity"
        && provider.installed
        && provider.status == "warning"
        && provider.auth.status == "unknown"
    {
        return None;
    }
    let message = provider.message.as_deref().unwrap_or_default();
    Some([provider.instance_id.as_str(), &provider.status, &provider.auth.status, message].join("\0"))
}

/// `hasProviderSetup`.
fn has_setup(provider: &Provider) -> bool {
    provider.driver == "antigravity"
        || provider.setup.as_ref().is_some_and(|setup| setup.can_authenticate || setup.can_install)
}

fn provider_name(provider: &Provider) -> String {
    provider
        .display_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| driver_label(&provider.driver))
}

/// `getProviderStatusMessage`.
pub(crate) fn provider_status_message(provider: &Provider) -> String {
    let (advisory_status, advisory_message) = advisory(provider);
    if provider.auth.status != "unauthenticated" && advisory_status == Some("broken") {
        if let Some(message) = advisory_message.filter(|m| !m.is_empty()) {
            return message.to_owned();
        }
    }
    if let Some(message) = provider.message.as_deref().filter(|m| !m.is_empty()) {
        return message.to_owned();
    }
    let name = provider_name(provider);
    if !provider.installed && has_setup(provider) {
        return format!("Open provider setup to install {} on this environment.", driver_label(&provider.driver));
    }
    if provider.auth.status == "unauthenticated" {
        if has_setup(provider) {
            return if provider.driver == "antigravity" {
                "Open provider setup to sign in with Google.".into()
            } else {
                "Open provider setup to sign in.".into()
            };
        }
        return "Sign in via the CLI to authenticate again.".into();
    }
    match provider.status.as_str() {
        "ready" => "No models are available for this provider.".into(),
        "error" => format!("{name} provider is unavailable."),
        _ => format!("{name} provider has limited availability."),
    }
}

/// The banner's title, message and whether it is a warning rather than an error.
fn provider_banner(provider: &Provider) -> (String, String, Variant) {
    let name = provider_name(provider);
    let incompatible = incompatible(provider);
    let title = if provider.status == "error" && provider.auth.status == "unauthenticated" {
        format!("{name} is unauthenticated")
    } else if let Some(status) = incompatible {
        let kind = if status == "broken" { "known to be broken" } else { "unsupported" };
        format!("{name} {} is {kind}", provider.version.as_deref().unwrap_or_default())
    } else {
        format!("{name} provider status")
    };
    let message = incompatible
        .and_then(|_| advisory(provider).1.map(str::to_owned))
        .unwrap_or_else(|| provider_status_message(provider));
    let warning = incompatible != Some("broken") && (provider.status == "warning" || incompatible.is_some());
    (title, message, if warning { Variant::Warning } else { Variant::Error })
}

/// The usage-limit recovery banner's state for the thread's latest run.
#[derive(Debug, PartialEq)]
pub(crate) struct Recovery {
    pub run_id: String,
    pub reset_at: Option<String>,
    /// The reset falls after the run stopped, so resuming or snoozing until it makes sense.
    pub can_schedule: bool,
    pub scheduled: bool,
    pub snoozed: bool,
    pub reset_passed: bool,
}

fn parse_ms(iso: &str) -> Option<i64> {
    iso.parse::<jiff::Timestamp>().ok().map(|t| t.as_millisecond())
}

/// `usageLimitRecoveryBannerItem` + `RecoveryActions`, for a thread whose latest run hit a limit.
pub(crate) fn recovery(thread: &ThreadShell, now_ms: i64) -> Option<Recovery> {
    if thread.status != "failed" || thread.last_error_class.as_deref() != Some("usage_limit") {
        return None;
    }
    let run_id = thread.latest_run_id.clone()?;
    let reset_at = thread.usage_limit_reset_at.clone();
    let stopped_at = thread.latest_run_completed_at.clone().flatten().unwrap_or_else(|| thread.updated_at.clone());
    let reset_ms = reset_at.as_deref().and_then(parse_ms);
    let can_schedule = matches!((reset_ms, parse_ms(&stopped_at)), (Some(reset), Some(stopped)) if reset > stopped);
    let recovery = thread.limit_recovery.as_ref();
    let field = |key: &str| recovery.and_then(|r| r.get(key));
    let same = field("runId").and_then(Value::as_str) == Some(run_id.as_str())
        && field("resetAt").and_then(Value::as_str) == reset_at.as_deref();
    let reset_passed = reset_ms.is_some_and(|reset| reset <= now_ms);
    let snoozed = same
        && field("snooze").and_then(Value::as_bool) == Some(true)
        && reset_ms.is_some()
        && thread.snoozed_until.as_deref().and_then(parse_ms) == reset_ms
        && !reset_passed;
    Some(Recovery {
        scheduled: same && field("autoResume").and_then(Value::as_bool) == Some(true),
        run_id,
        reset_at,
        can_schedule,
        snoozed,
        reset_passed,
    })
}

impl Workspace {
    fn open_shell_thread(&self) -> Option<&ThreadShell> {
        let Some(Target::Thread(id)) = &self.target else { return None };
        self.shell.threads.iter().find(|thread| &thread.id == id)
    }

    fn thread_key(&self) -> String {
        match &self.target {
            Some(Target::Thread(id)) => id.clone(),
            Some(Target::Draft { project_id }) => format!("draft:{project_id}"),
            None => String::new(),
        }
    }

    /// The error the banner shows: this client's last failure, else the thread's runtime error.
    /// Returns it with the runtime class and code when it came from the server.
    fn thread_error(&self) -> Option<(String, Option<&str>, Option<&str>)> {
        if let Some(error) = &self.error {
            return Some((error.to_string(), None, None));
        }
        let thread = self.open_shell_thread()?;
        let error = thread.last_error.clone().filter(|e| !e.trim().is_empty())?;
        // A usage-limited run shows its recovery banner instead.
        if thread.status == "failed"
            && thread.last_error_class.as_deref() == Some("usage_limit")
            && thread.latest_run_id.is_some()
        {
            return None;
        }
        Some((error, thread.last_error_class.as_deref(), thread.last_error_code.as_deref()))
    }

    fn active_provider(&self) -> Option<&Provider> {
        let selection = self.composer_model()?;
        self.server_info.provider(&selection.instance_id)
    }

    /// The banners over the timeline; only this client's error shows over a page.
    pub(super) fn render_banner_lane(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let t = *crate::theme::tokens(cx);
        let on_chat = self.page.is_none() && self.target.is_some();
        let provider = self.active_provider().filter(|_| on_chat).filter(|p| {
            provider_banner_key(p).is_some_and(|key| self.banners.dismissed_provider.as_ref() != Some(&key))
        });
        let error = self
            .thread_error()
            .filter(|(error, _, _)| !self.banners.dismissed_errors.contains(&format!("{}\0{error}", self.thread_key())))
            .filter(|_| on_chat || self.error.is_some());
        if provider.is_none() && error.is_none() {
            return None;
        }
        let mut lane = v_flex().absolute().top_0().left_0().right_0().items_center();
        if let Some(provider) = provider {
            lane = lane.child(self.render_provider_banner(provider, &t, cx));
        }
        if let Some((error, class, code)) = error {
            lane = lane.child(self.render_thread_error(&error, class, code, &t, cx));
        }
        Some(lane.into_any_element())
    }

    fn render_thread_error(
        &self,
        error: &str,
        class: Option<&str>,
        code: Option<&str>,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let workspace_limit = code == Some(CHATGPT_WORKSPACE_USAGE_LIMIT_CODE);
        let chatgpt_limit = workspace_limit || code == Some(CHATGPT_USAGE_LIMIT_CODE);
        let variant = if class == Some("usage_limit") || chatgpt_limit { Variant::Warning } else { Variant::Error };
        let (_, foreground, icon_color) = colors(variant, t);
        let icon = if chatgpt_limit {
            Icon::empty().path("icons/app/openai.svg").text_color(t.foreground)
        } else {
            Icon::new(IconName::CircleAlert).text_color(icon_color)
        };
        let body = if chatgpt_limit {
            v_flex()
                .gap(px(4.))
                .child(div().font_weight(FontWeight::MEDIUM).text_color(foreground).child("ChatGPT usage limit reached"))
                .when(workspace_limit, |el| {
                    el.child("You've reached your workspace's limit for using your ChatGPT plan in other apps. Copy a request to ask your workspace admin for increased usage limits.")
                })
        } else {
            let collapsed = thread_error_message(error);
            let can_expand = collapsed != error || error.chars().count() > 180 || error.contains('\n');
            let expanded = self.banners.expanded_error;
            let text = if expanded || !can_expand { error.to_owned() } else { collapsed };
            v_flex().min_w_0().child(div().when(can_expand && !expanded, |el| el.line_clamp(3)).child(text)).when(
                can_expand,
                |el| {
                    el.child(
                        div()
                            .id("thread-error-expand")
                            .mt(px(4.))
                            .cursor_pointer()
                            .text_color(t.muted_foreground)
                            .hover(|s| s.text_color(t.foreground))
                            .child(if expanded { "Show less" } else { "Show full error" })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.banners.expanded_error = !this.banners.expanded_error;
                                cx.notify();
                            })),
                    )
                },
            )
        };
        let key = format!("{}\0{error}", self.thread_key());
        let mut actions = h_flex().gap(px(4.));
        if workspace_limit {
            actions = actions.child(
                Button::new("copy-admin-request")
                    .primary()
                    .small()
                    .label("Copy request for admin")
                    .on_click(cx.listener(|_, _, window, cx| copy_admin_request(window, cx))),
            );
        } else if chatgpt_limit {
            actions = actions.child(super::pages::settings::chatgpt_usage_button("thread-error-usage", t));
        }
        actions = actions.child(
            Button::new("dismiss-thread-error")
                .ghost()
                .xsmall()
                .icon(Icon::new(IconName::X))
                .tooltip("Dismiss error")
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.error = None;
                    this.banners.expanded_error = false;
                    this.banners.dismissed_errors.insert(key.clone());
                    cx.notify();
                })),
        );
        alert(variant, icon, None, body.into_any_element(), actions.into_any_element(), px(768.), t)
    }

    fn render_provider_banner(&self, provider: &Provider, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let (title, message, variant) = provider_banner(provider);
        let (_, _, icon_color) = colors(variant, t);
        let name = provider_name(provider);
        let instance = provider.instance_id.clone();
        let key = provider_banner_key(provider);
        let refreshing = self.banners.refreshing_provider;
        let tooltip: SharedString = message.clone().into();
        let body = v_flex()
            .gap(px(8.))
            .child(
                div()
                    .id("provider-status-message")
                    .line_clamp(3)
                    .child(message)
                    .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx)),
            )
            .child(
                h_flex()
                    .flex_wrap()
                    .justify_end()
                    .gap(px(12.))
                    .when(has_setup(provider), |el| {
                        el.child(
                            div()
                                .id("open-provider-setup")
                                .cursor_pointer()
                                .text_color(t.foreground)
                                .hover(|s| s.underline())
                                .child("Open provider setup")
                                .on_click(cx.listener(|this, _, _, cx| this.open_settings("providers", cx))),
                        )
                    })
                    .child(
                        Button::new("refresh-provider-status")
                            .outline()
                            .xsmall()
                            .icon(Icon::new(IconName::RefreshCw))
                            .label(if refreshing { "Refreshing..." } else { "Refresh" })
                            .disabled(refreshing)
                            .tooltip(format!("Refresh {name} provider status"))
                            .on_click(cx.listener(move |this, _, _, cx| this.refresh_provider_status(&instance, cx))),
                    ),
            );
        let dismiss = Button::new("dismiss-provider-status")
            .ghost()
            .xsmall()
            .icon(Icon::new(IconName::X))
            .tooltip(format!("Dismiss {name} provider {}", provider.status))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.banners.dismissed_provider = key.clone();
                cx.notify();
            }));
        let icon = Icon::new(IconName::Info).text_color(icon_color);
        alert(variant, icon, Some(title.into()), body.into_any_element(), dismiss.into_any_element(), px(f32::MAX), t)
    }

    fn refresh_provider_status(&mut self, instance_id: &str, cx: &mut Context<Self>) {
        let Some(client) = self.client() else { return };
        if self.banners.refreshing_provider {
            return;
        }
        self.banners.refreshing_provider = true;
        cx.notify();
        let input = json!({"instanceId": instance_id, "refreshModels": true});
        cx.spawn(async move |this, cx| {
            let result = client.request("server.refreshProviders", input).await;
            let _ = this.update(cx, |this, cx| {
                this.banners.refreshing_provider = false;
                match result {
                    Ok(reply) => {
                        let _ = this.server_info.apply(&json!({"type": "providerStatuses", "payload": reply}));
                    }
                    Err(error) => this.error = Some(error.to_string().into()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The composer's machine notice, which stacks above every other (`ComposerBannerGroup`).
    pub(super) fn render_environment_banner(&self, t: &Tokens, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.render_offline_banner(t, cx)
    }

    /// Tracks drops after a first connection, holding the warning back for the grace period.
    pub(super) fn sync_connection_banner(&mut self, cx: &mut Context<Self>) {
        match &self.connection {
            Connection::Connected(_) => {
                self.banners.was_connected = true;
                self.banners.reconnect_warning = false;
                self.banners._grace = None;
            }
            Connection::Disconnected(_) | Connection::Connecting if self.banners.was_connected => {
                if self.banners._grace.is_none() && !self.banners.reconnect_warning {
                    self.banners._grace = Some(cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(RECONNECT_GRACE).await;
                        let _ = this.update(cx, |this, cx| {
                            this.banners.reconnect_warning = true;
                            this.banners._grace = None;
                            cx.notify();
                        });
                    }));
                }
            }
            _ => {}
        }
    }

    /// "<server> is reconnecting" while the socket retries, "offline" once the server is gone.
    fn render_offline_banner(&self, t: &Tokens, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (reconnecting, reason) = match &self.connection {
            Connection::Disconnected(reason) => (true, Some(reason.clone())),
            Connection::Connecting => (true, None),
            Connection::Failed(reason) => (false, Some(reason.clone())),
            _ => return None,
        };
        if !self.banners.was_connected || (reconnecting && !self.banners.reconnect_warning) {
            return None;
        }
        let label = self.server_info.environment_label.clone().unwrap_or_else(|| "Server".into());
        let title = format!("{label} is {}", if reconnecting { "reconnecting" } else { "offline" });
        let actions = (!reconnecting).then(|| {
            Button::new("reconnect-server")
                .ghost()
                .xsmall()
                .label("Reconnect")
                .on_click(cx.listener(|this, _, window, cx| this.reconnect(window, cx)))
                .into_any_element()
        });
        let variant = if reconnecting { Variant::Warning } else { Variant::Error };
        Some(composer_banner(variant, Icon::new(IconName::WifiOff), title.into(), reason, actions, t))
    }

    /// Starts the connection over after the server stopped.
    fn reconnect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.connection = Connection::Starting;
        self._connection_task = Self::connect(window, cx);
        cx.notify();
    }

    pub(super) fn render_recovery_banner(&self, t: &Tokens, cx: &mut Context<Self>) -> Option<AnyElement> {
        let thread = self.open_shell_thread()?;
        let state = recovery(thread, jiff::Timestamp::now().as_millisecond())?;
        let code = thread.last_error_code.as_deref();
        let workspace_limit = code == Some(CHATGPT_WORKSPACE_USAGE_LIMIT_CODE);
        let chatgpt_limit = workspace_limit || code == Some(CHATGPT_USAGE_LIMIT_CODE);
        let hour12 =
            crate::timestamp::hour12(self.client_settings.values.get("timestampFormat").as_str().unwrap_or(""));
        let reset = match state.reset_at.as_deref() {
            Some(at) => crate::timestamp::upcoming(at, &jiff::Zoned::now(), hour12)
                .map(|at| format!("Resets {at}"))
                .unwrap_or_else(|| "Reset time unavailable; retry manually".into()),
            None => "Reset time unavailable; retry manually".into(),
        };
        let (title, description) = match (chatgpt_limit, workspace_limit) {
            (false, _) => ("Usage limit reached", reset),
            (true, true) => (
                "ChatGPT usage limit reached",
                "You've reached your workspace's limit for using your ChatGPT plan in other apps.".to_owned(),
            ),
            (true, false) => ("ChatGPT usage limit reached", reset),
        };
        let mut actions = h_flex().gap(px(2.));
        if state.can_schedule {
            let pending = self.banners.recovery_pending;
            let resume = if pending {
                "Saving..."
            } else if state.scheduled {
                "Cancel auto-resume"
            } else {
                "Resume at reset"
            };
            let snooze = if pending {
                "Saving..."
            } else if state.snoozed {
                "Wake now"
            } else {
                "Snooze until reset"
            };
            let (thread_id, run_id, reset_at) = (thread.id.clone(), state.run_id.clone(), state.reset_at.clone());
            let update =
                move |field: &'static str, value: bool| json!({"runId": run_id, "resetAt": reset_at, field: value});
            let (resume_update, snooze_update) =
                (update("autoResume", !state.scheduled), update("snooze", !state.snoozed));
            let (resume_thread, snooze_thread) = (thread_id.clone(), thread_id);
            let snooze_blocked = state.reset_passed && !state.snoozed;
            actions = actions
                .child(Button::new("recovery-resume").ghost().xsmall().label(resume).disabled(pending).on_click(
                    cx.listener(move |this, _, window, cx| {
                        this.update_limit_recovery(&resume_thread, resume_update.clone(), false, window, cx)
                    }),
                ))
                .child(
                    Button::new("recovery-snooze")
                        .ghost()
                        .xsmall()
                        .label(snooze)
                        .disabled(pending || snooze_blocked)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.update_limit_recovery(&snooze_thread, snooze_update.clone(), true, window, cx)
                        })),
                );
        }
        if workspace_limit {
            actions = actions.child(
                Button::new("recovery-admin-request")
                    .ghost()
                    .xsmall()
                    .label("Copy request for admin")
                    .on_click(cx.listener(|_, _, window, cx| copy_admin_request(window, cx))),
            );
        } else if chatgpt_limit {
            actions = actions.child(super::pages::settings::chatgpt_usage_button("recovery-usage", t).xsmall());
        }
        let icon = if chatgpt_limit { Icon::empty().path("icons/app/openai.svg") } else { Icon::new(IconName::Gauge) };
        Some(composer_banner(
            Variant::Warning,
            icon,
            title.into(),
            Some(description.into()),
            Some(actions.into_any_element()),
            t,
        ))
    }

    /// `thread.metadata.update` with a `limitRecovery` change (`RecoveryActions.toggle`).
    fn update_limit_recovery(
        &mut self,
        thread_id: &str,
        update: Value,
        snooze: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.client() else { return };
        let reset_passed = update["resetAt"]
            .as_str()
            .and_then(parse_ms)
            .is_none_or(|reset| reset <= jiff::Timestamp::now().as_millisecond());
        if snooze && update["snooze"] == true && reset_passed {
            let note = Notification::error("The reset time has passed. Retry the thread manually.")
                .title("Could not snooze thread");
            window.push_notification(note, cx);
            cx.notify();
            return;
        }
        self.banners.recovery_pending = true;
        cx.notify();
        let command = json!({
            "type": "thread.metadata.update",
            "commandId": new_id(),
            "threadId": thread_id,
            "limitRecovery": update,
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("orchestration.dispatchCommand", command).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.banners.recovery_pending = false;
                if let Err(error) = result {
                    let title = if snooze { "Could not snooze thread" } else { "Could not change auto-resume" };
                    window.push_notification(Notification::error(error.to_string()).title(title), cx);
                }
                cx.notify();
            });
        })
        .detach();
    }
}

/// `copyAdminRequest`.
fn copy_admin_request(window: &mut Window, cx: &mut gpui_kit::App) {
    cx.write_to_clipboard(ClipboardItem::new_string(CHATGPT_WORKSPACE_ADMIN_REQUEST.to_owned()));
    let note = Notification::success("Send it to your workspace admin.").title("Request copied");
    window.push_notification(note, cx);
}

/// Border, text and icon colors of an alert variant (`alertVariants`).
fn colors(variant: Variant, t: &Tokens) -> (Hsla, Hsla, Hsla) {
    match variant {
        Variant::Error => (t.error.opacity(0.32), t.error_foreground, t.error),
        Variant::Warning => (t.warning.opacity(0.32), t.warning_foreground, t.warning),
    }
}

/// `Alert surface="glass" controlAlignment="first-line"`, centered with `pt-3`.
fn alert(
    variant: Variant,
    icon: Icon,
    title: Option<SharedString>,
    body: AnyElement,
    actions: AnyElement,
    max_width: gpui_kit::Pixels,
    t: &Tokens,
) -> AnyElement {
    let (border, foreground, tint) = colors(variant, t);
    let background = t.background.opacity(t.glass).blend(tint.opacity(0.04));
    div()
        .pt(px(12.))
        .px(px(16.))
        .max_w(max_width)
        .child(
            h_flex()
                .items_start()
                .gap(px(8.))
                .px(px(14.))
                .py(px(12.))
                .rounded(px(12.))
                .border_1()
                .border_color(border)
                .bg(background)
                .text_sm()
                .text_color(foreground)
                .child(div().h(rems(1.25)).flex().items_center().child(icon.size(px(16.))))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap(px(2.))
                        .children(title.map(|title| div().font_weight(FontWeight::MEDIUM).child(title)))
                        .child(div().text_color(foreground.opacity(0.8)).child(body)),
                )
                .child(h_flex().flex_none().self_start().child(actions)),
        )
        .into_any_element()
}

/// One `ComposerBannerStack` card, tucked under the composer like the fallback notice.
fn composer_banner(
    variant: Variant,
    icon: Icon,
    title: SharedString,
    description: Option<SharedString>,
    actions: Option<AnyElement>,
    t: &Tokens,
) -> AnyElement {
    let (outline, tint) = match variant {
        Variant::Error => (t.error.opacity(0.32), t.error.opacity(0.08)),
        Variant::Warning => (t.warning.opacity(0.28), t.warning.opacity(0.08)),
    };
    div()
        .mx(px(22.))
        .mb(px(-17.))
        .child(
            h_flex()
                .px(px(4.))
                .pt(px(5.))
                // 5px padding plus the 17px the composer covers.
                .pb(px(22.))
                .gap(px(4.))
                .rounded_t(px(16.))
                .border_1()
                .border_color(outline)
                .bg(t.background.blend(t.card.opacity(t.glass)).blend(tint))
                .text_xs()
                .child(
                    div()
                        .size(px(24.))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(icon.size(px(12.)).text_color(t.muted_foreground)),
                )
                .child(
                    h_flex()
                        .flex_1()
                        .min_w_0()
                        .gap(px(4.))
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .line_height(px(24.))
                                .font_weight(FontWeight::MEDIUM)
                                .child(title),
                        )
                        .children(description.map(|description| {
                            div().flex_1().min_w_0().truncate().text_color(t.muted_foreground).child(description)
                        })),
                )
                .children(actions.map(|actions| h_flex().flex_none().gap(px(2.)).child(actions))),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(value: Value) -> Provider {
        let mut base = json!({"instanceId": "codex", "driver": "codex", "status": "ready", "installed": true,
            "auth": {"status": "authenticated"}});
        base.as_object_mut().unwrap().extend(value.as_object().unwrap().clone());
        serde_json::from_value(base).unwrap()
    }

    #[test]
    fn provider_banner_shows_failures_and_incompatible_versions_only() {
        assert_eq!(provider_banner_key(&provider(json!({}))), None);
        assert_eq!(provider_banner_key(&provider(json!({"status": "disabled"}))), None);
        let failing = provider(json!({"status": "error", "message": "boom"}));
        assert_eq!(provider_banner_key(&failing).as_deref(), Some("codex\0error\0authenticated\0boom"));
        let (title, message, variant) = provider_banner(&failing);
        assert_eq!((title.as_str(), message.as_str(), variant), ("Codex provider status", "boom", Variant::Error));

        let unsupported = provider(
            json!({"version": "1.2", "compatibilityAdvisory": {"status": "unsupported", "message": "Update it"}}),
        );
        assert_eq!(provider_banner_key(&unsupported).as_deref(), Some("codex\0unsupported\01.2\0Update it"));
        let (title, message, variant) = provider_banner(&unsupported);
        assert_eq!(
            (title.as_str(), message.as_str(), variant),
            ("Codex 1.2 is unsupported", "Update it", Variant::Warning)
        );

        let signed_out = provider(json!({"status": "error", "auth": {"status": "unauthenticated"},
            "setup": {"canAuthenticate": true, "canInstall": false}, "displayName": "Work"}));
        let (title, message, _) = provider_banner(&signed_out);
        assert_eq!((title.as_str(), message.as_str()), ("Work is unauthenticated", "Open provider setup to sign in."));
        let antigravity =
            provider(json!({"driver": "antigravity", "status": "warning", "auth": {"status": "unknown"}}));
        assert_eq!(provider_banner_key(&antigravity), None);
        let limited = provider(json!({"driver": "claudeAgent", "status": "warning"}));
        assert_eq!(provider_status_message(&limited), "Claude Agent provider has limited availability.");
    }

    #[test]
    fn json_errors_collapse_to_their_message() {
        assert_eq!(thread_error_message(r#"{"error":{"message":"Rate limited"}}"#), "Rate limited");
        assert_eq!(thread_error_message(r#"{"error":{"message":"  "}}"#), r#"{"error":{"message":"  "}}"#);
        assert_eq!(thread_error_message("plain"), "plain");
        assert_eq!(driver_label("claudeAgent"), "Claude Agent");
        assert_eq!(driver_label("open_code-x"), "Open Code X");
    }

    fn shell(extra: Value) -> ThreadShell {
        let mut base = json!({"id": "t", "projectId": "p", "title": "T", "status": "failed", "updatedAt": "2026-01-01T00:00:00Z",
            "lastErrorClass": "usage_limit", "latestRunId": "r", "latestRunCompletedAt": "2026-01-01T10:00:00Z",
            "usageLimitResetAt": "2026-01-01T12:00:00Z"});
        base.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        serde_json::from_value(base).unwrap()
    }

    #[test]
    fn recovery_reads_the_runs_auto_resume_and_snooze() {
        let before_reset = parse_ms("2026-01-01T11:00:00Z").unwrap();
        let state = recovery(&shell(json!({})), before_reset).unwrap();
        assert!(state.can_schedule && !state.scheduled && !state.snoozed && !state.reset_passed);
        let set = shell(json!({"snoozedUntil": "2026-01-01T12:00:00.000Z",
            "limitRecovery": {"runId": "r", "resetAt": "2026-01-01T12:00:00Z", "autoResume": true, "snooze": true}}));
        let state = recovery(&set, before_reset).unwrap();
        assert!(state.scheduled && state.snoozed);
        // Past the reset the snooze has woken, though the server keeps `snoozedUntil`.
        let after = recovery(&set, parse_ms("2026-01-01T13:00:00Z").unwrap()).unwrap();
        assert!(after.scheduled && !after.snoozed && after.reset_passed);
        // Another run's recovery doesn't count, and a reset before the stop can't be scheduled.
        let other =
            shell(json!({"limitRecovery": {"runId": "old", "resetAt": "2026-01-01T12:00:00Z", "autoResume": true}}));
        assert!(!recovery(&other, before_reset).unwrap().scheduled);
        let early = shell(json!({"usageLimitResetAt": "2026-01-01T09:00:00Z"}));
        assert!(!recovery(&early, before_reset).unwrap().can_schedule);
        assert!(recovery(&shell(json!({"lastErrorClass": "provider"})), before_reset).is_none());
    }
}
