//! `HistorySwipeNavigation`: two-finger horizontal trackpad swipes go back and forward, with a
//! circle peeking in from the edge of the main content while the swipe tracks.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::component::{Icon, IconName, Sizable as _, WindowExt as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Animation, AnimationExt as _, AnyElement, App, Bounds, Context, DispatchPhase, ElementId, Global,
    InteractiveElement as _, IntoElement as _, ParentElement as _, Pixels, ScrollDelta, ScrollEvent, ScrollHandle,
    ScrollSource, Stateful, StatefulInteractiveElement as _, Styled as _, Task, TouchPhase, Window, canvas, div, px,
};

use super::Workspace;

const TRIGGER_DISTANCE: f32 = 120.;
/// Movement needed before the gesture picks an axis; first events are often diagonal.
const AXIS_LOCK_DISTANCE: f32 = 8.;
const MOMENTUM_SAMPLE_SIZE: usize = 5;
/// Without the platform's scroll phases (X11, Wayland), a pause this long ends the scroll, as
/// Chromium's wheel scroll latching does.
const IDLE_END: Duration = Duration::from_millis(150);
const INDICATOR: f32 = 56.;
const FADE: Duration = Duration::from_millis(200);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Direction {
    Back,
    Forward,
}

#[derive(Debug, PartialEq)]
enum Gesture {
    Pending {
        sum_x: f32,
        sum_y: f32,
        y: f32,
        dialog: bool,
        scroll_back: bool,
        scroll_forward: bool,
    },
    Tracking {
        direction: Direction,
        distance: f32,
        deltas: Vec<f32>,
        y: f32,
    },
    /// Not a swipe, or one already decided: the rest of its events are ignored.
    Ignored,
}

