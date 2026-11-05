//! The Pull requests page (`apps/web/src/routes/_chat.pull-requests.tsx`).

use gpui_kit::{AnyElement, Context, IntoElement, ParentElement as _, Styled as _, Window, div};

use super::super::Workspace;

impl Workspace {
    pub(super) fn render_pull_requests(&mut self, _: &mut Window, _: &mut Context<Self>) -> AnyElement {
        div().size_full().p_4().child("Pull requests").into_any_element()
    }
}
