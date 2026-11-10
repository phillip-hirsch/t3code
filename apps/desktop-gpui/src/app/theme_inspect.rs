//! The theme editor's Inspect and usage spotlight (`themeInspector.ts`). The web finds the roles an
//! element paints with by probing computed styles; native views have no styles to probe, so each
//! themed surface names its roles where it paints ([`probe`]). While the editor highlights or
//! inspects, those probes record their visible bounds every frame; the overlay spotlights the ones
//! that paint the selected roles and counts them, and in inspect mode a click picks the role of the
//! innermost surface under the pointer.

use gpui_kit::{
    AnyElement, App, BorderStyle, Bounds, DispatchPhase, Entity, FontWeight, Global, Hsla, IntoElement, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement as _, Pixels, Point, ScrollEvent, Styled as _, canvas,
    div, fill, point, prelude::FluentBuilder as _, px, quad, rems,
};

use super::theme_editor::{ThemeEditor, family_label};
use crate::theme::{self, mix_oklab};

/// A surface's roles and its visible bounds this frame. The first role is the one a click picks
/// (the web checks backgrounds, then borders, then text).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::app) struct Probe {
    pub roles: &'static [&'static str],
    pub bounds: Bounds<Pixels>,
}

#[derive(Default)]
pub(in crate::app) struct Inspector {
    /// Probes record only while the editor highlights or inspects.
    active: bool,
    /// The probes of the frame being drawn, then of the last one drawn.
    frame: Vec<Probe>,
    last: Vec<Probe>,
    hover: Option<Probe>,
    /// How many surfaces paint the highlighted roles.
    uses: usize,
    /// A pick swallowed a press; its release must not reach the app either.
    swallow_up: bool,
}

impl Global for Inspector {}

/// Inspection is on: probes record and the sidebar renders uncached so its probes run.
pub(in crate::app) fn active(cx: &App) -> bool {
    cx.try_global::<Inspector>().is_some_and(|inspector| inspector.active)
}

/// How many surfaces paint the highlighted roles, as of the last frame.
pub(in crate::app) fn uses(cx: &App) -> usize {
    cx.try_global::<Inspector>().map_or(0, |inspector| inspector.uses)
}

/// Starts a frame: inspection follows the open editor's state.
pub(in crate::app) fn begin_frame(editor: Option<&Entity<ThemeEditor>>, cx: &mut App) {
    let active = editor.is_some_and(|editor| editor.read(cx).inspector_active());
    if !active && !cx.has_global::<Inspector>() {
        return;
    }
    let inspector = cx.default_global::<Inspector>();
    inspector.active = active;
    inspector.frame.clear();
    if !active {
        *inspector = Inspector::default();
    }
}

