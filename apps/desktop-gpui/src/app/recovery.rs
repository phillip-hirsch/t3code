//! `FirstRunRecovery`: for a `client-settings.json` that exists but can't be read, and for a
//! first-run decision the server never let the app make.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::Button;
use gpui_kit::component::{Sizable as _, v_flex};
use gpui_kit::{AnyElement, Context, FontWeight, IntoElement as _, ParentElement as _, Styled as _, Window, div, px};

use super::{Workspace, titlebar};

impl Workspace {
    /// The whole window while `client-settings.json` can't be read; the window still moves,
    /// resizes and closes as usual.
    pub(super) fn render_settings_recovery(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let retry = Button::new("retry-client-settings")
            .outline()
            .small()
            .icon(IconName::RefreshCw)
            .label("Retry")
            .on_click(cx.listener(|this, _, window, cx| this.retry_client_settings(window, cx)));
        self.render_recovery("Could not read settings", "Your saved settings could not be loaded.", retry, window, cx)
    }

    /// The whole window while the first-run gate is stalled; Reload starts its evidence over.
    pub(super) fn render_connection_recovery(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let reload = Button::new("reload-first-run")
            .outline()
            .small()
            .icon(IconName::RefreshCw)
            .label("Reload")
            .on_click(cx.listener(|this, _, window, cx| this.reload_first_run(window, cx)));
        self.render_recovery("Still connecting", "T3 Code could not confirm this workspace.", reload, window, cx)
    }

    fn render_recovery(
        &self,
        title: &'static str,
        description: &'static str,
        action: Button,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = *crate::theme::tokens(cx);
        div()
            .size_full()
            .relative()
            .flex()
            .items_center()
            .justify_center()
            .px(px(24.))
            .bg(t.background)
            .text_color(t.foreground)
            .child(
                v_flex()
                    .max_w(px(384.))
                    .items_center()
                    .text_center()
                    .child(div().text_lg().line_height(px(28.)).font_weight(FontWeight::SEMIBOLD).child(title))
                    .child(div().mt(px(8.)).text_sm().text_color(t.muted_foreground).child(description))
                    .child(div().mt(px(20.)).child(action)),
            )
            .child(titlebar::drag_region(div().absolute().top_0().left_0().right_0().h(titlebar::TITLEBAR_HEIGHT)))
            .child(self.render_caption_overlay(window, cx))
            .children(titlebar::render_resize_edges(window, cx))
            .into_any_element()
    }

    /// `ensureClientSettingsHydrated`: reads the file again and, once it reads, applies what
    /// the app took from the defaults at startup.
    fn retry_client_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut settings = crate::settings::ClientSettings::load();
        if !settings.read_failed {
            settings.themes = settings.themes.with_published(self.client_settings.themes.published().clone());
            cx.set_global(crate::theme::Appearance::from_settings(&settings.values));
            crate::theme::set_preference(settings.theme_choice(), window, cx);
            self.details_visibility = super::details::DetailsVisibility::from_stored(
                settings.values.get(super::details::DETAILS_VISIBILITY_KEY),
            );
            self.client_settings = settings;
            // The first-run gate waited for these settings, which say whether setup was finished.
            self.land(window, cx);
        }
        cx.notify();
    }
}
