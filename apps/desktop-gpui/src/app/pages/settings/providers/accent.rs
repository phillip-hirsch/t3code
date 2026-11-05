//! The provider accent picker (`ProviderAccentColorPicker.tsx` over the
//! saturation/brightness plane and hue slider of `components/ui/color-picker.tsx`).
//! Drags preview and commit once on release, so a drag writes settings once.

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::{Icon, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Bounds, Context, ElementId, Entity, Hsla, InteractiveElement as _, IntoElement,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement as _, Pixels, Point, Render, SharedString,
    Styled as _, Subscription, Window, canvas, div, hsla, linear_color_stop, linear_gradient, px, rgb,
};

use crate::theme;

const FALLBACK_ACCENT_COLOR: &str = "#2563eb";

/// `normalizeProviderAccentColor` in `packages/client-runtime/src/state/providerInstanceDisplay.ts`.
pub(super) fn normalize_accent_color(value: Option<&str>) -> Option<String> {
    let trimmed = value?.trim();
    (trimmed.len() == 7 && trimmed.starts_with('#') && trimmed[1..].bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| trimmed.to_owned())
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Hsv {
    h: f32,
    s: f32,
    v: f32,
}

/// `hexToHsv` in `apps/web/src/lib/color.ts`.
fn hex_to_hsv(hex: &str) -> Hsv {
    let numeric = u32::from_str_radix(hex.get(1..).unwrap_or_default(), 16).unwrap_or(0);
    let [r, g, b] = [16, 8, 0].map(|shift| ((numeric >> shift) & 255) as f32 / 255.);
    let max = r.max(g).max(b);
    let delta = max - r.min(g).min(b);
    let mut h = 0.;
    if delta != 0. {
        h = if max == r {
            ((g - b) / delta) % 6.
        } else if max == g {
            (b - r) / delta + 2.
        } else {
            (r - g) / delta + 4.
        } * 60.;
        if h < 0. {
            h += 360.;
        }
    }
    Hsv { h, s: if max == 0. { 0. } else { delta / max }, v: max }
}

/// `hsvToHex` in `apps/web/src/lib/color.ts`.
fn hsv_to_hex(Hsv { h, s, v }: Hsv) -> String {
    let h = h.rem_euclid(360.);
    let chroma = v * s;
    let x = chroma * (1. - ((h / 60.) % 2. - 1.).abs());
    let (r, g, b) = match h {
        h if h < 60. => (chroma, x, 0.),
        h if h < 120. => (x, chroma, 0.),
        h if h < 180. => (0., chroma, x),
        h if h < 240. => (0., x, chroma),
        h if h < 300. => (x, 0., chroma),
        _ => (chroma, 0., x),
    };
    let m = v - chroma;
    let channel = |c: f32| ((c + m) * 255.).round() as u8;
    format!("#{:02x}{:02x}{:02x}", channel(r), channel(g), channel(b))
}

pub(super) fn hex_color(hex: &str) -> Hsla {
    rgb(u32::from_str_radix(&hex[1..], 16).unwrap_or(0)).into()
}

/// A color picked against `base`, the persisted value it replaces.
struct Draft {
    color: String,
    base: String,
    previewing: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Drag {
    Plane,
    Hue,
}

type Commit = Rc<dyn Fn(String, &mut Window, &mut App)>;

pub(super) struct AccentPicker {
    display_name: SharedString,
    value: String,
    draft: Option<Draft>,
    open: bool,
    hsv: Hsv,
    /// The latest dragged color while a pointer drag is in progress.
    drag: Option<(Drag, String)>,
    plane: Rc<Cell<Bounds<Pixels>>>,
    hue: Rc<Cell<Bounds<Pixels>>>,
    hex: Entity<InputState>,
    /// The hex field holds what the user typed until it blurs.
    typing: bool,
    on_commit: Commit,
    _subscriptions: Vec<Subscription>,
}

/// The accent swatch button and its popover (the web's `layout="inline"`, which every caller uses).
pub(super) fn accent_color_picker(
    id: impl Into<ElementId>,
    display_name: &str,
    value: Option<&str>,
    on_commit: impl Fn(String, &mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let on_commit: Commit = Rc::new(on_commit);
    let picker = window.use_keyed_state(id, cx, |window, cx| AccentPicker::new(window, cx));
    let value = value.unwrap_or_default().to_owned();
    let display_name: SharedString = display_name.to_owned().into();
    picker.update(cx, |picker, _| {
        if picker.draft.as_ref().is_some_and(|draft| draft.base != value) {
            picker.draft = None;
        }
        picker.value = value;
        picker.display_name = display_name;
        picker.on_commit = on_commit;
    });
    picker.into_any_element()
}

impl AccentPicker {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let hex = cx.new(|cx| InputState::new(window, cx));
        let subscription = cx.subscribe_in(&hex, window, |this, input, event: &InputEvent, window, cx| match event {
            InputEvent::Change if this.open => {
                this.typing = true;
                let typed = input.read(cx).value().to_string();
                if normalize_accent_color(Some(&typed)).as_deref() == Some(typed.as_str()) {
                    this.hsv = hex_to_hsv(&typed);
                    this.commit(typed, window, cx);
                }
            }
            InputEvent::Blur => {
                this.typing = false;
                this.sync_hex(window, cx);
            }
            _ => {}
        });
        Self {
            display_name: SharedString::default(),
            value: String::new(),
            draft: None,
            open: false,
            hsv: hex_to_hsv(FALLBACK_ACCENT_COLOR),
            drag: None,
            plane: Rc::default(),
            hue: Rc::default(),
            hex,
            typing: false,
            on_commit: Rc::new(|_, _, _| {}),
            _subscriptions: vec![subscription],
        }
    }

    /// The color on the swatch: a dragged or just-committed draft while the
    /// persisted value is still the one it was picked against.
    fn shown(&self) -> Option<String> {
        let shown = self.draft.as_ref().filter(|draft| draft.base == self.value).map_or(&self.value, |d| &d.color);
        normalize_accent_color(Some(shown))
    }

    fn current(&self) -> String {
        hsv_to_hex(self.hsv)
    }

    fn commit(&mut self, color: String, window: &mut Window, cx: &mut Context<Self>) {
        let normalized = normalize_accent_color(Some(&color)).unwrap_or_default();
        self.draft = Some(Draft { color: normalized.clone(), base: self.value.clone(), previewing: false });
        // Not deduplicated against `value`: it is stale while an earlier commit
        // is in flight, so returning to it must still be submitted.
        (self.on_commit)(normalized, window, cx);
        cx.notify();
    }

    fn sync_hex(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.typing {
            return;
        }
        let color = self.current();
        self.hex.update(cx, |input, cx| input.set_value(color, window, cx));
    }

    fn set_open(&mut self, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        if open == self.open {
            return;
        }
        self.open = open;
        if open {
            self.hsv = hex_to_hsv(&self.shown().unwrap_or_else(|| FALLBACK_ACCENT_COLOR.to_owned()));
            self.typing = false;
            self.sync_hex(window, cx);
        } else if let Some(draft) = self.draft.as_ref().filter(|d| d.previewing && d.base == self.value) {
            // Closing commits a drag that has not been committed yet.
            let color = draft.color.clone();
            self.commit(color, window, cx);
        }
        cx.notify();
    }

    fn drag_to(&mut self, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let Some((target, _)) = self.drag else { return };
        let ratio = |offset: Pixels, length: Pixels| {
            if length <= px(0.) { 0. } else { (offset / length).clamp(0., 1.) }
        };
        match target {
            Drag::Plane => {
                let bounds = self.plane.get();
                self.hsv.s = ratio(position.x - bounds.origin.x, bounds.size.width);
                self.hsv.v = 1. - ratio(position.y - bounds.origin.y, bounds.size.height);
            }
            Drag::Hue => {
                let bounds = self.hue.get();
                self.hsv.h = ratio(position.x - bounds.origin.x, bounds.size.width) * 360.;
            }
        }
        let color = self.current();
        self.drag = Some((target, color.clone()));
        self.draft = Some(Draft { color, base: self.value.clone(), previewing: true });
        self.sync_hex(window, cx);
        cx.notify();
    }

    fn end_drag(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((_, color)) = self.drag.take() {
            self.commit(color, window, cx);
        }
    }

    fn start_drag(&mut self, target: Drag, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.drag = Some((target, self.current()));
        self.drag_to(event.position, window, cx);
    }

    /// Records the element's bounds each frame and, while dragging, follows
    /// the pointer anywhere in the window until the button is released.
    fn tracker(&self, bounds: Rc<Cell<Bounds<Pixels>>>, cx: &mut Context<Self>) -> impl IntoElement {
        let dragging = self.drag.is_some();
        let picker = cx.entity();
        canvas(
            move |bounds, _, _| bounds,
            move |_, measured, window, cx| {
                // The thumbs are placed from these bounds, so a new size renders the picker again.
                if bounds.replace(measured) != measured {
                    let picker = picker.clone();
                    cx.defer(move |cx| picker.update(cx, |_, cx| cx.notify()));
                }
                if !dragging {
                    return;
                }
                let moved = picker.clone();
                window.on_mouse_event(move |event: &MouseMoveEvent, _, window, cx| {
                    moved.update(cx, |this, cx| this.drag_to(event.position, window, cx));
                });
                window.on_mouse_event(move |_: &MouseUpEvent, _, window, cx| {
                    picker.update(cx, |this, cx| this.end_drag(window, cx));
                });
            },
        )
        .absolute()
        .inset_0()
    }

    fn panel(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let t = *theme::tokens(cx);
        let Hsv { h, s, v } = self.hsv;
        let pure_hue = hsla(h / 360., 1., 0.5, 1.);
        let has_value = self.shown().is_some();
        let plane = div()
            .id("accent-plane")
            .relative()
            .h(px(144.))
            .w_full()
            .overflow_hidden()
            .cursor_crosshair()
            .bg(pure_hue)
            .child(div().absolute().inset_0().bg(linear_gradient(
                90.,
                linear_color_stop(hsla(0., 0., 1., 1.), 0.),
                linear_color_stop(hsla(0., 0., 1., 0.), 1.),
            )))
            .child(div().absolute().inset_0().bg(linear_gradient(
                0.,
                linear_color_stop(hsla(0., 0., 0., 1.), 0.),
                linear_color_stop(hsla(0., 0., 0., 0.), 1.),
            )))
            .child(self.tracker(self.plane.clone(), cx))
            .child(thumb(px(12.), None).left(thumb_offset(s, self.plane.get().size.width, px(12.))).top(thumb_offset(
                1. - v,
                self.plane.get().size.height,
                px(12.),
            )))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event, window, cx| this.start_drag(Drag::Plane, event, window, cx)),
            )
            // A quick click releases before the window-wide release listener is registered.
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _, window, cx| this.end_drag(window, cx)));
        // Six two-stop segments draw the web's seven-stop hue gradient.
        let stops = [0xff0000, 0xffff00, 0x00ff00, 0x00ffff, 0x0000ff, 0xff00ff, 0xff0000];
        let hue = div()
            .id("accent-hue")
            .relative()
            .h(px(24.))
            .w_full()
            .flex()
            .items_center()
            .cursor_pointer()
            .child(h_flex().h(px(10.)).w_full().rounded_full().overflow_hidden().children(stops.windows(2).map(
                |pair| {
                    div().flex_1().h_full().bg(linear_gradient(
                        90.,
                        linear_color_stop(rgb(pair[0]), 0.),
                        linear_color_stop(rgb(pair[1]), 1.),
                    ))
                },
            )))
            .child(self.tracker(self.hue.clone(), cx))
            .child(thumb(px(16.), Some(pure_hue)).left(thumb_offset(h / 360., self.hue.get().size.width, px(16.))))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event, window, cx| this.start_drag(Drag::Hue, event, window, cx)),
            )
            // A quick click releases before the window-wide release listener is registered.
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _, window, cx| this.end_drag(window, cx)));
        v_flex()
            .w(px(224.))
            .bg(t.popover)
            .child(plane)
            .child(
                v_flex().gap_3().p_3().child(hue).child(Input::new(&self.hex).small().font_family(t.fonts.mono)).when(
                    !has_value,
                    |el| {
                        el.child(Button::new("accent-add").primary().compact().label("Add color").on_click(
                            cx.listener(|this, _, window, cx| {
                                let color = this.current();
                                this.commit(color, window, cx);
                                this.set_open(false, window, cx);
                            }),
                        ))
                    },
                ),
            )
            .when(has_value, |el| {
                el.child(
                    div().border_t_1().border_color(t.border.opacity(0.6)).p_1().child(
                        Button::new("accent-clear")
                            .ghost()
                            .compact()
                            .w_full()
                            .justify_start()
                            .text_color(t.muted_foreground)
                            .icon(Icon::new(IconName::Close).size(px(14.)))
                            .label("Clear color")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.commit(String::new(), window, cx);
                                this.set_open(false, window, cx);
                            })),
                    ),
                )
            })
            .into_any_element()
    }
}