/// Marks the element it is a child of as painting `roles`, while inspection is on. The element
/// sizes it, so it needs no other layout.
pub(in crate::app) fn probe(roles: &'static [&'static str], cx: &App) -> Option<AnyElement> {
    active(cx).then(|| {
        canvas(
            move |bounds, window, cx| {
                let visible = bounds.intersect(&window.content_mask().bounds);
                if visible.size.width > px(0.) && visible.size.height > px(0.) && cx.has_global::<Inspector>() {
                    cx.global_mut::<Inspector>().frame.push(Probe { roles, bounds: visible });
                }
            },
            |_, (), _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .into_any_element()
    })
}

/// The innermost surface under `point` (`inspectThemeRoleAtElement` walks up from the target);
/// of equal ones, the one painted last, which is on top.
pub(super) fn hit(probes: &[Probe], point: Point<Pixels>) -> Option<Probe> {
    let area = |probe: &&Probe| f32::from(probe.bounds.size.width) * f32::from(probe.bounds.size.height);
    probes
        .iter()
        .rev()
        .filter(|probe| probe.bounds.contains(&point))
        .min_by(|a, b| area(a).total_cmp(&area(b)))
        .copied()
}

/// The distinct surfaces that paint any of `roles` (`highlightThemeRoleUsage`).
pub(super) fn matches(probes: &[Probe], roles: &[&str]) -> Vec<Bounds<Pixels>> {
    let mut found: Vec<Bounds<Pixels>> = Vec::new();
    let key = |bounds: &Bounds<Pixels>| {
        [bounds.origin.x, bounds.origin.y, bounds.size.width, bounds.size.height].map(|v| v.round())
    };
    for probe in probes.iter().filter(|probe| probe.roles.iter().any(|role| roles.contains(role))) {
        if !found.iter().any(|seen| key(seen) == key(&probe.bounds)) {
            found.push(probe.bounds);
        }
    }
    found
}

/// The viewport minus the spotlight holes, as disjoint rectangles: each horizontal band between
/// hole edges, less the holes that cross it.
pub(super) fn dimmed(viewport: Bounds<Pixels>, holes: &[Bounds<Pixels>]) -> Vec<Bounds<Pixels>> {
    let (top, bottom) = (viewport.top(), viewport.bottom());
    let mut edges: Vec<Pixels> = vec![top, bottom];
    for hole in holes {
        edges.extend([hole.top().clamp(top, bottom), hole.bottom().clamp(top, bottom)]);
    }
    edges.sort();
    edges.dedup();
    let mut rects = Vec::new();
    for band in edges.windows(2) {
        let (y0, y1) = (band[0], band[1]);
        let mut spans: Vec<(Pixels, Pixels)> = holes
            .iter()
            .filter(|hole| hole.top() < y1 && hole.bottom() > y0)
            .map(|hole| (hole.left().max(viewport.left()), hole.right().min(viewport.right())))
            .filter(|(left, right)| left < right)
            .collect();
        spans.sort();
        let mut x = viewport.left();
        for (left, right) in spans {
            if left > x {
                rects.push(Bounds::from_corners(point(x, y0), point(left, y1)));
            }
            x = x.max(right);
        }
        if x < viewport.right() {
            rects.push(Bounds::from_corners(point(x, y0), point(viewport.right(), y1)));
        }
    }
    rects
}

/// `spotlightRect`: 5px around the surface.
fn padded(bounds: Bounds<Pixels>) -> Bounds<Pixels> {
    bounds.dilate(px(5.))
}

/// `color-mix(in oklab, var(--ring) <percent>, white)`.
fn ring_toward_white(ring: Hsla, amount: f32) -> Hsla {
    mix_oklab(gpui_kit::white(), ring, amount)
}

/// The spotlight, the hover outline and its label, and in inspect mode the pointer handling, over
/// the app and under the editor (`#theme-inspector-spotlight`, `#theme-inspector-hover`).
pub(in crate::app) fn render_overlay(editor: Option<&Entity<ThemeEditor>>, cx: &App) -> Option<AnyElement> {
    let editor = editor?.clone();
    let inspector = cx.try_global::<Inspector>().filter(|inspector| inspector.active)?;
    let t = *theme::tokens(cx);
    let state = editor.read(cx);
    let inspecting = state.inspecting();
    let highlight = state.highlight_roles();
    let panel = state.panel_bounds();
    let hover = inspector.hover.filter(|_| inspecting).map(|probe| {
        let label = family_label(probe.roles[0]);
        let rect = padded(probe.bounds);
        let above = rect.top() >= px(32.);
        let tag = div()
            .absolute()
            .left_0()
            .map(|el| if above { el.bottom(rect.size.height + px(5.6)) } else { el.top(rect.size.height + px(5.6)) })
            .max_w(rems(12.))
            .overflow_hidden()
            .px(rems(0.45))
            .py(rems(0.2))
            .rounded(rems(0.4))
            .border_1()
            .border_color(mix_oklab(t.border, t.ring, 0.48))
            .bg(t.popover)
            .shadow_md()
            .text_color(t.foreground)
            .text_size(rems(0.6875))
            .font_weight(FontWeight::SEMIBOLD)
            .line_height(rems(1.))
            .whitespace_nowrap()
            .truncate()
            .child(label);
        div()
            .absolute()
            .left(rect.left())
            .top(rect.top())
            .w(rect.size.width)
            .h(rect.size.height)
            .rounded(px(9.))
            .border_1()
            .border_color(ring_toward_white(t.ring, 0.7))
            .bg(t.ring.opacity(0.07))
            .child(tag)
    });
    let weak = editor.downgrade();
    let spotlight = canvas(
        |_, _, _| {},
        move |_, (), window, cx| {
            let probes = std::mem::take(&mut cx.global_mut::<Inspector>().frame);
            let found = if highlight.is_empty() { Vec::new() } else { matches(&probes, &highlight) };
            let inspector = cx.global_mut::<Inspector>();
            if inspector.uses != found.len() {
                inspector.uses = found.len();
                // The editor's header shows the count; it rendered before these probes ran.
                window.on_next_frame(|window, _| window.refresh());
            }
            inspector.last = probes;
            if !found.is_empty() {
                let viewport = Bounds::new(Point::default(), window.viewport_size());
                let holes: Vec<_> = found.iter().copied().map(padded).collect();
                for rect in dimmed(viewport, &holes) {
                    window.paint_quad(fill(rect, gpui_kit::rgba(0x0203_088a)));
                }
                for hole in holes {
                    let glow = quad(
                        hole,
                        px(9.),
                        t.ring.opacity(0.09),
                        px(2.),
                        ring_toward_white(t.ring, 0.72).opacity(0.92),
                        BorderStyle::Solid,
                    );
                    window.paint_quad(glow);
                }
            }
            // A pick swallows its release too, even when it ends inspect mode before the next frame.
            window.on_mouse_event(|event: &MouseUpEvent, phase, _, cx| {
                let inspector = cx.global_mut::<Inspector>();
                if phase == DispatchPhase::Capture && event.button == MouseButton::Left && inspector.swallow_up {
                    inspector.swallow_up = false;
                    cx.stop_propagation();
                }
            });
            if !inspecting {
                return;
            }
            let outside = move |position: Point<Pixels>| !panel.contains(&position);
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                if phase != DispatchPhase::Capture {
                    return;
                }
                let inspector = cx.global_mut::<Inspector>();
                let next = if outside(event.position) { hit(&inspector.last, event.position) } else { None };
                if inspector.hover != next {
                    inspector.hover = next;
                    window.refresh();
                }
            });
            window.on_mouse_event(move |_: &ScrollEvent, phase, window, cx| {
                if phase == DispatchPhase::Capture && cx.global_mut::<Inspector>().hover.take().is_some() {
                    window.refresh();
                }
            });
            let picker = weak;
            window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                if phase != DispatchPhase::Capture || event.button != MouseButton::Left || !outside(event.position) {
                    return;
                }
                // The press and its release belong to the inspector, not the app under them.
                cx.stop_propagation();
                let inspector = cx.global_mut::<Inspector>();
                inspector.swallow_up = true;
                let Some(found) = hit(&inspector.last, event.position) else { return };
                inspector.hover = None;
                let _ = picker.update(cx, |editor, cx| editor.pick_from_app(found.roles[0], window, cx));
            });
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full();
    Some(div().absolute().top_0().left_0().size_full().child(spotlight).children(hover).into_any_element())
}

