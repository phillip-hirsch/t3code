//! Settings sections in the general group; see `SETTINGS` in `mod.rs`.

use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, FontWeight, Hsla, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, px,
};
use serde_json::json;

use super::widgets::{SettingRow, settings_section, subheading, switch};
use crate::app::Workspace;
use crate::settings::{ThemePreference, contract};
use crate::theme::{self, Tokens};

const NOTIFICATION_MODES: &[(&str, &str)] = &[
    ("off", "Off"),
    ("notifications", "Notifications only"),
    ("sound", "Sound only"),
    ("notifications-and-sound", "Notifications with sound"),
];
const PROJECT_ORDERS: &[(&str, &str)] =
    &[("updated_at", "Last user message"), ("created_at", "Created at"), ("manual", "Manual")];
const QUIT_MODES: &[(&str, &str)] = &[("direct", "Direct"), ("hold", "Hold"), ("double-click", "Double press")];
const TIMESTAMP_FORMATS: &[(&str, &str)] =
    &[("locale", "System default"), ("12-hour", "12-hour"), ("24-hour", "24-hour")];
const CHAT_WIDTHS: &[(&str, &str)] = &[("comfortable", "Comfortable"), ("wide", "Wide"), ("full", "Full")];
const EFFORT_DISPLAYS: &[(&str, &str)] = &[("label", "Word"), ("bars", "Bars")];
/// `environmentIdentificationOptions` for a build without Dev or Nightly artwork of its own.
const ENVIRONMENT_IDENTIFICATIONS: &[(&str, &str)] =
    &[("artwork", "None"), ("nightly-artwork", "Nightly artwork"), ("dev-artwork", "Dev artwork")];

impl Workspace {
    /// `GeneralSettingsPanel`. Desktop notifications need no permission prompt in a desktop app.
    pub(super) fn render_settings_general(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = *theme::tokens(cx);
        let view = self.settings_view();
        let grouping = view.str("sidebarProjectGroupingMode").to_owned();
        let grouping_switch = switch(&"project-grouping".into(), grouping != "separate", false, &t).on_click(
            cx.listener(move |this, _, window, cx| {
                let next = if grouping == "separate" {
                    this.settings_ui.last_grouping_mode
                } else {
                    this.settings_ui.last_grouping_mode =
                        if grouping == "repository_path" { "repository_path" } else { "repository" };
                    "separate"
                };
                this.update_settings(json!({ "sidebarProjectGroupingMode": next }), window, cx);
            }),
        );

        let mut row = |this: &mut Self, row: SettingRow| this.setting_row(&view, row, window, cx);
        let notifications = [
            row(self, SettingRow::new("thread-notifications")
                .description("System alerts when a thread finishes, fails, or needs input or approval. Applies to this device while T3 Code is open.")
                .setting("notificationMode")
                .select(NOTIFICATION_MODES, 224.)),
            row(self, SettingRow::new("in-app-notifications")
                .description("Show a toast when another thread finishes, fails, or needs input or approval while this app has focus.")
                .setting("inAppNotificationsEnabled")
                .switch()),
        ];
        let sidebar = [
            row(self, SettingRow::new("project-grouping")
                .description("Combine matching repositories across environments.")
                .setting("sidebarProjectGroupingMode")
                .control(grouping_switch)),
            row(self, SettingRow::new("project-order")
                .description("Order of projects in the sidebar project picker and command palette.")
                .setting("sidebarProjectSortOrder")
                .select(PROJECT_ORDERS, 176.)),
            row(self, SettingRow::new("working-shelf")
                .description("Fold working and monitoring threads into a Working section. They return to the top of the inbox when they need you. The boring sidebar in Labs has no Working section.")
                .setting("sidebarWorkingShelfEnabled")
                .switch()),
        ];
        let confirmations = [
            row(
                self,
                SettingRow::new("unpin-confirmation")
                    .description("Ask before unpinning a thread from the pinned section.")
                    .setting("confirmThreadUnpin")
                    .switch(),
            ),
            row(
                self,
                SettingRow::new("archive-confirmation")
                    .description("Require a second click on the inline archive action before a thread is archived.")
                    .setting("confirmThreadArchive")
                    .switch(),
            ),
            row(
                self,
                SettingRow::new("delete-confirmation")
                    .description("Ask before deleting a thread and its chat history.")
                    .setting("confirmThreadDelete")
                    .switch(),
            ),
            row(
                self,
                SettingRow::new("quit-confirmation")
                    .description("Hold mode also quits on two quick presses.")
                    .setting("confirmQuit")
                    .select(QUIT_MODES, 160.),
            ),
        ];
        let display = [row(
            self,
            SettingRow::new("time-format")
                .description("System default follows your browser or OS clock preference.")
                .setting("timestampFormat")
                .select(TIMESTAMP_FORMATS, 160.),
        )];
        let sections = [
            settings_section("notifications", "Notifications", notifications, &t),
            settings_section("sidebar", "Sidebar", sidebar, &t),
            settings_section("confirmations", "Confirmations", confirmations, &t),
            settings_section("display", "Display", display, &t),
        ];
        self.settings_page("general", &view, sections, cx)
    }

