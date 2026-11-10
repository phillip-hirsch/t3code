//! The chrome row's annotate button: elements, regions and drawings marked on one of this
//! computer's browser tabs become a `preview-annotation` record and a screenshot in the thread's
//! draft (`PreviewChromeRow`'s pick button, `handlePickElement` in `PreviewView.tsx`).
//!
//! The overlay is `PickPreload.ts` as a page script (`annotation/picker.js`) without its voice
//! input; it reports over wry's IPC with a session nonce. The screenshot is read like the
//! camera's (`capture.rs`) once the overlay has hidden its own chrome.

use gpui_kit::assets::IconName;
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::button::{ButtonCustomVariant, ButtonVariants as _};
use gpui_kit::component::notification::Notification;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{AnyElement, App, Bounds, Context, FocusHandle, IntoElement as _, Pixels, Window, point, px, size};
use serde::Deserialize;
use serde_json::{Value, json};

use super::capture;
use super::icon_button;
use crate::app::send::Intent;
use crate::app::{Target, Workspace};
use crate::composer_context::{kind_scoped_id, sanitize_label};
use crate::preview::{TabRef, ThreadRef};
use crate::theme::Tokens;

/// Defines `window.__t3Annotation` in every page a tab loads.
pub(super) const PICKER_SCRIPT: &str = include_str!("annotation/picker.js");

const KIND: &str = "preview-annotation";
/// `PREVIEW_LABEL_MAX_CHARS`.
const LABEL_MAX: usize = 48;
/// `ELEMENT_CONTEXT_HTML_PREVIEW_LIMIT` and `ELEMENT_CONTEXT_STYLES_LIMIT`.
const ELEMENT_TEXT_MAX: usize = 4000;

/// The pick in progress; this client annotates one tab at a time.
#[derive(Default)]
pub(super) struct Annotations {
    pick: Option<Pick>,
    /// Wakes a sent annotation that waits for its screenshot's upload.
    upload_settled: Option<async_channel::Sender<()>>,
}

struct Pick {
    /// The nonce the page's messages carry.
    session: String,
    tab: TabRef,
    /// Where the window's focus was before the page took it.
    restore_focus: Option<FocusHandle>,
    /// Whether the page offers Cmd/Ctrl+Enter, as last told.
    send_enabled: bool,
    /// The page submitted and its screenshot is being read.
    capturing: bool,
}

/// A CSS-pixel rectangle in the page's viewport.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
pub(super) struct Rect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct StackFrame {
    function_name: Option<String>,
    file_name: Option<String>,
    line_number: Option<serde_json::Number>,
    column_number: Option<serde_json::Number>,
}

/// `PickedElementPayload`.
#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct PickedElement {
    page_url: String,
    page_title: Option<String>,
    tag_name: String,
    selector: Option<String>,
    html_preview: String,
    component_name: Option<String>,
    source: Option<StackFrame>,
    stack: Vec<StackFrame>,
    styles: String,
    #[serde(rename = "pickedAt")]
    _picked_at: String,
}

#[derive(Debug, Deserialize, PartialEq)]
struct ElementTarget {
    id: String,
    element: PickedElement,
    rect: Rect,
}

#[derive(Debug, Deserialize, PartialEq)]
struct RegionTarget {
    #[serde(rename = "id")]
    _id: String,
    rect: Rect,
}

#[derive(Debug, Deserialize, PartialEq)]
struct Point {
    #[serde(rename = "x")]
    _x: f64,
    #[serde(rename = "y")]
    _y: f64,
}

#[derive(Debug, Deserialize, PartialEq)]
struct StrokeTarget {
    #[serde(rename = "id")]
    _id: String,
    #[serde(rename = "color")]
    _color: String,
    #[serde(rename = "width")]
    _width: f64,
    #[serde(rename = "points")]
    _points: Vec<Point>,
    bounds: Rect,
}

