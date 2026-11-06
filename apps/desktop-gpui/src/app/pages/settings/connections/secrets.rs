//! Secrets (`SecretsSettings.tsx`, `SecretProviderDialog.tsx`): the 1Password
//! accounts provider variables read from, stored in `settings.secretProviders`.

use std::collections::HashSet;

use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, ElementId, Entity, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window,
    div, px,
};
use serde_json::{Value, json};

use super::super::widgets::{RowFrame, section_with_action, settings_section};
use super::logic::{self, PROVIDERS_MANAGE, SecretProviderDraft};
use super::ui::{self, Size, Variant, WebButton, text_sm, text_xs};
use super::{SessionLoad, check_session, error_toast, update_settings};
use crate::app::Workspace;
use crate::app::pages::settings::widgets::SettingsView;
use crate::rpc::RpcClient;
use crate::theme::{self, Tokens};

/// What the page keeps: the session (for `providers:manage`) and revealed accounts.
#[derive(Default)]
pub(super) struct SecretsState {
    started: bool,
    session: SessionLoad,
    revealed: HashSet<String>,
    _session_task: Option<Task<()>>,
}

impl Workspace {
    pub(super) fn render_secrets(
        &mut self,
        view: &SettingsView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let state =
            window.use_keyed_state(self.settings_state_key("settings-secrets"), cx, |_, _| SecretsState::default());
        let started = state.read(cx).started;
        if let (Some(endpoint), Some(_), false) = (self.settings_endpoint().cloned(), self.settings_client(), started) {
            state.update(cx, |state, cx| {
                state.started = true;
                state._session_task = Some(check_session(endpoint, cx, |this, session, _| this.session = session));
            });
        }
        let t = *theme::tokens(cx);
        let config = &self.settings_info().config;
        let connected = self.settings_client().is_some() && config.is_object();
        let label = self.environment_label().unwrap_or("this environment").to_owned();
        let section = if connected && config["secretProviderSettings"] != true {
            message_section(format!("Update T3 Code on {label} to manage its secret providers."), &t)
        } else if connected {
            self.secret_providers(&state, label, cx, &t)
        } else {
            message_section(
                "Connect an environment to manage the secret providers its provider variables read from.".into(),
                &t,
            )
        };
        self.settings_page("secrets", view, [section], cx)
    }

    fn secret_providers(&self, state: &Entity<SecretsState>, environment: String, cx: &App, t: &Tokens) -> AnyElement {
        let s = state.read(cx);
        // Provider settings and secret providers need the same grant.
        let read_only = !s.session.grants(PROVIDERS_MANAGE);
        let empty = serde_json::Map::new();
        let providers = self.server_settings()["secretProviders"].as_object().unwrap_or(&empty);
        let ids: Vec<String> = providers.keys().cloned().collect();
        let client = self.settings_client();
        let add = (!read_only).then(|| {
            let (client, environment, ids) = (client.clone(), environment.clone(), ids.clone());
            WebButton::new("add-secret-provider", "Add provider")
                .icon(IconName::Plus)
                .on_click(move |_, window, cx| {
                    open_provider_dialog(client.clone(), environment.clone(), ids.clone(), None, window, cx);
                })
                .into_any_element()
        });
        let rows = if providers.is_empty() {
            vec![
                RowFrame::new("No secret providers configured.")
                    .description("Add a 1Password account, then pick it as the source of a provider variable.")
                    .render(t)
                    .into_any_element(),
            ]
        } else {
            providers
                .iter()
                .map(|(id, provider)| {
                    let label = provider["label"].as_str().unwrap_or(id).to_owned();
                    let account = logic::one_password_account(provider);
                    let title = h_flex()
                        .min_w_0()
                        .gap(px(8.))
                        .when(account.is_some(), |el| {
                            el.child(ui::glyph("icons/app/onepassword.svg", 18., t.foreground.opacity(0.8)))
                        })
                        .child(div().min_w_0().child(label.clone()));
                    let description = match &account {
                        Some(account) => {
                            let revealed = s.revealed.contains(id);
                            h_flex()
                                .min_w_0()
                                .gap(px(4.))
                                .child(div().flex_none().child("1Password ·"))
                                .child(redacted_account(state, id, account, revealed, t))
                                .into_any_element()
                        }
                        None => format!("Unsupported provider ({})", provider["kind"].as_str().unwrap_or("unknown"))
                            .into_any_element(),
                    };
                    let controls = (!read_only).then(|| {
                        let edit = account.is_some().then(|| {
                            let (client, environment, ids) = (client.clone(), environment.clone(), ids.clone());
                            let editing = (id.clone(), provider.clone());
                            WebButton::new(ElementId::Name(format!("secret-provider-edit-{id}").into()), "Edit")
                                .variant(Variant::Ghost)
                                .on_click(move |_, window, cx| {
                                    let editing = Some(editing.clone());
                                    open_provider_dialog(
                                        client.clone(),
                                        environment.clone(),
                                        ids.clone(),
                                        editing,
                                        window,
                                        cx,
                                    );
                                })
                        });
                        let remove = {
                            let (client, id, label) = (client.clone(), id.clone(), label.clone());
                            let uses: Vec<String> = logic::secret_provider_uses(self.server_settings(), &id)
                                .into_iter()
                                .map(|u| u.label)
                                .collect();
                            WebButton::new(ElementId::Name(format!("secret-provider-remove-{id}").into()), "Remove")
                                .variant(Variant::Ghost)
                                .on_click(move |_, window, cx| {
                                    confirm_remove(client.clone(), id.clone(), label.clone(), uses.clone(), window, cx);
                                })
                        };
                        h_flex().gap(px(4.)).children(edit).child(remove).into_any_element()
                    });
                    RowFrame::new(title)
                        .description(description)
                        .control(div().children(controls))
                        .render(t)
                        .into_any_element()
                })
                .collect()
        };
        section_with_action("secret-providers", "Secret providers", add, rows, t)
    }
}