    /// `AppearanceSettingsPanel`: the color scheme and the Interface rows. The theme library,
    /// thread details items and typography rows are ported separately.
    pub(super) fn render_settings_appearance(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = *theme::tokens(cx);
        let view = self.settings_view();
        let mut row = |this: &mut Self, row: SettingRow| this.setting_row(&view, row, window, cx);
        let interface = [
            row(self, SettingRow::new("setting-appearance-contrast")
                .description("Adjust the contrast of colors and borders across the interface.")
                .setting("appearanceContrast")
                .slider(50., 200., 5., "%")),
            row(self, SettingRow::new("setting-glass-opacity")
                .description("Higher values make menus, dialogs, and the composer more solid.")
                .setting("glassOpacity")
                .slider(40., 100., 5., "%")),
            row(self, SettingRow::new("chat-width")
                .description("Set how wide messages and the composer can grow on large screens.")
                .setting("chatWidth")
                .select(CHAT_WIDTHS, 160.)),
            row(self, SettingRow::new("effort-display")
                .description("Show reasoning effort in the composer as its name, such as High, or as bars.")
                .setting("effortDisplay")
                .select(EFFORT_DISPLAYS, 160.)),
            row(self, SettingRow::new("math-rendering")
                .description("Typeset LaTeX math, such as $x^2$ or \\[x^2\\], in messages and Markdown previews. The composer keeps TeX source editable.")
                .setting("mathRenderingEnabled")
                .switch()),
        ];
        let advanced = [
            row(
                self,
                SettingRow::new("panel-animations")
                    .description("Set how fast panels open and close.")
                    .setting("panelAnimationDurationMs")
                    .slider(0., 400., 25., " ms"),
            ),
            row(
                self,
                SettingRow::new("environment-identification")
                    .description("Show Dev or Nightly artwork in the sidebar header.")
                    .setting("environmentIdentificationMode")
                    .select(ENVIRONMENT_IDENTIFICATIONS, 160.),
            ),
            row(
                self,
                SettingRow::new("composer-context")
                    .description("Keep branch and worktree controls below the composer after a thread starts.")
                    .setting("persistComposerContextStrip")
                    .switch(),
            ),
        ];
        let fold = self.advanced_fold("appearance", advanced, cx);
        let interface = settings_section("appearance-interface", "Interface", interface.into_iter().chain([fold]), &t);
        let sections = [self.render_color_scheme(cx), interface];
        self.settings_page("appearance", &view, sections, cx)
    }

