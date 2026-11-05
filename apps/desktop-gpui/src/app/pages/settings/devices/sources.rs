//! Device sources (`DeviceSourcesSettings.tsx`): local simulators and every SSH, EAS
//! and Simlock host in settings, with what each offers and how it stands, plus the
//! editors that add and change them.
//!
//! GPUI connects to one environment, so each row has one environment's answer and the
//! web's per-environment breakdowns and partial-availability dots never appear. Statuses
//! come from the workspace's `subscribeDeviceState` stream (`Workspace::devices`).

mod editors;
mod hosts;

use std::collections::HashSet;
use std::time::Duration;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::hover_card::HoverCard;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::{
    Anchor, AnyElement, App, Context, ElementId, Entity, FontWeight, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    prelude::FluentBuilder as _, px, svg,
};
use serde_json::{Value, json};

use self::hosts::{EasHost, SimlockHost, SourceStatus, SshHost, Tone};
use crate::app::Workspace;
use crate::app::pages::settings::widgets::section_with_action;
use crate::device::{DeviceState, PlatformAvailability};
use crate::rpc::{RpcClient, RpcError};
use crate::theme::{Tokens, tokens};

/// This list's own in-flight work, kept while the section renders.
#[derive(Default)]
pub(super) struct DeviceSources {
    /// A settings write from a row is in flight.
    busy: bool,
    /// SSH hosts whose "Test connection" from the row menu is running.
    testing: HashSet<String>,
}

/// A fix offered beside a status line.
#[derive(Clone)]
enum StatusAction {
    Refresh,
    Retry(String),
}

#[derive(Clone)]
enum RowHost {
    Local,
    Ssh(SshHost),
    Eas(EasHost),
    Simlock(SimlockHost),
}

struct SourceRow {
    key: SharedString,
    title: SharedString,
    kind: Option<&'static str>,
    description: SharedString,
    /// One machine or account every environment reaches the same way, so a missing
    /// platform's reason is the source's own. Local simulators differ per machine.
    shared: bool,
    /// `None` while devices are off on the environment: the switch above says so.
    status: Option<SourceStatus>,
    action: Option<StatusAction>,
    platforms: Vec<PlatformAvailability>,
    host: RowHost,
}

impl Workspace {
    pub(in crate::app) fn render_device_sources(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let sources = window.use_keyed_state("device-sources", cx, |_, _| DeviceSources::default());
        let t = *tokens(cx);
        let settings = &self.server_info.settings;
        let enabled = settings.bool("enableDeviceSupport");
        let simlock_enabled = settings.bool("simlockDeviceHostsEnabled");
        let model = sources.read(cx);
        let state = self.devices.state.as_ref();
        let supports_eas = state.is_some_and(|state| state.supports_eas_hosts);
        let supports_simlock = state.is_some_and(|state| state.supports_simlock_hosts) && simlock_enabled;
        let disabled = model.busy || self.client().is_none();
        let rows = source_rows(settings, state, enabled, &model.testing, supports_eas, supports_simlock);
        let view = cx.entity();

        let add = outline_button("device-sources-add")
            .disabled(disabled)
            .child(Icon::new(Lucide::Plus).with_size(px(14.)))
            .child("Add source")
            .child(Icon::new(Lucide::ChevronDown).with_size(px(14.)))
            .dropdown_menu_with_anchor(Anchor::TopRight, {
                let view = view.clone();
                move |menu, window, _| {
                    let mut menu = menu.item(PopupMenuItem::new("Machine over SSH").on_click(
                        window.listener_for(&view, |this, _, window, cx| this.open_ssh_editor(None, window, cx)),
                    ));
                    if supports_eas {
                        menu = menu.item(PopupMenuItem::new("EAS Simulator").on_click(
                            window.listener_for(&view, |this, _, window, cx| this.open_eas_editor(None, window, cx)),
                        ));
                    }
                    if supports_simlock {
                        menu =
                            menu.item(PopupMenuItem::new("Simlock pool (alpha)").on_click(
                                window.listener_for(&view, |this, _, window, cx| {
                                    this.open_simlock_editor(None, window, cx)
                                }),
                            ));
                    }
                    menu
                }
            });

        let rows: Vec<AnyElement> =
            rows.into_iter().map(|row| self.render_source_row(row, &sources, disabled, &t, cx)).collect();
        section_with_action("device-sources", "Device sources", Some(add.into_any_element()), rows, &t)
    }

