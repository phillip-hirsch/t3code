//! "OpenTelemetry export" (`TelemetryExportSettings.tsx`): the OTLP endpoints in
//! `settings.observability`, what the running server uses, and endpoint checks.

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, ElementId, Entity, FontWeight, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, px,
};
use serde_json::{Map, Value, json};

use super::super::connections::logic::SETTINGS_WRITE;
use super::super::connections::ui::{Variant, WebButton};
use super::super::widgets::{SettingsView, section_with_action};
use super::{DiagnosticsState, rpc_message, success};
use crate::app::Workspace;
use crate::rpc::RpcClient;
use crate::settings::scope::ScopeKind;
use crate::theme::{self, Tokens};

/// `SIGNALS`: settings key, running source/variable keys, label, OTLP signal.
const SIGNALS: [(&str, &str, &str, &str, &str); 3] = [
    ("otlpTracesUrl", "otlpTracesSource", "otlpTracesVariable", "Traces", "traces"),
    ("otlpMetricsUrl", "otlpMetricsSource", "otlpMetricsVariable", "Metrics", "metrics"),
    ("otlpLogsUrl", "otlpLogsSource", "otlpLogsVariable", "Logs", "logs"),
];

/// An endpoint check: the URL sent and, once answered, the `OtlpEndpointCheckResult`.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Check {
    url: String,
    result: Option<Value>,
}

#[derive(Default)]
pub(super) struct ExportState {
    checks: [Option<Check>; 3],
    checks_started: bool,
    saving: bool,
    message: Option<&'static str>,
}

/// `runningOverride`: an endpoint the environment or desktop set, which settings cannot change.
fn running_override(running: &Value, index: usize) -> Option<(String, &'static str, Option<String>)> {
    let (key, source, variable, ..) = SIGNALS[index];
    let url = running.get(key)?.as_str()?.to_owned();
    let label = match running[source].as_str()? {
        "environment" => "Set by environment",
        "bootstrap" => "Set by desktop",
        _ => return None,
    };
    Some((url, label, running[variable].as_str().map(str::to_owned)))
}

fn is_http_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    ["http://", "https://"].iter().any(|scheme| lower.strip_prefix(scheme).is_some_and(|rest| !rest.is_empty()))
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Dot {
    Muted,
    Warning,
    Success,
    Error,
}

/// `endpointStatus`.
fn endpoint_status(url: &str, check: Option<&Check>) -> (String, Dot) {
    if url.is_empty() {
        return ("Off".into(), Dot::Muted);
    }
    let Some(check) = check.filter(|check| check.url == url) else { return ("Not checked".into(), Dot::Muted) };
    let Some(result) = &check.result else { return ("Checking\u{2026}".into(), Dot::Warning) };
    match result["_tag"].as_str() {
        Some("Accepted") => {
            (format!("Connected \u{b7} {} ms", result["latencyMs"].as_f64().unwrap_or(0.).round()), Dot::Success)
        }
        Some("Rejected") => (format!("Rejected \u{b7} HTTP {}", result["status"].as_f64().unwrap_or(0.)), Dot::Error),
        _ if result["timedOut"].as_bool() == Some(true) => ("Timed out".into(), Dot::Error),
        _ => ("Unreachable".into(), Dot::Error),
    }
}

/// The three URL inputs, kept while the page is open.
struct Inputs {
    inputs: [Entity<InputState>; 3],
    saved: [String; 3],
    _subscriptions: Vec<Subscription>,
}

impl DiagnosticsState {
    fn check_endpoint(&mut self, client: RpcClient, index: usize, url: String, cx: &mut Context<Self>) {
        if !is_http_url(&url) {
            return;
        }
        self.export.checks[index] = Some(Check { url: url.clone(), result: None });
        let signal = SIGNALS[index].4;
        cx.spawn(async move |this, cx| {
            let response = client.request("server.checkOtlpEndpoint", json!({ "signal": signal, "url": url })).await;
            let _ = this.update(cx, |this, cx| {
                let slot = &mut this.export.checks[index];
                if slot.as_ref().is_none_or(|check| check.url != url || check.result.is_some()) {
                    return;
                }
                *slot = response.ok().map(|result| Check { url, result: Some(result) });
                cx.notify();
            });
        })
        .detach();
    }
}

