//! Settings → Diffs & pull requests (`DiffsSettingsPanel` in
//! `apps/web/src/components/settings/SettingsPanels.tsx`, plus `PullRequestLinksOpenInAppSetting`
//! from `IntegrationsSettings.tsx`). Every row is a device-local client setting; none is scoped.

use gpui_kit::{AnyElement, Context, IntoElement as _, SharedString};

use super::placeholder::{Choice, Control, Literal, SettingRowSpec, page, section};
use crate::app::Workspace;
use crate::theme;

static DIFFS: [SettingRowSpec; 4] = [
    SettingRowSpec {
        id: "diff-layout",
        label: "Diff layout",
        description: "Show diffs stacked or side by side. The toggle in the diff toolbar changes this too.",
        path: "diffLayout",
        control: Control::Select(&[
            Choice { value: Literal::Str("stacked"), label: "Stacked", short: None },
            Choice { value: Literal::Str("split"), label: "Split", short: None },
        ]),
        scopes: &[],
    },
    SettingRowSpec {
        id: "hide-whitespace-changes",
        label: "Hide whitespace changes",
        description: "Set whether the diff panel ignores whitespace-only edits by default.",
        path: "diffIgnoreWhitespace",
        control: Control::Switch,
        scopes: &[],
    },
    SettingRowSpec {
        id: "default-diff-file-state",
        label: "Default diff file state",
        description: "Start with files expanded or collapsed when opening diffs or a pull request's Code tab.",
        path: "diffFilesCollapsed",
        control: Control::Select(&[
            Choice { value: Literal::Bool(false), label: "Expanded", short: None },
            Choice { value: Literal::Bool(true), label: "Collapsed", short: None },
        ]),
        scopes: &[],
    },
    SettingRowSpec {
        id: "diff-color-scheme",
        label: "Diff colors",
        description: "Choose colors for additions and deletions, including change counts.",
        path: "diffColorScheme",
        control: Control::Select(&[
            Choice { value: Literal::Str("red-green"), label: "Red & green (default)", short: Some("Red & green") },
            Choice { value: Literal::Str("blue-orange"), label: "Blue & orange", short: None },
        ]),
        scopes: &[],
    },
];

static PULL_REQUESTS: [SettingRowSpec; 2] = [
    SettingRowSpec {
        id: "proactive-panels",
        label: "Open pull requests and changes panels proactively",
        description: "Open linked pull requests first. Otherwise, open Changes for edits to at least 3 files or 50 lines.",
        path: "proactivePanelsEnabled",
        control: Control::Switch,
        scopes: &[],
    },
    SettingRowSpec {
        id: "pull-request-links-open-in-app",
        label: "Open pull requests in T3 Code",
        description: "Clicking a pull request link opens it in T3 Code. Turn off to open pull request links like any other link, on your git host.",
        path: "pullRequestLinksOpenInApp",
        control: Control::Switch,
        scopes: &[],
    },
];

impl Workspace {
    pub(super) fn render_diffs_section(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let t = *theme::tokens(cx);
        let mut rows = |specs: &'static [SettingRowSpec]| -> Vec<AnyElement> {
            specs
                .iter()
                .map(|spec| self.render_client_row(spec, SharedString::new_static(spec.description), false, cx))
                .collect()
        };
        let diffs = rows(&DIFFS);
        let pull_requests = rows(&PULL_REQUESTS);
        page(
            "settings-diffs",
            [
                section(Some("Diffs"), None, diffs, &t).into_any_element(),
                section(Some("Pull requests"), None, pull_requests, &t).into_any_element(),
            ],
        )
        .into_any_element()
    }
}