    fn render_source_row(
        &self,
        row: SourceRow,
        sources: &Entity<DeviceSources>,
        disabled: bool,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let status = row.status.clone();
        let pending = status.as_ref().filter(|status| status.tone == Tone::Pending);
        let problem = status.as_ref().filter(|status| status.tone != Tone::Pending);
        let key = row.key.clone();
        let mut text = v_flex()
            .flex_1()
            .min_w_0()
            .child(
                h_flex()
                    .min_w_0()
                    .gap(px(6.))
                    .child(div().truncate().text_sm().font_weight(FontWeight::MEDIUM).child(row.title.clone()))
                    .when_some(row.kind, |this, kind| {
                        this.child(
                            div()
                                .flex_none()
                                .rounded(px(4.))
                                .border_1()
                                .border_color(t.border)
                                .px(px(4.))
                                .text_size(px(10.))
                                .line_height(px(14.))
                                .text_color(t.muted_foreground)
                                .child(kind.to_uppercase()),
                        )
                    }),
            )
            .child(div().truncate().text_xs().text_color(t.muted_foreground).child(row.description.clone()));
        if let Some(problem) = problem {
            let color = if problem.tone == Tone::Error { t.error } else { t.warning };
            let message: SharedString = problem.detail.clone().unwrap_or_else(|| problem.label.to_owned()).into();
            text = text.child(
                h_flex()
                    .mt(px(4.))
                    .flex_wrap()
                    .gap(px(6.))
                    .text_xs()
                    .text_color(color)
                    .child(div().min_w_0().child(message))
                    .when_some(row.action.clone(), |this, action| {
                        let label = match action {
                            StatusAction::Refresh => "Refresh",
                            StatusAction::Retry(_) => "Retry",
                        };
                        this.child(
                            div()
                                .id(ElementId::Name(format!("device-source-fix-{key}").into()))
                                .cursor_pointer()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(t.foreground)
                                .hover(|style| style.underline())
                                .child(label)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let input = match &action {
                                        StatusAction::Refresh => json!({}),
                                        StatusAction::Retry(host_id) => json!({"retryHostId": host_id}),
                                    };
                                    this.list_device_hosts(input, cx);
                                })),
                        )
                    }),
            );
        }
        let side = match pending {
            Some(pending) => h_flex()
                .flex_none()
                .gap(px(6.))
                .text_xs()
                .text_color(t.muted_foreground)
                .child(Spinner::new().xsmall().color(t.muted_foreground))
                .child(pending.label)
                .into_any_element(),
            None => h_flex().flex_none().gap(px(2.)).children(platform_icons(&row, t)).into_any_element(),
        };
        h_flex()
            .id(ElementId::Name(format!("device-source-{key}").into()))
            .items_center()
            .gap(px(12.))
            .px(px(16.))
            .py(px(12.))
            .child(text)
            .child(side)
            .when_some(self.row_actions(&row, sources, disabled, t, cx), |this, actions| this.child(actions))
            .into_any_element()
    }

    /// Edit, and a menu with Remove (and Test connection for SSH hosts).
    fn row_actions(
        &self,
        row: &SourceRow,
        sources: &Entity<DeviceSources>,
        disabled: bool,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if matches!(row.host, RowHost::Local) {
            return None;
        }
        let key = row.key.clone();
        let host = row.host.clone();
        let edit = outline_button(ElementId::Name(format!("device-source-edit-{key}").into()))
            .label("Edit")
            .disabled(disabled)
            .on_click(cx.listener({
                let host = host.clone();
                move |this, _, window, cx| match &host {
                    RowHost::Ssh(host) => this.open_ssh_editor(Some(host.clone()), window, cx),
                    RowHost::Eas(host) => this.open_eas_editor(Some(host.clone()), window, cx),
                    RowHost::Simlock(host) => this.open_simlock_editor(Some(host.clone()), window, cx),
                    RowHost::Local => {}
                }
            }));
        let view = cx.entity();
        let sources = sources.clone();
        let error = t.error;
        let more = Button::new(ElementId::Name(format!("device-source-more-{key}").into()))
            .ghost()
            .small()
            .size(px(28.))
            .text_color(t.muted_foreground)
            .icon(Icon::new(Lucide::EllipsisVertical).with_size(px(14.)))
            .disabled(disabled)
            .dropdown_menu_with_anchor(Anchor::TopRight, move |mut menu, window, _| {
                if let RowHost::Ssh(ssh) = &host {
                    let ssh = ssh.clone();
                    let sources = sources.clone();
                    menu = menu.item(PopupMenuItem::new("Test connection").on_click(
                        window.listener_for(&view, move |this, _, window, cx| {
                            this.test_ssh_from_row(ssh.clone(), &sources, window, cx)
                        }),
                    ));
                }
                let host = host.clone();
                let sources = sources.clone();
                menu.item(PopupMenuItem::element(move |_, _| div().text_color(error).child("Remove")).on_click(
                    window.listener_for(&view, move |this, _, window, cx| {
                        this.remove_source(host.clone(), &sources, window, cx)
                    }),
                ))
            });
        Some(h_flex().flex_none().gap(px(4.)).child(edit).child(more).into_any_element())
    }

    /// `device.list`: a refresh, or a retry of one host.
    fn list_device_hosts(&mut self, input: Value, cx: &mut Context<Self>) {
        let Some(client) = self.client() else { return };
        cx.spawn(async move |_, _| {
            if let Err(error) = client.request("device.list", input).await {
                log::warn!("device.list failed: {error}");
            }
        })
        .detach();
    }

    fn remove_source(
        &mut self,
        host: RowHost,
        sources: &Entity<DeviceSources>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let settings = &self.server_info.settings;
        let planned = match &host {
            RowHost::Ssh(host) => hosts::host_list(settings, "deviceHosts").and_then(|saved| {
                hosts::update_device_hosts(&saved, host, true, host)
                    .map(|next| (json!({"deviceHosts": next}), "Device host not removed everywhere"))
                    .map_err(str::to_owned)
            }),
            RowHost::Eas(host) => hosts::host_list(settings, "easDeviceHosts").map(|saved| {
                (
                    json!({"easDeviceHosts": hosts::update_eas_hosts(&saved, host, true, false)}),
                    "EAS host not removed everywhere",
                )
            }),
            RowHost::Simlock(host) => hosts::host_list(settings, "simlockDeviceHosts").map(|saved| {
                (
                    json!({"simlockDeviceHosts": hosts::update_simlock_hosts(&saved, host, true)}),
                    "Simlock host not removed everywhere",
                )
            }),
            RowHost::Local => return,
        };
        let (patch, failed) = match planned {
            Ok(planned) => planned,
            Err(error) => return notify_error(window, cx, "Device host not saved", error),
        };
        let Some(client) = self.client() else { return };
        let label = self.environment_name();
        sources.update(cx, |sources, cx| {
            sources.busy = true;
            cx.notify();
        });
        let sources = sources.downgrade();
        cx.spawn_in(window, async move |_, cx| {
            let result = update_settings(&client, patch).await;
            let _ = cx.update(|window, cx| {
                let _ = sources.update(cx, |sources, cx| {
                    sources.busy = false;
                    cx.notify();
                });
                if let Err(error) = result {
                    notify_error(window, cx, failed, format!("{label}: {error}"));
                }
            });
        })
        .detach();
    }

    /// The row menu's "Test connection": a status on the row, then a toast with the answer.
    fn test_ssh_from_row(
        &mut self,
        host: SshHost,
        sources: &Entity<DeviceSources>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.client() else { return };
        let label = self.environment_name();
        let host_id = host.id.clone();
        let started = sources.update(cx, |sources, cx| {
            cx.notify();
            sources.testing.insert(host_id.clone())
        });
        if !started {
            return;
        }
        let sources = sources.downgrade();
        cx.spawn_in(window, async move |_, cx| {
            let result = test_host(&client, &host).await;
            let _ = cx.update(|window, cx| {
                let _ = sources.update(cx, |sources, cx| {
                    sources.testing.remove(&host_id);
                    cx.notify();
                });
                let note = match result {
                    Err(error) => Notification::error(format!("{label}: {error}"))
                        .title(format!("{}: 1 of 1 environments failed", host.label)),
                    Ok(_) => {
                        Notification::success("Connected or already available locally on each selected environment.")
                            .title(format!("{}: connection checks passed", host.label))
                    }
                };
                window.push_notification(note, cx);
            });
        })
        .detach();
    }

    /// The environment's name, as the web labels its rows and toasts.
    fn environment_name(&self) -> String {
        self.server_info.environment_label.clone().unwrap_or_else(|| "This environment".into())
    }
}

