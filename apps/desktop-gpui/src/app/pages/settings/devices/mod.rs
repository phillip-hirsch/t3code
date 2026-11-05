//! Settings → Devices (`DevicesSettingsPanel` → `DeviceIntegrationSettings` in
//! `apps/web/src/components/settings/IntegrationsSettings.tsx`, with `DeviceToolVersions` and
//! `DeviceHostUpdates` from `apps/web/src/components/device/`). Device sources are in `sources.rs`.
//!
//! The web runs each device change on every selected environment; GPUI has one. Scoped in the web:
//! "Agents can use devices" (`enableAgentDeviceAccess`) has a project override, written as a plain
//! setting there; "Use devices" (`enableDeviceSupport`) is environment-only. Without a scope here,
//! both go through `device.configure` like the web's environment scope.

mod agent_access;
mod sources;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::popover::Popover;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{Disableable as _, Icon, IconName, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, Div, ElementId, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, Styled as _, Task, Window, div, px,
};
use serde_json::{Value, json};

use crate::app::Workspace;
use crate::app::pages::settings::widgets::{RowFrame, settings_section};
use crate::device::{DeviceState, Pending, SetupStatus, Tool, ToolVersions, tool_version_label};
use crate::rpc::RpcError;
use crate::theme::{self, Tokens};

impl Workspace {
    /// Streams `subscribeDeviceState` for this connection; the task ends with the socket.
    pub(in crate::app) fn subscribe_device_state(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Task<()> {
        self.devices.state = None;
        let Some(client) = self.client() else { return Task::ready(()) };
        let stream = client.stream("subscribeDeviceState", json!({}));
        Self::pump(
            stream,
            window,
            cx,
            |this, value| match DeviceState::decode(value) {
                Ok(state) => this.devices.state = Some(state),
                Err(error) => log::warn!("bad device state: {error}"),
            },
            |_, _, _| {},
        )
    }

    pub(super) fn render_devices_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let view = self.settings_view();
        let sections = [self.render_device_integration(cx), self.render_device_sources(window, cx)];
        self.settings_page("devices", &view, sections, cx)
    }

    /// `DeviceIntegrationControls`: the hub and agent access switches, and the local host's progress.
    fn render_device_integration(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = *theme::tokens(cx);
        let empty = DeviceState::default();
        let state = self.devices.state.as_ref().unwrap_or(&empty);
        let connected = self.client().is_some();
        let loaded = connected && self.devices.state.is_some();
        let setting = |key: &str| self.server_info.settings.bool(key);
        let (enabled, agent_enabled) = (setting("enableDeviceSupport"), setting("enableAgentDeviceAccess"));
        let busy = state.readiness().busy;
        let pending = self.devices.pending;

        let hub = device_row(
            "device-hub",
            "Use devices",
            description("Open simulators and emulators in the Device panel. Hub:", &t)
                .child(self.render_tool_versions(state, Tool::Hub, loaded && !busy, cx)),
            h_flex()
                .gap(px(8.))
                .when(pending == Some(Pending::Hub), |this| {
                    this.children(state.hub_status(true, true).map(|status| setup_status(status, &t)))
                })
                .child(
                    Switch::new("device-hub-switch")
                        .small()
                        .checked(enabled)
                        .accessibility_label("Device hub")
                        .disabled(!loaded || busy || pending.is_some())
                        .on_click(cx.listener(|this, checked: &bool, window, cx| {
                            let mut input = json!({ "enabled": *checked });
                            if *checked {
                                input["onboardingCompleted"] = true.into();
                            } else {
                                input["agentAccessEnabled"] = false.into();
                            }
                            this.configure_devices(Pending::Hub, input, window, cx);
                        })),
                ),
            &t,
        );
        let agent = device_row(
            "agent-device-access",
            "Agents can use devices",
            description("New agent sessions can open and drive these devices. agent-device:", &t)
                .child(self.render_tool_versions(state, Tool::Agent, loaded && !busy, cx)),
            h_flex()
                .gap(px(8.))
                .when(pending == Some(Pending::Agent), |this| {
                    this.children(state.agent_status(true, true).map(|status| setup_status(status, &t)))
                })
                .child(
                    Switch::new("agent-device-access-switch")
                        .small()
                        .checked(agent_enabled)
                        .accessibility_label("Agent device access")
                        .disabled(!loaded || !enabled || busy || pending.is_some())
                        .on_click(cx.listener(|this, checked: &bool, window, cx| {
                            this.configure_devices(
                                Pending::Agent,
                                json!({ "agentAccessEnabled": *checked }),
                                window,
                                cx,
                            );
                        })),
                ),
            &t,
        );
        let mut rows = vec![hub, agent];
        if connected {
            rows.extend(self.render_host_updates(state, cx));
        }
        settings_section("devices", "Devices", rows, &t)
    }

