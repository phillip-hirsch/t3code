//! Agent browser tabs: tabs in the server's own Chromium (`runtime: "server"`), streamed over
//! `/api/preview-stream/ws` as `ServerBrowserSurface.tsx` and `serverBrowserStream.ts` do. JPEG
//! frames are decoded off the UI thread and only the latest is painted; input reaches the page
//! once this viewer has taken control.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use futures::{SinkExt as _, StreamExt as _};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Bounds, ClickEvent, Context, FocusHandle, InteractiveElement as _, IntoElement, KeyDownEvent,
    KeyUpEvent, Keystroke, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ObjectFit,
    ParentElement as _, Pixels, Point, RenderImage, ScrollDelta, ScrollEvent, SharedString, Styled as _,
    StyledImage as _, Task, Window, canvas, div, img, px,
};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};

use super::super::Workspace;
use super::super::right_panel::Surface;
use crate::preview::{Session, Viewport};
use crate::theme::Tokens;

const TAB_GONE: u16 = 4404;
/// `PREVIEW_STREAM_HOST_SETUP_CLOSE_CODE`.
const HOST_SETUP: u16 = 4503;
const WHEEL_LINE_PX: f32 = 16.;

/// `PreviewStreamControl`.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Control {
    can_operate: bool,
    controller: String,
    dialog: Option<Dialog>,
}

#[derive(Clone, Debug, PartialEq)]
struct Dialog {
    kind: String,
    message: String,
    default_value: String,
}

impl Control {
    fn parse(value: &Value) -> Option<Self> {
        let controller = value["controller"].as_str()?;
        if !matches!(controller, "agent" | "you" | "another-viewer" | "unclaimed") {
            return None;
        }
        let dialog = match &value["dialog"] {
            Value::Null => None,
            dialog => Some(Dialog {
                kind: dialog["type"].as_str()?.to_owned(),
                message: dialog["message"].as_str()?.to_owned(),
                default_value: dialog["defaultValue"].as_str()?.to_owned(),
            }),
        };
        Some(Self { can_operate: value["canOperate"].as_bool()?, controller: controller.to_owned(), dialog })
    }

    fn mine(&self) -> bool {
        self.controller == "you"
    }
}

/// `previewStreamControlLabel`.
fn control_label(control: Option<&Control>) -> &'static str {
    match control {
        None => "Connecting...",
        Some(control) if !control.can_operate => "Read-only",
        Some(control) => match control.controller.as_str() {
            "you" => "You have control",
            "agent" => "Agent has control",
            "another-viewer" => "Another viewer has control",
            _ => "Watching",
        },
    }
}

/// What the stream's socket last told the view.
enum Incoming {
    Connected(bool),
    Frame(Arc<RenderImage>, (u32, u32)),
    Message(Value),
    /// The socket stopped for good: the tab closed, the host needs setup, or access was refused.
    Stopped(Stop),
}

#[derive(Clone, Debug, PartialEq)]
enum Stop {
    Gone,
    HostSetup { message: &'static str, command: String },
    Refused,
}

/// `decodeHostSetup` + `previewStreamHostSetupMessage`.
fn host_setup(reason: &str) -> Stop {
    let value: Value = serde_json::from_str(reason).unwrap_or_default();
    let (need, command) = match (value["need"].as_str(), value["command"].as_str()) {
        (Some(need @ ("sandbox" | "libraries")), Some(command)) => (need, command),
        _ => ("sandbox", "sudo npx t3 browser setup"),
    };
    let message = if need == "sandbox" {
        "This server's host blocks the sandbox its browser runs in. Run this once on the host, then try again:"
    } else {
        "This server's host is missing libraries its browser needs. Run this once on the host, then try again:"
    };
    Stop::HostSetup { message, command: command.to_owned() }
}

/// One agent tab's stream, live while the panel shows it.
pub(super) struct ServerStream {
    frame: Option<Arc<RenderImage>>,
    /// The frame's size in device px.
    frame_size: (u32, u32),
    /// Frames replaced since the last paint, whose textures the window still holds.
    stale: Vec<Arc<RenderImage>>,
    /// The page's CSS viewport.
    viewport: Option<(f32, f32)>,
    control: Option<Control>,
    connected: bool,
    stopped: Option<Stop>,
    /// The agent's pointer in page px, while the agent drives the page.
    agent_pointer: Option<(f32, f32)>,
    outgoing: mpsc::UnboundedSender<String>,
    /// Where the frame area was laid out last frame.
    bounds: Rc<Cell<Bounds<Pixels>>>,
    sent_size: Option<(u32, u32)>,
    buttons: u8,
    focus: FocusHandle,
    _task: Task<()>,
}

impl ServerStream {
    /// `PreviewStreamClient.send`: only the controller sends, except the claim itself.
    fn send(&self, input: Value) -> bool {
        let Some(control) = self.control.as_ref().filter(|c| self.connected && c.can_operate) else { return false };
        if input["type"] != "takeControl" && !control.mine() {
            return false;
        }
        self.outgoing.send(input.to_string()).is_ok()
    }
}

#[derive(Default)]
pub(super) struct ServerStreams {
    streams: HashMap<String, ServerStream>,
    /// The agent tab the panel drew this frame.
    pub(super) drawn: Option<String>,
}

impl Workspace {
    /// Keeps the stream of the agent tab on screen and closes the others.
    pub(super) fn retain_server_streams(&mut self, cx: &mut App) {
        let drawn = self.preview.server.drawn.take();
        self.preview.server.streams.retain(|tab, stream| {
            let keep = drawn.as_ref() == Some(tab);
            if !keep {
                for image in stream.stale.drain(..).chain(stream.frame.take()) {
                    cx.drop_image(image, None);
                }
            }
            keep
        });
    }

