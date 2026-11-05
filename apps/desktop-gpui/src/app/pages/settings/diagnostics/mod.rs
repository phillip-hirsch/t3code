//! Settings › Diagnostics (`DiagnosticsSettings.tsx`, `ResourceTelemetryDiagnostics.tsx`,
//! `ProcessSignalActions.tsx`, `TelemetryExportSettings.tsx`): the resource monitor stream, live
//! processes, resource history, trace diagnostics and OpenTelemetry export.
//!
//! State lives in keyed element state, so it (and the telemetry subscription, which keeps the
//! server sampling) lasts only while the page is on screen, like the web's mounted component.

mod export;
mod logic;

use std::collections::HashSet;
use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::ButtonVariant;
use gpui_kit::component::dialog::DialogEntrance;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{Icon, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ClipboardItem, Context, ElementId, Entity, FontWeight, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Task, Window, div, px, relative,
    rems,
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use self::logic::*;
use super::connections::auth_request;
use super::connections::logic::{DIAGNOSTICS_READ, ENVIRONMENT_MAINTAIN, ORCHESTRATION_OPERATE, Session};
use super::connections::ui::WebButton;
use super::widgets::section_with_action;
use crate::app::Workspace;
use crate::app::sidebar::now_ms;
use crate::rpc::{RpcClient, RpcError, StreamEvent};
use crate::theme::{self, Tokens};

/// `RESOURCE_HISTORY_WINDOWS` (process history): label, window, bucket.
const PROCESS_WINDOWS: [(&str, u64, u64); 4] =
    [("5m", 300_000, 30_000), ("15m", 900_000, 60_000), ("30m", 1_800_000, 120_000), ("1h", 3_600_000, 300_000)];
/// `HISTORY_WINDOWS` (resource telemetry).
const TELEMETRY_WINDOWS: [(&str, u64, u64); 4] =
    [("5m", 300_000, 15_000), ("15m", 900_000, 30_000), ("30m", 1_800_000, 60_000), ("1h", 3_600_000, 120_000)];

/// `--color-violet-500`, the server process dot.
const VIOLET: u32 = 0x8e51ff;

/// One RPC query's result, like `useEnvironmentQuery`.
struct Query<T> {
    data: Option<T>,
    error: Option<SharedString>,
    pending: bool,
    _task: Option<Task<()>>,
}

impl<T> Default for Query<T> {
    fn default() -> Self {
        Self { data: None, error: None, pending: false, _task: None }
    }
}

#[derive(Default)]
enum SessionLoad {
    #[default]
    Pending,
    Ready(Session),
    Failed,
}

#[derive(Default)]
pub(super) struct DiagnosticsState {
    started: bool,
    session: SessionLoad,
    trace: Query<TraceDiagnostics>,
    processes: Query<ProcessDiagnostics>,
    history: Query<ProcessHistory>,
    history_window: usize,
    telemetry: Option<Telemetry>,
    telemetry_error: Option<SharedString>,
    telemetry_pending: bool,
    telemetry_history: Query<TelemetryHistory>,
    telemetry_window: usize,
    collapsed_pids: HashSet<u64>,
    collapsed_keys: HashSet<String>,
    signaling_pid: Option<u64>,
    signaling_keys: HashSet<String>,
    retrying: bool,
    opening_logs: bool,
    logs_error: Option<SharedString>,
    /// `ExpandableText` rows shown in full, by row key.
    expanded: HashSet<String>,
    copied_trace: Option<String>,
    export: export::ExportState,
    _session_task: Option<Task<()>>,
    _telemetry_task: Option<Task<()>>,
    _copied_task: Option<Task<()>>,
}

fn rpc_message(error: &RpcError) -> String {
    match error {
        RpcError::Failure(value) => value.get("message").and_then(Value::as_str).map(str::to_owned),
        _ => None,
    }
    .unwrap_or_else(|| error.to_string())
}

impl DiagnosticsState {
    fn start(&mut self, client: Option<RpcClient>, endpoint: Option<crate::backend::Endpoint>, cx: &mut Context<Self>) {
        let Some(client) = client else { return };
        if self.started {
            return;
        }
        self.started = true;
        self.history_window = 1;
        self.telemetry_window = 1;
        self._session_task = Some(cx.spawn(async move |this, cx| {
            let result = auth_request(endpoint, "/api/auth/session", None).await;
            let _ = this.update(cx, |this, cx| {
                this.session = match result {
                    Ok(value) => SessionLoad::Ready(Session::from_json(&value)),
                    Err(_) => SessionLoad::Failed,
                };
                if this.grants(DIAGNOSTICS_READ) {
                    this.refresh_all(&client, cx);
                }
                cx.notify();
            });
        }));
    }

    fn grants(&self, scope: &str) -> bool {
        matches!(&self.session, SessionLoad::Ready(session) if session.grants(scope))
    }

    fn refresh_all(&mut self, client: &RpcClient, cx: &mut Context<Self>) {
        self.subscribe_telemetry(client.clone(), cx);
        self.load_telemetry_history(client.clone(), cx);
        self.load_processes(client.clone(), cx);
        self.load_history(client.clone(), cx);
        self.load_trace(client.clone(), cx);
    }

    fn load<T: DeserializeOwned + 'static>(
        &mut self,
        client: RpcClient,
        tag: &'static str,
        payload: Value,
        slot: fn(&mut Self) -> &mut Query<T>,
        cx: &mut Context<Self>,
    ) {
        let query = slot(self);
        query.pending = true;
        query._task = Some(cx.spawn(async move |this, cx| {
            let result = client.request(tag, payload).await;
            let _ = this.update(cx, |this, cx| {
                let query = slot(this);
                query.pending = false;
                match result
                    .map_err(|e| rpc_message(&e))
                    .and_then(|v| serde_json::from_value(v).map_err(|e| e.to_string()))
                {
                    Ok(data) => {
                        query.data = Some(data);
                        query.error = None;
                    }
                    Err(error) => query.error = Some(error.into()),
                }
                cx.notify();
            });
        }));
    }

    fn load_trace(&mut self, client: RpcClient, cx: &mut Context<Self>) {
        self.load(client, "server.getTraceDiagnostics", json!({}), |s| &mut s.trace, cx);
    }

    fn load_processes(&mut self, client: RpcClient, cx: &mut Context<Self>) {
        self.load(client, "server.getProcessDiagnostics", json!({}), |s| &mut s.processes, cx);
    }

    fn load_history(&mut self, client: RpcClient, cx: &mut Context<Self>) {
        let (_, window_ms, bucket_ms) = PROCESS_WINDOWS[self.history_window];
        let input = json!({ "windowMs": window_ms, "bucketMs": bucket_ms });
        self.load(client, "server.getProcessResourceHistory", input, |s| &mut s.history, cx);
    }

    fn load_telemetry_history(&mut self, client: RpcClient, cx: &mut Context<Self>) {
        let (_, window_ms, bucket_ms) = TELEMETRY_WINDOWS[self.telemetry_window];
        let input = json!({ "windowMs": window_ms, "bucketMs": bucket_ms });
        self.load(client, "server.getResourceTelemetryHistory", input, |s| &mut s.telemetry_history, cx);
    }

    /// `subscribeResourceTelemetry`; refreshing resubscribes, like refreshing the web's atom.
    fn subscribe_telemetry(&mut self, client: RpcClient, cx: &mut Context<Self>) {
        self.telemetry_pending = true;
        let stream = client.stream("subscribeResourceTelemetry", json!({}));
        self._telemetry_task = Some(cx.spawn(async move |this, cx| {
            while let Some(event) = stream.next().await {
                let ended = this.update(cx, |this, cx| {
                    this.telemetry_pending = false;
                    let ended = match event {
                        StreamEvent::Item(value) => {
                            match serde_json::from_value(value) {
                                Ok(snapshot) => {
                                    this.telemetry = Some(snapshot);
                                    this.telemetry_error = None;
                                }
                                Err(error) => this.telemetry_error = Some(error.to_string().into()),
                            }
                            false
                        }
                        StreamEvent::End(result) => {
                            if let Err(error) = result {
                                this.telemetry_error = Some(rpc_message(&error).into());
                            }
                            true
                        }
                    };
                    cx.notify();
                    ended
                });
                if ended.unwrap_or(true) {
                    break;
                }
            }
        }));
    }

    fn retry_monitor(&mut self, client: RpcClient, window: &mut Window, cx: &mut Context<Self>) {
        if !self.grants(ENVIRONMENT_MAINTAIN) {
            return;
        }
        self.retrying = true;
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("server.retryResourceTelemetry", json!({})).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.retrying = false;
                match result {
                    Ok(_) => {
                        this.subscribe_telemetry(client.clone(), cx);
                        this.load_telemetry_history(client.clone(), cx);
                    }
                    Err(error) => window.push_notification(
                        Notification::error(rpc_message(&error)).title("Could not restart resource monitor"),
                        cx,
                    ),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// `signalProcess` for the Live Processes table.
    fn signal_live(&mut self, client: RpcClient, pid: u64, signal: &'static str, cx: &mut Context<Self>) {
        let Some(process) = self.processes.data.as_ref().and_then(|d| d.processes.iter().find(|p| p.pid == pid)) else {
            return;
        };
        if !self.grants(ENVIRONMENT_MAINTAIN) || self.signaling_pid.is_some() {
            return;
        }
        self.signaling_pid = Some(pid);
        let input = json!({ "pid": pid, "startTimeMs": process.start_time_ms, "signal": signal });
        cx.notify();
        let entity = cx.entity();
        cx.spawn(async move |_, cx| {
            let result = client.request("server.signalProcess", input).await;
            let _ = cx.update(|cx| {
                let Some(window) = cx.active_window() else { return };
                let _ = window.update(cx, |_, window, cx| {
                    entity.update(cx, |this, cx| {
                        this.signaling_pid = None;
                        let title = format!("Could not send {signal}");
                        match result {
                            Err(RpcError::Disconnected(_)) => {}
                            Err(error) => {
                                window.push_notification(Notification::error(rpc_message(&error)).title(title), cx)
                            }
                            Ok(value) if value["signaled"].as_bool() == Some(true) => {
                                this.load_processes(client.clone(), cx)
                            }
                            Ok(value) => {
                                let message = some(&value["message"]).and_then(Value::as_str).map(str::to_owned);
                                this.load_processes(client.clone(), cx);
                                if is_stale_signal_message(message.as_deref()) {
                                    window.push_notification(
                                        Notification::info(
                                            "The process is not a child of the T3 Server. It might already have exited.",
                                        )
                                        .title("Process already exited"),
                                        cx,
                                    );
                                } else {
                                    let message = message.unwrap_or_else(|| format!("Failed to send {signal}."));
                                    window.push_notification(Notification::error(message).title(title), cx);
                                }
                            }
                        }
                        cx.notify();
                    })
                });
            });
        })
        .detach();
    }

    /// `signalProcess` for the Live process tree.
    fn signal_telemetry(
        &mut self,
        client: RpcClient,
        identity: Identity,
        signal: &'static str,
        cx: &mut Context<Self>,
    ) {
        let key = identity.key();
        if !self.grants(ENVIRONMENT_MAINTAIN) || !self.signaling_keys.insert(key.clone()) {
            return;
        }
        let input = json!({ "pid": identity.pid, "startTimeMs": identity.start_time_ms, "signal": signal });
        cx.notify();
        let entity = cx.entity();
        cx.spawn(async move |_, cx| {
            let result = client.request("server.signalProcess", input).await;
            let _ = cx.update(|cx| {
                let Some(window) = cx.active_window() else { return };
                let _ = window.update(cx, |_, window, cx| {
                    entity.update(cx, |this, cx| {
                        this.signaling_keys.remove(&key);
                        let title = format!("Could not send {signal}");
                        let failure = match result {
                            Err(RpcError::Disconnected(_)) => None,
                            Err(error) => Some(rpc_message(&error)),
                            Ok(value) if value["signaled"].as_bool() == Some(true) => None,
                            Ok(value) => Some(some(&value["message"]).and_then(Value::as_str).map_or_else(
                                || format!("Failed to send {signal} to process {}.", identity.pid),
                                str::to_owned,
                            )),
                        };
                        if let Some(message) = failure {
                            window.push_notification(Notification::error(message).title(title), cx);
                        }
                        cx.notify();
                    })
                });
            });
        })
        .detach();
    }
}

/// Asks before SIGKILL (`dialogs.confirm(..., { variant: "destructive" })`), then signals.
fn confirm_kill(pid: u64, window: &mut Window, cx: &mut App, on_confirm: impl Fn(&mut App) + 'static) {
    let on_confirm = std::rc::Rc::new(on_confirm);
    window.open_alert_dialog(cx, move |alert, _, _| {
        let on_confirm = on_confirm.clone();
        alert
            .entrance(DialogEntrance::None)
            .title(format!("Send SIGKILL to process {pid}?"))
            .description("This cannot be handled by the process.")
            .show_cancel(true)
            .ok_text("Confirm")
            .ok_variant(ButtonVariant::Danger)
            .on_ok(move |_, _, cx| {
                on_confirm(cx);
                true
            })
    });
}

// ---------------------------------------------------------------------------------------------
// Small pieces

fn text_2xs<E: gpui_kit::Styled>(el: E) -> E {
    el.text_size(rems(0.6875)).line_height(rems(1.))
}

fn text_3xs<E: gpui_kit::Styled>(el: E) -> E {
    el.text_size(rems(0.625)).line_height(rems(0.875))
}

fn success() -> Hsla {
    gpui_kit::rgb(0x00bc7d).into()
}

fn tone_color(tone: Tone, t: &Tokens) -> Hsla {
    match tone {
        Tone::Neutral | Tone::Default => t.foreground,
        Tone::Warning => t.warning_foreground,
        Tone::Danger => t.error_foreground,
    }
}

fn tooltip(text: impl Into<SharedString>) -> impl Fn(&mut Window, &mut App) -> gpui_kit::AnyView + 'static {
    let text = text.into();
    move |window, cx| gpui_kit::component::tooltip::Tooltip::new(text.clone()).build(window, cx)
}

/// A ghost icon button (`size="icon-xs"`/`"icon-micro"`, `variant="ghost-muted"`).
fn icon_button(
    id: impl Into<ElementId>,
    icon: IconName,
    label: impl Into<SharedString>,
    disabled: bool,
    t: &Tokens,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> AnyElement {
    let hover = t.accent;
    let color = t.muted_foreground;
    let fg = t.foreground;
    div()
        .id(id.into())
        .size(px(24.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.))
        .text_color(color)
        .child(Icon::new(icon).size(px(14.)))
        .tooltip(tooltip(label))
        .map(|el| {
            if disabled {
                el.opacity(0.64)
            } else {
                el.cursor_pointer()
                    .hover(move |s| s.bg(hover).text_color(fg))
                    .on_click(move |_, window, cx| on_click(window, cx))
            }
        })
        .into_any_element()
}

/// `InlineButton`: text that underlines on hover.
fn inline_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    color: Hsla,
    hover: Hsla,
    disabled: bool,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    div()
        .id(id.into())
        .flex_none()
        .font_weight(FontWeight::MEDIUM)
        .whitespace_nowrap()
        .text_color(color)
        .child(label.into())
        .map(|el| {
            if disabled {
                el.opacity(0.64)
            } else {
                el.cursor_pointer()
                    .hover(move |s| s.underline().text_color(hover))
                    .on_click(move |_, window, cx| on_click(window, cx))
            }
        })
}

/// `ProcessSignalActions`: INT and KILL.
fn signal_actions(
    id: &str,
    disabled: bool,
    t: &Tokens,
    on_signal: impl Fn(&'static str, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let on_signal = std::rc::Rc::new(on_signal);
    let (int, kill) = (on_signal.clone(), on_signal);
    text_2xs(h_flex().justify_end().gap(px(6.)))
        .child(
            inline_button(
                ElementId::Name(format!("{id}-int").into()),
                "INT",
                t.muted_foreground,
                t.foreground,
                disabled,
                move |w, cx| int("SIGINT", w, cx),
            )
            .tooltip(tooltip("Send SIGINT")),
        )
        .child(
            inline_button(
                ElementId::Name(format!("{id}-kill").into()),
                "KILL",
                t.error_foreground.opacity(0.8),
                t.error_foreground,
                disabled,
                move |w, cx| kill("SIGKILL", w, cx),
            )
            .tooltip(tooltip("Send SIGKILL")),
        )
        .into_any_element()
}

/// A section's title with its icon.
fn titled(icon: Option<IconName>, title: &'static str, t: &Tokens) -> AnyElement {
    h_flex()
        .gap(px(8.))
        .when_some(icon, |el, icon| el.child(Icon::new(icon).size(px(16.)).text_color(t.muted_foreground)))
        .child(title)
        .into_any_element()
}

fn error_line(id: impl Into<ElementId>, message: SharedString, color: Hsla) -> AnyElement {
    h_flex()
        .id(id.into())
        .items_start()
        .gap(px(8.))
        .text_color(color)
        .child(div().h(rems(1.)).flex().items_center().child(Icon::new(IconName::TriangleAlert).size(px(14.))))
        .child(div().min_w_0().child(message))
        .into_any_element()
}

/// The errors block under a stats grid.
fn errors_block(lines: Vec<AnyElement>) -> Option<AnyElement> {
    (!lines.is_empty())
        .then(|| v_flex().px(px(20.)).py(px(12.)).gap(px(8.)).text_xs().children(lines).into_any_element())
}

/// `StatBlock` inside `StatsGrid`: four columns divided by hairlines.
fn stats_grid(blocks: [(&'static str, String, Option<String>, Tone); 4], t: &Tokens) -> AnyElement {
    let mono = t.fonts.mono;
    h_flex()
        .children(blocks.into_iter().enumerate().map(|(index, (label, value, tip, tone))| {
            v_flex()
                .flex_1()
                .min_w_0()
                .px(px(20.))
                .py(px(12.))
                .when(index > 0, |el| el.border_l_1().border_color(t.border.opacity(0.6)))
                .child(
                    text_2xs(h_flex().min_w_0().gap(px(6.)))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(t.muted_foreground.opacity(0.7))
                        .child(div().min_w_0().truncate().child(label))
                        .when_some(tip, |el, tip| {
                            el.child(
                                div()
                                    .id(ElementId::Name(format!("stat-tip-{label}").into()))
                                    .text_color(t.muted_foreground.opacity(0.6))
                                    .child(Icon::new(IconName::Info).size(px(12.)))
                                    .tooltip(tooltip(tip)),
                            )
                        }),
                )
                .child(
                    div()
                        .mt(px(4.))
                        .truncate()
                        .font_family(mono)
                        .text_lg()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(tone_color(tone, t))
                        .child(value),
                )
        }))
        .into_any_element()
}

/// One table column: share of the width and alignment.
#[derive(Clone, Copy)]
struct Col(f32, bool);

const L: bool = false;
const R: bool = true;

/// A web `<table>`: an uppercase header and hairline-divided rows, scrolling sideways below
/// `min_width`. `pad` is each cell's (x, y) padding; the outer cells get 20px at the edges.
fn table(
    id: &'static str,
    min_width: f32,
    cols: &[Col],
    headers: &[&'static str],
    rows: Vec<(ElementId, Vec<AnyElement>)>,
    empty: Option<SharedString>,
    pad: (f32, f32),
    t: &Tokens,
) -> AnyElement {
    let last = cols.len() - 1;
    let cell = |index: usize, py: f32| {
        let Col(width, right) = cols[index];
        div()
            .w(relative(width))
            .min_w_0()
            .flex_none()
            .pl(px(if index == 0 { 20. } else { pad.0 }))
            .pr(px(if index == last { 20. } else { pad.0 }))
            .py(px(py))
            .when(right, |el| el.flex().justify_end())
    };
    let header = text_2xs(h_flex())
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(t.muted_foreground.opacity(0.7))
        .border_b_1()
        .border_color(t.border.opacity(0.6))
        .children(headers.iter().enumerate().map(|(index, label)| cell(index, 8.).whitespace_nowrap().child(*label)));
    let hover = t.muted.opacity(0.2);
    let body = match empty {
        Some(label) if rows.is_empty() => {
            div().px(px(20.)).py(px(16.)).text_xs().text_color(t.muted_foreground).child(label)
        }
        _ => v_flex().children(rows.into_iter().enumerate().map(|(index, (id, cells))| {
            h_flex()
                .id(id)
                .items_start()
                .text_xs()
                .when(index > 0, |el| el.border_t_1().border_color(t.border.opacity(0.5)))
                .hover(move |s| s.bg(hover))
                .children(cells.into_iter().enumerate().map(|(index, content)| cell(index, pad.1).child(content)))
        })),
    };
    div()
        .id(id)
        .w_full()
        .overflow_x_scroll()
        .child(v_flex().w_full().min_w(px(min_width)).child(header).child(body))
        .into_any_element()
}

fn mono(text: impl Into<SharedString>, t: &Tokens) -> gpui_kit::Div {
    div().font_family(t.fonts.mono).whitespace_nowrap().child(text.into())
}

fn truncated(text: impl Into<SharedString>, color: Hsla) -> gpui_kit::Div {
    div().min_w_0().truncate().text_color(color).child(text.into())
}

/// A truncated cell whose tooltip shows the whole command.
fn command_cell(id: ElementId, text: &str, color: Hsla) -> AnyElement {
    div()
        .id(id)
        .w_full()
        .min_w_0()
        .truncate()
        .text_color(color)
        .child(text.to_owned())
        .tooltip(tooltip(text.to_owned()))
        .into_any_element()
}

/// The segmented period picker (`ToggleGroup variant="segmented"`).
fn segmented(
    id: &'static str,
    labels: impl Iterator<Item = &'static str>,
    current: usize,
    t: &Tokens,
    on_pick: impl Fn(usize, &mut App) + 'static,
) -> AnyElement {
    let on_pick = std::rc::Rc::new(on_pick);
    let track = Hsla { a: t.input.a * 0.4, ..t.input };
    let pressed_bg = if t.dark { Hsla { a: t.input.a * 0.72, ..t.input } } else { t.background };
    let hover_bg = Hsla { a: t.input.a * 0.32, ..t.input };
    let (fg, muted) = (t.foreground, t.muted_foreground);
    h_flex()
        .flex_none()
        .gap(px(2.))
        .p(px(2.))
        .rounded(px(8.))
        .bg(track)
        .children(labels.enumerate().map(|(index, label)| {
            let on_pick = on_pick.clone();
            div()
                .id(ElementId::Name(format!("{id}-{label}").into()))
                .h(px(24.))
                .px(px(10.))
                .flex()
                .items_center()
                .rounded(px(6.))
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .cursor_pointer()
                .map(|el| {
                    if index == current {
                        el.text_color(fg).bg(pressed_bg)
                    } else {
                        el.text_color(muted).hover(move |s| s.text_color(fg).bg(hover_bg))
                    }
                })
                .child(label)
                .on_click(move |_, _, cx| on_pick(index, cx))
        }))
        .into_any_element()
}

/// `DiagnosticsLastChecked`.
fn last_checked(read_at: Option<&str>, now: i64, t: &Tokens) -> AnyElement {
    let label = match read_at {
        None => ("Checking".to_owned(), 0.5),
        Some(iso) => match relative_label(iso, now) {
            Some(relative) => (format!("Checked {relative}"), 0.6),
            None => ("Checked unavailable".to_owned(), 0.5),
        },
    };
    text_2xs(div()).text_color(t.muted_foreground.opacity(label.1)).child(label.0).into_any_element()
}

/// An `ExpandableText` cell: clamped until "Show full …" expands it.
fn expandable(
    state: &Entity<DiagnosticsState>,
    key: String,
    text: &str,
    clamp: usize,
    expand_label: &'static str,
    expanded: bool,
    t: &Tokens,
) -> AnyElement {
    let can_expand = text.chars().count() > 180 || text.contains('\n');
    let state = state.clone();
    let id = ElementId::Name(format!("expand-{key}").into());
    v_flex()
        .min_w_0()
        .child(if can_expand && !expanded {
            // GPUI's line clamp counts wrapped lines of one paragraph, not newline-separated ones.
            let head: Vec<&str> = text.lines().take(clamp).collect();
            div().min_w_0().overflow_hidden().line_clamp(clamp).child(head.join("\n"))
        } else {
            div().min_w_0().child(text.to_owned())
        })
        .when(can_expand, |el| {
            el.child(text_2xs(div().mt(px(4.))).child(inline_button(
                id,
                if expanded { "Show less" } else { expand_label },
                t.muted_foreground,
                t.foreground,
                false,
                move |_, cx| {
                    let key = key.clone();
                    state.update(cx, |s, cx| {
                        if !s.expanded.remove(&key) {
                            s.expanded.insert(key);
                        }
                        cx.notify();
                    })
                },
            )))
        })
        .into_any_element()
}

/// `TraceIdCell`: the shortened id with a copy button.
fn trace_id_cell(state: &Entity<DiagnosticsState>, row: &str, trace_id: &str, copied: bool, t: &Tokens) -> AnyElement {
    let state = state.clone();
    let full = trace_id.to_owned();
    h_flex()
        .w_full()
        .min_w_0()
        .gap(px(8.))
        .child(
            text_2xs(div().id(ElementId::Name(format!("trace-{row}").into())).flex_1().min_w_0().truncate())
                .font_family(t.fonts.mono)
                .child(shorten_trace_id(trace_id))
                .tooltip(tooltip(full.clone())),
        )
        .child(icon_button(
            ElementId::Name(format!("trace-copy-{row}").into()),
            IconName::Copy,
            if copied { "Copied" } else { "Copy full trace ID" },
            false,
            t,
            move |_, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(full.clone()));
                let full = full.clone();
                state.update(cx, |s, cx| {
                    s.copied_trace = Some(full);
                    s._copied_task = Some(cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(Duration::from_millis(1_200)).await;
                        let _ = this.update(cx, |s, cx| {
                            s.copied_trace = None;
                            cx.notify();
                        });
                    }));
                    cx.notify();
                })
            },
        ))
        .into_any_element()
}

/// The indent, chevron and dot in front of a process name.
fn tree_name(
    id: ElementId,
    depth: u32,
    toggle: Option<(bool, Box<dyn Fn(&mut App)>)>,
    dot: Hsla,
    name: String,
    full: String,
    t: &Tokens,
) -> AnyElement {
    let chevron = match toggle {
        Some((open, on_toggle)) => icon_button(
            ElementId::Name(format!("{id}-toggle").into()),
            if open { IconName::ChevronDown } else { IconName::ChevronRight },
            if open { format!("Collapse {name}") } else { format!("Expand {name}") },
            false,
            t,
            move |_, cx| on_toggle(cx),
        ),
        None => div().size(px(20.)).flex_none().into_any_element(),
    };
    h_flex()
        .min_w_0()
        .gap(px(8.))
        .pl(px(depth as f32 * 10.))
        .child(div().w(px(20.)).flex_none().flex().justify_center().child(chevron))
        .child(div().size(px(6.)).flex_none().rounded_full().bg(dot))
        .child(
            div()
                .id(id)
                .min_w_0()
                .truncate()
                .font_weight(FontWeight::MEDIUM)
                .text_color(t.foreground)
                .child(name)
                .tooltip(tooltip(full)),
        )
        .into_any_element()
}

impl Workspace {
    pub(super) fn render_settings_diagnostics(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let state = window.use_keyed_state("settings-diagnostics", cx, |_, _| DiagnosticsState::default());
        let (client, endpoint) = (self.client(), self.endpoint.clone());
        state.update(cx, |s, cx| s.start(client.clone(), endpoint, cx));
        let t = *theme::tokens(cx);
        let view = self.settings_view();
        let s = state.read(cx);
        let blocked = match (&client, &s.session) {
            (None, _) => Some("Connect an environment to see diagnostics."),
            (_, SessionLoad::Pending) => Some("Checking diagnostics access\u{2026}"),
            (_, SessionLoad::Failed) => Some("Could not check this connection's access to diagnostics and usage."),
            (_, SessionLoad::Ready(session)) if !session.grants(DIAGNOSTICS_READ) => {
                Some("This connection does not have access to diagnostics and usage.")
            }
            _ => None,
        };
        let content = match (blocked, client) {
            (None, Some(client)) => {
                let mut sections = self.telemetry_sections(&state, &client, &t, cx);
                sections.extend(self.diagnostics_sections(&state, &client, &t, cx));
                sections.push(self.telemetry_export_section(&state, &view, window, cx));
                sections
            }
            (message, _) => {
                vec![
                    div()
                        .text_sm()
                        .text_color(t.muted_foreground)
                        .child(message.unwrap_or_default())
                        .into_any_element(),
                ]
            }
        };
        self.settings_page_sized("diagnostics", &view, 1152., 40., content, cx)
    }

    /// The `ResourceTelemetryDiagnostics` sections.
    fn telemetry_sections(
        &self,
        state: &Entity<DiagnosticsState>,
        client: &RpcClient,
        t: &Tokens,
        cx: &App,
    ) -> Vec<AnyElement> {
        let s = state.read(cx);
        let now = now_ms();
        let snapshot = s.telemetry.as_ref();
        let all = snapshot.map(|s| &s.groups.all_t3);
        let dots = t.muted_foreground;
        let maintain = s.grants(ENVIRONMENT_MAINTAIN);

        // Resource monitor
        let refresh = {
            let (state, client) = (state.clone(), client.clone());
            icon_button(
                "telemetry-refresh",
                IconName::RefreshCw,
                "Refresh telemetry snapshot",
                s.telemetry_pending,
                t,
                move |_, cx| state.update(cx, |s, cx| s.subscribe_telemetry(client.clone(), cx)),
            )
        };
        let updated = match snapshot.and_then(|snap| relative_label(&snap.read_at, now)) {
            Some(relative) => format!("Updated {relative}"),
            None => "Waiting for sample".into(),
        };
        let monitor_action = h_flex()
            .gap(px(8.))
            .when_some(snapshot, |el, snap| el.child(status_badge("Native ", &snap.health.native.status, None, t)))
            .child(text_2xs(div()).text_color(t.muted_foreground.opacity(0.6)).child(updated))
            .child(refresh)
            .into_any_element();
        let stat = |icon: IconName, label: &'static str, value: String, detail: Option<String>, tone: Tone| {
            v_flex()
                .flex_1()
                .min_w_0()
                .px(px(20.))
                .py(px(16.))
                .child(
                    text_3xs(h_flex().gap(px(8.)))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(t.muted_foreground.opacity(0.7))
                        .child(Icon::new(icon).size(px(14.)).text_color(dots.opacity(0.55)))
                        .child(div().truncate().child(label)),
                )
                .child(
                    div()
                        .mt(px(10.))
                        .truncate()
                        .font_family(t.fonts.mono)
                        .text_2xl()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(tone_color(tone, t))
                        .child(value),
                )
                .when_some(detail, |el, detail| {
                    el.child(
                        text_3xs(div().mt(px(6.)).truncate()).text_color(t.muted_foreground.opacity(0.6)).child(detail),
                    )
                })
        };
        let dash = || "...".to_owned();
        let write_tone = match all.map(|a| a.io_write_bytes_per_second) {
            Some(rate) if rate >= 10. * 1_048_576. => Tone::Danger,
            Some(rate) if rate >= 1_048_576. => Tone::Warning,
            _ => Tone::Default,
        };
        let speed_limit = snapshot.and_then(Telemetry::speed_limit);
        let stats = [
            stat(
                IconName::Cpu,
                "CURRENT CPU",
                all.map_or_else(dash, |a| percent(a.current_cpu_percent)),
                all.map(|a| format!("{} observed CPU time", format_cpu_time(a.cpu_time_ms / 1_000.))),
                Tone::Default,
            ),
            stat(
                IconName::MemoryStick,
                "RESIDENT MEMORY",
                all.map_or_else(dash, |a| super::system::format_bytes(a.current_rss_bytes)),
                all.map(|a| format!("{} combined process peaks", super::system::format_bytes(a.peak_rss_bytes))),
                Tone::Default,
            ),
            stat(
                IconName::Activity,
                "PROCESS COUNT",
                all.map_or_else(dash, |a| a.process_count.to_string()),
                all.map(|a| format!("{} starts \u{b7} {} exits", a.process_starts, a.process_exits)),
                Tone::Default,
            ),
            stat(
                IconName::HardDrive,
                "READ THROUGHPUT",
                all.map_or_else(dash, |a| format_rate(a.io_read_bytes_per_second)),
                all.map(|a| format!("{} observed", super::system::format_bytes(a.io_read_bytes))),
                Tone::Default,
            ),
            stat(
                IconName::Database,
                "WRITE THROUGHPUT",
                all.map_or_else(dash, |a| format_rate(a.io_write_bytes_per_second)),
                all.map(|a| format!("{} observed", super::system::format_bytes(a.io_write_bytes))),
                write_tone,
            ),
            stat(
                IconName::Gauge,
                "CPU SPEED LIMIT",
                match (snapshot, speed_limit) {
                    (None, _) => dash(),
                    (Some(_), None) => "Unknown".into(),
                    (Some(_), Some(limit)) => format!("{limit:.0}%"),
                },
                snapshot.map(|snap| format!("{} thermal state", snap.power.thermal_state)),
                if speed_limit.is_some_and(|l| l < 80.) { Tone::Warning } else { Tone::Default },
            ),
        ];
        let line = t.border.opacity(0.55);
        let mut stats = stats.into_iter();
        let grid_row = |cells: Vec<gpui_kit::Div>| {
            h_flex().children(
                cells
                    .into_iter()
                    .enumerate()
                    .map(|(i, cell)| cell.when(i > 0, |el| el.border_l_1().border_color(line))),
            )
        };
        let first: Vec<_> = stats.by_ref().take(3).collect();
        let second: Vec<_> = stats.collect();
        let footprint = v_flex()
            .child(
                h_flex()
                    .justify_between()
                    .gap(px(12.))
                    .px(px(20.))
                    .py(px(16.))
                    .border_b_1()
                    .border_color(t.border.opacity(0.6))
                    .bg(t.muted.opacity(0.3))
                    .child(
                        v_flex()
                            .child(
                                text_3xs(div())
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(t.muted_foreground.opacity(0.7))
                                    .child("T3 SYSTEM FOOTPRINT"),
                            )
                            .child(div().mt(px(4.)).max_w(px(576.)).text_xs().text_color(t.muted_foreground).child(
                                "Live native counters for the server, providers, terminals, desktop processes, and the monitor itself.",
                            )),
                    )
                    .child(
                        text_3xs(h_flex().flex_none().gap(px(8.)))
                            .text_color(t.muted_foreground.opacity(0.65))
                            .child(div().size(px(6.)).rounded_full().bg(success()))
                            .child(format!(
                                "Sampling every {}",
                                snapshot.map_or_else(dash, |snap| format_sample_interval(snap.sample_interval_ms))
                            )),
                    ),
            )
            .child(grid_row(first))
            .child(grid_row(second).border_t_1().border_color(line))
            .when_some(s.telemetry_error.clone(), |el, error| {
                el.child(
                    div()
                        .px(px(20.))
                        .py(px(12.))
                        .text_xs()
                        .border_t_1()
                        .border_color(t.error.opacity(0.2))
                        .bg(t.error.opacity(0.05))
                        .child(error_line("telemetry-error", error, t.error_foreground)),
                )
            })
            .when_some(snapshot, |el, snap| {
                let groups = [
                    ("BACKEND + AGENTS", success().opacity(0.8), &snap.groups.backend),
                    ("DESKTOP", t.info.opacity(0.8), &snap.groups.electron),
                    ("MONITOR OVERHEAD", t.warning.opacity(0.8), &snap.groups.monitor),
                ];
                el.child(
                    h_flex().border_t_1().border_color(t.border.opacity(0.6)).bg(t.muted.opacity(0.1)).children(
                        groups.into_iter().enumerate().map(|(i, (label, accent, aggregate))| {
                            aggregate_card(label, accent, aggregate, t)
                                .when(i > 0, |el| el.border_l_1().border_color(t.border.opacity(0.6)))
                        }),
                    ),
                )
            })
            .into_any_element();
        let mut sections = vec![section_with_action(
            "resource-monitor",
            titled(Some(IconName::Activity), "Resource monitor", t),
            Some(monitor_action),
            [footprint],
            t,
        )];

        // Host & collection
        let retry = should_show_monitor_retry(
            snapshot.map(|snap| snap.health.native.status.as_str()),
            s.telemetry_error.is_some(),
        )
        .then(|| {
            let (state, client) = (state.clone(), client.clone());
            WebButton::new("retry-monitor", "Retry monitor")
                .icon(IconName::RefreshCw)
                .disabled(s.retrying)
                .on_click(move |_, window, cx| state.update(cx, |s, cx| s.retry_monitor(client.clone(), window, cx)))
                .into_any_element()
        });
        let heading = |icon: IconName, label: &'static str| {
            text_3xs(h_flex().mb(px(12.)).gap(px(8.)))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(t.muted_foreground.opacity(0.7))
                .child(
                    div()
                        .size(px(24.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(6.))
                        .bg(t.muted.opacity(0.6))
                        .child(Icon::new(icon).size(px(14.))),
                )
                .child(label)
        };
        let detail = |label: &'static str, value: String, color: Option<Hsla>| {
            h_flex()
                .justify_between()
                .gap(px(16.))
                .py(px(10.))
                .child(text_2xs(div()).text_color(t.muted_foreground.opacity(0.75)).child(label))
                .child(
                    text_2xs(div().min_w_0().truncate())
                        .font_family(t.fonts.mono)
                        .text_color(color.unwrap_or(t.foreground.opacity(0.85)))
                        .child(value),
                )
                .into_any_element()
        };
        let host_rows: Vec<AnyElement> = match snapshot.filter(|snap| snap.has_host_power_signal()) {
            Some(snap) => {
                let p = &snap.power;
                let idle = format!(
                    "{}{}",
                    boolean_state(&p.idle, "Idle", "Active"),
                    p.idle_seconds.map_or_else(String::new, |s| format!(" \u{b7} {}s", s.round()))
                );
                let session =
                    if p.suspended { "Suspended" } else { boolean_state(&p.locked, "Locked", "Unlocked") }.to_owned();
                let hot = matches!(p.thermal_state.as_str(), "serious" | "critical").then_some(t.error_foreground);
                vec![
                    detail("Power source", boolean_state(&p.on_battery, "Battery", "External power").into(), None),
                    detail("Low power mode", boolean_state(&p.low_power_mode, "Enabled", "Disabled").into(), None),
                    detail("Idle", idle, None),
                    detail("Session", session, None),
                    detail("Thermal", p.thermal_state.clone(), hot),
                ]
            }
            None => vec![
                v_flex()
                    .rounded(px(12.))
                    .border_1()
                    .border_dashed()
                    .border_color(t.border.opacity(0.7))
                    .bg(t.muted.opacity(0.2))
                    .px(px(16.))
                    .py(px(20.))
                    .child(div().text_sm().font_weight(FontWeight::MEDIUM).child("Desktop host signals not connected"))
                    .child(text_2xs(div().mt(px(6.)).max_w(px(384.))).text_color(t.muted_foreground.opacity(0.7)).child(
                        "Power, idle, lock, and thermal state are supplied by the desktop host. Process telemetry remains fully active in this browser session.",
                    ))
                    .into_any_element(),
            ],
        };
        let health_rows: Vec<AnyElement> = match snapshot {
            Some(snap) => {
                let h = &snap.health;
                let sidecar = match some(&h.sidecar_version).and_then(Value::as_str) {
                    None => "Unavailable".to_owned(),
                    Some(version) => match some(&h.sidecar_pid).and_then(Value::as_u64) {
                        Some(pid) => format!("{version} \u{b7} PID {pid}"),
                        None => version.to_owned(),
                    },
                };
                vec![
                    health_source("Native process monitor", &h.native, t),
                    health_source("Electron main process", &h.desktop, t),
                    detail("Collection time", format_duration_micros(h.collection_duration_micros), None),
                    detail(
                        "Process scan",
                        format!("{}/{} retained", h.retained_process_count, h.scanned_process_count),
                        None,
                    ),
                    detail(
                        "Inaccessible",
                        h.inaccessible_process_count.to_string(),
                        (h.inaccessible_process_count > 0).then_some(t.warning_foreground),
                    ),
                    detail("Sidecar", sidecar, None),
                    detail("Restarts", h.restart_count.to_string(), None),
                ]
            }
            None => vec![
                div()
                    .py(px(16.))
                    .text_xs()
                    .text_color(t.muted_foreground)
                    .child("Waiting for collector health.")
                    .into_any_element(),
            ],
        };
        let divided =
            |rows: Vec<AnyElement>| {
                v_flex().children(rows.into_iter().enumerate().map(|(i, row)| {
                    div().when(i > 0, |el| el.border_t_1().border_color(t.border.opacity(0.5))).child(row)
                }))
            };
        let host = h_flex()
            .items_start()
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .px(px(20.))
                    .py(px(16.))
                    .child(heading(IconName::Battery, "HOST STATE"))
                    .child(divided(host_rows)),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .px(px(20.))
                    .py(px(16.))
                    .border_l_1()
                    .border_color(t.border.opacity(0.6))
                    .child(heading(IconName::Gauge, "COLLECTION HEALTH"))
                    .child(divided(health_rows)),
            )
            .into_any_element();
        sections.push(section_with_action(
            "host-collection",
            titled(Some(IconName::Gauge), "Host & collection", t),
            retry,
            [host],
            t,
        ));

        // Resource timeline
        let history = s.telemetry_history.data.as_ref();
        let timeline_action = {
            let (pick_state, pick_client) = (state.clone(), client.clone());
            let (state, client) = (state.clone(), client.clone());
            h_flex()
                .gap(px(8.))
                .child(segmented(
                    "telemetry-window",
                    TELEMETRY_WINDOWS.iter().map(|w| w.0),
                    s.telemetry_window,
                    t,
                    move |index, cx| {
                        pick_state.update(cx, |s, cx| {
                            s.telemetry_window = index;
                            s.load_telemetry_history(pick_client.clone(), cx);
                            cx.notify();
                        })
                    },
                ))
                .child(icon_button(
                    "telemetry-history-refresh",
                    IconName::RefreshCw,
                    "Refresh resource history",
                    s.telemetry_history.pending,
                    t,
                    move |_, cx| state.update(cx, |s, cx| s.load_telemetry_history(client.clone(), cx)),
                ))
                .into_any_element()
        };
        let mut timeline = Vec::new();
        if let Some(error) = s.telemetry_history.error.clone() {
            timeline.push(
                div()
                    .px(px(20.))
                    .py(px(12.))
                    .text_xs()
                    .bg(t.error.opacity(0.05))
                    .child(error_line("telemetry-history-error", error, t.error_foreground))
                    .into_any_element(),
            );
        }
        timeline.push(telemetry_chart(history.map_or(&[][..], |h| &h.buckets), t));
        let summaries: Vec<(ElementId, Vec<AnyElement>)> = history
            .map_or(&[][..], |h| &h.top_processes)
            .iter()
            .map(|p| {
                let key = p.identity.key();
                let name = if p.name.is_empty() { p.command.clone() } else { p.name.clone() };
                let full = if p.command.is_empty() { p.name.clone() } else { p.command.clone() };
                let row_id = ElementId::Name(format!("tl-{key}").into());
                (
                    row_id,
                    vec![
                        div()
                            .id(ElementId::Name(format!("tl-name-{key}").into()))
                            .min_w_0()
                            .truncate()
                            .font_weight(FontWeight::MEDIUM)
                            .child(name)
                            .tooltip(tooltip(full))
                            .into_any_element(),
                        text_2xs(truncated(category_label(&p.category), t.muted_foreground)).into_any_element(),
                        mono(format_cpu_time(p.cpu_time_ms / 1_000.), t).into_any_element(),
                        mono(percent(p.max_cpu_percent), t).into_any_element(),
                        mono(super::system::format_bytes(p.peak_rss_bytes), t).into_any_element(),
                        mono(super::system::format_bytes(p.io_read_bytes), t)
                            .text_color(t.info_foreground)
                            .into_any_element(),
                        mono(super::system::format_bytes(p.io_write_bytes), t)
                            .text_color(t.warning_foreground)
                            .into_any_element(),
                        mono(p.sample_count.to_string(), t).text_color(t.muted_foreground).into_any_element(),
                        mono(p.identity.pid.to_string(), t).text_color(t.muted_foreground).into_any_element(),
                    ],
                )
            })
            .collect();
        timeline.push(table(
            "telemetry-history-table",
            1020.,
            &[
                Col(0.24, L),
                Col(0.11, L),
                Col(0.10, R),
                Col(0.10, R),
                Col(0.11, R),
                Col(0.11, R),
                Col(0.11, R),
                Col(0.07, R),
                Col(0.05, R),
            ],
            &["PROCESS", "CATEGORY", "CPU TIME", "PEAK CPU", "PEAK MEM", "READ", "WRITE", "SAMPLES", "PID"],
            summaries,
            Some("No retained process samples in this window.".into()),
            (12., 8.),
            t,
        ));
        sections.push(section_with_action(
            "resource-timeline",
            titled(Some(IconName::HardDrive), "Resource timeline", t),
            Some(timeline_action),
            [v_flex().children(divide(timeline, t)).into_any_element()],
            t,
        ));

        // Live process tree
        let processes = snapshot.map_or(&[][..], |snap| &snap.processes);
        let rows: Vec<(ElementId, Vec<AnyElement>)> = visible_telemetry_processes(processes, &s.collapsed_keys)
            .into_iter()
            .map(|p| {
                let key = p.identity.key();
                let name = telemetry_process_name(&p.name, &p.command);
                let full = if p.command.is_empty() { p.name.clone() } else { p.command.clone() };
                let toggle = (!p.child_pids.is_empty()).then(|| {
                    let (state, key) = (state.clone(), key.clone());
                    let open = !s.collapsed_keys.contains(&key);
                    let on_toggle: Box<dyn Fn(&mut App)> = Box::new(move |cx| {
                        let key = key.clone();
                        state.update(cx, |s, cx| {
                            if !s.collapsed_keys.remove(&key) {
                                s.collapsed_keys.insert(key);
                            }
                            cx.notify();
                        })
                    });
                    (open, on_toggle)
                });
                let dot = match p.category.as_str() {
                    "resource-monitor" => t.warning,
                    "server" => gpui_kit::rgb(VIOLET).into(),
                    category if category.starts_with("electron-") => t.info,
                    _ => success(),
                };
                let actions = if can_signal(&p.category) {
                    let (state, client, identity) = (state.clone(), client.clone(), p.identity);
                    let disabled = !maintain || s.signaling_keys.contains(&key);
                    signal_actions(&format!("tree-{key}"), disabled, t, move |signal, window, cx| {
                        let (state, client) = (state.clone(), client.clone());
                        let send = move |cx: &mut App| {
                            state.update(cx, |s, cx| s.signal_telemetry(client.clone(), identity, signal, cx))
                        };
                        if signal == "SIGKILL" {
                            confirm_kill(identity.pid, window, cx, send);
                        } else {
                            send(cx);
                        }
                    })
                } else {
                    text_3xs(div()).text_color(t.muted_foreground.opacity(0.35)).child("-").into_any_element()
                };
                (
                    ElementId::Name(format!("tree-{key}").into()),
                    vec![
                        tree_name(
                            ElementId::Name(format!("tree-name-{key}").into()),
                            p.depth.min(7),
                            toggle,
                            dot,
                            name,
                            full,
                            t,
                        ),
                        text_2xs(truncated(category_label(&p.category), t.muted_foreground)).into_any_element(),
                        mono(percent(p.cpu_percent), t).into_any_element(),
                        mono(format_cpu_time(p.cpu_time_ms / 1_000.), t).into_any_element(),
                        mono(super::system::format_bytes(p.resident_bytes), t).into_any_element(),
                        mono(format_rate(p.io_read_bytes_per_second), t)
                            .text_color(t.info_foreground)
                            .into_any_element(),
                        mono(format_rate(p.io_write_bytes_per_second), t)
                            .text_color(t.warning_foreground)
                            .into_any_element(),
                        mono(super::system::format_bytes(p.io_read_bytes), t)
                            .text_color(t.muted_foreground)
                            .into_any_element(),
                        div()
                            .id(ElementId::Name(format!("tree-write-{key}").into()))
                            .font_family(t.fonts.mono)
                            .text_color(t.muted_foreground)
                            .child(super::system::format_bytes(p.io_write_bytes))
                            .tooltip(tooltip(io_semantics_label(&p.io_semantics)))
                            .into_any_element(),
                        mono(p.identity.pid.to_string(), t).text_color(t.muted_foreground).into_any_element(),
                        actions,
                    ],
                )
            })
            .collect();
        let tree = table(
            "telemetry-tree-table",
            1320.,
            &[
                Col(0.20, L),
                Col(0.10, L),
                Col(0.07, R),
                Col(0.08, R),
                Col(0.09, R),
                Col(0.09, R),
                Col(0.09, R),
                Col(0.10, R),
                Col(0.08, R),
                Col(0.06, R),
                Col(0.04, R),
            ],
            &[
                "PROCESS",
                "CATEGORY",
                "CPU",
                "CPU TIME",
                "MEMORY",
                "READ/S",
                "WRITE/S",
                "READ TOTAL",
                "WRITE TOTAL",
                "PID",
                "KILL",
            ],
            rows,
            Some("Waiting for the native process monitor.".into()),
            (12., 8.),
            t,
        );
        let identity = snapshot.map(|_| {
            text_3xs(h_flex().gap(px(4.)))
                .text_color(t.muted_foreground.opacity(0.55))
                .child("Identity:")
                .child(div().font_family(t.fonts.mono).child("PID + start time"))
                .into_any_element()
        });
        sections.push(section_with_action(
            "live-process-tree",
            titled(Some(IconName::Cpu), "Live process tree", t),
            identity,
            [tree],
            t,
        ));

        // Instrumented application I/O
        let entries = snapshot.map_or(&[][..], |snap| &snap.attribution.entries);
        let rows = entries
            .iter()
            .map(|e| {
                let key = format!("{}:{}", e.component, e.operation);
                (
                    ElementId::Name(format!("io-{key}").into()),
                    vec![
                        truncated(e.component.clone(), t.foreground).font_weight(FontWeight::MEDIUM).into_any_element(),
                        truncated(e.operation.clone(), t.muted_foreground).into_any_element(),
                        mono(super::system::format_bytes(e.logical_read_bytes), t)
                            .text_color(t.info_foreground)
                            .into_any_element(),
                        mono(super::system::format_bytes(e.logical_write_bytes), t)
                            .text_color(t.warning_foreground)
                            .into_any_element(),
                        mono(e.count.to_string(), t).into_any_element(),
                        mono(format!("{}s", format_seconds(e.duration_ms)), t)
                            .text_color(t.muted_foreground)
                            .into_any_element(),
                    ],
                )
            })
            .collect();
        let io = v_flex()
            .child(text_2xs(div().px(px(20.)).py(px(12.))).bg(t.muted.opacity(0.15)).text_color(t.muted_foreground).child(
                "Native counters identify which process is reading or writing. These application-level counters identify known T3 operations so process spikes can be correlated with specific persistence and logging paths.",
            ))
            .child(div().border_t_1().border_color(t.border.opacity(0.6)).child(table(
                "io-table",
                720.,
                &[Col(0.22, L), Col(0.28, L), Col(0.14, R), Col(0.14, R), Col(0.10, R), Col(0.12, R)],
                &["COMPONENT", "OPERATION", "LOGICAL READ", "LOGICAL WRITE", "COUNT", "TIME"],
                rows,
                Some("No instrumented application I/O has been recorded yet.".into()),
                (12., 8.),
                t,
            )))
            .into_any_element();
        let io_action = text_3xs(div())
            .text_color(t.muted_foreground.opacity(0.55))
            .child("Logical bytes by operation")
            .into_any_element();
        sections.push(section_with_action(
            "instrumented-io",
            titled(Some(IconName::Database), "Instrumented application I/O", t),
            Some(io_action),
            [io],
            t,
        ));
        sections
    }

    /// Live Processes, Resource History and the trace sections of `DiagnosticsSettingsBody`.
    fn diagnostics_sections(
        &self,
        state: &Entity<DiagnosticsState>,
        client: &RpcClient,
        t: &Tokens,
        cx: &App,
    ) -> Vec<AnyElement> {
        let s = state.read(cx);
        let now = now_ms();
        let maintain = s.grants(ENVIRONMENT_MAINTAIN);
        let dash = || "...".to_owned();
        let refresh =
            |id: &'static str,
             label: &'static str,
             pending: bool,
             load: fn(&mut DiagnosticsState, RpcClient, &mut Context<DiagnosticsState>)| {
                let (state, client) = (state.clone(), client.clone());
                icon_button(id, IconName::RefreshCw, label, pending, t, move |_, cx| {
                    state.update(cx, |s, cx| load(s, client.clone(), cx))
                })
            };

        // Live Processes
        let data = s.processes.data.as_ref();
        let action = h_flex()
            .gap(px(6.))
            .child(last_checked(data.map(|d| d.read_at.as_str()), now, t))
            .child(refresh(
                "processes-refresh",
                "Refresh process diagnostics",
                s.processes.pending,
                DiagnosticsState::load_processes,
            ))
            .into_any_element();
        let mut blocks = vec![stats_grid(
            [
                ("CHILD PROCESSES", data.map_or_else(dash, |d| format_count(d.process_count)), None, Tone::Default),
                (
                    "CPU",
                    data.map_or_else(dash, |d| percent(d.total_cpu_percent)),
                    Some("Total CPU across live child processes of the current server process. The desktop shell and other parent processes are not included.".into()),
                    Tone::Default,
                ),
                (
                    "MEMORY",
                    data.map_or_else(dash, |d| super::system::format_bytes(d.total_rss_bytes)),
                    Some("Total resident memory across live child processes of the current server process. The desktop shell and other parent processes are not included.".into()),
                    Tone::Default,
                ),
                ("SERVER PID", data.map_or_else(dash, |d| d.server_pid.to_string()), None, Tone::Default),
            ],
            t,
        )];
        let mut errors = Vec::new();
        if let Some(message) = data.and_then(|d| some(&d.error)).and_then(|e| e["message"].as_str()) {
            errors.push(error_line("processes-data-error", message.to_owned().into(), t.error_foreground));
        }
        if let Some(error) = s.processes.error.clone() {
            errors.push(error_line("processes-error", error, t.error_foreground));
        }
        blocks.extend(errors_block(errors));
        let entries = data.map_or(&[][..], |d| &d.processes);
        let rows = visible_process_tree(entries, &s.collapsed_pids)
            .into_iter()
            .map(|p| {
                let pid = p.pid;
                let name = process_name(&p.command);
                let toggle = (!p.child_pids.is_empty()).then(|| {
                    let state = state.clone();
                    let on_toggle: Box<dyn Fn(&mut App)> = Box::new(move |cx| {
                        state.update(cx, |s, cx| {
                            if !s.collapsed_pids.remove(&pid) {
                                s.collapsed_pids.insert(pid);
                            }
                            cx.notify();
                        })
                    });
                    (!s.collapsed_pids.contains(&pid), on_toggle)
                });
                let (signal_state, signal_client) = (state.clone(), client.clone());
                let disabled = !maintain || s.signaling_pid == Some(pid);
                let actions = signal_actions(&format!("proc-{pid}"), disabled, t, move |signal, window, cx| {
                    let (state, client) = (signal_state.clone(), signal_client.clone());
                    let send =
                        move |cx: &mut App| state.update(cx, |s, cx| s.signal_live(client.clone(), pid, signal, cx));
                    if signal == "SIGKILL" {
                        confirm_kill(pid, window, cx, send);
                    } else {
                        send(cx);
                    }
                });
                (
                    ElementId::Name(format!("proc-{pid}").into()),
                    vec![
                        tree_name(
                            ElementId::Name(format!("proc-name-{pid}").into()),
                            p.depth.min(6),
                            toggle,
                            success().opacity(0.8),
                            name,
                            p.command.clone(),
                            t,
                        ),
                        mono(percent(p.cpu_percent), t).into_any_element(),
                        mono(super::system::format_bytes(p.rss_bytes), t).into_any_element(),
                        command_cell(ElementId::Name(format!("proc-cmd-{pid}").into()), &p.command, t.muted_foreground),
                        mono(pid.to_string(), t).text_color(t.muted_foreground).into_any_element(),
                        truncated(process_type(p.depth, &p.command), t.muted_foreground).into_any_element(),
                        actions,
                    ],
                )
            })
            .collect();
        let empty = if s.processes.pending && data.is_none() {
            "Loading live processes..."
        } else {
            "No live descendant processes found."
        };
        blocks.push(table(
            "processes-table",
            1040.,
            &[Col(0.24, L), Col(0.08, R), Col(0.10, R), Col(0.33, L), Col(0.08, R), Col(0.11, L), Col(0.06, R)],
            &["NAME", "CPU", "MEMORY", "COMMAND", "PID", "TYPE", "KILL"],
            rows,
            Some(empty.into()),
            (12., 8.),
            t,
        ));
        let mut sections = vec![section_with_action(
            "live-processes",
            "Live Processes",
            Some(action),
            [v_flex().children(divide(blocks, t)).into_any_element()],
            t,
        )];

        // Resource History
        let data = s.history.data.as_ref();
        let action = {
            let (pick_state, pick_client) = (state.clone(), client.clone());
            h_flex()
                .gap(px(6.))
                .child(segmented(
                    "process-window",
                    PROCESS_WINDOWS.iter().map(|w| w.0),
                    s.history_window,
                    t,
                    move |index, cx| {
                        pick_state.update(cx, |s, cx| {
                            s.history_window = index;
                            s.load_history(pick_client.clone(), cx);
                            cx.notify();
                        })
                    },
                ))
                .child(last_checked(data.map(|d| d.read_at.as_str()), now, t))
                .child(refresh(
                    "history-refresh",
                    "Refresh resource history",
                    s.history.pending,
                    DiagnosticsState::load_history,
                ))
                .into_any_element()
        };
        let mut blocks = vec![stats_grid(
            [
                (
                    "CPU TIME",
                    data.map_or_else(dash, |d| format_cpu_time(d.total_cpu_seconds_approx)),
                    Some("Approximate active CPU time for the T3 server root process and its descendants during the selected window. It grows only while sampled processes use CPU and older samples leave as the window moves.".into()),
                    Tone::Default,
                ),
                (
                    "SAMPLES",
                    data.map_or_else(dash, |d| format_count(d.retained_sample_count)),
                    Some("In-memory process samples retained by the server. The server samples only while diagnostics is open, and this resets when the server restarts.".into()),
                    Tone::Default,
                ),
                (
                    "INTERVAL",
                    data.map_or_else(dash, |d| {
                        if d.sample_interval_ms == 0. { "On demand".into() } else { format_duration(d.sample_interval_ms) }
                    }),
                    None,
                    Tone::Default,
                ),
                ("PROCESSES", data.map_or_else(dash, |d| format_count(d.top_processes.len() as u64)), None, Tone::Default),
            ],
            t,
        )];
        let mut errors = Vec::new();
        if let Some(message) = data.and_then(|d| some(&d.error)).and_then(|e| e["message"].as_str()) {
            errors.push(error_line("history-data-error", message.to_owned().into(), t.error_foreground));
        }
        if let Some(error) = s.history.error.clone() {
            errors.push(error_line("history-error", error, t.error_foreground));
        }
        blocks.extend(errors_block(errors));
        blocks.push(cpu_chart(data.map_or(&[][..], |d| &d.buckets), t));
        let entries = data.map_or(&[][..], |d| &d.top_processes);
        let depths = history_visual_depths(entries);
        let rows = entries
            .iter()
            .zip(depths)
            .map(|(p, depth)| {
                let key = &p.process_key;
                let dot = if p.is_server_root { t.warning.opacity(0.9) } else { success().opacity(0.8) };
                (
                    ElementId::Name(format!("hist-{key}").into()),
                    vec![
                        tree_name(
                            ElementId::Name(format!("hist-name-{key}").into()),
                            depth.min(6),
                            None,
                            dot,
                            short_process_name(&p.command),
                            p.command.clone(),
                            t,
                        ),
                        mono(format_cpu_time(p.cpu_seconds_approx), t).into_any_element(),
                        mono(percent(p.current_cpu_percent), t).into_any_element(),
                        mono(percent(p.avg_cpu_percent), t).into_any_element(),
                        mono(percent(p.max_cpu_percent), t).into_any_element(),
                        mono(super::system::format_bytes(p.max_rss_bytes), t).into_any_element(),
                        command_cell(ElementId::Name(format!("hist-cmd-{key}").into()), &p.command, t.muted_foreground),
                        mono(p.pid.to_string(), t).text_color(t.muted_foreground).into_any_element(),
                    ],
                )
            })
            .collect();
        let empty = if s.history.pending && data.is_none() {
            "Collecting process resource samples..."
        } else {
            "No process resource samples found for this window."
        };
        blocks.push(table(
            "history-table",
            980.,
            &[
                Col(0.24, L),
                Col(0.10, R),
                Col(0.10, R),
                Col(0.10, R),
                Col(0.10, R),
                Col(0.10, R),
                Col(0.16, L),
                Col(0.10, R),
            ],
            &["PROCESS", "CPU TIME", "CURRENT", "AVERAGE", "PEAK", "MAX MEM", "COMMAND", "PID"],
            rows,
            Some(empty.into()),
            (12., 8.),
            t,
        ));
        sections.push(section_with_action(
            "resource-history",
            "Resource History",
            Some(action),
            [v_flex().children(divide(blocks, t)).into_any_element()],
            t,
        ));

        // Trace Diagnostics
        let trace = s.trace.data.as_ref();
        let logs_path = self
            .server_info
            .config
            .pointer("/observability/logsDirectoryPath")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let open_logs = {
            let state = state.clone();
            let client = client.clone();
            let editor = self.logs_editor();
            let disabled =
                !s.grants(ORCHESTRATION_OPERATE) || logs_path.is_none() || s.opening_logs || editor.is_none();
            icon_button("open-logs", IconName::FolderOpen, "Open logs folder", disabled, t, move |_, cx| {
                let (Some(path), Some(editor)) = (logs_path.clone(), editor.clone()) else { return };
                let client = client.clone();
                state.update(cx, |s, cx| {
                    s.opening_logs = true;
                    s.logs_error = None;
                    cx.spawn(async move |this, cx| {
                        let result =
                            client.request("shell.openInEditor", json!({ "cwd": path, "editor": editor })).await;
                        let _ = this.update(cx, |s, cx| {
                            s.opening_logs = false;
                            if let Err(error) = result
                                && !matches!(error, RpcError::Disconnected(_))
                            {
                                s.logs_error = Some(rpc_message(&error).into());
                            }
                            cx.notify();
                        });
                    })
                    .detach();
                    cx.notify();
                })
            })
        };
        let action = h_flex()
            .gap(px(6.))
            .child(last_checked(trace.map(|d| d.read_at.as_str()), now, t))
            .child(open_logs)
            .child(refresh("trace-refresh", "Refresh trace diagnostics", s.trace.pending, DiagnosticsState::load_trace))
            .into_any_element();
        let mut blocks = vec![stats_grid(
            [
                ("SPANS", trace.map_or_else(dash, |d| format_count(d.record_count)), None, Tone::Default),
                (
                    "FAILURES",
                    trace.map_or_else(dash, |d| format_count(d.failure_count)),
                    None,
                    if trace.is_some_and(|d| d.failure_count > 0) { Tone::Danger } else { Tone::Default },
                ),
                (
                    "SLOW SPANS",
                    trace.map_or_else(dash, |d| format_count(d.slow_span_count)),
                    Some(trace.map_or_else(
                        || "Spans at or above the configured slow-span threshold.".to_owned(),
                        |d| {
                            format!("Spans with a duration of {} or longer.", format_duration(d.slow_span_threshold_ms))
                        },
                    )),
                    if trace.is_some_and(|d| d.slow_span_count > 0) { Tone::Warning } else { Tone::Default },
                ),
                (
                    "PARSE ERRORS",
                    trace.map_or_else(dash, |d| format_count(d.parse_error_count)),
                    None,
                    if trace.is_some_and(|d| d.parse_error_count > 0) { Tone::Warning } else { Tone::Default },
                ),
            ],
            t,
        )];
        let mut errors = Vec::new();
        if let Some(error) = s.logs_error.clone() {
            errors.push(error_line("logs-error", error, t.error_foreground));
        }
        if let Some(d) = trace
            && let Some(message) = d.error_message()
        {
            let (text, color) = if d.partial() {
                (
                    format!("Some trace files could not be read, so diagnostics may be incomplete. {message}"),
                    t.warning_foreground,
                )
            } else {
                (message.to_owned(), t.error_foreground)
            };
            errors.push(error_line("trace-data-error", text.into(), color));
        }
        if let Some(error) = s.trace.error.clone() {
            errors.push(error_line("trace-error", error, t.error_foreground));
        }
        blocks.extend(errors_block(errors));
        sections.push(section_with_action(
            "trace-diagnostics",
            "Trace Diagnostics",
            Some(action),
            [v_flex().children(divide(blocks, t)).into_any_element()],
            t,
        ));

        let loading = s.trace.pending && trace.is_none();
        let empty_rows = |label: &str| {
            div()
                .px(px(20.))
                .py(px(16.))
                .text_xs()
                .text_color(t.muted_foreground)
                .child(label.to_owned())
                .into_any_element()
        };
        let trace_pad = (16., 12.);
        let relative = |iso: &str| relative_label(iso, now).unwrap_or_default();

        // Latest Failures
        let body = match trace.filter(|d| !d.latest_failures.is_empty()) {
            Some(d) => table(
                "latest-failures",
                640.,
                &[Col(0.30, L), Col(0.46, L), Col(0.12, L), Col(0.12, L)],
                &["SPAN", "CAUSE", "DURATION", "ENDED"],
                d.latest_failures
                    .iter()
                    .map(|f| {
                        let key = format!("lf-{}:{}", f.trace_id, f.span_id);
                        (
                            ElementId::Name(key.clone().into()),
                            vec![
                                div().font_weight(FontWeight::MEDIUM).child(f.name.clone()).into_any_element(),
                                div()
                                    .text_color(t.muted_foreground)
                                    .child(expandable(
                                        state,
                                        key.clone(),
                                        &f.cause,
                                        3,
                                        "Show full error",
                                        s.expanded.contains(&key),
                                        t,
                                    ))
                                    .into_any_element(),
                                mono(format_duration(f.duration_ms), t).into_any_element(),
                                mono(relative(&f.ended_at), t).text_color(t.muted_foreground).into_any_element(),
                            ],
                        )
                    })
                    .collect(),
                None,
                trace_pad,
                t,
            ),
            None => empty_rows(if loading { "Loading failures..." } else { "No failed spans found." }),
        };
        sections.push(section_with_action("latest-failures-section", "Latest Failures", None, [body], t));

        // Most Common Failures
        let body = match trace.filter(|d| !d.common_failures.is_empty()) {
            Some(d) => table(
                "common-failures",
                760.,
                &[Col(0.30, L), Col(0.10, L), Col(0.46, L), Col(0.14, L)],
                &["SPAN", "COUNT", "CAUSE", "LAST SEEN"],
                d.common_failures
                    .iter()
                    .enumerate()
                    .map(|(index, f)| {
                        let key = format!("cf-{index}-{}", f.name);
                        (
                            ElementId::Name(key.clone().into()),
                            vec![
                                div().font_weight(FontWeight::MEDIUM).child(f.name.clone()).into_any_element(),
                                mono(format_count(f.count), t).into_any_element(),
                                div()
                                    .text_color(t.muted_foreground)
                                    .child(expandable(
                                        state,
                                        key.clone(),
                                        &f.cause,
                                        3,
                                        "Show full error",
                                        s.expanded.contains(&key),
                                        t,
                                    ))
                                    .into_any_element(),
                                mono(relative(&f.last_seen_at), t).text_color(t.muted_foreground).into_any_element(),
                            ],
                        )
                    })
                    .collect(),
                None,
                trace_pad,
                t,
            ),
            None => empty_rows(if loading { "Loading failure groups..." } else { "No repeated failures found." }),
        };
        sections.push(section_with_action("common-failures-section", "Most Common Failures", None, [body], t));

        // Slowest Spans
        let body = match trace.filter(|d| !d.slowest_spans.is_empty()) {
            Some(d) => table(
                "slowest-spans",
                900.,
                &[Col(0.44, L), Col(0.14, L), Col(0.12, L), Col(0.30, L)],
                &["SPAN", "DURATION", "ENDED", "TRACE"],
                d.slowest_spans
                    .iter()
                    .map(|span| {
                        let key = format!("ss-{}:{}", span.trace_id, span.span_id);
                        let copied = s.copied_trace.as_deref() == Some(span.trace_id.as_str());
                        (
                            ElementId::Name(key.clone().into()),
                            vec![
                                div().font_weight(FontWeight::MEDIUM).child(span.name.clone()).into_any_element(),
                                mono(format_duration(span.duration_ms), t).into_any_element(),
                                mono(relative(&span.ended_at), t).text_color(t.muted_foreground).into_any_element(),
                                trace_id_cell(state, &key, &span.trace_id, copied, t),
                            ],
                        )
                    })
                    .collect(),
                None,
                trace_pad,
                t,
            ),
            None => empty_rows(if loading { "Loading slow spans..." } else { "No spans found." }),
        };
        sections.push(section_with_action("slowest-spans-section", "Slowest Spans", None, [body], t));

        // Span Logs
        let body = match trace.filter(|d| !d.latest_warning_and_error_logs.is_empty()) {
            Some(d) => table(
                "span-logs",
                920.,
                &[Col(0.11, L), Col(0.09, L), Col(0.24, L), Col(0.26, L), Col(0.30, L)],
                &["TIME", "LEVEL", "SPAN", "MESSAGE", "TRACE"],
                d.latest_warning_and_error_logs
                    .iter()
                    .enumerate()
                    .map(|(index, e)| {
                        let key = format!("log-{index}-{}:{}", e.trace_id, e.span_id);
                        let copied = s.copied_trace.as_deref() == Some(e.trace_id.as_str());
                        (
                            ElementId::Name(key.clone().into()),
                            vec![
                                mono(relative(&e.seen_at), t).text_color(t.muted_foreground).into_any_element(),
                                text_2xs(div().px(px(6.)).py(px(2.)).rounded(px(4.)).bg(t.muted))
                                    .font_family(t.fonts.mono)
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(t.foreground.opacity(0.8))
                                    .child(e.level.to_uppercase())
                                    .into_any_element(),
                                truncated(e.span_name.clone(), t.foreground)
                                    .font_weight(FontWeight::MEDIUM)
                                    .into_any_element(),
                                div()
                                    .text_color(t.muted_foreground)
                                    .child(expandable(
                                        state,
                                        key.clone(),
                                        &e.message,
                                        2,
                                        "Show full message",
                                        s.expanded.contains(&key),
                                        t,
                                    ))
                                    .into_any_element(),
                                trace_id_cell(state, &key, &e.trace_id, copied, t),
                            ],
                        )
                    })
                    .collect(),
                None,
                trace_pad,
                t,
            ),
            None => empty_rows(if loading { "Loading recent logs..." } else { "No warnings or errors found." }),
        };
        sections.push(section_with_action("span-logs-section", "Span Logs", None, [body], t));

        // Top Span Names
        let body = match trace.filter(|d| !d.top_spans_by_count.is_empty()) {
            Some(d) => table(
                "top-spans",
                760.,
                &[Col(0.48, L), Col(0.13, L), Col(0.13, L), Col(0.13, L), Col(0.13, L)],
                &["SPAN", "COUNT", "FAILURES", "AVERAGE", "MAX"],
                d.top_spans_by_count
                    .iter()
                    .map(|span| {
                        (
                            ElementId::Name(format!("top-{}", span.name).into()),
                            vec![
                                div().font_weight(FontWeight::MEDIUM).child(span.name.clone()).into_any_element(),
                                mono(format_count(span.count), t).into_any_element(),
                                mono(format_count(span.failure_count), t).into_any_element(),
                                mono(format_duration(span.average_duration_ms), t).into_any_element(),
                                mono(format_duration(span.max_duration_ms), t).into_any_element(),
                            ],
                        )
                    })
                    .collect(),
                None,
                trace_pad,
                t,
            ),
            None => empty_rows(if loading { "Loading span names..." } else { "No spans found." }),
        };
        sections.push(section_with_action("top-spans-section", "Top Span Names", None, [body], t));
        sections
    }

    /// The editor "Open logs folder" uses: the last one picked, else the first available.
    fn logs_editor(&self) -> Option<String> {
        let available = &self.server_info.available_editors;
        self.preferred_editor
            .as_deref()
            .filter(|editor| available.iter().any(|id| id == editor))
            .or_else(|| available.first().map(String::as_str))
            .map(str::to_owned)
    }
}

