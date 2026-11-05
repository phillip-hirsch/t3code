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

use super::logic::{self, PROVIDERS_MANAGE, SecretProviderDraft, Session};
use super::ui::{self, Size, Variant, WebButton, text_sm, text_xs};
use super::{auth_request, error_toast, update_settings};
use crate::app::Workspace;
use crate::rpc::RpcClient;
use crate::theme::{self, FONT_MONO, Tokens};

/// What the page keeps: the session (for `providers:manage`) and revealed accounts.
#[derive(Default)]
pub(super) struct SecretsState {
    started: bool,
    session: Option<Session>,
    revealed: HashSet<String>,
    _session_task: Option<Task<()>>,
}

impl Workspace {
    pub(super) fn render_secrets(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let state = window.use_keyed_state("settings-secrets", cx, |_, _| SecretsState::default());
        let endpoint = self.endpoint.clone();
        if self.client().is_some() && !state.read(cx).started {
            state.update(cx, |state, cx| {
                state.started = true;
                state._session_task = Some(cx.spawn(async move |this, cx| {
                    let session = auth_request(endpoint, "/api/auth/session", None).await;
                    let _ = this.update(cx, |this, cx| {
                        // A failed check reads as no grant, like `resolveRemoteOperateAccess`.
                        this.session = Some(session.map(|v| Session::from_json(&v)).unwrap_or_default());
                        cx.notify();
                    });
                }));
            });
        }
        let t = *theme::tokens(cx);
        let config = &self.server_info.config;
        let connected = self.client().is_some() && config.is_object();
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
        ui::page("settings-secrets-page", [section])
    }

    fn secret_providers(&self, state: &Entity<SecretsState>, environment: String, cx: &App, t: &Tokens) -> AnyElement {
        let s = state.read(cx);
        // Provider settings and secret providers need the same grant.
        let read_only = !s.session.as_ref().is_some_and(|session| session.grants(PROVIDERS_MANAGE));
        let providers = self.server_settings()["secretProviders"].as_object().cloned().unwrap_or_default();
        let ids: Vec<String> = providers.keys().cloned().collect();
        let client = self.client();
        let add = (!read_only).then(|| {
            let (client, environment, ids) = (client.clone(), environment.clone(), ids.clone());
            WebButton::new("add-secret-provider", "Add provider")
                .icon(IconName::Plus)
                .on_click(move |_, window, cx| {
                    open_provider_dialog(client.clone(), environment.clone(), ids.clone(), None, window, cx)
                })
                .into_any_element()
        });
        let rows = if providers.is_empty() {
            vec![
                ui::row(
                    "No secret providers configured.",
                    Some(
                        "Add a 1Password account, then pick it as the source of a provider variable."
                            .into_any_element(),
                    ),
                    None,
                    None,
                    t,
                )
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
                                    )
                                })
                        });
                        let remove = {
                            let (client, id, label) = (client.clone(), id.clone(), label.clone());
                            WebButton::new(ElementId::Name(format!("secret-provider-remove-{id}").into()), "Remove")
                                .variant(Variant::Ghost)
                                .on_click(move |_, window, cx| {
                                    confirm_remove(client.clone(), id.clone(), label.clone(), window, cx)
                                })
                        };
                        h_flex().gap(px(4.)).children(edit).child(remove).into_any_element()
                    });
                    ui::row(title, Some(description), None, controls, t).into_any_element()
                })
                .collect()
        };
        ui::section("Secret providers", None, add, rows, t)
    }
}

fn message_section(message: String, t: &Tokens) -> AnyElement {
    let text = text_sm(div().px(px(16.)).py(px(12.)).text_color(t.muted_foreground)).child(message);
    ui::section("Secret providers", None, None, vec![text.into_any_element()], t)
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
        .font_family(FONT_MONO)
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
            })
        })
        .child(text)
        .into_any_element()
}

fn confirm_remove(client: Option<RpcClient>, id: String, label: String, window: &mut Window, cx: &mut App) {
    window.open_alert_dialog(cx, move |dialog, _, _| {
        let (client, id) = (client.clone(), id.clone());
        dialog
            .title(format!("Remove {label}?"))
            .description(format!(
                "Provider variables that read from {label} stop resolving until a provider with the same name is added again. The secrets themselves are untouched."
            ))
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