    fn ensure_server_stream(&mut self, thread_id: &str, tab_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.preview.server.streams.contains_key(tab_id) {
            return;
        }
        let Some(endpoint) = self.endpoint() else { return };
        let scale = window.scale_factor();
        let size = window.viewport_size();
        let max = |px: Pixels| ((f32::from(px) * scale).round() as u32).max(1).to_string();
        let query = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("threadId", thread_id)
            .append_pair("tabId", tab_id)
            .append_pair("maxWidth", &max(size.width))
            .append_pair("maxHeight", &max(size.height))
            .finish();
        let ws_base = endpoint.http_base.replacen("http", "ws", 1);
        let url = format!("{ws_base}/api/preview-stream/ws?{query}");
        let token = endpoint.bearer_token.clone();
        let (outgoing, outgoing_rx) = mpsc::unbounded_channel();
        let (events_tx, events) = async_channel::unbounded();
        crate::runtime().spawn(run_stream(url, token, events_tx, outgoing_rx));
        let tab = tab_id.to_owned();
        let task = cx.spawn_in(window, async move |this, cx| {
            while let Ok(first) = events.recv().await {
                // The latest frame wins; older ones in the batch are never painted.
                let mut batch = vec![first];
                batch.extend(std::iter::from_fn(|| events.try_recv().ok()));
                let alive = this.update_in(cx, |this, window, cx| {
                    for event in batch {
                        this.on_server_stream(&tab, event, window, cx);
                    }
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
            }
        });
        let stream = ServerStream {
            frame: None,
            frame_size: (0, 0),
            stale: Vec::new(),
            viewport: None,
            control: None,
            connected: false,
            stopped: None,
            agent_pointer: None,
            outgoing,
            bounds: Rc::default(),
            sent_size: None,
            buttons: 0,
            focus: cx.focus_handle(),
            _task: task,
        };
        self.preview.server.streams.insert(tab_id.to_owned(), stream);
    }

    fn on_server_stream(&mut self, tab_id: &str, event: Incoming, window: &mut Window, cx: &mut Context<Self>) {
        let Some(stream) = self.preview.server.streams.get_mut(tab_id) else { return };
        match event {
            Incoming::Connected(connected) => {
                stream.connected = connected;
                if !connected {
                    stream.control = None;
                    stream.buttons = 0;
                }
            }
            Incoming::Frame(image, size) => {
                stream.stale.extend(stream.frame.replace(image));
                stream.frame_size = size;
                let _ = stream.outgoing.send(ACK.to_owned());
            }
            Incoming::Stopped(stop) => {
                stream.stopped = Some(stop);
                stream.connected = false;
                stream.control = None;
            }
            Incoming::Message(message) => match message["type"].as_str() {
                Some("control") => {
                    let Some(control) = Control::parse(&message) else { return };
                    let gained = control.mine() && !stream.control.as_ref().is_some_and(Control::mine);
                    stream.control = Some(control);
                    if gained {
                        stream.sent_size = None;
                    }
                    if !stream.control.as_ref().is_some_and(Control::mine) {
                        stream.buttons = 0;
                    }
                }
                Some("viewport") => {
                    if let (Some(width), Some(height)) = (message["width"].as_f64(), message["height"].as_f64()) {
                        stream.viewport = Some((width as f32, height as f32));
                    }
                }
                Some("pointer") => {
                    if let (Some(x), Some(y)) = (message["x"].as_f64(), message["y"].as_f64()) {
                        stream.agent_pointer = Some((x as f32, y as f32));
                    }
                }
                Some("clipboard") => {
                    if let Some(text) = message["text"].as_str() {
                        cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(text.to_owned()));
                    }
                }
                // `showPreviewPopup`: a page's new window opens as its own tab.
                Some("popup") => {
                    if let Some(popup) = message["tabId"].as_str() {
                        self.open_surface(Surface::Browser(popup.to_owned()), window, cx);
                    }
                }
                _ => {}
            },
        }
    }

    /// Sends `input` to the agent tab on screen, if this viewer may.
    pub(super) fn send_server_input(&self, tab_id: &str, input: Value) -> bool {
        self.preview.server.streams.get(tab_id).is_some_and(|stream| stream.send(input))
    }

    /// The agent tab's page: the control bar over its latest frame.
    pub(super) fn render_server_page(
        &mut self,
        thread_id: &str,
        tab_id: &str,
        follow_size: bool,
        tokens: &Tokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.ensure_server_stream(thread_id, tab_id, window, cx);
        self.preview.server.drawn = Some(tab_id.to_owned());
        let Some(stream) = self.preview.server.streams.get_mut(tab_id) else {
            return div().flex_1().into_any_element();
        };
        for image in stream.stale.drain(..) {
            cx.drop_image(image, Some(window));
        }
        // The page follows the panel's size while this viewer drives it (`followSize`).
        let bounds = stream.bounds.get();
        let size = (f32::from(bounds.size.width).round() as u32, f32::from(bounds.size.height).round() as u32);
        if follow_size
            && size.0 > 0
            && stream.control.as_ref().is_some_and(Control::mine)
            && stream.sent_size != Some(size)
        {
            stream.sent_size = Some(size);
            stream.send(json!({"type": "resize", "width": size.0, "height": size.1}));
        }
        let stream = &self.preview.server.streams[tab_id];
        let control = stream.control.clone();
        let mine = control.as_ref().is_some_and(Control::mine);
        let bar = h_flex()
            .flex_none()
            .justify_between()
            .gap_2()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(tokens.border)
            .bg(tokens.background)
            .child(div().text_xs().text_color(tokens.muted_foreground).child(control_label(control.as_ref())))
            .children(control.as_ref().filter(|c| c.can_operate).map(|control| {
                let tab = tab_id.to_owned();
                let claim = if mine { "releaseControl" } else { "takeControl" };
                Button::new("server-browser-control")
                    .outline()
                    .xsmall()
                    .label(if mine { "Release control" } else { "Take control" })
                    .disabled(control.controller == "another-viewer")
                    .on_click(cx.listener(move |this, _, _, _| {
                        this.send_server_input(&tab, json!({"type": claim}));
                    }))
            }));
        let fit = page_fit(stream.bounds.get(), stream.frame_size, stream.viewport);
        let cursor = stream.agent_pointer.filter(|_| !mine).zip(fit).map(|((x, y), fit)| fit.to_local(x, y)).map(
            |(left, top)| {
                div()
                    .absolute()
                    .left(px(left - 6.))
                    .top(px(top - 6.))
                    .size(px(12.))
                    .rounded_full()
                    .border_2()
                    .border_color(tokens.background)
                    .bg(tokens.primary)
            },
        );
        let frame = stream.frame.clone().map(|frame| img(frame).size_full().object_fit(ObjectFit::Contain));
        let bounds_cell = stream.bounds.clone();
        let tab = tab_id.to_owned();
        let (down_tab, up_tab, move_tab, wheel_tab, key_tab, key_up_tab) =
            (tab.clone(), tab.clone(), tab.clone(), tab.clone(), tab.clone(), tab.clone());
        let body = div()
            .id("server-browser-page")
            .relative()
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .track_focus(&stream.focus)
            .child(canvas(move |bounds, _, _| bounds_cell.set(bounds), |_, _, _, _| {}).absolute().size_full())
            .children(frame)
            .children(cursor)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, e: &MouseDownEvent, w, cx| this.server_mouse_down(&down_tab, e, w, cx)),
            )
            .on_mouse_down(MouseButton::Right, {
                let tab = tab.clone();
                cx.listener(move |this, e: &MouseDownEvent, w, cx| this.server_mouse_down(&tab, e, w, cx))
            })
            .on_mouse_down(MouseButton::Middle, {
                let tab = tab.clone();
                cx.listener(move |this, e: &MouseDownEvent, w, cx| this.server_mouse_down(&tab, e, w, cx))
            })
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, e: &MouseUpEvent, _, cx| this.server_mouse_up(&up_tab, e, cx)),
            )
            .on_mouse_up(MouseButton::Right, {
                let tab = tab.clone();
                cx.listener(move |this, e: &MouseUpEvent, _, cx| this.server_mouse_up(&tab, e, cx))
            })
            .on_mouse_up(
                MouseButton::Middle,
                cx.listener(move |this, e: &MouseUpEvent, _, cx| this.server_mouse_up(&tab, e, cx)),
            )
            .on_mouse_move(cx.listener(move |this, e: &MouseMoveEvent, _, _| this.server_mouse_move(&move_tab, e)))
            .on_scroll(cx.listener(move |this, e: &ScrollEvent, _, cx| {
                if this.server_wheel(&wheel_tab, e) {
                    cx.stop_propagation();
                }
            }))
            .on_key_down(cx.listener(move |this, e: &KeyDownEvent, _, cx| {
                if this.server_key(&key_tab, "down", &e.keystroke, cx) {
                    cx.stop_propagation();
                }
            }))
            .on_key_up(cx.listener(move |this, e: &KeyUpEvent, _, cx| {
                if this.server_key(&key_up_tab, "up", &e.keystroke, cx) {
                    cx.stop_propagation();
                }
            }));
        let overlay = self.render_server_overlay(tab_id, tokens, cx);
        v_flex().flex_1().min_h_0().relative().child(bar).child(body.children(overlay)).into_any_element()
    }

    /// The page's dialog, or the stop that ended the stream.
    fn render_server_overlay(&self, tab_id: &str, tokens: &Tokens, cx: &mut Context<Self>) -> Option<AnyElement> {
        let stream = self.preview.server.streams.get(tab_id)?;
        let card = || {
            v_flex()
                .absolute()
                .left_2()
                .right_2()
                .top_2()
                .gap_2()
                .p_3()
                .rounded(px(8.))
                .border_1()
                .border_color(tokens.border)
                .bg(tokens.background)
                .shadow_lg()
        };
        let cover = || {
            v_flex()
                .absolute()
                .inset_0()
                .items_center()
                .justify_center()
                .gap_3()
                .p_4()
                .bg(tokens.background)
                .text_center()
        };
        let tab = tab_id.to_owned();
        match &stream.stopped {
            Some(Stop::HostSetup { message, command }) => {
                return Some(
                    cover()
                        .child(div().max_w(px(384.)).text_sm().text_color(tokens.muted_foreground).child(*message))
                        .child(
                            div()
                                .w_full()
                                .max_w(px(448.))
                                .p_2()
                                .rounded(px(6.))
                                .bg(tokens.muted)
                                .font_family(tokens.fonts.mono)
                                .text_xs()
                                .text_left()
                                .child(command.clone()),
                        )
                        .child(
                            Button::new("server-browser-retry")
                                .outline()
                                .small()
                                .label("Try again")
                                .on_click(cx.listener(move |this, _, _, cx| this.restart_server_stream(&tab, cx))),
                        )
                        .into_any_element(),
                );
            }
            Some(Stop::Refused) => {
                return Some(
                    cover()
                        .gap_2()
                        .child(
                            div()
                                .text_xs()
                                .text_color(tokens.muted_foreground)
                                .child("Browser connection was refused."),
                        )
                        .child(
                            Button::new("server-browser-reconnect")
                                .outline()
                                .small()
                                .label("Reconnect")
                                .on_click(cx.listener(move |this, _, _, cx| this.restart_server_stream(&tab, cx))),
                        )
                        .into_any_element(),
                );
            }
            Some(Stop::Gone) | None => {}
        }
        let control = stream.control.as_ref()?;
        let dialog = control.dialog.as_ref()?;
        let respond = |id: &'static str, label: &'static str, accept: bool, cx: &mut Context<Self>| {
            let (tab, prompt) = (tab.clone(), dialog.default_value.clone());
            let is_prompt = dialog.kind == "prompt";
            let button = Button::new(id).small().label(label);
            let button = if accept { button.primary() } else { button.outline() };
            button.on_click(cx.listener(move |this, _: &ClickEvent, _, _| {
                let mut input = json!({"type": "dialog", "accept": accept});
                if accept && is_prompt {
                    input["promptText"] = prompt.clone().into();
                }
                this.send_server_input(&tab, input);
            }))
        };
        Some(
            card()
                .child(div().text_sm().child(SharedString::from(dialog.message.clone())))
                .map(|el| {
                    if control.mine() {
                        el.child(
                            h_flex()
                                .justify_end()
                                .gap_2()
                                .child(respond("server-dialog-dismiss", "Dismiss", false, cx))
                                .child(respond("server-dialog-accept", "Accept", true, cx)),
                        )
                    } else {
                        el.child(div().text_xs().text_color(tokens.muted_foreground).child("Take control to respond."))
                    }
                })
                .into_any_element(),
        )
    }

    fn restart_server_stream(&mut self, tab_id: &str, cx: &mut Context<Self>) {
        if let Some(stream) = self.preview.server.streams.remove(tab_id) {
            for image in stream.stale.into_iter().chain(stream.frame) {
                cx.drop_image(image, None);
            }
        }
        cx.notify();
    }

    fn server_point(&self, tab_id: &str, position: Point<Pixels>, captured: bool) -> Option<(f32, f32)> {
        let stream = self.preview.server.streams.get(tab_id)?;
        page_fit(stream.bounds.get(), stream.frame_size, stream.viewport)?.to_page(position, captured)
    }

    fn server_mouse_down(&mut self, tab_id: &str, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(stream) = self.preview.server.streams.get(tab_id) else { return };
        if !stream.control.as_ref().is_some_and(Control::mine) {
            return;
        }
        window.focus(&stream.focus.clone(), cx);
        let Some((button, bit)) = mouse_button(event.button) else { return };
        let Some((x, y)) = self.server_point(tab_id, event.position, false) else { return };
        let Some(stream) = self.preview.server.streams.get_mut(tab_id) else { return };
        stream.buttons |= bit;
        stream.send(json!({
            "type": "mouse", "action": "down", "x": x, "y": y, "button": button, "buttons": stream.buttons,
            "clickCount": event.click_count, "modifiers": modifier_bits(&event.modifiers),
        }));
        cx.stop_propagation();
    }

    fn server_mouse_up(&mut self, tab_id: &str, event: &MouseUpEvent, cx: &mut Context<Self>) {
        let Some((button, bit)) = mouse_button(event.button) else { return };
        if !self.preview.server.streams.get(tab_id).is_some_and(|s| s.buttons & bit != 0) {
            return;
        }
        let Some((x, y)) = self.server_point(tab_id, event.position, true) else { return };
        let Some(stream) = self.preview.server.streams.get_mut(tab_id) else { return };
        stream.buttons &= !bit;
        stream.send(json!({
            "type": "mouse", "action": "up", "x": x, "y": y, "button": button, "buttons": stream.buttons,
            "clickCount": event.click_count, "modifiers": modifier_bits(&event.modifiers),
        }));
        cx.stop_propagation();
    }

    fn server_mouse_move(&mut self, tab_id: &str, event: &MouseMoveEvent) {
        let Some(stream) = self.preview.server.streams.get(tab_id) else { return };
        if !stream.control.as_ref().is_some_and(Control::mine) {
            return;
        }
        let held = stream.buttons;
        let Some((x, y)) = self.server_point(tab_id, event.position, held != 0) else { return };
        let button = match held {
            0 => "none",
            b if b & 1 != 0 => "left",
            b if b & 2 != 0 => "right",
            _ => "middle",
        };
        stream.send(json!({
            "type": "mouse", "action": "move", "x": x, "y": y, "button": button, "buttons": held,
            "clickCount": 0, "modifiers": modifier_bits(&event.modifiers),
        }));
    }

    fn server_wheel(&self, tab_id: &str, event: &ScrollEvent) -> bool {
        let Some((x, y)) = self.server_point(tab_id, event.position, false) else { return false };
        let (dx, dy) = match event.delta {
            ScrollDelta::Pixels(delta) => (f32::from(delta.x), f32::from(delta.y)),
            ScrollDelta::Lines(delta) => (delta.x * WHEEL_LINE_PX, delta.y * WHEEL_LINE_PX),
        };
        // GPUI's deltas point where the content moves; the DOM's where the page scrolls.
        self.send_server_input(
            tab_id,
            json!({"type": "wheel", "x": x, "y": y, "deltaX": -dx, "deltaY": -dy, "modifiers": modifier_bits(&event.modifiers)}),
        )
    }

    fn server_key(&mut self, tab_id: &str, action: &str, keystroke: &Keystroke, cx: &mut Context<Self>) -> bool {
        let Some(stream) = self.preview.server.streams.get(tab_id) else { return false };
        if !stream.control.as_ref().is_some_and(Control::mine) {
            return false;
        }
        let m = &keystroke.modifiers;
        // Paste types the clipboard's text, as the page's own paste would.
        if action == "down" && (m.control || m.platform) && keystroke.key == "v" {
            if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                return stream.send(json!({"type": "text", "text": text}));
            }
            return true;
        }
        let Some(key) = dom_key(keystroke) else { return false };
        let mut input = json!({
            "type": "key", "action": action, "key": key.key, "code": key.code, "keyCode": key.key_code,
            "modifiers": modifier_bits(m),
        });
        if action == "down"
            && let Some(text) = key.text
        {
            input["text"] = text.into();
        }
        stream.send(input)
    }
}

