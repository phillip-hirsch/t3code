//! "Usage providers" on the Providers page (`UsageProviderSettings.tsx`):
//! CLIProxyAPI hubs whose pooled quotas show beside the providers, and the
//! macOS-only Cursor keychain switch. Adding a hub sends its management key
//! once to the server's secret storage (`AddUsageLimitSourceDialog.tsx`).

use gpui_kit::component::button::ButtonVariants as _;
use gpui_kit::component::dialog::DialogEntrance;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{Disableable as _, IconName, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, FontWeight, IntoElement, ParentElement as _, SharedString,
    Styled as _, Subscription, WeakEntity, Window, div, px,
};
use reqwest::Url;
use serde_json::{Map, Value, json};

use super::accounts::{button, settings_row};
use crate::app::Workspace;
use crate::theme;

/// `sourceIdFromUrl` (`AddUsageLimitSourceDialog.logic.ts`): a saved hub keeps
/// its id; a new one gets an id no other hub uses, since settings merge by id.
fn source_id_from_url(url: &str, sources: &Map<String, Value>) -> String {
    if let Some((id, _)) = sources.iter().find(|(_, source)| source["url"].as_str() == Some(url)) {
        return id.clone();
    }
    // `new URL(url).host`: host and port, empty for scheme-only parses like `localhost:8318`.
    let host = match Url::parse(url) {
        Ok(parsed) => match (parsed.host_str(), parsed.port()) {
            (Some(host), Some(port)) => format!("{host}:{port}"),
            (Some(host), None) => host.to_owned(),
            (None, _) => String::new(),
        },
        Err(_) => url.to_owned(),
    };
    let mut slug = String::new();
    for c in host.to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '-' {
            slug.push(c);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');
    let base = format!("cliproxy-{}", if slug.is_empty() { "hub" } else { slug });
    let mut id = base.clone();
    let mut suffix = 2;
    while sources.contains_key(&id) {
        id = format!("{base}-{suffix}");
        suffix += 1;
    }
    id
}

/// Whether the Cursor switch is saving, kept with the section.
#[derive(Default)]
struct UsageSourcesState {
    updating_cursor: bool,
}

impl Workspace {
    pub(in crate::app) fn render_usage_limit_sources(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = *theme::tokens(cx);
        let state =
            window.use_keyed_state(self.settings_state_key("usage-sources"), cx, |_, _| UsageSourcesState::default());
        let sources = self.settings_info().settings["usageLimitSources"].as_object().cloned().unwrap_or_default();
        let cursor_enabled = self.settings_info().settings["cursorKeychainUsageEnabled"].as_bool() == Some(true);
        let darwin = self.settings_info().platform_os.as_deref() == Some("darwin");
        let updating = state.read(cx).updating_cursor;

        let mut rows = v_flex();
        if darwin {
            let state = state.clone();
            rows = rows.child(settings_row(
                &t,
                "Cursor account usage",
                Some(div().child("Read your existing Cursor CLI login from macOS Keychain to show account history and monthly limits. macOS may ask you to allow access.").into_any_element()),
                Some(
                    Switch::new("cursor-keychain-usage")
                        .checked(cursor_enabled)
                        .disabled(updating)
                        .on_change(cx.listener(move |this, enabled: &bool, _, cx| {
                            this.set_cursor_usage(*enabled, state.clone(), cx)
                        }))
                        .into_any_element(),
                ),
            ));
        }
        if sources.is_empty() {
            rows = rows.child(settings_row(&t, "No hubs configured.", None, None));
        }
        for (id, source) in &sources {
            let url = source["url"].as_str().unwrap_or_default().to_owned();
            let label = source["label"].as_str().map(str::trim).filter(|l| !l.is_empty()).unwrap_or(&url).to_owned();
            let mut description =
                format!("CLI Proxy{}", if source["enabled"].as_bool() == Some(false) { " · Disabled" } else { "" });
            if label != url {
                description.push_str(&format!(" · {url}"));
            }
            let id = id.clone();
            let remove_label = label.clone();
            rows = rows.child(settings_row(
                &t,
                label,
                Some(div().child(description).into_any_element()),
                Some(
                    button(SharedString::from(format!("remove-hub-{id}")), "Remove")
                        .ghost()
                        .xsmall()
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.confirm_remove_hub(id.clone(), remove_label.clone(), window, cx)
                        }))
                        .into_any_element(),
                ),
            ));
        }
        v_flex()
            .gap_2p5()
            .child(
                h_flex()
                    .min_h(px(28.))
                    .px_4()
                    .justify_between()
                    .child(
                        div()
                            .text_base()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(t.foreground)
                            .child("Usage providers"),
                    )
                    .child(
                        button("add-usage-hub", "Add hub")
                            .outline()
                            .xsmall()
                            .icon(IconName::Plus)
                            .on_click(cx.listener(|this, _, window, cx| this.open_add_hub_dialog(window, cx))),
                    ),
            )
            .child(super::group(rows, true, &t))
            .into_any_element()
    }

    fn set_cursor_usage(&mut self, enabled: bool, state: Entity<UsageSourcesState>, cx: &mut Context<Self>) {
        let Some(client) = self.settings_client() else { return };
        state.update(cx, |state, cx| {
            state.updating_cursor = true;
            cx.notify();
        });
        cx.spawn(async move |this, cx| {
            let saved = client
                .request("server.updateSettings", json!({ "patch": { "cursorKeychainUsageEnabled": enabled } }))
                .await;
            if saved.is_ok() {
                let _ = client.request("server.refreshProviders", json!({})).await;
            }
            let _ = this.update(cx, |_, cx| {
                state.update(cx, |state, cx| {
                    state.updating_cursor = false;
                    cx.notify();
                })
            });
        })
        .detach();
    }

    fn confirm_remove_hub(&mut self, id: String, label: String, window: &mut Window, cx: &mut Context<Self>) {
        // The hub's key lives on this environment's server, even if Settings moves on meanwhile.
        let (workspace, scope) = (cx.weak_entity(), self.settings_environment());
        window.open_alert_dialog(cx, move |alert, _, _| {
            let alert = alert.entrance(DialogEntrance::None);
            let (workspace, scope) = (workspace.clone(), scope.clone());
            let id = id.clone();
            alert
                .title(format!("Remove {label}?"))
                .description(
                    "The hub's management key is deleted from this server. Its accounts leave the Limits view; the hub itself is untouched. Add it again with the URL and key to bring them back.",
                )
                .show_cancel(true)
                .ok_text("Remove hub")
                .ok_variant(gpui_kit::component::button::ButtonVariant::Danger)
                .on_ok(move |_, _, cx| {
                    let patch = json!({ "usageLimitSources": { id.clone(): null } });
                    let scope = scope.clone();
                    let _ = workspace.update(cx, |this, cx| this.update_settings_on(scope, patch, cx, |_, _, _| {}));
                    true
                })
        });
    }

    fn open_add_hub_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (workspace, scope) = (cx.weak_entity(), self.settings_environment());
        let dialog = cx.new(|cx| AddHub::new(workspace, scope, window, cx));
        window.open_dialog(cx, move |modal, _, cx| {
            dialog.update(cx, |this, cx| this.render_dialog(modal.entrance(DialogEntrance::None), cx))
        });
    }
}