/// `seconds.toFixed(2).replace(/\.0+$/, "")`.
fn format_seconds(ms: f64) -> String {
    let text = format!("{:.2}", ms / 1_000.);
    text.strip_suffix(".00").map_or(text.clone(), str::to_owned)
}

/// Blocks separated by the card's hairlines.
fn divide(blocks: Vec<AnyElement>, t: &Tokens) -> Vec<AnyElement> {
    blocks
        .into_iter()
        .enumerate()
        .map(|(i, block)| {
            div().when(i > 0, |el| el.border_t_1().border_color(t.border.opacity(0.6))).child(block).into_any_element()
        })
        .collect()
}

/// `SourceStatusBadge`.
fn status_badge(label: &str, status: &str, neutral: Option<&'static str>, t: &Tokens) -> AnyElement {
    let tone = if neutral.is_some() { Tone::Neutral } else { source_status_tone(status) };
    let (border, bg, fg, dot) = match tone {
        Tone::Neutral => {
            (t.border.opacity(0.7), t.muted.opacity(0.45), t.muted_foreground, t.muted_foreground.opacity(0.55))
        }
        Tone::Default => (success().opacity(0.25), success().opacity(0.1), t.success_foreground, success()),
        Tone::Warning => (t.warning.opacity(0.3), t.warning.opacity(0.1), t.warning_foreground, t.warning),
        Tone::Danger => (t.error.opacity(0.3), t.error.opacity(0.1), t.error_foreground, t.error),
    };
    text_3xs(h_flex().flex_none().gap(px(6.)).rounded_full().border_1().px(px(10.)).py(px(4.)))
        .font_weight(FontWeight::SEMIBOLD)
        .border_color(border)
        .bg(bg)
        .text_color(fg)
        .child(div().size(px(6.)).rounded_full().bg(dot))
        .child(format!("{label}{}", neutral.unwrap_or(status)).to_uppercase())
        .into_any_element()
}

