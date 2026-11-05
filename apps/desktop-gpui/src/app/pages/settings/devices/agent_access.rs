//! Settings › Browser (`BrowserSettingsPanel` in `apps/web/src/components/settings/IntegrationsSettings.tsx`):
//! its Agent access section. The Browsing section is not ported.

use gpui_kit::{AnyElement, Context, Window};

use crate::app::Workspace;
use crate::app::pages::settings::widgets::{SettingRow, settings_section};
use crate::theme;

impl Workspace {
    /// `AgentBrowserAccessSetting`; at project scope the web words it for the project.
    pub(in crate::app::pages::settings) fn render_agent_browser_access(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = *theme::tokens(cx);
        let view = self.settings_view();
        let description = if view.scope.project_id().is_some() {
            "Allow agents in this project to use the shared browser. Applies when the agent session next starts."
        } else {
            "Allow agents to use the shared browser. Projects can override it."
        };
        let row = SettingRow::new("agent-browser-access")
            .description(description)
            .setting("enableAgentBrowserAccess")
            .switch();
        let row = self.setting_row(&view, row, window, cx);
        let sections = [settings_section("browser-agent-access", "Agent access", [row], &t)];
        self.settings_page("browser", &view, sections, cx)
    }
}