    /// "Color scheme": System, Light and Dark tiles over a wireframe of each palette.
    fn render_color_scheme(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = *theme::tokens(cx);
        let current = self.client_settings.theme;
        let title = contract().search_item("color-scheme").map_or("Color scheme", |i| i.title.as_str()).to_owned();
        let tile = |preference: ThemePreference, label: &'static str| {
            let active = current == preference;
            v_flex()
                .id(SharedString::from(format!("color-scheme-{}", preference.id())))
                .flex_1()
                .gap(px(6.))
                .p(px(8.))
                .rounded(px(12.))
                .cursor_pointer()
                .map(|el| {
                    if active {
                        el.border_2().border_color(t.ring).bg(t.accent.opacity(0.3)).p(px(7.))
                    } else {
                        el.border_1()
                            .border_color(t.border.opacity(0.7))
                            .bg(t.card.opacity(0.6))
                            .hover(|s| s.bg(t.accent.opacity(0.1)))
                    }
                })
                .on_click(cx.listener(move |this, _, window, cx| this.set_theme_preference(preference, window, cx)))
                .child(match preference {
                    ThemePreference::System => h_flex()
                        .h(px(140.))
                        .rounded(px(8.))
                        .overflow_hidden()
                        .child(div().flex_1().h_full().child(wireframe(&Tokens::light())))
                        .child(div().flex_1().h_full().child(wireframe(&Tokens::dark())))
                        .into_any_element(),
                    ThemePreference::Light => div().h(px(140.)).child(wireframe(&Tokens::light())).into_any_element(),
                    ThemePreference::Dark => div().h(px(140.)).child(wireframe(&Tokens::dark())).into_any_element(),
                })
                .child(
                    div()
                        .text_center()
                        .text_size(px(12.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(if active { t.foreground } else { t.muted_foreground })
                        .child(label),
                )
        };
        v_flex()
            .id("color-scheme")
            .gap(px(12.))
            .child(subheading(title))
            .child(
                h_flex()
                    .gap(px(12.))
                    .child(tile(ThemePreference::System, "System"))
                    .child(tile(ThemePreference::Light, "Light"))
                    .child(tile(ThemePreference::Dark, "Dark")),
            )
            .into_any_element()
    }
}

/// A miniature of the app in one palette: sidebar rows beside a chat with a composer.
fn wireframe(t: &Tokens) -> impl IntoElement {
    let line = |width: f32, color: Hsla| div().h(px(4.)).w(px(width)).rounded_full().bg(color);
    h_flex()
        .size_full()
        .rounded(px(8.))
        .overflow_hidden()
        .border_1()
        .border_color(t.border)
        .bg(t.background)
        .child(
            v_flex()
                .w(px(36.))
                .h_full()
                .gap(px(6.))
                .p(px(6.))
                .bg(t.sidebar.background)
                .border_r_1()
                .border_color(t.sidebar.border)
                .child(line(20., t.sidebar.muted_foreground.opacity(0.5)))
                .child(div().h(px(8.)).rounded(px(2.)).bg(t.sidebar.row_active))
                .child(line(18., t.sidebar.muted_foreground.opacity(0.4)))
                .child(line(22., t.sidebar.muted_foreground.opacity(0.4))),
        )
        .child(
            v_flex()
                .flex_1()
                .h_full()
                .p(px(8.))
                .gap(px(6.))
                .child(h_flex().justify_end().child(div().h(px(10.)).w(px(40.)).rounded(px(4.)).bg(t.message_surface)))
                .child(line(56., t.muted_foreground.opacity(0.5)))
                .child(line(44., t.muted_foreground.opacity(0.4)))
                .child(div().flex_1())
                .child(div().h(px(18.)).rounded(px(5.)).border_1().border_color(t.border).bg(t.card)),
        )
}

impl Workspace {
    pub(super) fn render_settings_chat(&mut self, _: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        self.render_chat_section(cx)
    }

    pub(super) fn render_settings_diffs(&mut self, _: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        self.render_diffs_section(cx)
    }

    pub(super) fn render_settings_keybindings(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        self.render_keybindings_section(window, cx)
    }

    pub(super) fn render_settings_snap_shot(&mut self, _: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        self.render_snap_shot_section(cx)
    }
}