/// One scroll step, in the DOM's sign convention (positive `dx` scrolls the content left).
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Wheel {
    pub dx: f32,
    pub dy: f32,
    /// Where the pointer is, from the top of the main content.
    pub y: f32,
    /// `deltaMode === DOM_DELTA_PIXEL`: a trackpad, not wheel notches.
    pub pixels: bool,
    pub ctrl: bool,
    /// A dialog is open over the pointer (`[role="dialog"]` on the event's path).
    pub dialog: bool,
    /// A horizontal scroller under the pointer can still scroll left or right.
    pub scroll_back: bool,
    pub scroll_forward: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Indicator {
    pub direction: Direction,
    pub progress: f32,
    pub y: f32,
    pub tracking: bool,
}

#[derive(Debug, PartialEq)]
pub(super) enum Step {
    Unchanged,
    Show(Indicator),
    /// The fingers lifted or the swipe ended: navigate when it went far enough.
    Release(Option<Direction>),
}

/// The gesture state machine, free of GPUI so its rules can be tested.
#[derive(Debug, Default)]
pub(super) struct Swipe {
    gesture: Option<Gesture>,
}

/// macOS momentum after the fingers lift decays by a near-constant ratio per event; finger
/// movement is noisier. Deltas are signed along the swipe.
pub(super) fn is_momentum_tail(deltas: &[f32]) -> bool {
    if deltas.len() < MOMENTUM_SAMPLE_SIZE {
        return false;
    }
    let recent = &deltas[deltas.len() - MOMENTUM_SAMPLE_SIZE..];
    if recent.iter().any(|delta| *delta <= 0.) {
        return false;
    }
    let ratios: Vec<f32> = recent.windows(2).map(|pair| pair[1] / pair[0]).collect();
    let (min, max) = ratios.iter().fold((f32::MAX, f32::MIN), |(min, max), r| (min.min(*r), max.max(*r)));
    ratios.iter().all(|ratio| (0.7..1.).contains(ratio)) && max - min < 0.1
}

impl Swipe {
    /// A new trackpad scroll began: a decided gesture's leftovers no longer count.
    pub(super) fn begin(&mut self) {
        if self.gesture == Some(Gesture::Ignored) {
            self.gesture = None;
        }
    }

    /// The fingers lifted: what follows is momentum, ignored until the next [`Self::begin`].
    pub(super) fn lift(&mut self) -> Step {
        match self.gesture {
            Some(Gesture::Tracking { .. }) => self.release(),
            _ => {
                self.gesture = Some(Gesture::Ignored);
                Step::Unchanged
            }
        }
    }

    /// The scroll ended (a pause where the platform has no phases): the next one starts fresh.
    pub(super) fn end(&mut self) -> Step {
        let step =
            if matches!(self.gesture, Some(Gesture::Tracking { .. })) { self.release() } else { Step::Unchanged };
        self.gesture = None;
        step
    }

    /// The system took the touch: unwind without navigating.
    pub(super) fn cancel(&mut self) -> Step {
        let tracking = matches!(self.gesture, Some(Gesture::Tracking { .. }));
        self.gesture = Some(Gesture::Ignored);
        if tracking { Step::Release(None) } else { Step::Unchanged }
    }

    pub(super) fn active(&self) -> bool {
        self.gesture.is_some()
    }

    fn release(&mut self) -> Step {
        let navigate = match self.gesture {
            Some(Gesture::Tracking { direction, distance, .. }) if distance >= TRIGGER_DISTANCE => Some(direction),
            _ => None,
        };
        self.gesture = Some(Gesture::Ignored);
        Step::Release(navigate)
    }

    pub(super) fn wheel(&mut self, wheel: Wheel, can_navigate: impl Fn(Direction) -> bool) -> Step {
        let gesture = self.gesture.get_or_insert(if wheel.ctrl || !wheel.pixels {
            Gesture::Ignored
        } else {
            Gesture::Pending {
                sum_x: 0.,
                sum_y: 0.,
                y: wheel.y,
                dialog: false,
                scroll_back: false,
                scroll_forward: false,
            }
        });
        match gesture {
            Gesture::Ignored => Step::Unchanged,
            Gesture::Pending { sum_x, sum_y, y, dialog, scroll_back, scroll_forward } => {
                *sum_x += wheel.dx;
                *sum_y += wheel.dy;
                *dialog |= wheel.dialog;
                *scroll_back |= wheel.scroll_back;
                *scroll_forward |= wheel.scroll_forward;
                if sum_x.abs() + sum_y.abs() < AXIS_LOCK_DISTANCE {
                    return Step::Unchanged;
                }
                let direction = if *sum_x < 0. { Direction::Back } else { Direction::Forward };
                let scrolls = match direction {
                    Direction::Back => *scroll_back,
                    Direction::Forward => *scroll_forward,
                };
                let ignored = sum_x.abs() <= sum_y.abs() || *dialog || scrolls || !can_navigate(direction);
                *gesture = if ignored {
                    Gesture::Ignored
                } else {
                    Gesture::Tracking { direction, distance: sum_x.abs(), deltas: Vec::new(), y: *y }
                };
                self.indicator()
            }
            Gesture::Tracking { direction, distance, deltas, .. } => {
                if wheel.dx == 0. && wheel.dy == 0. {
                    return self.release();
                }
                let delta = if *direction == Direction::Back { -wheel.dx } else { wheel.dx };
                if deltas.len() >= MOMENTUM_SAMPLE_SIZE {
                    deltas.remove(0);
                }
                deltas.push(delta);
                if is_momentum_tail(deltas) {
                    return self.release();
                }
                *distance = (*distance + delta).max(0.);
                self.indicator()
            }
        }
    }

    fn indicator(&self) -> Step {
        match self.gesture {
            Some(Gesture::Tracking { direction, distance, y, .. }) => {
                Step::Show(Indicator { direction, progress: (distance / TRIGGER_DISTANCE).min(1.), y, tracking: true })
            }
            _ => Step::Unchanged,
        }
    }
}

/// What horizontal scrollers under the pointer reported for the scroll event being dispatched.
/// They run before the workspace's listener, which takes it.
#[derive(Default)]
struct ScrollerClaims {
    back: bool,
    forward: bool,
}

impl Global for ScrollerClaims {}

/// Tracks `el`'s horizontal scrolling with `handle`, so a swipe that would scroll it does not
/// navigate (`canScrollHorizontally`).
pub(super) fn horizontal_scroller(el: Stateful<gpui_kit::Div>, handle: &ScrollHandle) -> Stateful<gpui_kit::Div> {
    let handle = handle.clone();
    el.overflow_x_scroll().track_scroll(&handle).on_scroll(move |_, _, cx| {
        let (offset, max) = (-handle.offset().x, handle.max_offset().x);
        let claims = cx.default_global::<ScrollerClaims>();
        claims.back |= offset > px(0.);
        claims.forward |= offset < max - px(1.);
    })
}

/// The workspace's swipe state: the gesture, the indicator and the main content's bounds.
#[derive(Default)]
pub(super) struct SwipeState {
    swipe: Swipe,
    /// Seen a scroll phase: the platform brackets scrolls, so no pause ends one.
    phased: bool,
    indicator: Option<Indicator>,
    /// Bumped on each release, so the fade-out animation restarts.
    fades: u64,
    bounds: Rc<Cell<Bounds<Pixels>>>,
    _idle: Option<Task<()>>,
}

impl Workspace {
    /// Measures the main content and listens to every scroll in the window, after the
    /// scrollers under the pointer (it paints first, so its bubble listener runs last).
    pub(super) fn render_history_swipe_probe(&self, cx: &mut Context<Self>) -> AnyElement {
        let bounds = self.swipe.bounds.clone();
        let workspace = cx.weak_entity();
        canvas(
            move |element_bounds, _, _| bounds.set(element_bounds),
            move |_, (), window, _| {
                window.on_mouse_event(move |event: &ScrollEvent, phase, window, cx| {
                    if phase != DispatchPhase::Bubble {
                        return;
                    }
                    let claims = std::mem::take(cx.default_global::<ScrollerClaims>());
                    let _ = workspace.update(cx, |this, cx| this.history_swipe_scroll(event, &claims, window, cx));
                });
            },
        )
        .absolute()
        .size_full()
        .into_any_element()
    }

    fn history_swipe_scroll(
        &mut self,
        event: &ScrollEvent,
        claims: &ScrollerClaims,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.source == ScrollSource::Touch {
            return;
        }
        if event.touch_phase != TouchPhase::Moved {
            self.swipe.phased = true;
        }
        let step = match event.touch_phase {
            TouchPhase::Ended => self.swipe.swipe.lift(),
            TouchPhase::Cancelled => self.swipe.swipe.cancel(),
            phase => {
                if phase == TouchPhase::Started {
                    self.swipe.swipe.begin();
                }
                let (pixels, delta) = match event.delta {
                    ScrollDelta::Pixels(delta) => (true, (f32::from(delta.x), f32::from(delta.y))),
                    ScrollDelta::Lines(delta) => (false, (delta.x, delta.y)),
                };
                let top = self.swipe.bounds.get().origin.y;
                let wheel = Wheel {
                    // GPUI's deltas move the content; the DOM's move the viewport.
                    dx: -delta.0,
                    dy: -delta.1,
                    y: f32::from(event.position.y - top),
                    pixels,
                    ctrl: event.modifiers.control,
                    dialog: self.palette.is_some() || self.sidebar_open.sheet() || window.has_active_dialog(cx),
                    scroll_back: claims.back,
                    scroll_forward: claims.forward,
                };
                let history = &self.history;
                self.swipe.swipe.wheel(wheel, |direction| match direction {
                    Direction::Back => history.can_go_back(),
                    Direction::Forward => history.can_go_forward(),
                })
            }
        };
        self.watch_swipe_idle(window, cx);
        self.apply_swipe_step(step, window, cx);
    }

    /// Where the platform has no scroll phases, a pause ends the scroll.
    fn watch_swipe_idle(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.swipe.phased || !self.swipe.swipe.active() {
            self.swipe._idle = None;
            return;
        }
        // Replacing the task cancels the previous wait.
        self.swipe._idle = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(IDLE_END).await;
            let _ = this.update_in(cx, |this, window, cx| {
                let step = this.swipe.swipe.end();
                this.apply_swipe_step(step, window, cx);
            });
        }));
    }

    fn apply_swipe_step(&mut self, step: Step, window: &mut Window, cx: &mut Context<Self>) {
        match step {
            Step::Unchanged => {}
            Step::Show(indicator) => {
                self.swipe.indicator = Some(indicator);
                cx.notify();
            }
            Step::Release(navigate) => {
                if let Some(indicator) = &mut self.swipe.indicator
                    && indicator.tracking
                {
                    indicator.tracking = false;
                    self.swipe.fades += 1;
                    cx.notify();
                }
                match navigate {
                    Some(Direction::Back) => self.go_back(window, cx),
                    Some(Direction::Forward) => self.go_forward(window, cx),
                    None => {}
                }
            }
        }
    }

    /// `SwipeIndicator`: a circle pinned to the edge of the main content that peeks in up to
    /// half its width.
    pub(super) fn render_history_swipe(&self, cx: &App) -> Option<AnyElement> {
        let indicator = self.swipe.indicator?;
        let t = crate::theme::tokens(cx);
        let bounds = self.swipe.bounds.get();
        let size = px(INDICATOR);
        let top = (px(indicator.y) - size / 2.).min(bounds.size.height - size).max(px(0.));
        let hidden = size * (1. - indicator.progress * 0.5);
        let back = indicator.direction == Direction::Back;
        let (background, foreground) =
            if t.dark { (gpui_kit::black(), gpui_kit::white()) } else { (t.muted, t.foreground) };
        let armed = indicator.progress >= 1.;
        let circle = div()
            .absolute()
            .top(top)
            .size(size)
            .flex()
            .items_center()
            .rounded_full()
            .bg(background)
            .text_color(foreground)
            .when(back, |el| el.left(-hidden).justify_end().pr(px(8.)))
            .when(!back, |el| el.right(-hidden).justify_start().pl(px(8.)))
            .child(
                Icon::new(if back { IconName::ArrowLeft } else { IconName::ArrowRight })
                    .with_size(px(16.))
                    .text_color(foreground.opacity(if armed { 1. } else { 0.4 })),
            );
        let circle = if indicator.tracking {
            circle.into_any_element()
        } else {
            circle
                .with_animation(
                    ElementId::NamedInteger("history-swipe-fade".into(), self.swipe.fades),
                    Animation::new(FADE).with_easing(|t| 1. - (1. - t).powi(2)),
                    |el, progress| el.opacity(1. - progress),
                )
                .into_any_element()
        };
        Some(
            div()
                .absolute()
                .left(bounds.origin.x)
                .top(bounds.origin.y)
                .w(bounds.size.width)
                .h(bounds.size.height)
                .overflow_hidden()
                .child(circle)
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wheel(dx: f32, dy: f32) -> Wheel {
        Wheel { dx, dy, y: 40., pixels: true, ..Wheel::default() }
    }

    fn swipe(steps: &[(f32, f32)], can: bool) -> (Swipe, Step) {
        let mut swipe = Swipe::default();
        let mut last = Step::Unchanged;
        for (dx, dy) in steps {
            last = swipe.wheel(wheel(*dx, *dy), |_| can);
        }
        (swipe, last)
    }

    #[test]
    fn momentum_tails_decay_by_a_steady_ratio() {
        assert!(is_momentum_tail(&[40., 37., 34., 31.3, 28.8]));
        assert!(!is_momentum_tail(&[12., 18., 9., 15., 11.]), "fingers still moving");
        assert!(!is_momentum_tail(&[30., 22., 20., 12., 11.]), "fingers slowing unevenly");
        assert!(!is_momentum_tail(&[10., 8., 6., 4., -2.]), "reversing");
        assert!(!is_momentum_tail(&[40., 37., 34.]), "too few samples");
    }

    #[test]
    fn a_horizontal_swipe_locks_its_axis_then_tracks_toward_the_trigger() {
        let (_, step) = swipe(&[(-3., 1.)], true);
        assert_eq!(step, Step::Unchanged, "too little movement to pick an axis");
        let (mut swipe, step) = swipe(&[(-3., 1.), (-9., 2.)], true);
        let Step::Show(indicator) = step else { panic!("{step:?}") };
        assert_eq!(indicator.direction, Direction::Back);
        assert!((indicator.progress - 12. / TRIGGER_DISTANCE).abs() < 1e-6);
        assert_eq!(indicator.y, 40.);
        swipe.wheel(wheel(-200., 0.), |_| true);
        assert_eq!(swipe.lift(), Step::Release(Some(Direction::Back)));
        assert_eq!(swipe.wheel(wheel(-30., 0.), |_| true), Step::Unchanged, "momentum after the lift is ignored");
        swipe.begin();
        assert!(matches!(
            swipe.wheel(wheel(20., 0.), |_| true),
            Step::Show(Indicator { direction: Direction::Forward, .. })
        ));
    }

    #[test]
    fn swipes_that_are_not_navigation_are_ignored() {
        assert_eq!(swipe(&[(2., 9.), (-200., 0.)], true).1, Step::Unchanged, "vertical first");
        assert_eq!(swipe(&[(-20., 0.)], false).1, Step::Unchanged, "no history that way");
        let mut lines = Swipe::default();
        assert_eq!(lines.wheel(Wheel { pixels: false, ..wheel(-20., 0.) }, |_| true), Step::Unchanged);
        let mut zoom = Swipe::default();
        assert_eq!(zoom.wheel(Wheel { ctrl: true, ..wheel(-20., 0.) }, |_| true), Step::Unchanged);
        let mut dialog = Swipe::default();
        assert_eq!(dialog.wheel(Wheel { dialog: true, ..wheel(-20., 0.) }, |_| true), Step::Unchanged);
        // A scroller that can still scroll that way takes the swipe; one at its edge does not.
        let mut scroller = Swipe::default();
        scroller.wheel(Wheel { scroll_back: true, ..wheel(-4., 0.) }, |_| true);
        assert_eq!(scroller.wheel(wheel(-6., 0.), |_| true), Step::Unchanged);
        let mut edge = Swipe::default();
        assert!(matches!(edge.wheel(Wheel { scroll_forward: true, ..wheel(-20., 0.) }, |_| true), Step::Show(_)));
    }

    #[test]
    fn a_swipe_releases_on_a_stop_or_momentum_and_navigates_only_past_the_trigger() {
        let (mut short, _) = swipe(&[(-20., 0.)], true);
        assert_eq!(short.wheel(wheel(0., 0.), |_| true), Step::Release(None), "a zero step is the release");
        let (mut decaying, _) = swipe(&[(-100., 0.)], true);
        let mut last = Step::Unchanged;
        for delta in [-40., -37., -34., -31.3, -28.8] {
            last = decaying.wheel(wheel(delta, 0.), |_| true);
        }
        assert_eq!(last, Step::Release(Some(Direction::Back)), "momentum releases the swipe");
        // Swiping back past the start never goes negative, and ending the scroll releases it.
        let (mut reversed, _) = swipe(&[(-20., 0.), (50., 0.)], true);
        let Step::Show(indicator) = reversed.wheel(wheel(-1., 0.), |_| true) else { panic!() };
        assert!((indicator.progress - 1. / TRIGGER_DISTANCE).abs() < 1e-6);
        assert_eq!(reversed.end(), Step::Release(None));
        assert!(!reversed.active(), "the next scroll starts fresh");
        let (mut cancelled, _) = swipe(&[(-200., 0.)], true);
        assert_eq!(cancelled.cancel(), Step::Release(None), "a cancelled touch never navigates");
    }
}
