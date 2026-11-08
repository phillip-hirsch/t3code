//! `ToggleGroup variant="segmented"`: Base UI's composite keyboard model on native buttons.

use std::rc::Rc;

use gpui_kit::component::h_flex;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, ElementId, FocusHandle, FontWeight, Hsla, InteractiveElement as _, IntoElement, KeyDownEvent,
    ParentElement as _, RenderOnce, SharedString, Styled as _, Window, px,
};

use crate::theme;

type Press = Rc<dyn Fn(&mut Window, &mut App)>;

struct Segment {
    key: SharedString,
    label: SharedString,
    pressed: bool,
    press: Press,
}

/// One Tab stop (the focused segment, else the pressed one); the arrow keys, Home and End move
/// focus between segments and wrap; Enter or Space presses the focused segment.
#[derive(IntoElement)]
pub(super) struct Segmented {
    id: SharedString,
    segments: Vec<Segment>,
    disabled: bool,
}

impl Segmented {
    pub(super) fn new(id: impl Into<SharedString>) -> Self {
        Self { id: id.into(), segments: Vec::new(), disabled: false }
    }

    pub(super) fn segment(
        mut self,
        key: impl Into<SharedString>,
        label: impl Into<SharedString>,
        pressed: bool,
        press: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        self.segments.push(Segment { key: key.into(), label: label.into(), pressed, press: Rc::new(press) });
        self
    }

    pub(super) fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

/// The segment a navigation key moves focus to, from `current` of `len`.
fn roving_target(key: &str, current: usize, len: usize) -> Option<usize> {
    match key {
        _ if len == 0 => None,
        "left" => Some((current + len - 1) % len),
        "right" => Some((current + 1) % len),
        "home" => Some(0),
        "end" => Some(len - 1),
        _ => None,
    }
}

impl RenderOnce for Segmented {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = *theme::tokens(cx);
        let handles: Vec<FocusHandle> = self
            .segments
            .iter()
            .map(|segment| {
                let key = ElementId::Name(format!("{}-{}-focus", self.id, segment.key).into());
                window.use_keyed_state(key, cx, |_, cx| cx.focus_handle()).read(cx).clone()
            })
            .collect();
        let stop = handles
            .iter()
            .position(|h| h.is_focused(window))
            .or_else(|| self.segments.iter().position(|s| s.pressed))
            .unwrap_or(0);
        let nav = handles.clone();
        let (track, ring) = (Hsla { a: t.input.a * 0.4, ..t.input }, t.ring);
        let pressed_bg = if t.dark { Hsla { a: t.input.a * 0.72, ..t.input } } else { t.background };
        let hover_bg = if t.dark {
            Hsla { a: t.input.a * 0.32, ..t.input }
        } else {
            Hsla { a: t.background.a * 0.55, ..t.background }
        };
        let id = self.id.clone();
        h_flex()
            .id(ElementId::Name(self.id))
            .flex_none()
            .gap(px(2.))
            .p(px(2.))
            .rounded(px(8.))
            .bg(track)
            .on_key_down(move |event: &KeyDownEvent, window, cx| {
                if event.keystroke.modifiers.modified() {
                    return;
                }
                let Some(current) = nav.iter().position(|h| h.is_focused(window)) else { return };
                if let Some(next) = roving_target(event.keystroke.key.as_str(), current, nav.len()) {
                    cx.stop_propagation();
                    nav[next].focus(window, cx);
                }
            })
            .children(self.segments.into_iter().zip(handles).enumerate().map(|(index, (segment, focus))| {
                let press = segment.press;
                gpui_kit::base::Button::new(ElementId::Name(format!("{id}-{}", segment.key).into()))
                    .track_focus(&focus)
                    .tab_stop(index == stop)
                    .disabled(self.disabled)
                    .h(px(24.))
                    .px(px(10.))
                    .rounded(px(6.))
                    // `text-xs` with the base's `sm:text-sm`, which wins at desktop widths.
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .whitespace_nowrap()
                    .focus_visible(move |s| {
                        s.shadow(vec![gpui_kit::BoxShadow::new(px(0.), px(0.), ring).spread_radius(px(2.)).inset()])
                    })
                    .map(|el| {
                        if segment.pressed {
                            el.text_color(t.foreground).bg(pressed_bg)
                        } else {
                            el.text_color(t.muted_foreground)
                                .when(!self.disabled, |el| el.hover(|s| s.text_color(t.foreground).bg(hover_bg)))
                        }
                    })
                    .when(self.disabled, |el| el.opacity(0.64))
                    .when(!self.disabled, |el| el.cursor_pointer())
                    .child(segment.label)
                    .on_click(move |_, window, cx| press(window, cx))
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::roving_target;

    #[test]
    fn arrows_wrap_and_home_end_jump() {
        assert_eq!(roving_target("right", 0, 3), Some(1));
        assert_eq!(roving_target("right", 2, 3), Some(0));
        assert_eq!(roving_target("left", 0, 3), Some(2));
        assert_eq!(roving_target("left", 2, 3), Some(1));
        assert_eq!(roving_target("home", 2, 3), Some(0));
        assert_eq!(roving_target("end", 0, 3), Some(2));
        assert_eq!(roving_target("up", 1, 3), None);
        assert_eq!(roving_target("right", 0, 0), None);
    }
}