/// `HealthSource`.
fn health_source(label: &'static str, health: &SourceHealth, t: &Tokens) -> AnyElement {
    let desktop_only = expected_outside_desktop(health);
    let detail = if desktop_only {
        "Available when this page runs inside the desktop app.".to_owned()
    } else {
        some(&health.last_error).and_then(Value::as_str).unwrap_or("No reported errors").to_owned()
    };
    h_flex()
        .items_start()
        .justify_between()
        .gap(px(16.))
        .py(px(12.))
        .child(
            v_flex()
                .min_w_0()
                .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(label))
                .child(text_2xs(div().mt(px(4.))).text_color(t.muted_foreground.opacity(0.65)).child(detail)),
        )
        .child(status_badge("", &health.status, desktop_only.then_some("Desktop only"), t))
        .into_any_element()
}

/// `AggregateCard`.
fn aggregate_card(label: &'static str, accent: Hsla, aggregate: &Aggregate, t: &Tokens) -> gpui_kit::Div {
    let pair = |label: &'static str, value: String| {
        v_flex()
            .min_w_0()
            .child(
                text_3xs(div())
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(t.muted_foreground.opacity(0.45))
                    .child(label),
            )
            .child(
                div()
                    .truncate()
                    .font_family(t.fonts.mono)
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(t.foreground.opacity(0.9))
                    .child(value),
            )
    };
    let count = aggregate.process_count;
    v_flex()
        .relative()
        .flex_1()
        .min_w_0()
        .px(px(20.))
        .py(px(16.))
        .child(div().absolute().top_0().left(px(20.)).right(px(20.)).h(px(2.)).rounded_full().bg(accent))
        .child(
            h_flex()
                .justify_between()
                .gap(px(12.))
                .child(
                    text_3xs(div())
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(t.muted_foreground.opacity(0.75))
                        .child(label),
                )
                .child(
                    text_3xs(div().rounded(px(6.)).bg(t.muted.opacity(0.55)).px(px(6.)).py(px(2.)))
                        .font_family(t.fonts.mono)
                        .text_color(t.muted_foreground.opacity(0.7))
                        .child(format!("{count} {}", if count == 1 { "process" } else { "processes" })),
                ),
        )
        .child(
            v_flex()
                .mt(px(14.))
                .gap(px(10.))
                .child(
                    h_flex()
                        .gap(px(16.))
                        .child(pair("CPU", percent(aggregate.current_cpu_percent)).flex_1())
                        .child(pair("MEMORY", super::system::format_bytes(aggregate.current_rss_bytes)).flex_1()),
                )
                .child(
                    h_flex()
                        .gap(px(16.))
                        .child(pair("READ", format_rate(aggregate.io_read_bytes_per_second)).flex_1())
                        .child(pair("WRITE", format_rate(aggregate.io_write_bytes_per_second)).flex_1()),
                ),
        )
}

