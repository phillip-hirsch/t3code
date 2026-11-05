//! Settings → Diffs & pull requests (`DiffsSettingsPanel` in
//! `apps/web/src/components/settings/SettingsPanels.tsx`, plus `PullRequestLinksOpenInAppSetting`
//! from `IntegrationsSettings.tsx`). Every row is a device-local client setting; none is scoped.

use gpui_kit::component::h_flex;
use gpui_kit::{AnyElement, Context, Div, Hsla, ParentElement as _, Styled as _, Window, div, px};
use serde_json::{Value, json};

use super::widgets::{SettingRow, settings_section};
use crate::app::Workspace;
use crate::app::pages::settings::widgets::SettingsView;
use crate::theme::{self, Tokens};

const LAYOUTS: &[(&str, &str)] = &[("stacked", "Stacked"), ("split", "Split")];

impl Workspace {
    pub(super) fn render_diffs_section(
        &mut self,
        view: &SettingsView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = *theme::tokens(cx);
        let collapsed = view.value("diffFilesCollapsed").clone();
        let file_state_label = if collapsed == true { "Collapsed" } else { "Expanded" };
        let file_state = self.value_select(
            "default-diff-file-state",
            "diffFilesCollapsed",
            &collapsed,
            vec![(json!(false), "Expanded".into()), (json!(true), "Collapsed".into())],
            file_state_label,
            160.,
            cx,
        );
        let scheme = view.value("diffColorScheme").clone();
        let scheme_label = h_flex().gap(px(8.)).child(diff_color_dots(&scheme, &t)).child(if scheme == "blue-orange" {
            "Blue & orange"
        } else {
            "Red & green"
        });
        let colors = self.value_select(
            "diff-color-scheme",
            "diffColorScheme",
            &scheme,
            vec![(json!("red-green"), "Red & green (default)".into()), (json!("blue-orange"), "Blue & orange".into())],
            scheme_label,
            0.,
            cx,
        );
        let mut row = |this: &mut Self, row: SettingRow| this.setting_row(&view, row, window, cx);
        let diffs = [
            row(
                self,
                SettingRow::new("diff-layout")
                    .description("Show diffs stacked or side by side. The toggle in the diff toolbar changes this too.")
                    .setting("diffLayout")
                    .select(LAYOUTS, 160.),
            ),
            row(
                self,
                SettingRow::new("hide-whitespace-changes")
                    .description("Set whether the diff panel ignores whitespace-only edits by default.")
                    .setting("diffIgnoreWhitespace")
                    .switch(),
            ),
            row(
                self,
                SettingRow::new("default-diff-file-state")
                    .description(
                        "Start with files expanded or collapsed when opening diffs or a pull request's Code tab.",
                    )
                    .setting("diffFilesCollapsed")
                    .control(file_state),
            ),
            row(
                self,
                SettingRow::new("diff-color-scheme")
                    .description("Choose colors for additions and deletions, including change counts.")
                    .setting("diffColorScheme")
                    .control(colors),
            ),
        ];
        let pull_requests = [
            row(
                self,
                SettingRow::new("proactive-panels")
                    .description(
                        "Open linked pull requests first. Otherwise, open Changes for edits to at least 3 files or 50 lines.",
                    )
                    .setting("proactivePanelsEnabled")
                    .switch(),
            ),
            row(
                self,
                SettingRow::new("pull-request-links-open-in-app")
                    .description("Clicking a pull request link opens it in T3 Code. Turn off to open pull request links like any other link, on your git host.")
                    .setting("pullRequestLinksOpenInApp")
                    .switch(),
            ),
        ];
        let sections = [
            settings_section("diffs", "Diffs", diffs, &t),
            settings_section("pull-requests", "Pull requests", pull_requests, &t),
        ];
        self.settings_page("diffs", &view, sections, cx)
    }
}

/// The trigger's deletion/addition dots; `blue-orange` reverses them.
fn diff_color_dots(scheme: &Value, t: &Tokens) -> Div {
    let blue_orange = scheme.as_str() == Some("blue-orange");
    let (deletion, addition): (Hsla, Hsla) = if blue_orange {
        let (orange, blue) = if t.dark { (0xff8904, 0x51a2ff) } else { (0xf54900, 0x155dfc) };
        (gpui_kit::rgb(orange).into(), gpui_kit::rgb(blue).into())
    } else {
        (t.error, gpui_kit::rgb(0x00bc7d).into())
    };
    let dot = |color: Hsla| div().size(px(8.)).rounded_full().bg(color);
    let (first, second) = if blue_orange { (addition, deletion) } else { (deletion, addition) };
    h_flex().flex_none().gap(px(4.)).child(dot(first)).child(dot(second))
}