fn source_rows(
    settings: &Value,
    state: Option<&DeviceState>,
    enabled: bool,
    testing: &HashSet<String>,
    supports_eas: bool,
    supports_simlock: bool,
) -> Vec<SourceRow> {
    let platforms = |id: &str| {
        if enabled {
            state.and_then(|state| state.platforms(id)).map(<[_]>::to_vec).unwrap_or_default()
        } else {
            Vec::new()
        }
    };
    let status = |id: &str| enabled.then(|| hosts::host_status(state, id)).flatten();
    let local = state.and_then(DeviceState::local_host);
    let local_status = status(local.map_or("", |local| local.id.as_str()));
    let mut rows = vec![SourceRow {
        key: "local".into(),
        title: "Local simulators".into(),
        kind: None,
        description: "Simulators and emulators installed on this environment's machine.".into(),
        shared: false,
        action: local_status.as_ref().filter(|status| status.tone == Tone::Error).map(|_| StatusAction::Refresh),
        status: local_status,
        platforms: local.map(|local| platforms(&local.id)).unwrap_or_default(),
        host: RowHost::Local,
    }];
    let retry = state.is_some_and(|state| state.supports_host_retry);
    for host in hosts::readable_hosts::<SshHost>(settings, "deviceHosts") {
        let mut status = if testing.contains(&host.id) && enabled {
            Some(SourceStatus { tone: Tone::Pending, label: "Testing connection", detail: None })
        } else {
            status(&host.id)
        };
        let mut action = None;
        if let Some(failed) = status.as_mut().filter(|status| status.tone == Tone::Error && retry) {
            failed.label = "Can't connect";
            action = Some(StatusAction::Retry(host.id.clone()));
        }
        rows.push(SourceRow {
            key: format!("ssh:{}", host.id).into(),
            title: host.label.clone().into(),
            kind: Some("SSH"),
            description: host.target.clone().into(),
            shared: true,
            status,
            action,
            platforms: platforms(&host.id),
            host: RowHost::Ssh(host),
        });
    }
    if supports_eas {
        for host in hosts::readable_hosts::<EasHost>(settings, "easDeviceHosts") {
            rows.push(SourceRow {
                key: format!("eas:{}", host.id).into(),
                title: host.label.clone().into(),
                kind: Some("EAS"),
                description: hosts::eas_session_limits(&host).into(),
                shared: true,
                status: status(&host.id),
                action: None,
                platforms: platforms(&host.id),
                host: RowHost::Eas(host),
            });
        }
    }
    if supports_simlock {
        for host in hosts::readable_hosts::<SimlockHost>(settings, "simlockDeviceHosts") {
            let origin = host.gateway_url.trim_start_matches("https://").trim_start_matches("http://");
            rows.push(SourceRow {
                key: format!("simlock:{}", host.id).into(),
                title: host.label.clone().into(),
                kind: Some("Simlock"),
                description: format!("{origin} · {}", hosts::simlock_lease_limits(&host)).into(),
                shared: true,
                // A blank token can still resolve to the machine's own, so the host's answer decides.
                status: status(&host.id),
                action: None,
                platforms: platforms(&host.id),
                host: RowHost::Simlock(host),
            });
        }
    }
    rows
}