/// `ProcessResourceHistoryChart`: peak and average CPU per bucket.
fn cpu_chart(buckets: &[CpuBucket], t: &Tokens) -> AnyElement {
    let max = scale_max(buckets.iter().map(|b| b.max_cpu_percent));
    let (peak, average) = (t.foreground.opacity(0.15), t.foreground.opacity(0.6));
    div()
        .px(px(20.))
        .py(px(12.))
        .child(
            h_flex()
                .h(px(112.))
                .items_end()
                .gap(px(4.))
                .overflow_hidden()
                .rounded(px(2.))
                .bg(t.muted.opacity(0.1))
                .p(px(8.))
                .children(buckets.iter().map(|b| {
                    let peak_h = 2f64.max(b.max_cpu_percent / max * 100.);
                    let avg_h = 2f64.max(b.avg_cpu_percent / max * 100.);
                    let tip = format!("Avg {:.1}%, peak {:.1}%", b.avg_cpu_percent, b.max_cpu_percent);
                    div()
                        .id(ElementId::Name(format!("cpu-{}", b.started_at).into()))
                        .relative()
                        .h_full()
                        .flex_1()
                        .min_w(px(4.))
                        .child(
                            div()
                                .absolute()
                                .bottom_0()
                                .left_0()
                                .right_0()
                                .h(relative((peak_h / 100.) as f32))
                                .rounded_t(px(2.))
                                .bg(peak),
                        )
                        .child(
                            div()
                                .absolute()
                                .bottom_0()
                                .left_0()
                                .right_0()
                                .h(relative((avg_h / 100.) as f32))
                                .rounded_t(px(2.))
                                .bg(average),
                        )
                        .tooltip(tooltip(tip))
                })),
        )
        .into_any_element()
}