impl Workspace {
    pub(super) fn telemetry_export_section(
        &mut self,
        state: &Entity<DiagnosticsState>,
        view: &SettingsView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = *theme::tokens(cx);
        if view.scope.kind != ScopeKind::Environment || self.client().is_none() {
            return self.export_scope_notice(view, &t, cx);
        }
        let client = self.client().expect("checked above");
        let saved_values = &self.server_info.settings.get("observability").clone();
        let saved: [String; 3] =
            SIGNALS.map(|(key, ..)| saved_values[key].as_str().unwrap_or_default().trim().to_owned());
        let running = self.server_info.config.get("observability").cloned().unwrap_or_default();
        let workspace = cx.weak_entity();
        let inputs = window.use_keyed_state("otlp-inputs", cx, |window, cx| {
            let inputs = std::array::from_fn(|index| {
                let (signal, value) = (SIGNALS[index].4, saved[index].clone());
                cx.new(|cx| {
                    InputState::new(window, cx)
                        .placeholder(format!("http://localhost:4318/v1/{signal}"))
                        .default_value(value)
                })
            });
            let subscriptions = inputs
                .iter()
                .map(|input| {
                    let workspace = workspace.clone();
                    cx.subscribe(input, move |_, _, event: &InputEvent, cx| {
                        if matches!(event, InputEvent::Change) {
                            let _ = workspace.update(cx, |_, cx| cx.notify());
                        }
                    })
                })
                .collect();
            Inputs { inputs, saved: saved.clone(), _subscriptions: subscriptions }
        });
        // Another client's save replaces the fields this one has not edited.
        inputs.update(cx, |inputs, cx| {
            for index in 0..3 {
                if inputs.saved[index] != saved[index] {
                    let input = inputs.inputs[index].clone();
                    if input.read(cx).value().trim() == inputs.saved[index] {
                        let value = saved[index].clone();
                        input.update(cx, |input, cx| input.set_value(value, window, cx));
                    }
                    inputs.saved[index] = saved[index].clone();
                }
            }
        });
        let fields: [Entity<InputState>; 3] = inputs.read(cx).inputs.clone();
        let values: [String; 3] = std::array::from_fn(|i| fields[i].read(cx).value().trim().to_owned());

        let s = state.read(cx);
        let can_edit = s.grants(SETTINGS_WRITE);
        let can_check = can_edit && self.server_info.capability("otlpEndpointCheck");
        if can_check && !s.export.checks_started {
            let initial: Vec<(usize, String)> = (0..3)
                .filter_map(|i| {
                    let url = running_override(&running, i).map_or_else(|| saved[i].clone(), |(url, ..)| url);
                    is_http_url(&url).then_some((i, url))
                })
                .collect();
            let client = client.clone();
            state.update(cx, |s, cx| {
                s.export.checks_started = true;
                for (index, url) in initial {
                    s.check_endpoint(client.clone(), index, url, cx);
                }
            });
        }
        let s = state.read(cx);
        let changed: Vec<usize> = (0..3).filter(|&i| values[i] != saved[i]).collect();
        let invalid = changed.iter().any(|&i| !values[i].is_empty() && !is_http_url(&values[i]));
        let saving = s.export.saving;

        let label = self.server_info.environment_label.clone().unwrap_or_else(|| "the selected environment".into());
        let mut body = vec![
            div()
                .text_xs()
                .text_color(t.muted_foreground)
                .child("Send traces, metrics, and logs to an OTLP HTTP receiver. Restart the server to apply. Environment variables override these settings.")
                .into_any_element(),
        ];
        if !can_edit {
            body.push(
                div()
                    .text_xs()
                    .text_color(t.muted_foreground)
                    .child(format!("This connection lacks permission to change settings on {label}."))
                    .into_any_element(),
            );
        }
        for (index, (key, _, _, title, _)) in SIGNALS.iter().enumerate() {
            let over = running_override(&running, index);
            let url = over.as_ref().map_or_else(|| values[index].clone(), |(url, ..)| url.clone());
            let check = can_check.then(|| endpoint_status(&url, s.export.checks[index].as_ref()));
            let status = match (&over, check) {
                (Some((_, label, _)), check) => Some((
                    check.as_ref().map_or_else(|| (*label).to_owned(), |(text, _)| format!("{label} \u{b7} {text}")),
                    check.map_or(Dot::Muted, |(_, dot)| dot),
                )),
                (None, check) => check,
            };
            let dot_color = |dot: Dot| -> Hsla {
                match dot {
                    Dot::Muted => t.muted_foreground.opacity(0.4),
                    Dot::Warning => t.warning,
                    Dot::Success => success(),
                    Dot::Error => t.error,
                }
            };
            let test = can_check.then(|| {
                let pending = s.export.checks[index].as_ref().is_some_and(|c| c.result.is_none());
                let (state, client, url) = (state.clone(), client.clone(), url.clone());
                WebButton::new(ElementId::Name(format!("otlp-test-{key}").into()), "Test")
                    .disabled(!is_http_url(&url) || pending)
                    .on_click(move |_, _, cx| {
                        let url = url.clone();
                        state.update(cx, |s, cx| {
                            s.check_endpoint(client.clone(), index, url, cx);
                            cx.notify();
                        })
                    })
            });
            let running_url = running[*key].as_str().unwrap_or_default();
            let variable = over.as_ref().and_then(|(_, _, variable)| variable.clone());
            let running_line = (saved[index] != running_url || variable.is_some()).then(|| {
                h_flex()
                    .flex_wrap()
                    .gap(px(4.))
                    .text_xs()
                    .text_color(t.muted_foreground)
                    .child(format!("Running: {}", if running_url.is_empty() { "Disabled" } else { running_url }))
                    .when_some(variable, |el, variable| {
                        el.child("from").child(div().font_family(t.fonts.mono).child(variable))
                    })
            });
            body.push(
                v_flex()
                    .gap(px(6.))
                    .child(
                        h_flex()
                            .justify_between()
                            .gap(px(12.))
                            .child(
                                h_flex()
                                    .gap(px(6.))
                                    .when_some(status.as_ref(), |el, (_, dot)| {
                                        el.child(div().size(px(8.)).rounded_full().bg(dot_color(*dot)))
                                    })
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(FontWeight::MEDIUM)
                                            .child(format!("{title} endpoint")),
                                    ),
                            )
                            .when_some(status.map(|(text, _)| text), |el, text| {
                                el.child(div().text_xs().text_color(t.muted_foreground).child(text))
                            }),
                    )
                    .child(
                        h_flex()
                            .gap(px(8.))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .child(Input::new(&fields[index]).small().disabled(saving || !can_edit)),
                            )
                            .children(test),
                    )
                    .children(running_line)
                    .into_any_element(),
            );
        }
        let discard = {
            let (state, client, fields, saved, running) =
                (state.clone(), client.clone(), fields.clone(), saved.clone(), running.clone());
            WebButton::new("otlp-discard", "Discard").disabled(changed.is_empty() || saving || !can_edit).on_click(
                move |_, window, cx| {
                    for (input, value) in fields.iter().zip(saved.iter()) {
                        let value = value.clone();
                        input.update(cx, |input, cx| input.set_value(value, window, cx));
                    }
                    let (client, saved, running) = (client.clone(), saved.clone(), running.clone());
                    state.update(cx, |s, cx| {
                        s.export.message = None;
                        if can_check {
                            for index in 0..3 {
                                let stale = s.export.checks[index].as_ref().is_none_or(|c| c.url != saved[index]);
                                if running_override(&running, index).is_none() && stale {
                                    s.check_endpoint(client.clone(), index, saved[index].clone(), cx);
                                }
                            }
                        }
                        cx.notify();
                    });
                },
            )
        };
        let save = {
            let (state, client) = (state.clone(), client.clone());
            let patch: Map<String, Value> =
                changed.iter().map(|&i| (SIGNALS[i].0.to_owned(), Value::String(values[i].clone()))).collect();
            WebButton::new("otlp-save", if saving { "Saving\u{2026}" } else { "Save" })
                .variant(Variant::Primary)
                .disabled(changed.is_empty() || saving || !can_edit || invalid)
                .on_click(move |_, window, cx| {
                    let (client, patch) = (client.clone(), patch.clone());
                    state.update(cx, |s, cx| {
                        s.export.saving = true;
                        s.export.message = None;
                        cx.spawn_in(window, async move |this, cx| {
                            let input = json!({ "patch": { "observability": patch } });
                            let result = client.request("server.updateSettings", input).await;
                            let _ = this.update_in(cx, |s, window, cx| {
                                s.export.saving = false;
                                match result {
                                    Ok(_) => s.export.message = Some("Saved. Restart the server to apply."),
                                    Err(error) => window.push_notification(
                                        gpui_kit::component::notification::Notification::error(rpc_message(&error))
                                            .title("Could not save settings"),
                                        cx,
                                    ),
                                }
                                cx.notify();
                            });
                        })
                        .detach();
                        cx.notify();
                    })
                })
        };
        body.push(
            h_flex()
                .justify_between()
                .gap(px(12.))
                .child(div().text_xs().text_color(t.muted_foreground).children(s.export.message))
                .child(h_flex().flex_none().gap(px(8.)).child(discard).child(save))
                .into_any_element(),
        );
        section_with_action(
            "telemetry-export",
            "OpenTelemetry export",
            None,
            [v_flex().gap(px(16.)).px(px(20.)).py(px(16.)).children(body).into_any_element()],
            &t,
        )
    }

    /// `SettingsScopeNotice target="environment"` inside the section.
    fn export_scope_notice(&mut self, view: &SettingsView, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let environment = view.environment();
        let (id, label) = (environment.id.to_owned(), SharedString::from(environment.label.to_owned()));
        let choice = h_flex()
            .id("telemetry-export-environment")
            .px(px(16.))
            .py(px(12.))
            .gap(px(12.))
            .cursor_pointer()
            .hover(|s| s.bg(t.accent.opacity(0.5)))
            .child(div().flex_1().min_w_0().truncate().text_sm().font_weight(FontWeight::MEDIUM).child(label))
            .child(
                gpui_kit::component::Icon::new(gpui_kit::assets::IconName::ChevronRight)
                    .size(px(16.))
                    .text_color(t.muted_foreground),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.settings_ui.scope = this.settings_ui.scope.with_machine(Some(id.clone()));
                cx.notify();
            }));
        let notice = v_flex()
            .child(
                div()
                    .px(px(16.))
                    .py(px(12.))
                    .text_sm()
                    .text_color(t.muted_foreground)
                    .child("Choose one environment to configure its telemetry exports."),
            )
            .child(div().border_t_1().border_color(t.border.opacity(0.5)).child(choice))
            .into_any_element();
        section_with_action("telemetry-export", "OpenTelemetry export", None, [notice], t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_status_follows_the_last_check() {
        let check = |url: &str, result: Option<Value>| Check { url: url.into(), result };
        let url = "http://localhost:4318/v1/traces";
        assert_eq!(endpoint_status("", None).0, "Off");
        assert_eq!(endpoint_status(url, None).0, "Not checked");
        assert_eq!(endpoint_status(url, Some(&check("http://other", None))).0, "Not checked");
        assert_eq!(endpoint_status(url, Some(&check(url, None))), ("Checking\u{2026}".into(), Dot::Warning));
        let accepted = json!({"_tag": "Accepted", "latencyMs": 12.4});
        assert_eq!(endpoint_status(url, Some(&check(url, Some(accepted)))).0, "Connected \u{b7} 12 ms");
        let rejected = json!({"_tag": "Rejected", "status": 404});
        assert_eq!(endpoint_status(url, Some(&check(url, Some(rejected)))).0, "Rejected \u{b7} HTTP 404");
        let timed_out = json!({"_tag": "Unreachable", "timedOut": true});
        assert_eq!(endpoint_status(url, Some(&check(url, Some(timed_out)))).0, "Timed out");
    }

    #[test]
    fn only_environment_and_desktop_sources_override() {
        let running = json!({
            "otlpTracesUrl": "http://collector/v1/traces",
            "otlpTracesSource": "environment",
            "otlpTracesVariable": "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
            "otlpMetricsUrl": "http://collector/v1/metrics",
            "otlpMetricsSource": "settings",
        });
        let traces = running_override(&running, 0).unwrap();
        assert_eq!((traces.1, traces.2.as_deref()), ("Set by environment", Some("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT")));
        assert!(running_override(&running, 1).is_none());
        assert!(running_override(&running, 2).is_none());
        assert!(is_http_url("HTTPS://x") && !is_http_url("http://") && !is_http_url("ftp://x"));
    }
}
