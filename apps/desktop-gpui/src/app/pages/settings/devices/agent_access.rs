//! Plain server-setting rows, declared for the scoped-settings widgets to take over
//! (`AgentBrowserAccessSetting` in `apps/web/src/components/settings/IntegrationsSettings.tsx`).
//! Until then each renders as a global `server.updateSettings` patch.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{Icon, Sizable as _, WindowExt as _};
use gpui_kit::{AnyElement, Context, IntoElement as _, Styled as _, Window, px};
use serde_json::{Value, json};

use super::{description, settings_row};
use crate::app::Workspace;
use crate::theme;

struct SettingRowSpec {
    id: &'static str,
    label: &'static str,
    description: &'static str,
    /// The key in `ServerSettings`.
    path: &'static str,
    control: Control,
    /// Where the web lets it be set (`PROJECT_SCOPED_SERVER_SETTING_KEYS`).
    #[expect(dead_code, reason = "for the scoped-settings widgets that replace this renderer")]
    scopes: &'static [&'static str],
}

enum Control {
    Switch { default: bool },
}

/// "Agent browser access" is project-scoped in the web: at project scope its description reads
/// "Allow agents in this project to use the shared browser. Applies when the agent session next starts."
const AGENT_BROWSER_ACCESS: SettingRowSpec = SettingRowSpec {
    id: "agent-browser-access",
    label: "Agent browser access",
    description: "Allow agents to use the shared browser. Projects can override it.",
    path: "enableAgentBrowserAccess",
    control: Control::Switch { default: true },
    scopes: &["environment", "project", "checkout"],
};

impl Workspace {
    /// The Browser section's agent access row.
    pub(in crate::app::pages::settings) fn render_agent_browser_access(
        &mut self,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.render_device_setting_row(&AGENT_BROWSER_ACCESS, cx)
    }

    fn render_device_setting_row(&self, spec: &'static SettingRowSpec, cx: &mut Context<Self>) -> AnyElement {
        let t = *theme::tokens(cx);
        let Control::Switch { default } = spec.control;
        let value = self.server_info.settings.get(spec.path).and_then(Value::as_bool).unwrap_or(default);
        let reset = (value != default).then(|| {
            Button::new("reset")
                .ghost()
                .xsmall()
                .size(px(20.))
                .icon(Icon::new(gpui_kit::assets::IconName::Undo2).with_size(px(12.)))
                .tooltip("Reset to default")
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.patch_settings(json!({ spec.path: default }), window, cx)
                }))
                .into_any_element()
        });
        let control =
            Switch::new(spec.id).small().checked(value).accessibility_label(spec.label).on_click(cx.listener(
                move |this, checked: &bool, window, cx| this.patch_settings(json!({ spec.path: *checked }), window, cx),
            ));
        settings_row(spec.id, spec.label, reset, description(spec.description, &t), control, &t)
    }

    /// `server.updateSettings` with a global patch; `settingsUpdated` brings the new value back.
    fn patch_settings(&mut self, patch: Value, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.client() else { return };
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("server.updateSettings", json!({ "patch": patch })).await;
            if let Err(error) = result {
                let _ = this.update_in(cx, |_, window, cx| {
                    let note = gpui_kit::component::notification::Notification::error(error.to_string())
                        .title("Setting not saved");
                    window.push_notification(note, cx);
                });
            }
        })
        .detach();
    }
}