#[derive(Clone, Debug, Deserialize, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct StyleChange {
    target_id: String,
    selector: Option<String>,
    property: String,
    previous_value: String,
    value: String,
}

/// `PreviewAnnotationPayload` as the page submits it (`isPreviewAnnotationPayload`).
#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(super) struct Annotation {
    id: String,
    page_url: String,
    page_title: Option<String>,
    comment: String,
    elements: Vec<ElementTarget>,
    regions: Vec<RegionTarget>,
    strokes: Vec<StrokeTarget>,
    style_changes: Vec<StyleChange>,
    #[serde(rename = "createdAt")]
    _created_at: String,
}

impl Annotation {
    /// Every target's rectangle, which a screenshot must show part of.
    fn target_rects(&self) -> Vec<Rect> {
        let elements = self.elements.iter().map(|target| target.rect);
        let regions = self.regions.iter().map(|target| target.rect);
        elements.chain(regions).chain(self.strokes.iter().map(|target| target.bounds)).collect()
    }
}

/// What a tab's page says about its pick.
#[derive(Debug, PartialEq)]
pub(in crate::app) enum PickMessage {
    /// Escape, or a submission that wasn't an annotation.
    Cancel,
    Submit(Box<Submission>),
}

#[derive(Debug, PartialEq)]
pub(in crate::app) struct Submission {
    annotation: Annotation,
    /// The padded union of the targets, else the whole viewport.
    crop: Option<Rect>,
    /// Cmd/Ctrl+Enter: send the draft once the annotation is in it.
    send: bool,
    /// `innerWidth` and `innerHeight`.
    viewport: (f64, f64),
}

/// The overlay's IPC message: its session and what it says; `None` for any other message.
pub(super) fn pick_message(body: &str) -> Option<(String, PickMessage)> {
    let message: Value = serde_json::from_str(body).ok()?;
    let message = message.get("t3Annotation")?;
    let session = message.get("session")?.as_str()?.to_owned();
    let parsed = match message.get("type")?.as_str()? {
        "cancel" => PickMessage::Cancel,
        "submit" => submission(message).map_or(PickMessage::Cancel, |s| PickMessage::Submit(Box::new(s))),
        _ => return None,
    };
    Some((session, parsed))
}

/// `normalizeCaptureRect`: a rectangle with an area.
fn capture_rect(value: &Value) -> Option<Rect> {
    let rect: Rect = serde_json::from_value(value.clone()).ok()?;
    (rect.width > 0. && rect.height > 0.).then_some(rect)
}

fn submission(message: &Value) -> Option<Submission> {
    let annotation: Annotation = serde_json::from_value(message.get("annotation")?.clone()).ok()?;
    let viewport = message.get("viewport")?;
    let viewport = (viewport.get("width")?.as_f64()?, viewport.get("height")?.as_f64()?);
    Some(Submission {
        annotation,
        crop: message.get("cropRect").and_then(capture_rect),
        send: message.get("submission").and_then(Value::as_str) == Some("send"),
        viewport,
    })
}

/// `cropAnnotationScreenshot` in CSS pixels: the part of `crop` (else of the whole viewport)
/// inside the viewport, or `None` when no target has pixels there.
fn crop_area(crop: Option<Rect>, (width, height): (f64, f64), targets: &[Rect]) -> Option<Rect> {
    if width <= 0. || height <= 0. {
        return None;
    }
    // Padding or a union of offscreen targets can overlap an empty viewport area. A zero-width or
    // zero-height line can still paint at the top or left edge.
    let painted = |rect: &Rect| {
        (rect.width > 0. || rect.height > 0.)
            && rect.x < width
            && rect.y < height
            && (if rect.width == 0. { rect.x >= 0. } else { rect.x + rect.width > 0. })
            && (if rect.height == 0. { rect.y >= 0. } else { rect.y + rect.height > 0. })
    };
    if !targets.is_empty() && !targets.iter().any(painted) {
        return None;
    }
    let Some(crop) = crop else { return Some(Rect { x: 0., y: 0., width, height }) };
    let (x, y) = (crop.x.max(0.), crop.y.max(0.));
    let (right, bottom) = ((crop.x + crop.width).min(width), (crop.y + crop.height).min(height));
    // A target scrolled out of view has no pixels here; clamping it onto the edge would attach an
    // unrelated strip.
    (right > x && bottom > y).then_some(Rect { x, y, width: right - x, height: bottom - y })
}