const PLATFORMS: [(&str, &str, &str); 2] =
    [("ios", "iOS", "icons/app/apple.svg"), ("android", "Android", "icons/app/android.svg")];

/// `SourcePlatforms`: one icon per platform the source reported, solid where available.
fn platform_icons(row: &SourceRow, t: &Tokens) -> Vec<AnyElement> {
    PLATFORMS
        .iter()
        .filter_map(|&(platform, label, icon)| {
            let entry = row.platforms.iter().find(|entry| entry.platform == platform)?.clone();
            let available = entry.available;
            let title: SharedString =
                format!("{label} {}", if available { "available" } else { "not available" }).into();
            // A shared source's reason is its own; a machine without a reason is not set up.
            let reason: Option<SharedString> = match (available, row.shared) {
                (true, _) => None,
                (false, true) => entry.reason.map(Into::into),
                (false, false) => Some(entry.reason.unwrap_or_else(|| "Not set up on this machine.".into()).into()),
            };
            let (foreground, muted) = (t.foreground, t.muted_foreground);
            let trigger = div()
                .id(ElementId::Name(format!("device-platform-{}-{platform}", row.key).into()))
                .size(px(24.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(6.))
                .hover(|style| style.bg(t.accent))
                .child(
                    svg()
                        .path(icon)
                        .size(px(14.))
                        .when(platform == "ios", |this| this.mt(px(-1.)))
                        .text_color(if available { foreground } else { muted.opacity(0.45) }),
                );
            Some(
                HoverCard::new(ElementId::Name(format!("device-platform-card-{}-{platform}", row.key).into()))
                    .anchor(Anchor::BottomRight)
                    .open_delay(Duration::from_millis(150))
                    .close_delay(Duration::from_millis(100))
                    .trigger(trigger)
                    .content(move |_, _, _| {
                        v_flex()
                            .w(px(256.))
                            .gap(px(4.))
                            .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(title.clone()))
                            .when_some(reason.clone(), |this, reason| {
                                this.child(div().text_xs().text_color(muted).child(reason))
                            })
                    })
                    .into_any_element(),
            )
        })
        .collect()
}