impl Workspace {
    /// `alternatePreviewRuntime`'s label: a remote environment's tab can move between this
    /// computer and the environment's own browser. `None` for this app's own server.
    pub(super) fn move_tab_label(&self, session: &Session) -> Option<SharedString> {
        if !self.environment_remote {
            return None;
        }
        Some(if session.server_hosted() {
            "Open on this computer".into()
        } else {
            let label = self.server_info().environment_label.as_deref().unwrap_or("the environment");
            format!("Open in {label}'s browser").into()
        })
    }

    /// `handleMoveTab`: opens the page in the other runtime, then closes this tab.
    pub(super) fn move_preview_tab(&mut self, session: Session, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.client() else { return };
        let mut input = json!({"threadId": session.thread_id, "viewport": viewport_json(&session.viewport)});
        if let Some(url) = session.nav_status.url() {
            input["url"] = url.into();
        }
        if !session.server_hosted() {
            input["runtime"] = "server".into();
        }
        if let Some(profile) = &session.profile_id {
            input["profileId"] = profile.clone().into();
        }
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("preview.open", input).await;
            let _ = this.update_in(cx, |this, window, cx| {
                match result.map_err(|e| e.to_string()).and_then(|v| {
                    serde_json::from_value::<Session>(v).map_err(|e| format!("Unexpected preview.open result: {e}"))
                }) {
                    Ok(moved) => {
                        let tab = moved.tab_id.clone();
                        this.preview.threads.entry(session.thread_id.clone()).or_default().apply_snapshot(moved, true);
                        this.open_surface(Surface::Browser(tab), window, cx);
                        this.close_preview_tab(session.thread_id.clone(), session.tab_id.clone(), window, cx);
                    }
                    Err(error) => window.push_notification(
                        gpui_kit::component::notification::Notification::error(error)
                            .title("Unable to move the browser tab"),
                        cx,
                    ),
                }
            });
        })
        .detach();
    }
}

