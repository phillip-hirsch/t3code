//! The Providers settings section. The instance cards are ported separately;
//! until they land, `render_provider_accounts_preview` lists each instance
//! with the account pieces from this folder so they can be used and checked.

mod accent;
mod accounts;
mod codex_callback;
mod usage_sources;

use gpui_kit::component::{Sizable as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, Context, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div,
};
use serde_json::json;

use crate::app::Workspace;
use crate::theme;

impl Workspace {
    pub(in crate::app) fn render_provider_accounts_preview(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = *theme::tokens(cx);
        let providers: Vec<_> = self
            .server_info
            .providers
            .iter()
            .map(|p| {
                (p.instance_id.clone(), p.display_name.clone().unwrap_or_else(|| p.driver.clone()), p.driver.clone())
            })
            .collect();
        let mut cards = v_flex().gap_4();
        for (instance_id, name, driver) in providers {
            let instance = self.server_info.settings["providerInstances"][instance_id.as_str()].clone();
            let accent = instance["accentColor"].as_str().map(str::to_owned);
            let managed = driver == "codex" && accounts::codex_setup_managed(&instance["config"]);
            let commit_instance = if instance.is_object() { instance.clone() } else { json!({ "driver": driver }) };
            let commit_id = instance_id.clone();
            let workspace = cx.weak_entity();
            let picker = accent::accent_color_picker(
                SharedString::from(format!("accent-{instance_id}")),
                &name,
                accent.as_deref(),
                None,
                move |color, _, cx| {
                    let Some(client) = workspace.upgrade().and_then(|w| w.read(cx).client()) else { return };
                    let mut next = commit_instance.clone();
                    match color.as_str() {
                        "" => _ = next.as_object_mut().map(|o| o.remove("accentColor")),
                        color => next["accentColor"] = color.into(),
                    }
                    let payload = json!({
                        "patch": {},
                        "providerInstanceMutation": { "operation": "upsert", "instanceId": commit_id, "instance": next },
                    });
                    cx.background_executor()
                        .spawn(async move { client.request("server.updateSettings", payload).await })
                        .detach();
                },
                window,
                cx,
            );
            let runtime = managed.then(|| self.render_codex_runtime_fields(&instance_id, window, cx));
            let account = self.render_provider_account(&instance_id, window, cx);
            cards = cards.child(
                div()
                    .id(SharedString::from(format!("provider-card-{instance_id}")))
                    .rounded_xl()
                    .border_1()
                    .border_color(t.border)
                    .bg(t.card)
                    .child(
                        h_flex()
                            .px_4()
                            .py_3()
                            .gap_2()
                            .child(picker)
                            .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(name))
                            .child(div().text_xs().text_color(t.muted_foreground).child(instance_id.clone())),
                    )
                    .children(runtime)
                    .child(account),
            );
        }
        v_flex()
            .id("providers-preview")
            .size_full()
            .overflow_y_scroll()
            .gap_6()
            .child(
                h_flex().justify_end().child(
                    accounts::button("add-chatgpt-account", "Add ChatGPT account").outline().small().on_click(
                        cx.listener(|this, _, window, cx| this.open_add_codex_account_dialog(None, window, cx)),
                    ),
                ),
            )
            .child(cards)
            .child(self.render_usage_limit_sources(window, cx))
            .into_any_element()
    }
}
