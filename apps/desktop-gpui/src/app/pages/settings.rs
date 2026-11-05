//! The Settings modal (`apps/web/src/routes/_chat.settings.tsx`, an `AppRouteModal`
//! over the whole window with `SettingsSidebarNav` and one section at a time).

use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString, Styled as _, Window,
    div,
};

use super::super::Workspace;
use crate::theme;

impl Workspace {
    /// The modal layer, drawn over everything while Settings is open.
    pub(in crate::app) fn render_settings(&mut self, _: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let section: SharedString = self.settings.clone()?;
        let tokens = *theme::tokens(cx);
        Some(
            div()
                .id("settings-modal")
                .absolute()
                .inset_0()
                .occlude()
                .bg(tokens.background)
                .p_4()
                .child(format!("Settings · {section}"))
                .into_any_element(),
        )
    }
}