fn viewport_json(viewport: &Viewport) -> Value {
    match viewport {
        Viewport::Fill => json!({"_tag": "fill"}),
        Viewport::Freeform { width, height } => json!({"_tag": "freeform", "width": width, "height": height}),
        Viewport::Preset { width, height, preset_id } => {
            json!({"_tag": "preset", "presetId": preset_id, "width": width, "height": height})
        }
    }
}

/// `previewStreamModifiers`: alt 1, ctrl 2, meta 4, shift 8.
fn modifier_bits(m: &Modifiers) -> u8 {
    u8::from(m.alt) | u8::from(m.control) << 1 | u8::from(m.platform) << 2 | u8::from(m.shift) << 3
}

fn mouse_button(button: MouseButton) -> Option<(&'static str, u8)> {
    match button {
        MouseButton::Left => Some(("left", 1)),
        MouseButton::Right => Some(("right", 2)),
        MouseButton::Middle => Some(("middle", 4)),
        MouseButton::Navigate(_) => None,
    }
}

/// How the frame sits in its area: `object-fit: contain`, mapped to page CSS px.
#[derive(Clone, Copy, Debug, PartialEq)]
struct PageFit {
    origin: (f32, f32),
    size: (f32, f32),
    viewport: (f32, f32),
    area: Bounds<Pixels>,
}