    /// `DeviceToolVersions` for one tool: the inline version, with details and actions in a popover.
    fn render_tool_versions(
        &self,
        state: &DeviceState,
        tool: Tool,
        actionable: bool,
        cx: &mut Context<Self>,
    ) -> Popover {
        let t = *theme::tokens(cx);
        let tools: Option<ToolVersions> = state.local_host().and_then(|host| host.tools.clone());
        let label = tool_version_label(tools.as_ref(), tool);
        let update_to = tools
            .as_ref()
            .map(|tools| tools.get(tool))
            .filter(|version| state.supports_tool_update && version.needs_update())
            .map(|version| version.required_version.clone());
        let can_check = state.supports_tool_inspection;
        let pending = self.devices.pending;
        let error = self.devices.update_error.as_ref().filter(|(failed, _)| *failed == tool).map(|(_, e)| e.clone());
        let disabled = !actionable || pending.is_some();
        let workspace = cx.entity().downgrade();
        Popover::new(ElementId::Name(format!("device-tool-versions-{}", tool.wire()).into()))
            .anchor(gpui_kit::Anchor::TopRight)
            .w(px(320.))
            .p(px(16.))
            .trigger(
                Button::new(ElementId::Name(format!("device-tool-versions-trigger-{}", tool.wire()).into()))
                    .text()
                    .xsmall()
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(t.muted_foreground)
                    .label(label),
            )
            .content(move |_, _, cx| {
                let t = *theme::tokens(cx);
                let rows = tools.as_ref().map(|tools| {
                    let version = tools.get(tool);
                    let installed = if version.installed_versions.is_empty() {
                        "None".to_owned()
                    } else {
                        version.installed_versions.join(", ")
                    };
                    v_flex().mt(px(16.)).gap(px(4.)).text_xs().children([
                        version_row("Running", version.running_version.clone().unwrap_or("Not running".into()), &t),
                        version_row("Required", version.required_version.clone(), &t),
                        version_row("Installed", installed, &t),
                    ])
                });
                let workspace = workspace.clone();
                let update_button = update_to.clone().map(|required| {
                    let workspace = workspace.clone();
                    Button::new("device-tool-update")
                        .primary()
                        .small()
                        .h(px(28.))
                        .px(px(9.))
                        .text_sm()
                        .disabled(disabled)
                        .label(if pending == Some(Pending::Update(tool)) {
                            "Updating…".to_owned()
                        } else {
                            format!("Update to v{required}")
                        })
                        .on_click(move |_, window, cx| {
                            let _ = workspace.update(cx, |this, cx| {
                                this.devices.update_error = None;
                                let input = json!({ "updateTool": tool.wire() });
                                this.list_devices(Pending::Update(tool), input, window, cx);
                            });
                        })
                });
                let check_button = can_check.then(|| {
                    let workspace = workspace.clone();
                    Button::new("device-tool-check")
                        .outline()
                        .small()
                        .h(px(28.))
                        .px(px(9.))
                        .text_sm()
                        .disabled(disabled)
                        .label(if pending == Some(Pending::Check) { "Checking…" } else { "Check versions" })
                        .on_click(move |_, window, cx| {
                            let _ = workspace.update(cx, |this, cx| {
                                this.list_devices(Pending::Check, json!({ "inspectOnly": true }), window, cx)
                            });
                        })
                });
                v_flex()
                    .text_color(t.foreground)
                    .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).line_height(px(14.)).child(tool.label()))
                    .map(|this| match rows {
                        Some(rows) => this.child(rows),
                        None => this.child(
                            div()
                                .mt(px(12.))
                                .text_xs()
                                .text_color(t.muted_foreground)
                                .child("Versions have not been checked."),
                        ),
                    })
                    .child(
                        div()
                            .mt(px(16.))
                            .pt(px(12.))
                            .border_t_1()
                            .border_color(t.border.opacity(0.5))
                            .text_xs()
                            .text_color(t.muted_foreground)
                            .child("Tools update automatically on this host when needed."),
                    )
                    .when(update_button.is_some() || check_button.is_some(), |this| {
                        this.child(
                            v_flex()
                                .mt(px(12.))
                                .gap(px(8.))
                                .child(h_flex().flex_wrap().gap(px(8.)).children(update_button).children(check_button))
                                .children(error.clone().map(|error| div().text_xs().text_color(t.error).child(error))),
                        )
                    })
            })
    }

    /// `DeviceHostUpdates` for this machine: install and start progress, and failures with a retry.
    fn render_host_updates(&self, state: &DeviceState, cx: &mut Context<Self>) -> Option<AnyElement> {
        let t = *theme::tokens(cx);
        let updates = state.host_updates(state.hosts.iter().filter(|host| host.kind == "local"));
        if updates.is_empty() {
            return None;
        }
        let retrying = self.devices.retrying.as_deref();
        Some(
            v_flex()
                .gap(px(8.))
                .children(updates.into_iter().map(|update| {
                    let host_id = update.host.id.clone();
                    h_flex()
                        .items_start()
                        .gap(px(12.))
                        .rounded(px(6.))
                        .border_1()
                        .border_color(t.border.opacity(0.6))
                        .px(px(12.))
                        .py(px(8.))
                        .text_xs()
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(div().font_weight(FontWeight::MEDIUM).child(update.host.label.clone()))
                                .child(div().text_color(t.muted_foreground).child(update.detail.to_owned()))
                                .when(update.failed, |this| {
                                    this.child(div().mt(px(4.)).text_color(t.muted_foreground).child(
                                        "Check the host connection and network access, then retry. Your device \
                                         settings are saved.",
                                    ))
                                }),
                        )
                        .when(update.failed && state.supports_host_retry, |this| {
                            let label = if retrying == Some(host_id.as_str()) { "Retrying…" } else { "Retry" };
                            this.child(
                                Button::new(ElementId::Name(format!("device-host-retry-{host_id}").into()))
                                    .outline()
                                    .xsmall()
                                    .h(px(28.))
                                    .label(label)
                                    .disabled(retrying.is_some())
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.retry_device_host(host_id.clone(), window, cx)
                                    })),
                            )
                        })
                }))
                .into_any_element(),
        )
    }

    /// `device.configure` with the hub or agent switch's change; the state stream reports the result.
    fn configure_devices(&mut self, kind: Pending, input: Value, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.client() else { return };
        self.devices.pending = Some(kind);
        let label = self.server_info.environment_label.clone().unwrap_or_else(|| "this environment".into());
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("device.configure", input).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.devices.pending = None;
                if let Err(error) = result {
                    log::warn!("device.configure failed: {error}");
                    let note = Notification::error(format!("Could not update {label}."))
                        .title("Device settings not saved on all environments");
                    window.push_notification(note, cx);
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// `device.list` to check versions (`inspectOnly`) or install a tool (`updateTool`).
    fn list_devices(&mut self, kind: Pending, input: Value, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.client() else { return };
        self.devices.pending = Some(kind);
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("device.list", input).await;
            let _ = this.update(cx, |this, cx| {
                this.devices.pending = None;
                if let (Err(error), Pending::Update(tool)) = (result, kind) {
                    this.devices.update_error = Some((tool, error_message(&error)));
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// `device.list { retryHostId }` for a host that failed to start.
    fn retry_device_host(&mut self, host_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.client() else { return };
        self.devices.retrying = Some(host_id.clone());
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("device.list", json!({ "retryHostId": host_id })).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.devices.retrying = None;
                if let Err(error) = result {
                    window.push_notification(Notification::error(error_message(&error)), cx);
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

/// `formatEnvironmentQueryError`: the failure's message, else a generic line.
fn error_message(error: &RpcError) -> String {
    match error {
        RpcError::Failure(value) => value.get("message").and_then(Value::as_str).map(str::trim),
        _ => None,
    }
    .filter(|message| !message.is_empty())
    .map_or_else(|| "The environment request failed.".to_owned(), str::to_owned)
}

fn device_row(
    id: &'static str,
    title: &'static str,
    description: Div,
    control: impl IntoElement,
    t: &Tokens,
) -> AnyElement {
    RowFrame::new(title).description(description).control(control).render(t).id(id).into_any_element()
}

/// A row description, `text-xs` muted; inline controls are appended after the text.
fn description(text: &'static str, t: &Tokens) -> Div {
    h_flex().flex_wrap().max_w(px(672.)).gap(px(3.)).text_xs().text_color(t.muted_foreground).child(text)
}

fn setup_status(status: SetupStatus, t: &Tokens) -> AnyElement {
    h_flex()
        .gap(px(8.))
        .text_xs()
        .text_color(t.muted_foreground)
        .child(if status.pending {
            Spinner::new().xsmall().color(t.muted_foreground).into_any_element()
        } else {
            Icon::new(IconName::Check).with_size(px(12.)).text_color(t.success_foreground).into_any_element()
        })
        .child(status.label)
        .into_any_element()
}

fn version_row(label: &'static str, value: String, t: &Tokens) -> Div {
    h_flex()
        .justify_between()
        .gap(px(24.))
        .child(div().text_color(t.muted_foreground).child(label))
        .child(div().font_family(t.fonts.mono).text_right().child(SharedString::from(value)))
}