struct AddHub {
    workspace: WeakEntity<Workspace>,
    /// The environment whose server keeps the hub's key.
    scope: Option<SharedString>,
    url: Entity<InputState>,
    key: Entity<InputState>,
    label: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

impl AddHub {
    fn new(
        workspace: WeakEntity<Workspace>,
        scope: Option<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let url = cx.new(|cx| InputState::new(window, cx).placeholder("https://hub.example.ts.net:8318"));
        let key = cx.new(|cx| InputState::new(window, cx).masked(true));
        let label = cx.new(|cx| InputState::new(window, cx).placeholder("Defaults to the hub's host name"));
        let subscriptions = [&url, &key, &label]
            .into_iter()
            .map(|input| {
                cx.subscribe_in(input, window, |this, _, event: &InputEvent, window, cx| match event {
                    InputEvent::PressEnter { .. } => this.save(window, cx),
                    InputEvent::Change => cx.notify(),
                    _ => {}
                })
            })
            .collect();
        url.update(cx, |url, cx| url.focus(window, cx));
        Self { workspace, scope, url, key, label, _subscriptions: subscriptions }
    }

    fn info<'a>(&self, cx: &'a App) -> Option<&'a crate::model::ServerInfo> {
        self.workspace.upgrade()?.read(cx).env_info(self.scope.as_ref()?)
    }

    fn values(&self, cx: &App) -> (String, String, String) {
        let read = |input: &Entity<InputState>| input.read(cx).value().trim().to_owned();
        (read(&self.url), read(&self.key), read(&self.label))
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (url, key, label) = self.values(cx);
        if url.is_empty() || key.is_empty() {
            return;
        }
        // A disconnected environment keeps the dialog open rather than saving elsewhere.
        let Some(info) = self.info(cx) else { return };
        let sources = info.settings["usageLimitSources"].as_object().cloned().unwrap_or_default();
        let id = source_id_from_url(&url, &sources);
        let mut source = json!({ "kind": "cliproxy", "url": url, "managementKey": key, "enabled": true });
        if !label.is_empty() {
            source["label"] = label.into();
        }
        let patch = json!({ "usageLimitSources": { id: source } });
        let scope = self.scope.clone();
        let _ = self.workspace.update(cx, |this, cx| this.update_settings_on(scope, patch, cx, |_, _, _| {}));
        window.close_dialog(cx);
    }

    fn render_dialog(
        &mut self,
        modal: gpui_kit::component::dialog::Dialog,
        cx: &mut Context<Self>,
    ) -> gpui_kit::component::dialog::Dialog {
        let t = *theme::tokens(cx);
        let environment =
            self.info(cx).and_then(|info| info.environment_label.clone()).unwrap_or_else(|| "this environment".into());
        let (url, key, _) = self.values(cx);
        let field = |label: &'static str, input: &Entity<InputState>| {
            v_flex()
                .gap_1p5()
                .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(label))
                .child(Input::new(input))
        };
        modal
            .w(px(448.))
            .title("Add a CLIProxyAPI hub")
            .child(
                v_flex()
                    .gap_4()
                    .child(div().text_sm().text_color(t.muted_foreground).child(format!(
                        "Show the quota of every account the hub pools, next to the providers on {environment}. The key stays on that server."
                    )))
                    .child(field("Hub URL", &self.url))
                    .child(field("Management key", &self.key))
                    .child(field("Label (optional)", &self.label)),
            )
            .footer(
                h_flex()
                    .w_full()
                    .justify_end()
                    .gap_2()
                    .child(button("add-hub-cancel", "Cancel").outline().on_click(|_, window, cx| window.close_dialog(cx)))
                    .child(
                        button("add-hub-save", "Add hub").primary()
                            .disabled(url.is_empty() || key.is_empty())
                            .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Adds each hub the way the dialog does, then applies the merge-by-id patch.
    fn add(sources: &mut Map<String, Value>, url: &str) -> String {
        let id = source_id_from_url(url, sources);
        sources.insert(id.clone(), json!({ "kind": "cliproxy", "url": url }));
        id
    }

    #[test]
    fn hubs_on_one_host_keep_separate_entries() {
        for (first, second) in
            [("http://proxy.test:8318/hub-a", "http://proxy.test:8318/hub-b"), ("localhost:8318", "localhost:9318")]
        {
            let mut sources = Map::new();
            let ids = [first, second, &format!("{second}/extra")].map(|url| add(&mut sources, url));
            assert_eq!(sources.len(), 3, "{ids:?}");
        }
        let mut sources = Map::new();
        assert_eq!(add(&mut sources, "http://Proxy.Test:8318/a"), "cliproxy-proxy.test-8318");
        assert_eq!(add(&mut sources, "http://proxy.test:8318/b"), "cliproxy-proxy.test-8318-2");
        assert_eq!(add(&mut sources, "localhost:8318"), "cliproxy-hub");
    }

    #[test]
    fn re_adding_a_url_updates_its_entry() {
        let mut sources = Map::new();
        let first = add(&mut sources, "http://proxy.test:8318/hub-a");
        assert_eq!(add(&mut sources, "http://proxy.test:8318/hub-a"), first);
        assert_eq!(sources.len(), 1);
    }
}