fn page_fit(area: Bounds<Pixels>, frame: (u32, u32), viewport: Option<(f32, f32)>) -> Option<PageFit> {
    let (fw, fh) = (frame.0 as f32, frame.1 as f32);
    let (aw, ah) = (f32::from(area.size.width), f32::from(area.size.height));
    if fw < 1. || fh < 1. || aw < 1. || ah < 1. {
        return None;
    }
    let fit = (aw / fw).min(ah / fh);
    let size = (fw * fit, fh * fit);
    let origin = (f32::from(area.origin.x) + (aw - size.0) / 2., f32::from(area.origin.y) + (ah - size.1) / 2.);
    // The page's viewport, else the frame's own size, which a device scale of 1 matches.
    Some(PageFit { origin, size, viewport: viewport.unwrap_or((fw, fh)), area })
}

impl PageFit {
    /// A window point as page px; outside the frame only while a button is held (`captured`).
    fn to_page(&self, point: Point<Pixels>, captured: bool) -> Option<(f32, f32)> {
        let x = (f32::from(point.x) - self.origin.0) / self.size.0;
        let y = (f32::from(point.y) - self.origin.1) / self.size.1;
        if !captured && !((0. ..=1.).contains(&x) && (0. ..=1.).contains(&y)) {
            return None;
        }
        Some((x.clamp(0., 1.) * self.viewport.0, y.clamp(0., 1.) * self.viewport.1))
    }