fn message_section(message: String, t: &Tokens) -> AnyElement {
    let text = text_sm(div().px(px(16.)).py(px(12.)).text_color(t.muted_foreground)).child(message);
    settings_section("secret-providers", "Secret providers", [text.into_any_element()], t)
}

/// `RedactedSensitiveText`: the account as look-alike text until clicked.
fn redacted_account(state: &Entity<SecretsState>, id: &str, account: &str, revealed: bool, t: &Tokens) -> AnyElement {
    let text = if revealed { account.to_owned() } else { logic::redacted_placeholder(account) };
    let tip = if revealed { "Click to hide account" } else { "Click to reveal account" };
    let (state, id_owned) = (state.clone(), id.to_owned());
    let fg = t.foreground;
    div()
        .id(ElementId::Name(format!("secret-provider-account-{id}").into()))
        .min_w_0()
        .truncate()
        .cursor_pointer()
        .font_family(t.fonts.mono)
        .text_size(px(11.))
        .text_color(t.muted_foreground)
        // The web blurs the placeholder; GPUI has no text blur, so it dims it instead.
        .when(!revealed, |el| el.opacity(0.6))
        .hover(move |style| style.text_color(fg))
        .tooltip(ui::tooltip(tip))
        .on_click(move |_, _, cx| {
            state.update(cx, |state, cx| {
                if !state.revealed.remove(&id_owned) {
                    state.revealed.insert(id_owned.clone());
                }
                cx.notify();
            });
        })
        .child(text)
        .into_any_element()
}

/// `RemoveSecretProviderButton`; `uses` are what reads from the provider and will be unset.
fn confirm_remove(
    client: Option<RpcClient>,
    id: String,
    label: String,
    uses: Vec<String>,
    window: &mut Window,
    cx: &mut App,
) {
    window.open_alert_dialog(cx, move |dialog, _, cx| {
        let (client, id) = (client.clone(), id.clone());
        let t = theme::tokens(cx);
        let description = if uses.is_empty() {
            format!("Nothing reads from {label}. The secrets themselves are untouched.").into_any_element()
        } else {
            let list = v_flex()
                .id("secret-provider-uses")
                .max_h(px(192.))
                .overflow_y_scroll()
                .gap(px(2.))
                .text_color(t.foreground)
                .children(uses.iter().map(|use_label| {
                    h_flex()
                        .items_start()
                        .child(div().w(px(20.)).flex_none().text_center().child("•"))
                        .child(text_sm(div().min_w_0()).child(use_label.clone()))
                }));
            v_flex()
                .gap(px(8.))
                .child(format!("These read from {label} and will be unset. The secrets themselves are untouched."))
                .child(list)
                .into_any_element()
        };
        dialog
            .title(format!("Remove {label}?"))
            .description(description)
            .show_cancel(true)
            .ok_text("Remove provider")
            .ok_variant(gpui_kit::component::button::ButtonVariant::Danger)
            .on_ok(move |_, window, cx| {
                let patch = json!({ "secretProviders": { id.clone(): null } });
                save(client.clone(), patch, window, cx);
                true
            })
    });
}

fn save(client: Option<RpcClient>, patch: Value, window: &mut Window, cx: &mut App) {
    window
        .spawn(cx, async move |cx| {
            if let Err(error) = update_settings(client, patch).await {
                let _ = cx.update(|window, cx| error_toast(window, cx, "Could not update settings", error));
            }
        })
        .detach();
}