#[cfg(test)]
mod tests {
    use gpui_kit::size;

    use super::*;

    fn rect(x: f32, y: f32, w: f32, h: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(x), px(y)), size(px(w), px(h)))
    }

    #[test]
    fn a_click_picks_the_innermost_surface_and_the_topmost_of_equals() {
        let probes = [
            Probe { roles: &["chrome"], bounds: rect(0., 0., 100., 100.) },
            Probe { roles: &["canvas"], bounds: rect(10., 10., 80., 80.) },
            Probe { roles: &["surface"], bounds: rect(20., 20., 20., 20.) },
            Probe { roles: &["messageSurface"], bounds: rect(20., 20., 20., 20.) },
        ];
        let at = |x: f32, y: f32| hit(&probes, point(px(x), px(y))).map(|probe| probe.roles[0]);
        assert_eq!(at(25., 25.), Some("messageSurface"));
        assert_eq!(at(50., 50.), Some("canvas"));
        assert_eq!(at(5., 5.), Some("chrome"));
        assert_eq!(at(150., 5.), None);
    }

    #[test]
    fn usage_counts_each_surface_once() {
        let probes = [
            Probe { roles: &["codeBackground", "codeForeground"], bounds: rect(0., 0., 10., 10.) },
            Probe { roles: &["codeBackground"], bounds: rect(0., 0., 10., 10.) },
            Probe { roles: &["terminalForeground"], bounds: rect(0., 20., 10., 10.) },
            Probe { roles: &["canvas"], bounds: rect(0., 40., 10., 10.) },
        ];
        assert_eq!(matches(&probes, &["codeForeground", "terminalForeground"]).len(), 2);
        assert_eq!(matches(&probes, &["codeBackground"]), [rect(0., 0., 10., 10.)]);
        assert!(matches(&probes, &["update"]).is_empty());
    }

    #[test]
    fn the_dimmer_covers_everything_but_the_holes() {
        let viewport = rect(0., 0., 100., 100.);
        let holes = [rect(10., 10., 20., 20.), rect(20., 20., 20., 20.), rect(90., 90., 30., 30.)];
        let dim = dimmed(viewport, &holes);
        let area = |r: &Bounds<Pixels>| f32::from(r.size.width) * f32::from(r.size.height);
        // 400 + 400 - 100 overlapping, and the 10x10 of the last hole inside the viewport.
        assert_eq!(dim.iter().map(area).sum::<f32>(), 10_000. - 700. - 100.);
        for (i, a) in dim.iter().enumerate() {
            assert!(holes.iter().all(|hole| !a.intersects(hole)), "{a:?} dims a hole");
            assert!(dim[i + 1..].iter().all(|b| !a.intersects(b)), "{a:?} overlaps");
        }
        assert_eq!(dimmed(viewport, &[]), [viewport]);
    }
}