    /// A page point relative to the frame area, for overlays drawn in it.
    fn to_local(&self, x: f32, y: f32) -> (f32, f32) {
        (
            self.origin.0 - f32::from(self.area.origin.x) + x / self.viewport.0 * self.size.0,
            self.origin.1 - f32::from(self.area.origin.y) + y / self.viewport.1 * self.size.1,
        )
    }
}

#[derive(Debug, PartialEq)]
struct DomKey {
    key: String,
    code: String,
    key_code: u32,
    /// Typed text: printable keys without Ctrl or Meta, and Enter's `"\r"`.
    text: Option<String>,
}

/// A GPUI keystroke as the DOM `KeyboardEvent` fields Puppeteer replays.
fn dom_key(keystroke: &Keystroke) -> Option<DomKey> {
    let m = &keystroke.modifiers;
    let shortcut = m.control || m.platform;
    let named = |key: &str, code: &str, key_code: u32| (key.to_owned(), code.to_owned(), key_code);
    let (key, code, key_code) = match keystroke.key.as_str() {
        "enter" => named("Enter", "Enter", 13),
        "backspace" => named("Backspace", "Backspace", 8),
        "tab" => named("Tab", "Tab", 9),
        "escape" => named("Escape", "Escape", 27),
        "delete" => named("Delete", "Delete", 46),
        "left" => named("ArrowLeft", "ArrowLeft", 37),
        "up" => named("ArrowUp", "ArrowUp", 38),
        "right" => named("ArrowRight", "ArrowRight", 39),
        "down" => named("ArrowDown", "ArrowDown", 40),
        "home" => named("Home", "Home", 36),
        "end" => named("End", "End", 35),
        "pageup" => named("PageUp", "PageUp", 33),
        "pagedown" => named("PageDown", "PageDown", 34),
        "space" => named(" ", "Space", 32),
        key if key.chars().count() == 1 => {
            let c = key.chars().next()?;
            let typed = keystroke.key_char.clone().filter(|t| t.chars().count() == 1).unwrap_or_else(|| key.to_owned());
            let code = match c {
                'a'..='z' => format!("Key{}", c.to_ascii_uppercase()),
                '0'..='9' => format!("Digit{c}"),
                _ => String::new(),
            };
            let key_code = if c.is_ascii_alphanumeric() { c.to_ascii_uppercase() as u32 } else { 0 };
            (typed, code, key_code)
        }
        _ => return None,
    };
    let text = match key.as_str() {
        "Enter" if !shortcut => Some("\r".to_owned()),
        k if !shortcut && k.chars().count() == 1 => Some(k.to_owned()),
        _ => None,
    };
    Some(DomKey { key, code, key_code, text })
}