/// UTF-16 units, as JavaScript counts a string's length.
fn units(text: &str) -> usize {
    text.encode_utf16().count()
}

/// The first chars of `text` that fit in `max` UTF-16 units, never splitting a pair.
fn take_units(text: &str, max: usize) -> &str {
    let mut used = 0;
    let end = text.char_indices().find(|(_, c)| {
        used += c.len_utf16();
        used > max
    });
    &text[..end.map_or(text.len(), |(at, _)| at)]
}

/// `truncateString`: at most `limit` units, the last an ellipsis.
fn truncate(text: &str, limit: usize) -> String {
    if units(text) <= limit { text.to_owned() } else { format!("{}…", take_units(text, limit.saturating_sub(1))) }
}

/// `previewAnnotationContextLabel`: the comment on one line, else the page's title.
fn label(annotation: &Annotation) -> String {
    let comment = annotation.comment.split_whitespace().collect::<Vec<_>>().join(" ");
    if comment.is_empty() {
        let title = annotation.page_title.as_deref().map(str::trim).filter(|title| !title.is_empty());
        return title.unwrap_or("Preview annotation").to_owned();
    }
    truncate(&comment, LABEL_MAX)
}

/// `previewAnnotationTargetSummary`.
fn target_summary(annotation: &Annotation) -> String {
    let plural = |count: usize, noun: &str| format!("{count} {noun}{}", if count == 1 { "" } else { "s" });
    let counts = [
        (annotation.elements.len(), "selected element"),
        (annotation.regions.len(), "marked region"),
        (annotation.strokes.len(), "drawing"),
    ];
    counts
        .into_iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, noun)| plural(count, noun))
        .collect::<Vec<_>>()
        .join(", ")
}

fn trimmed(value: Option<&str>) -> Value {
    value.map(str::trim).filter(|value| !value.is_empty()).map_or(Value::Null, Value::from)
}

/// `normalizeElementContextSelection`; `None` without a page or a tag.
fn element_details(raw: &PickedElement) -> Option<Value> {
    let (page_url, tag_name) = (raw.page_url.trim(), raw.tag_name.trim().to_lowercase());
    if page_url.is_empty() || tag_name.is_empty() {
        return None;
    }
    let normalize = |text: &str| truncate(text.replace("\r\n", "\n").trim_matches('\n'), ELEMENT_TEXT_MAX);
    let frame = raw.source.as_ref().or_else(|| raw.stack.first());
    let source = frame.map_or(Value::Null, |frame| {
        let number = |value: &Option<serde_json::Number>| value.as_ref().and_then(serde_json::Number::as_u64);
        json!({
            "functionName": trimmed(frame.function_name.as_deref()),
            "fileName": trimmed(frame.file_name.as_deref()),
            "lineNumber": number(&frame.line_number),
            "columnNumber": number(&frame.column_number),
        })
    });
    Some(json!({
        "pageUrl": page_url,
        "pageTitle": raw.page_title.as_deref().map(str::trim),
        "tagName": tag_name,
        "selector": trimmed(raw.selector.as_deref()),
        "htmlPreview": normalize(&raw.html_preview),
        "componentName": trimmed(raw.component_name.as_deref()),
        "source": source,
        "styles": normalize(&raw.styles),
    }))
}

