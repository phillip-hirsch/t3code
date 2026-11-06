//! Settings sections in the general group; see `SETTINGS` in `mod.rs`.

use gpui_kit::component::menu::DropdownMenu as _;
use gpui_kit::{Anchor, AnyElement, Context, StatefulInteractiveElement as _, Window};
use serde_json::json;

use super::widgets::{SettingRow, select_trigger, settings_section, switch};
use crate::app::Workspace;
use crate::app::details::thread_details_items_menu;
use crate::app::pages::settings::widgets::SettingsView;
use crate::theme::{self, Tokens};
use crate::thread_details;

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
    pub(super) fn render_settings_general(
        &mut self,
        view: &SettingsView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = *theme::tokens(cx);
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

        let mut row = |this: &mut Self, row: SettingRow| this.setting_row(view, row, window, cx);
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
        self.settings_page("general", view, sections, cx)
    }

    /// `AppearanceSettingsPanel`: the theme library, the Interface rows and Typography.
    pub(super) fn render_settings_appearance(
        &mut self,
        view: &SettingsView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = *theme::tokens(cx);
        let details_row = self.thread_details_row(&t, cx);
        let mut row = |this: &mut Self, row: SettingRow| this.setting_row(view, row, window, cx);
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
            row(self, details_row),
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
        let sections = [self.render_theme_library(cx), interface, self.render_typography(view, window, cx)];
        self.settings_page("appearance", view, sections, cx)
    }

    /// `ThreadDetailsItemsRow`: "N of 12 on" opens the card's own items menu.
    fn thread_details_row(&self, t: &Tokens, cx: &mut Context<Self>) -> SettingRow {
        let settings = &self.client_settings.values;
        let trigger = format!("{} of {} on", thread_details::visible_count(settings), thread_details::ITEMS.len());
        let control = select_trigger(&"thread-details-items".into(), trigger, 160., t)
            .dropdown_menu_with_anchor(Anchor::TopRight, thread_details_items_menu(cx.weak_entity()));
        SettingRow::new("thread-details-items")
            .description("Choose what the thread details card shows. You can also right-click the card.")
            .control(control)
            .reset((!thread_details::is_default(settings)).then(thread_details::reset))
    }
}

impl Workspace {
    pub(super) fn render_settings_chat(
        &mut self,
        view: &SettingsView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.render_chat_section(view, window, cx)
    }

    pub(super) fn render_settings_diffs(
        &mut self,
        view: &SettingsView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.render_diffs_section(view, window, cx)
    }

    pub(super) fn render_settings_keybindings(
        &mut self,
        view: &SettingsView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.render_keybindings_section(view, window, cx)
    }

    pub(super) fn render_settings_snap_shot(
        &mut self,
        view: &SettingsView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.render_snap_shot_section(view, window, cx)
    }
}