/// The frame acknowledgement; the server sends the next frame after it.
const ACK: &str = r#"{"type":"ack"}"#;

/// The socket's life: (re)connects with backoff until the view drops `outgoing` or the server
/// says to stop. The view acknowledges each frame it takes, so a slow viewer gets fewer of them.
async fn run_stream(
    url: String,
    token: String,
    events: async_channel::Sender<Incoming>,
    mut outgoing: mpsc::UnboundedReceiver<String>,
) {
    let mut failures = 0u32;
    loop {
        let Ok(mut request) = url.as_str().into_client_request() else { return };
        if let Ok(value) = HeaderValue::from_str(&format!("Bearer {token}")) {
            request.headers_mut().insert("Authorization", value);
        }
        let socket = match tokio_tungstenite::connect_async(request).await {
            Ok((socket, _)) => socket,
            Err(WsError::Http(response)) if matches!(response.status().as_u16(), 401 | 403) => {
                let _ = events.send(Incoming::Stopped(Stop::Refused)).await;
                return;
            }
            Err(WsError::Http(response)) if response.status().as_u16() == 404 => {
                let _ = events.send(Incoming::Stopped(Stop::Gone)).await;
                return;
            }
            Err(error) => {
                log::warn!("preview stream connect failed: {error}");
                if !backoff(&mut failures, &events).await {
                    return;
                }
                continue;
            }
        };
        if events.send(Incoming::Connected(true)).await.is_err() {
            return;
        }
        let (mut sink, mut source) = socket.split();
        let stop = loop {
            tokio::select! {
                message = source.next() => match message {
                    Some(Ok(Message::Binary(jpeg))) => {
                        failures = 0;
                        // Off the IO workers, which also carry the RPC socket.
                        let decoded = tokio::task::spawn_blocking(move || decode_jpeg(&jpeg)).await.ok().flatten();
                        match decoded {
                            Some((image, size)) => {
                                if events.send(Incoming::Frame(image, size)).await.is_err() {
                                    return;
                                }
                            }
                            // The view never sees an undecodable frame, so it is acknowledged here.
                            None => {
                                if sink.send(Message::text(ACK)).await.is_err() {
                                    break None;
                                }
                            }
                        }
                    }
                    Some(Ok(Message::Text(text))) => {
                        let Ok(value) = serde_json::from_str::<Value>(&text) else { continue };
                        if value["type"] == "viewport" {
                            failures = 0;
                        }
                        if events.send(Incoming::Message(value)).await.is_err() {
                            return;
                        }
                    }
                    Some(Ok(Message::Close(frame))) => {
                        break frame.and_then(|frame| match u16::from(frame.code) {
                            TAB_GONE => Some(Stop::Gone),
                            HOST_SETUP => Some(host_setup(&frame.reason)),
                            4401 => Some(Stop::Refused),
                            _ if frame.code == CloseCode::Policy => Some(Stop::Refused),
                            _ => None,
                        });
                    }
                    Some(Ok(_)) => {}
                    Some(Err(_)) | None => break None,
                },
                input = outgoing.recv() => match input {
                    Some(text) => {
                        if sink.send(Message::text(text)).await.is_err() {
                            break None;
                        }
                    }
                    // The view closed the tab.
                    None => {
                        let _ = sink.close().await;
                        return;
                    }
                },
            }
        };
        if events.send(Incoming::Connected(false)).await.is_err() {
            return;
        }
        if let Some(stop) = stop {
            let _ = events.send(Incoming::Stopped(stop)).await;
            return;
        }
        if !backoff(&mut failures, &events).await {
            return;
        }
    }
}