/// `previewAnnotationContextRecord`; `screenshot` when its image joins the draft.
fn annotation_record(annotation: &Annotation, screenshot: bool) -> Value {
    let targets: Vec<(&str, Value)> = annotation
        .elements
        .iter()
        .filter_map(|target| element_details(&target.element).map(|details| (target.id.as_str(), details)))
        .collect();
    let changes = annotation.style_changes.iter().map(|change| {
        let previous = if change.previous_value.is_empty() { "(unset)" } else { &change.previous_value };
        format!("{}: {previous} → {}", change.property, change.value)
    });
    let mut record = json!({
        "version": 1,
        "contextId": kind_scoped_id(KIND, &annotation.id),
        "kind": KIND,
        "label": sanitize_label(&label(annotation), KIND),
        "annotationId": annotation.id,
        "pageUrl": annotation.page_url,
        "pageTitle": annotation.page_title,
        "comment": annotation.comment.trim(),
        "targetSummary": target_summary(annotation),
        "styleChanges": changes.collect::<Vec<_>>(),
        "styleChangeDetails": annotation.style_changes,
    });
    if !targets.is_empty() {
        record["elementIds"] = targets.iter().map(|(id, _)| *id).collect();
        record["elements"] = targets.into_iter().map(|(_, details)| details).collect();
    }
    if !annotation.regions.is_empty() {
        record["regionCount"] = annotation.regions.len().into();
    }
    if !annotation.strokes.is_empty() {
        record["strokeCount"] = annotation.strokes.len().into();
    }
    if screenshot {
        record["screenshotContextId"] = kind_scoped_id("image", &annotation.id).into();
    }
    record
}

/// `readPreviewAnnotationTheme`: the app's colours and fonts for the overlay.
fn page_theme(tokens: &Tokens) -> Value {
    let color = |value| capture::css_color(value);
    json!({
        "colorScheme": if tokens.dark { "dark" } else { "light" },
        "radius": "10px",
        "background": color(tokens.background),
        "foreground": color(tokens.foreground),
        "popover": color(tokens.popover),
        "popoverForeground": color(tokens.foreground),
        "primary": color(tokens.primary),
        "primaryForeground": color(tokens.primary_foreground),
        "destructive": color(tokens.error),
        "muted": color(tokens.muted),
        "mutedForeground": color(tokens.muted_foreground),
        "accent": color(tokens.accent),
        "accentForeground": color(tokens.foreground),
        "border": color(tokens.border),
        "input": color(tokens.input),
        "ring": color(tokens.ring),
        "fontSans": format!("{}, system-ui, sans-serif", serde_json::to_string(tokens.fonts.sans).unwrap_or_default()),
        "fontMono": format!("{}, ui-monospace, monospace", serde_json::to_string(tokens.fonts.mono).unwrap_or_default()),
    })
}

/// A script calling `__t3Annotation.<call>`, if the page has it.
fn page_call(call: &str) -> String {
    format!("window.__t3Annotation&&window.__t3Annotation.{call}")
}

impl Workspace {
    /// The pick button for a tab this computer draws; `page_ready` is a reachable page.
    pub(super) fn render_annotate_button(
        &self,
        tab: &TabRef,
        page_ready: bool,
        unreachable: bool,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let active = self.annotation_pick(tab).is_some();
        let enabled = page_ready || active;
        let tooltip = if unreachable {
            "Page didn't load. Pick is unavailable until the page renders"
        } else if active {
            "Cancel annotation (Esc)"
        } else {
            "Annotate elements, regions, and drawings"
        };
        let tab = tab.clone();
        icon_button("preview-annotate", IconName::MousePointerClick, enabled, tokens, cx)
            // `variant="secondary"` with a primary icon while picking.
            .when(active, |el| {
                el.custom(
                    ButtonCustomVariant::new(cx)
                        .color(tokens.secondary)
                        .foreground(tokens.primary)
                        .hover(tokens.secondary)
                        .active(tokens.secondary),
                )
            })
            .tooltip(tooltip)
            .when(enabled, |el| {
                el.on_click(cx.listener(move |this, _, window, cx| {
                    if this.annotation_pick(&tab).is_some() {
                        this.cancel_annotation(window, cx);
                    } else {
                        this.start_annotation(&tab, window, cx);
                    }
                }))
            })
            .into_any_element()
    }

