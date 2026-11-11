//! `SettingsRow`'s `@container/settings-row` query. GPUI has no container queries, so a row lays
//! out by the width its content box had last frame and draws again when that width crosses 32rem.

use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, Bounds, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement, LayoutId,
    ParentElement as _, Pixels, Styled as _, Window, div, px,
};

/// `@min-[32rem]/settings-row`: below this content width the control sits under the title.
const SIDE_BY_SIDE_REMS: f32 = 32.;

/// Whether a row whose content box is `width` wide puts its control under its title.
pub(super) fn stacks(width: Pixels, rem: Pixels) -> bool {
    width < rem * SIDE_BY_SIDE_REMS
}

/// The title column beside the control (`grid-cols-[minmax(0,1fr)_auto] gap-8`), or above it at
/// full width (`flex-col gap-3`).
pub(super) struct RowLayout {
    id: ElementId,
    label: Option<AnyElement>,
    control: Option<AnyElement>,
    /// `control` as drawn beside the title, when that differs.
    beside: Option<AnyElement>,
    /// A `w-auto` control: as wide as its content under the title, and at least this wide (its
    /// content grows to fill) beside it.
    hug: Option<Pixels>,
}

impl RowLayout {
    /// `id` keys the width remembered across frames; rows sharing one share a width.
    pub(super) fn new(id: ElementId, label: AnyElement, control: Option<AnyElement>) -> Self {
        Self { id, label: Some(label), control, beside: None, hug: None }
    }

    pub(super) fn beside(mut self, control: Option<AnyElement>) -> Self {
        self.beside = control;
        self
    }

    pub(super) fn hugging(mut self, min: Option<Pixels>) -> Self {
        self.hug = min;
        self
    }
}

impl IntoElement for RowLayout {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for RowLayout {
    /// The laid-out row and whether it stacked.
    type RequestLayoutState = (AnyElement, bool);
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let id = id.expect("RowLayout has an id");
        let stacked = window.with_element_state(id, |stacked: Option<bool>, _| {
            let stacked = stacked.unwrap_or(false);
            (stacked, stacked)
        });
        let label = self.label.take().expect("RowLayout lays out once");
        let control = self.control.take();
        let control = if stacked { control } else { self.beside.take().or(control) };
        let mut row = if stacked {
            v_flex()
                .w_full()
                .gap(px(12.))
                .child(label)
                .children(control.map(|control| match self.hug {
                    // A flex item, so it keeps its content's width.
                    Some(_) => h_flex().w_full().min_w_0().child(control).into_any_element(),
                    // A block box, so a `w-full` control spans the row.
                    None => div().w_full().min_w_0().child(control).into_any_element(),
                }))
                .into_any_element()
        } else {
            h_flex()
                .w_full()
                .gap(px(32.))
                .child(label)
                .children(control.map(|control| match self.hug {
                    // A block box at least `min` wide, which the control stretches to fill.
                    Some(min) => div().flex_none().min_w(min).child(control).into_any_element(),
                    None => h_flex().flex_none().justify_end().gap(px(8.)).child(control).into_any_element(),
                }))
                .into_any_element()
        };
        (row.request_layout(window, cx), (row, stacked))
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        (row, stacked): &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let now = stacks(bounds.size.width, window.rem_size());
        if now != *stacked {
            let id = id.expect("RowLayout has an id");
            window.with_element_state(id, |_: Option<bool>, _| ((), now));
            window.request_animation_frame();
        }
        row.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        (row, _): &mut Self::RequestLayoutState,
        (): &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        row.paint(window, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_stack_below_32rem_of_content() {
        let rem = px(16.);
        assert!(stacks(px(511.), rem));
        assert!(!stacks(px(512.), rem));
        // The query is in rems, so a larger interface size stacks wider rows.
        assert!(stacks(px(600.), px(20.)));
    }
}
