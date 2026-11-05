//! Device sources and host editors (`DeviceSourcesSettings`); replaced by pass 2.

use gpui_kit::{AnyElement, Context, IntoElement as _, Window, div};

use crate::app::Workspace;

impl Workspace {
    pub(super) fn render_device_sources(&mut self, _: &mut Window, _: &mut Context<Self>) -> AnyElement {
        div().into_any_element()
    }
}