    fn annotation_pick(&self, tab: &TabRef) -> Option<&Pick> {
        self.preview.annotation.pick.as_ref().filter(|pick| pick.tab == *tab)
    }

    /// Whether a pick is in progress in any tab.
    pub(super) fn annotating(&self) -> bool {
        self.preview.annotation.pick.is_some()
    }

    /// `handlePickElement`: starts the overlay in the tab's page and hands it the keyboard.
    fn start_annotation(&mut self, key: &TabRef, window: &mut Window, cx: &mut Context<Self>) {
        if self.preview.annotation.pick.is_some() {
            self.cancel_annotation(window, cx);
        }
        let send_enabled = self.annotation_send_enabled(&key.thread);
        let Some(tab) = self.preview.tabs.get(key) else { return };
        let session = uuid::Uuid::new_v4().simple().to_string();
        let options = json!({
            "session": session,
            "sendEnabled": send_enabled,
            "theme": page_theme(crate::theme::tokens(cx)),
        });
        let view = tab.view.read(cx).raw();
        if let Err(error) = view.evaluate_script(&page_call(&format!("start({options})"))) {
            log::warn!("could not start the annotation overlay: {error}");
            return;
        }
        let restore_focus = window.focused(cx);
        if let Err(error) = view.focus() {
            log::debug!("could not focus the browser tab: {error}");
        }
        self.preview.annotation.pick =
            Some(Pick { session, tab: key.clone(), restore_focus, send_enabled, capturing: false });
        cx.notify();
    }

    /// `canSendAnnotation`: the thread's environment lets this client send.
    fn annotation_send_enabled(&self, thread: &ThreadRef) -> bool {
        self.can_operate(&thread.environment)
    }