/// Waits `min(500ms * 2^failures, 10s)`; false once the view is gone.
async fn backoff(failures: &mut u32, events: &async_channel::Sender<Incoming>) -> bool {
    let delay = Duration::from_millis((500u64 << (*failures).min(5)).min(10_000));
    *failures += 1;
    tokio::time::sleep(delay).await;
    !events.is_closed()
}

fn decode_jpeg(bytes: &[u8]) -> Option<(Arc<RenderImage>, (u32, u32))> {
    let decoded = image::load_from_memory_with_format(bytes, image::ImageFormat::Jpeg).ok()?;
    let mut rgba = decoded.into_rgba8();
    let size = rgba.dimensions();
    // GPUI textures are BGRA.
    for pixel in rgba.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    Some((Arc::new(RenderImage::new([image::Frame::new(rgba)])), size))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::{point, size};

    #[test]
    fn control_labels_follow_the_controller() {
        let control = |can_operate: bool, controller: &str| {
            Control::parse(
                &json!({"canOperate": can_operate, "controller": controller, "generation": 1, "dialog": null}),
            )
        };
        assert_eq!(control_label(None), "Connecting...");
        assert_eq!(control_label(control(false, "agent").as_ref()), "Read-only");
        assert_eq!(control_label(control(true, "you").as_ref()), "You have control");
        assert_eq!(control_label(control(true, "agent").as_ref()), "Agent has control");
        assert_eq!(control_label(control(true, "unclaimed").as_ref()), "Watching");
        assert!(control(true, "robot").is_none());
        let dialog = Control::parse(&json!({"canOperate": true, "controller": "you", "generation": 2,
            "dialog": {"type": "prompt", "message": "Name?", "defaultValue": "x"}}))
        .unwrap();
        assert_eq!(dialog.dialog.unwrap().default_value, "x");
    }

    #[test]
    fn points_map_through_the_contained_frame() {
        // A 1000x500 frame of a 500x250 page in a 400x400 area at (100, 0): 400x200, centered.
        let area = Bounds { origin: point(px(100.), px(0.)), size: size(px(400.), px(400.)) };
        let fit = page_fit(area, (1000, 500), Some((500., 250.))).unwrap();
        assert_eq!(fit.to_page(point(px(100.), px(100.)), false), Some((0., 0.)));
        assert_eq!(fit.to_page(point(px(300.), px(200.)), false), Some((250., 125.)));
        assert_eq!(fit.to_page(point(px(300.), px(50.)), false), None, "above the frame");
        assert_eq!(fit.to_page(point(px(300.), px(50.)), true), Some((250., 0.)), "a drag clamps to the edge");
        assert_eq!(fit.to_local(250., 125.), (200., 200.));
        assert!(page_fit(area, (0, 0), None).is_none());
    }

    #[test]
    fn keystrokes_become_dom_keys() {
        let key = |text: &'static str| dom_key(&Keystroke::parse(text).ok().unwrap()).unwrap();
        assert_eq!(
            key("enter"),
            DomKey { key: "Enter".into(), code: "Enter".into(), key_code: 13, text: Some("\r".into()) }
        );
        assert_eq!(key("a").code, "KeyA");
        assert_eq!((key("a").key_code, key("a").text.as_deref()), (65, Some("a")));
        assert_eq!(key("ctrl-a").text, None, "shortcuts type nothing");
        assert_eq!(key("5").code, "Digit5");
        assert_eq!(key("left").key, "ArrowLeft");
        assert_eq!(key("space").text.as_deref(), Some(" "));
        assert!(dom_key(&Keystroke::parse("f13").ok().unwrap()).is_none());
        assert_eq!(modifier_bits(&Modifiers { shift: true, control: true, ..Default::default() }), 10);
    }

    #[test]
    fn host_setup_reasons_fall_back_to_the_sandbox() {
        let Stop::HostSetup { message, command } = host_setup(r#"{"need":"libraries","command":"apt install x"}"#)
        else {
            panic!()
        };
        assert!(message.contains("missing libraries"));
        assert_eq!(command, "apt install x");
        assert_eq!(host_setup("garbage"), host_setup(r#"{"need":"sandbox","command":"sudo npx t3 browser setup"}"#));
    }
}
