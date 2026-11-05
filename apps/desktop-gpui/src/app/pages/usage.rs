//! The Usage page (`apps/web/src/routes/_chat.usage.tsx`).

use gpui_kit::{AnyElement, Context, IntoElement, ParentElement as _, Styled as _, Window, div};

use super::super::Workspace;

impl Workspace {
    pub(super) fn render_usage(&mut self, _: &mut Window, _: &mut Context<Self>) -> AnyElement {
        div().size_full().p_4().child("Usage").into_any_element()
    }
}