/// The web's `failureMessage`: the error's `message`, which for the device errors is a
/// getter in `packages/contracts/src/device.ts` and so is rebuilt here from its fields.
fn failure_message(error: &RpcError) -> String {
    let RpcError::Failure(failure) = error else { return error.to_string() };
    let text = |key: &str| failure.get(key).and_then(Value::as_str).unwrap_or_default();
    if let Some(message) = failure.get("message").and_then(Value::as_str) {
        return message.to_owned();
    }
    let reason = text("reason");
    match text("_tag") {
        "DeviceHostUnavailableError" => {
            let host = failure
                .get("hostLabel")
                .and_then(Value::as_str)
                .map_or_else(|| format!("Device host {}", text("hostId")), str::to_owned);
            format!("{host} is unavailable: {reason}")
        }
        "DevicePlatformUnavailableError" => {
            format!("{} devices are unavailable on host {}: {reason}", text("platform"), text("hostId"))
        }
        "DeviceNotFoundError" => format!("Device {} was not found on host {}.", text("deviceId"), text("hostId")),
        "DeviceOperationError" => {
            let explanation = match reason {
                "command_failed" => match failure.get("exitCode").and_then(Value::as_i64) {
                    Some(code) => format!("The device command failed (exit code {code})."),
                    None => "The device command failed.".to_owned(),
                },
                "request_failed" => "Could not communicate with device support. Try refreshing devices.".to_owned(),
                "invalid_payload" => "The device request could not be encoded.".to_owned(),
                "settings_failed" => "Could not read or save device settings.".to_owned(),
                _ => "The device hub could not complete the request.".to_owned(),
            };
            format!("Device {} failed: {explanation}", text("operation"))
        }
        _ if !reason.is_empty() => reason.to_owned(),
        _ => error.to_string(),
    }
}

/// `server.updateSettings` with one patch.
async fn update_settings(client: &RpcClient, patch: Value) -> Result<(), String> {
    client.request("server.updateSettings", json!({"patch": patch})).await.map(drop).map_err(|e| failure_message(&e))
}

/// `device.testHost`; a host on this same machine answers `kind: "local"`.
async fn test_host(client: &RpcClient, host: &SshHost) -> Result<Value, String> {
    let payload = serde_json::to_value(host).map_err(|error| error.to_string())?;
    client.request("device.testHost", payload).await.map_err(|e| failure_message(&e))
}

fn notify_error(window: &mut Window, cx: &mut App, title: &'static str, message: String) {
    window.push_notification(Notification::error(message).title(title), cx);
}

/// A web `outline` `sm` button.
fn outline_button(id: impl Into<ElementId>) -> Button {
    Button::new(id).outline().small().h(px(28.)).px(px(9.)).gap(px(6.)).text_sm().font_weight(FontWeight::MEDIUM)
}

fn muted_text(text: impl Into<SharedString>, color: Hsla) -> gpui_kit::Div {
    div().text_xs().text_color(color).child(text.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_messages_read_like_the_errors_own() {
        let message = |error: Value| failure_message(&RpcError::Failure(error));
        assert_eq!(
            message(json!({"_tag": "DeviceOperationError", "operation": "test host", "reason": "request_failed"})),
            "Device test host failed: Could not communicate with device support. Try refreshing devices."
        );
        assert_eq!(
            message(json!({"_tag": "DeviceHostUnavailableError", "hostId": "mac", "reason": "ssh: timeout"})),
            "Device host mac is unavailable: ssh: timeout"
        );
        assert_eq!(
            message(json!({"_tag": "DeviceEasAccountError", "reason": "No Expo credential"})),
            "No Expo credential"
        );
        assert_eq!(message(json!({"_tag": "EnvironmentAuthorizationError", "message": "Forbidden"})), "Forbidden");
    }
}