/// The add/edit dialog's form (`SecretProviderDialog`); one per open, so each starts fresh.
struct ProviderForm {
    name: Entity<InputState>,
    account: Entity<InputState>,
    existing_ids: Vec<String>,
    /// The provider being edited: its id and stored config.
    editing: Option<(String, Value)>,
    submitted: bool,
    client: Option<RpcClient>,
    _subscriptions: [Subscription; 2],
}

impl ProviderForm {
    fn draft(&self, cx: &App) -> SecretProviderDraft {
        SecretProviderDraft {
            name: self.name.read(cx).value().to_string(),
            account: self.account.read(cx).value().to_string(),
        }
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.submitted = true;
        cx.notify();
        let editing = self.editing.as_ref().map(|(id, _)| id.as_str());
        let Ok(entry) = logic::secret_provider_from_draft(&self.draft(cx), &self.existing_ids, editing) else { return };
        let patch = json!({ "secretProviders": { entry.id: {
            "kind": "one_password", "label": entry.label, "config": { "account": entry.account },
        } } });
        save(self.client.clone(), patch, window, cx);
        window.close_dialog(cx);
    }
}

impl Render for ProviderForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *theme::tokens(cx);
        let editing = self.editing.as_ref().map(|(id, _)| id.as_str());
        let result = logic::secret_provider_from_draft(&self.draft(cx), &self.existing_ids, editing);
        let issues =
            if self.submitted { result.as_ref().err().cloned().unwrap_or_default() } else { Default::default() };
        let field =
            |label: &'static str, input: &Entity<InputState>, error: Option<String>, hint: Option<&'static str>| {
                v_flex()
                    .gap(px(6.))
                    .child(text_sm(div().font_weight(FontWeight::MEDIUM)).child(label))
                    .child(Input::new(input))
                    .when_some(error, |el, error| el.child(text_xs(div().text_color(t.error_foreground)).child(error)))
                    .when_some(hint, |el, hint| el.child(text_xs(div().text_color(t.muted_foreground)).child(hint)))
            };
        v_flex()
            .gap(px(16.))
            .child(field("Name", &self.name, issues.name, None))
            .child(field(
                "Account",
                &self.account,
                issues.account,
                Some("The account shorthand, sign-in address without https://, or ID that op account list shows."),
            ))
            .child(
                h_flex()
                    .justify_end()
                    .gap(px(8.))
                    .child(
                        WebButton::new("secret-provider-cancel", "Cancel")
                            .size(Size::Default)
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        WebButton::new(
                            "secret-provider-save",
                            if self.editing.is_some() { "Save changes" } else { "Add provider" },
                        )
                        .size(Size::Default)
                        .variant(Variant::Primary)
                        .disabled(self.submitted && result.is_err())
                        .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
                    ),
            )
    }
}

fn open_provider_dialog(
    client: Option<RpcClient>,
    environment: String,
    existing_ids: Vec<String>,
    editing: Option<(String, Value)>,
    window: &mut Window,
    cx: &mut App,
) {
    let draft = editing.as_ref().map(|(_, config)| SecretProviderDraft {
        name: config["label"].as_str().unwrap_or_default().to_owned(),
        account: logic::one_password_account(config).unwrap_or_default(),
    });
    let draft = draft.unwrap_or_default();
    let name = cx.new(|cx| InputState::new(window, cx).placeholder("Personal").default_value(draft.name));
    let account = cx.new(|cx| InputState::new(window, cx).placeholder("my.1password.com").default_value(draft.account));
    name.update(cx, |input, cx| input.focus(window, cx));
    let title: SharedString = match &editing {
        Some((_, config)) => format!("Edit {}", config["label"].as_str().unwrap_or_default()).into(),
        None => "Add a 1Password provider".into(),
    };
    let description: SharedString = format!(
        "Provider variables on {environment} can read values from it by name. Values are read with the op CLI on that server when a provider starts."
    )
    .into();
    let form = cx.new(|cx| {
        let on_input = |this: &mut ProviderForm,
                        _: &Entity<InputState>,
                        event: &InputEvent,
                        window: &mut Window,
                        cx: &mut Context<ProviderForm>| {
            match event {
                InputEvent::PressEnter { .. } => this.save(window, cx),
                InputEvent::Change => cx.notify(),
                _ => {}
            }
        };
        ProviderForm {
            _subscriptions: [cx.subscribe_in(&name, window, on_input), cx.subscribe_in(&account, window, on_input)],
            name,
            account,
            existing_ids,
            editing,
            submitted: false,
            client,
        }
    });
    window.open_dialog(cx, move |dialog, _, cx| {
        let t = theme::tokens(cx);
        dialog
            .title(title.clone())
            .w(px(448.))
            .child(text_sm(div().mb(px(16.)).text_color(t.muted_foreground)).child(description.clone()))
            .child(form.clone())
    });
}
