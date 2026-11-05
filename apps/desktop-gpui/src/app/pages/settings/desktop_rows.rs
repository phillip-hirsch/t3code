//! About rows that belong to the desktop app itself: the version's update state
//! (`AboutVersionSection`) and the `t3` command (`CliCommandSettingsRow`).

use gpui_kit::component::button::Button;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{Disableable as _, Sizable as _, WindowExt as _};
use gpui_kit::{AnyElement, Context, IntoElement as _, ParentElement as _, SharedString, Window, div};
use serde_json::Value;

use super::widgets::RowFrame;
use crate::app::Workspace;
use crate::desktop::cli::{self, CliState};
use crate::theme::Tokens;

/// `getAutoUpdateDisabledReason` for this build: no update feed exists for it, so updates are
/// always off; a managed policy that turns updates off speaks first (`managedUpdatesDenial`).
pub(super) fn update_disabled_reason(policy: &Value) -> String {
    match (policy.pointer("/limits/updates/mode").and_then(Value::as_str), policy["organization"].as_str()) {
        (Some("off"), Some(organization)) => format!("{organization} manages T3 Code updates."),
        _ => "Automatic updates are not available because no update feed is configured.".into(),
    }
}

impl Workspace {
    /// The Version row's control: "Check for Updates", disabled while updates are off, as the
    /// web shows a desktop build without an update feed.
    pub(super) fn about_update_button(&self) -> AnyElement {
        Button::new("about-check-updates")
            .outline()
            .small()
            .label("Check for Updates")
            .disabled(true)
            .tooltip("Up to date")
            .into_any_element()
    }

    pub(super) fn about_update_description(&self) -> String {
        update_disabled_reason(&self.server_info.managed_policy)
    }

    /// `CliCommandSettingsRow`; hidden until the launcher state is known, or when this app has
    /// no server entry and Node to run.
    pub(super) fn about_cli_row(
        &mut self,
        t: &Tokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.desktop.cli.is_none() && !self.desktop.cli_pending {
            self.refresh_cli_state(None, window, cx);
        }
        let state = self.desktop.cli.clone().filter(|state| state.supported)?;
        let installed = state.installed_path.is_some();
        let pending = self.desktop.cli_pending;
        let button = Button::new("about-cli-command")
            .outline()
            .small()
            .label(if installed { "Remove" } else { "Install" })
            .disabled(pending || (!installed && state.shadowed_by.is_some()))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.refresh_cli_state(Some(!installed), window, cx);
            }));
        Some(
            RowFrame::new("t3 command")
                .description(div().child(SharedString::from(state.description())))
                .control(button)
                .render(t)
                .into_any_element(),
        )
    }

    /// Reads the launcher state, after installing (`Some(true)`) or removing (`Some(false)`) it.
    fn refresh_cli_state(&mut self, change: Option<bool>, window: &mut Window, cx: &mut Context<Self>) {
        if self.desktop.cli_pending {
            return;
        }
        self.desktop.cli_pending = true;
        cx.notify();
        let work = cx.background_executor().spawn(async move {
            match change {
                None => Ok(cli::state()),
                Some(true) => cli::install(),
                Some(false) => cli::uninstall(),
            }
        });
        self.desktop._cli_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result: anyhow::Result<CliState> = work.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.desktop.cli_pending = false;
                match result {
                    Ok(state) => this.desktop.cli = Some(state),
                    Err(error) => {
                        let title = if change == Some(true) { "Could not install t3" } else { "Could not remove t3" };
                        window.push_notification(Notification::error(error.to_string()).title(title), cx);
                    }
                }
                cx.notify();
            });
        }));
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_policy_that_turns_updates_off_explains_itself() {
        assert_eq!(
            update_disabled_reason(&json!({})),
            "Automatic updates are not available because no update feed is configured."
        );
        let managed = json!({ "organization": "Acme", "limits": { "updates": { "mode": "off" } } });
        assert_eq!(update_disabled_reason(&managed), "Acme manages T3 Code updates.");
    }
}
