//! `SettingsRow`'s `@container/settings-row` query. GPUI has no container queries, so a row lays
//! out by the width its content box had last frame and draws again when that width crosses 32rem.

use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, Bounds, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement, LayoutId,
    ParentElement as _, Pixels, Styled as _, Window, px,
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
}

impl RowLayout {
    /// `id` keys the width remembered across frames; rows sharing one share a width.
    pub(super) fn new(id: ElementId, label: AnyElement, control: Option<AnyElement>) -> Self {
        Self { id, label: Some(label), control }
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
        let mut row = if stacked {
            v_flex()
                .w_full()
                .gap(px(12.))
                .child(label)
                .children(control.map(|control| h_flex().w_full().min_w_0().gap(px(8.)).child(control)))
                .into_any_element()
        } else {
            h_flex()
                .w_full()
                .gap(px(32.))
                .child(label)
                .children(control.map(|control| h_flex().flex_none().justify_end().gap(px(8.)).child(control)))
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