    /// Ends the pick and tears the overlay down, also mid-capture: that capture is dropped.
    pub(super) fn cancel_annotation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pick) = self.preview.annotation.pick.as_ref() else { return };
        if let Some(tab) = self.preview.tabs.get(&pick.tab)
            && let Err(error) = tab.view.read(cx).raw().evaluate_script(&page_call("cancel()"))
        {
            log::debug!("could not cancel the annotation overlay: {error}");
        }
        self.finish_annotation(window, cx);
    }

    /// `finishPick`: forgets the pick and gives the keyboard back to where it was.
    fn finish_annotation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pick) = self.preview.annotation.pick.take() else { return };
        if let Some(tab) = self.preview.tabs.get(&pick.tab)
            && let Err(error) = tab.view.read(cx).raw().focus_parent()
        {
            log::debug!("could not take the focus back from the browser tab: {error}");
        }
        if let Some(handle) = pick.restore_focus {
            window.focus(&handle, cx);
        }
        cx.notify();
    }

    /// Each frame: a pick ends once the panel stops showing its tab (a tab or thread change, a
    /// closed panel), and its page hears when sending becomes allowed or not.
    /// Call it before the panel's drawn tab is taken.
    pub(super) fn sync_annotation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pick) = self.preview.annotation.pick.as_ref() else { return };
        if self.preview.panel_view.as_ref().is_none_or(|(tab, _)| *tab != pick.tab) {
            return self.cancel_annotation(window, cx);
        }
        let enabled = self.annotation_send_enabled(&pick.tab.thread);
        if enabled == pick.send_enabled {
            return;
        }
        if let Some(tab) = self.preview.tabs.get(&pick.tab) {
            let _ = tab.view.read(cx).raw().evaluate_script(&page_call(&format!("setSendEnabled({enabled})")));
        }
        if let Some(pick) = self.preview.annotation.pick.as_mut() {
            pick.send_enabled = enabled;
        }
    }

    /// A tab's page started loading: its overlay went with the old page.
    pub(super) fn annotation_page_left(&mut self, tab: &TabRef, window: &mut Window, cx: &mut Context<Self>) {
        if self.annotation_pick(tab).is_some() {
            self.finish_annotation(window, cx);
        }
    }

    /// The overlay's message from `tab`; one from any other session is ignored.
    pub(super) fn on_pick_message(
        &mut self,
        tab: &TabRef,
        session: &str,
        message: PickMessage,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(pick) = self.preview.annotation.pick.as_mut().filter(|p| p.tab == *tab && p.session == session) else {
            return;
        };
        let submission = match message {
            PickMessage::Cancel => return self.finish_annotation(window, cx),
            PickMessage::Submit(_) if pick.capturing => return,
            PickMessage::Submit(submission) => submission,
        };
        pick.capturing = true;
        let crop = crop_area(submission.crop, submission.viewport, &submission.annotation.target_rects())
            .and_then(|area| self.annotation_view_area(tab, area, submission.viewport, cx));
        let session = session.to_owned();
        let shot = crop.and_then(|crop| self.read_page(tab, Some(crop), window, cx, |image| capture::png(&image)));
        cx.spawn_in(window, async move |this, cx| {
            let shot = match shot {
                Some(shot) => Some(shot.await),
                None => None,
            };
            let _ = this
                .update_in(cx, |this, window, cx| this.annotation_captured(&session, *submission, shot, window, cx));
        })
        .detach();
    }

    /// Where `area` of the tab's viewport is in its view, in the view's pixels.
    fn annotation_view_area(
        &self,
        tab: &TabRef,
        area: Rect,
        (width, _): (f64, f64),
        cx: &App,
    ) -> Option<Bounds<Pixels>> {
        let bounds = self.preview.tabs.get(tab).filter(|hosted| hosted.shown)?.view.read(cx).bounds();
        // The page's CSS pixels to the view's, through its zoom.
        #[expect(clippy::cast_possible_truncation, reason = "on-screen sizes fit in an f32")]
        let scale = (f64::from(f32::from(bounds.size.width)) / width) as f32;
        #[expect(clippy::cast_possible_truncation, reason = "on-screen sizes fit in an f32")]
        let pixels = |value: f64| px(value as f32 * scale);
        Some(Bounds {
            origin: point(pixels(area.x), pixels(area.y)),
            size: size(pixels(area.width), pixels(area.height)),
        })
    }

    /// The screenshot was read (or not): the overlay goes, and the annotation joins the draft.
    fn annotation_captured(
        &mut self,
        session: &str,
        submission: Submission,
        shot: Option<Result<Vec<u8>, String>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A cancel during the capture drops it.
        let Some(pick) = self.preview.annotation.pick.as_ref().filter(|pick| pick.session == session) else { return };
        let key = pick.tab.clone();
        if let Some(tab) = self.preview.tabs.get(&key) {
            let _ = tab.view.read(cx).raw().evaluate_script(&page_call(&format!("captured({})", json!(session))));
        }
        self.finish_annotation(window, cx);
        let environment = key.thread.environment.clone();
        let target = Target::Thread { env: key.thread.environment, id: key.thread.thread_id };
        let png = match shot {
            Some(Ok(png)) => Some(png),
            Some(Err(error)) => {
                log::warn!("could not capture the annotation's screenshot: {error}");
                None
            }
            None => None,
        };
        let annotation = &submission.annotation;
        if self.add_context_record(&target, annotation_record(annotation, png.is_some()), window, cx).is_none() {
            return;
        }
        let name = format!("preview-annotation-{}.png", annotation.id);
        let attached =
            png.is_some_and(|png| self.attach_annotation_screenshot(&target, annotation.id.clone(), name, png, cx));
        if !attached {
            self.replace_context_record(&target, annotation_record(annotation, false), cx);
            let toast = Notification::error("The annotation was kept without the screenshot.")
                .title("Could not capture the picked element");
            window.push_notification(toast, cx);
        }
        // The overlay offers sending only with the grant; it's checked again here.
        if submission.send && self.target.as_ref() == Some(&target) && self.can_operate(&environment) {
            self.send_annotated_draft(target, window, cx);
        }
    }

    /// `onSendAnnotation`: sends the draft once its uploads (the screenshot's) are done.
    fn send_annotated_draft(&mut self, target: Target, window: &mut Window, cx: &mut Context<Self>) {
        if !self.attachments_busy() {
            return self.submit_with(Intent::Foreground, window, cx);
        }
        let (sender, settled) = async_channel::bounded(1);
        self.preview.annotation.upload_settled = Some(sender);
        cx.spawn_in(window, async move |this, cx| {
            while settled.recv().await.is_ok() {
                let done = this.update_in(cx, |this, window, cx| {
                    if this.target.as_ref() == Some(&target) && this.attachments_busy() {
                        return false;
                    }
                    this.preview.annotation.upload_settled = None;
                    if this.target.as_ref() == Some(&target) {
                        this.submit_with(Intent::Foreground, window, cx);
                    }
                    true
                });
                if !matches!(done, Ok(false)) {
                    break;
                }
            }
        })
        .detach();
    }

    /// An attachment's upload finished or failed.
    pub(in crate::app) fn annotation_upload_settled(&self) {
        if let Some(settled) = &self.preview.annotation.upload_settled {
            let _ = settled.try_send(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn submit(annotation: &Value) -> String {
        json!({"t3Annotation": {
            "session": "s1", "type": "submit", "annotation": annotation, "submission": "send",
            "cropRect": {"x": -20, "y": 10, "width": 140, "height": 60},
            "viewport": {"width": 800, "height": 600},
        }})
        .to_string()
    }

    fn element(tag: &str) -> Value {
        json!({
            "pageUrl": " https://example.com/ ", "pageTitle": " Home ", "tagName": tag, "selector": " #go ",
            "htmlPreview": "\r\n<button>Go</button>\n", "componentName": "", "source": null,
            "stack": [{"functionName": " Cta ", "fileName": "src/Cta.tsx", "lineNumber": 12, "columnNumber": null}],
            "styles": "color: red;", "pickedAt": "2026-01-01T00:00:00.000Z",
        })
    }

    fn annotation(comment: &str) -> Value {
        json!({
            "id": "annotation_x1_3", "pageUrl": "https://example.com/", "pageTitle": "Home", "comment": comment,
            "elements": [{"id": "element_x1_1", "element": element("BUTTON"), "rect": {"x": 0, "y": 20, "width": 100, "height": 20}}],
            "regions": [{"id": "region_x1_2", "rect": {"x": 5, "y": 5, "width": 10, "height": 10}}],
            "strokes": [],
            "styleChanges": [{"targetId": "element_x1_1", "selector": "#go", "property": "color", "previousValue": "", "value": "blue"}],
            "screenshot": null, "createdAt": "2026-01-01T00:00:00.000Z",
        })
    }

    #[test]
    fn the_overlay_messages_parse_and_anything_else_is_ignored() {
        let (session, message) = pick_message(&submit(&annotation(" Make it  blue "))).expect("a pick message");
        assert_eq!(session, "s1");
        let PickMessage::Submit(submission) = message else { panic!("a submission") };
        assert!(submission.send);
        assert_eq!(submission.viewport, (800., 600.));
        assert_eq!(submission.crop, Some(Rect { x: -20., y: 10., width: 140., height: 60. }));

        let cancel = json!({"t3Annotation": {"session": "s1", "type": "cancel"}}).to_string();
        assert_eq!(pick_message(&cancel), Some(("s1".into(), PickMessage::Cancel)));
        let mut broken = annotation("x");
        broken["elements"][0]["element"]["stack"] = json!("nope");
        assert_eq!(
            pick_message(&submit(&broken)).map(|(_, message)| message),
            Some(PickMessage::Cancel),
            "a malformed annotation ends the pick, as Electron's guard does"
        );
        assert_eq!(pick_message(r#"{"t3History":{"url":"x","back":true,"forward":false}}"#), None);
        assert_eq!(pick_message(r#"{"t3Annotation":{"type":"cancel"}}"#), None, "no session");
    }

    #[test]
    fn a_crop_keeps_to_the_viewport_and_needs_a_painted_target() {
        let rect = |x, y, width, height| Rect { x, y, width, height };
        let targets = [rect(0., 20., 100., 20.)];
        assert_eq!(crop_area(Some(rect(-20., 0., 140., 60.)), (800., 600.), &targets), Some(rect(0., 0., 120., 60.)));
        assert_eq!(crop_area(None, (800., 600.), &targets), Some(rect(0., 0., 800., 600.)));
        assert_eq!(crop_area(Some(rect(0., 700., 10., 10.)), (800., 600.), &targets), None, "below the fold");
        assert_eq!(crop_area(None, (800., 600.), &[rect(-50., 0., 40., 10.)]), None, "every target offscreen");
        assert!(crop_area(None, (800., 600.), &[rect(0., 5., 0., 10.)]).is_some(), "a vertical line at the edge");
    }

    #[test]
    fn records_match_preview_annotation_context_record() {
        let parsed: Annotation = serde_json::from_value(annotation(" Make it\n blue ")).unwrap();
        let record = annotation_record(&parsed, true);
        assert_eq!(record["contextId"], "preview-annotation_annotation_x1_3");
        assert_eq!(record["label"], "Make it blue");
        assert_eq!(record["comment"], "Make it\n blue");
        assert_eq!(record["targetSummary"], "1 selected element, 1 marked region");
        assert_eq!(record["styleChanges"], json!(["color: (unset) → blue"]));
        assert_eq!(record["styleChangeDetails"][0]["previousValue"], "");
        assert_eq!(record["elementIds"], json!(["element_x1_1"]));
        assert_eq!(record["regionCount"], 1);
        assert!(record.get("strokeCount").is_none());
        assert_eq!(record["screenshotContextId"], "image_annotation_x1_3");
        assert_eq!(
            record["elements"][0],
            json!({
                "pageUrl": "https://example.com/", "pageTitle": "Home", "tagName": "button", "selector": "#go",
                "htmlPreview": "<button>Go</button>", "componentName": null,
                "source": {"functionName": "Cta", "fileName": "src/Cta.tsx", "lineNumber": 12, "columnNumber": null},
                "styles": "color: red;",
            }),
            "the source falls back to the stack's first frame"
        );
        assert!(annotation_record(&parsed, false).get("screenshotContextId").is_none());

        let untitled: Annotation = serde_json::from_value(json!({
            "id": "a", "pageUrl": "u", "pageTitle": null, "comment": " ", "elements": [], "regions": [],
            "strokes": [], "styleChanges": [], "createdAt": "t",
        }))
        .unwrap();
        assert_eq!(annotation_record(&untitled, false)["label"], "Preview annotation");
    }

    #[test]
    fn long_labels_end_in_an_ellipsis_within_48_units() {
        let parsed: Annotation = serde_json::from_value(annotation(&"é".repeat(60))).unwrap();
        assert_eq!(label(&parsed), format!("{}…", "é".repeat(47)));
        let emoji: Annotation = serde_json::from_value(annotation(&"😀".repeat(30))).unwrap();
        assert_eq!(label(&emoji), format!("{}…", "😀".repeat(23)), "a pair is never split");
    }
}