/// `ResourceHistoryChart`: CPU average, reads and writes per bucket.
fn telemetry_chart(buckets: &[TelemetryBucket], t: &Tokens) -> AnyElement {
    let max_cpu = scale_max(buckets.iter().map(|b| b.avg_cpu_percent));
    let max_io = scale_max(buckets.iter().map(|b| b.io_read_bytes + b.io_write_bytes));
    let (cpu, read, write) = (t.foreground.opacity(0.65), t.info.opacity(0.7), t.warning.opacity(0.8));
    let legend = |color: Hsla, label: &'static str| {
        h_flex().gap(px(6.)).child(div().h(px(6.)).w(px(12.)).rounded_full().bg(color)).child(label)
    };
    let bar = |height: f64, color: Hsla| {
        v_flex()
            .flex_1()
            .h_full()
            .justify_end()
            .child(div().w_full().h(relative((height / 100.) as f32)).rounded_t(px(2.)).bg(color))
    };
    v_flex()
        .px(px(20.))
        .py(px(16.))
        .child(
            text_3xs(h_flex().mb(px(12.)).gap(px(16.)))
                .text_color(t.muted_foreground.opacity(0.65))
                .child(legend(t.foreground.opacity(0.7), "CPU average"))
                .child(legend(read, "I/O reads"))
                .child(legend(write, "I/O writes")),
        )
        .child(
            h_flex()
                .h(px(128.))
                .items_end()
                .gap(px(4.))
                .overflow_hidden()
                .rounded(px(8.))
                .border_1()
                .border_color(t.border.opacity(0.4))
                .bg(t.muted.opacity(0.08))
                .px(px(8.))
                .pt(px(12.))
                .pb(px(8.))
                .children(buckets.iter().map(|b| {
                    let tip = format!(
                        "CPU avg {:.1}%\nCPU peak {:.1}%\nRead {}\nWrite {}",
                        b.avg_cpu_percent,
                        b.max_cpu_percent,
                        super::system::format_bytes(b.io_read_bytes),
                        super::system::format_bytes(b.io_write_bytes)
                    );
                    h_flex()
                        .id(ElementId::Name(format!("tl-bucket-{}", b.started_at).into()))
                        .h_full()
                        .flex_1()
                        .min_w(px(4.))
                        .items_end()
                        .gap(px(1.))
                        .child(bar(bar_height(b.avg_cpu_percent, max_cpu, 2.), cpu))
                        .child(bar(bar_height(b.io_read_bytes, max_io, 1.), read))
                        .child(bar(bar_height(b.io_write_bytes, max_io, 1.), write))
                        .tooltip(tooltip(tip))
                })),
        )
        .into_any_element()
}