/// `calc(ratio * (100% - size) + size / 2)`, less half the thumb to centre it.
fn thumb_offset(ratio: f32, length: Pixels, size: Pixels) -> Pixels {
    (length - size) * ratio
}

fn thumb(size: Pixels, fill: Option<Hsla>) -> gpui_kit::Div {
    div()
        .absolute()
        .size(size)
        .rounded_full()
        .border_2()
        .border_color(hsla(0., 0., 1., 1.))
        .when_some(fill, |el, fill| el.bg(fill))
        .shadow(vec![gpui_kit::BoxShadow::new(px(0.), px(0.), hsla(0., 0., 0., 0.4)).spread_radius(px(1.))])
}

impl Render for AccentPicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *theme::tokens(cx);
        let shown = self.shown();
        let label =
            format!("{} accent color for {}", if shown.is_some() { "Change" } else { "Add" }, self.display_name);
        let trigger = Button::new("accent-trigger")
            .ghost()
            .size(px(28.))
            .text_color(t.muted_foreground)
            .accessibility_label(label)
            .map(|button| match &shown {
                Some(color) => button.bg(hex_color(color)),
                None => button.icon(IconName::Plus),
            });
        let picker = cx.entity();
        let content_picker = picker.clone();
        Popover::new("accent-popover")
            .p_0()
            .overflow_hidden()
            .offset(px(6.))
            .open(self.open)
            .on_open_change(move |open, window, cx| {
                picker.update(cx, |this, cx| this.set_open(*open, window, cx));
            })
            .trigger(trigger)
            .content(move |_, _, cx| content_picker.update(cx, |this, cx| this.panel(cx)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips_through_hsv() {
        for hex in ["#2563eb", "#000000", "#ffffff", "#ff0000", "#00d492", "#d97757"] {
            assert_eq!(hsv_to_hex(hex_to_hsv(hex)), hex);
        }
        assert_eq!(hsv_to_hex(Hsv { h: 360., s: 1., v: 1. }), "#ff0000");
    }

    #[test]
    fn only_six_digit_hex_accents_are_kept() {
        assert_eq!(normalize_accent_color(Some(" #A1b2C3 ")).as_deref(), Some("#A1b2C3"));
        for value in [None, Some(""), Some("#abc"), Some("blue"), Some("#gggggg")] {
            assert_eq!(normalize_accent_color(value), None);
        }
    }
}
