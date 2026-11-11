//! "Use T3 Code in another app (MCP)" (`UseInAnotherAppDialog.tsx`). Cloud apps reach an
//! environment only through its T3 Connect tunnel, and this client has no T3 Connect, so the
//! dialog shows what the web shows in a build without it (`hasCloudPublicConfig()` false).

use gpui_kit::component::dialog::DialogEntrance;
use gpui_kit::component::{WindowExt as _, v_flex};
use gpui_kit::{Context, FontWeight, ParentElement as _, SharedString, Styled as _, Window, div, hsla, px};

use super::Workspace;
use super::sessions::access_session_index;
use crate::settings::scope;
use crate::theme;

impl Workspace {
    /// `openUseInAnotherApp`: for `requested`, else the environment Sharing & access picks.
    pub(super) fn open_use_in_another_app(
        &mut self,
        requested: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let label = self.access_environment_label(requested);
        let title: SharedString =
            format!("Use T3 Code in another app (MCP) \u{b7} {}", label.as_deref().unwrap_or("T3 Code")).into();
        let message = if label.is_some() {
            "T3 Connect is not available in this build."
        } else {
            "Connect to an environment first."
        };
        window.open_dialog(cx, move |dialog, _, cx| {
            let t = *theme::tokens(cx);
            // `WizardHeader`, then a `WizardPanel` holding the gate's message.
            let header =
                v_flex().gap(px(8.)).p(px(24.)).pb(px(12.)).child(
                    div().text_xl().line_height(px(27.5)).font_weight(FontWeight::SEMIBOLD).child(title.clone()),
                );
            let panel = v_flex()
                .min_w_0()
                .px(px(24.))
                .py(px(20.))
                .bg(if t.dark { hsla(0., 0., 1., 0.02) } else { t.background.opacity(0.8) })
                .border_y_1()
                .border_color(if t.dark { hsla(0., 0., 1., 0.05) } else { hsla(0., 0., 0., 0.05) })
                .child(div().text_base().font_weight(FontWeight::MEDIUM).child(message));
            dialog.entrance(DialogEntrance::None).p_0().w(px(576.)).centered().child(header).child(panel)
        });
    }

    /// `useAccessEnvironment`'s label: the environment's name, with what tells it apart from
    /// another of that name (`settingsScopeEnvironmentLabel`). `None` until it has an id.
    fn access_environment_label(&self, requested: Option<&str>) -> Option<String> {
        let id = self.sessions[access_session_index(&self.sessions, requested)].id.as_deref()?;
        let labeled: Vec<scope::Labeled> = self
            .sessions
            .iter()
            .filter_map(|s| {
                let id = s.id.as_deref()?;
                let label = s.server_info.environment_label.as_deref().unwrap_or(id);
                Some((id, label, s.endpoint.as_ref().map(|e| e.http_base.as_str())))
            })
            .collect();
        let &(_, label, _) = labeled.iter().find(|e| e.0 == id)?;
        Some(match scope::environment_secondary(id, &labeled) {
            Some(secondary) => format!("{label} \u{b7} {secondary}"),
            None => label.to_owned(),
        })
    }
}
