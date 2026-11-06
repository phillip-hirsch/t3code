//! Secrets and links an agent asks the user for, answered inline in the timeline
//! (`SecretRequestCard.tsx`, `LinkRequestCard.tsx`). The typed secret and pairing code
//! live only in their inputs and the RPC payload: never logged, toasted or persisted.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::DialogEntrance;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, FontWeight, IntoElement, ParentElement as _, Render,
    SharedString, Styled as _, Subscription, WeakEntity, Window, div, prelude::FluentBuilder as _, px,
};
use serde_json::{Value, json};

use super::Workspace;
use crate::rpc::{RpcClient, RpcError};
use crate::theme::{self, Tokens};
use crate::timeline::{self, RequestCard, RequestKind};

/// The access levels a link may grant, as the access picker names them (`LinkAccessPicker`).
const ACCESS: [(&str, &str); 5] = [
    ("read-only", "Read projects and threads. Cannot start, message or change anything."),
    ("approval-required", "Ask before commands and file changes."),
    ("auto-accept-edits", "Auto-approve edits, ask before other actions."),
    ("auto", "Supported providers approve routine actions; others still ask."),
    ("full-access", "Allow commands and edits without prompts."),
];

#[derive(Default)]
pub(super) struct RequestUi {
    secrets: HashMap<SharedString, (Entity<InputState>, Subscription)>,
    /// The secret provider each card reads its answer from; absent, the typed value is saved.
    sources: HashMap<SharedString, SharedString>,
    references: HashMap<SharedString, (Entity<InputState>, Subscription)>,
    busy: HashSet<SharedString>,
    errors: HashMap<SharedString, SharedString>,
}

/// A configured secret provider a pending card can read its answer from (`SecretRequestSource`).
#[derive(Clone, Debug, PartialEq)]
pub(super) struct SecretSource {
    pub id: SharedString,
    pub label: SharedString,
}

/// 1Password's definition: `referencePlaceholder` and `invalidReferenceMessage`.
const REFERENCE_PLACEHOLDER: &str = "op://vault/item/field";
const INVALID_REFERENCE: &str = "Use a 1Password reference like op://vault/item/field.";

/// `secretRequestSources`: empty unless the server accepts reference answers; providers this
/// build can't read (any kind but 1Password, or a config that no longer decodes) are left out.
pub(super) fn secret_sources(accepts_references: bool, providers: &Value) -> Vec<SecretSource> {
    if !accepts_references {
        return Vec::new();
    }
    let Some(providers) = providers.as_object() else { return Vec::new() };
    providers
        .iter()
        .filter(|(_, provider)| super::pages::settings::one_password_account(provider).is_some())
        .map(|(id, provider)| SecretSource {
            id: id.clone().into(),
            label: provider["label"].as_str().unwrap_or(id).to_owned().into(),
        })
        .collect()
}

/// 1Password's `isReference`: `op://` then three or four names, and an optional `?key=value`.
/// Names outside this charset must be written as item ids.
fn is_one_password_reference(reference: &str) -> bool {
    let name_ok = |name: &str| {
        let edge = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-');
        name.chars().next().is_some_and(edge)
            && name.chars().last().is_some_and(edge)
            && name.chars().all(|c| edge(c) || c == ' ')
    };
    let Some(rest) = reference.strip_prefix("op://").filter(|_| reference.len() <= 1024) else { return false };
    let (path, query) = match rest.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (rest, None),
    };
    let names: Vec<&str> = path.split('/').collect();
    let query_ok = query.is_none_or(|query| {
        query.split_once('=').is_some_and(|(key, value)| {
            !key.is_empty()
                && key.chars().all(|c| c.is_ascii_lowercase() || c == '-')
                && !value.is_empty()
                && value.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
        })
    });
    (3..=4).contains(&names.len()) && names.iter().all(|name| name_ok(name)) && query_ok
}

/// `secretRequestReference`: the reference `text` names for `source`, or `None` when it is not
/// that provider's syntax.
pub(super) fn secret_reference(source: &str, text: &str) -> Option<Value> {
    let reference = text.trim();
    is_one_password_reference(reference).then(|| json!({"provider": source, "reference": reference}))
}

/// `secretRequestFailureMessage` / `linkRequestFailureMessage`: only errors written for the
/// user pass their message through, so a typed value can never surface.
fn failure_message(error: &RpcError, tags: &[&str]) -> SharedString {
    if let RpcError::Failure(failure) = error {
        let tag = failure["_tag"].as_str().unwrap_or_default();
        if let Some(message) = failure["message"].as_str().map(str::trim).filter(|m| !m.is_empty())
            && tags.contains(&tag)
        {
            return message.to_owned().into();
        }
    }
    "Could not answer the request. Try again.".into()
}

const SECRET_TAGS: [&str; 2] = ["SecretRequestError", "EnvironmentAuthorizationError"];
const LINK_TAGS: [&str; 3] = ["PeerLinkRequestError", "EnvironmentAuthorizationError", "PeerPairingCodeError"];

impl Workspace {
    /// Drops typed secrets and errors of requests no longer shown: answered elsewhere,
    /// declined or deleted, the typed value must not outlive its card.
    pub(super) fn prune_requests(&mut self) {
        let side = self.side_chat.view.as_ref().map(|view| view.rows.clone());
        let shown = |id: &SharedString| {
            self.rows
                .iter()
                .chain(side.iter().flat_map(|rows| rows.iter()))
                .any(|row| matches!(row, crate::timeline::Row::Request(card) if card.item_id == *id))
        };
        let ui = &mut self.requests;
        ui.secrets.retain(|id, _| shown(id));
        ui.references.retain(|id, _| shown(id));
        ui.sources.retain(|id, _| shown(id));
        ui.errors.retain(|id, _| shown(id));
    }

    fn secret_input(&mut self, card: &RequestCard, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        if let Some((input, _)) = self.requests.secrets.get(&card.item_id) {
            return input.clone();
        }
        let placeholder = match &card.kind {
            RequestKind::Secret { placeholder: Some(p), .. } => p.clone(),
            _ => "Paste the secret".into(),
        };
        let input = cx.new(|cx| InputState::new(window, cx).masked(true).placeholder(placeholder));
        let id = card.item_id.clone();
        let card = card.clone();
        let subscription = cx.subscribe_in(&input, window, move |this, _, event: &InputEvent, _, cx| match event {
            InputEvent::PressEnter { .. } => this.answer_secret(&card, true, cx),
            InputEvent::Change => cx.notify(),
            _ => {}
        });
        self.requests.secrets.insert(id, (input.clone(), subscription));
        input
    }

    /// The secret providers this environment's cards can read from.
    fn secret_sources(&self) -> Vec<SecretSource> {
        if !super::pages::settings::secrets_enabled() {
            return Vec::new();
        }
        let info = self.server_info();
        secret_sources(info.config["secretRequestReferences"] == true, info.settings.get("secretProviders"))
    }

    fn reference_input(
        &mut self,
        card: &RequestCard,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        if let Some((input, _)) = self.requests.references.get(&card.item_id) {
            return input.clone();
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(REFERENCE_PLACEHOLDER));
        let id = card.item_id.clone();
        let card = card.clone();
        let subscription = cx.subscribe_in(&input, window, move |this, _, event: &InputEvent, _, cx| match event {
            InputEvent::PressEnter { .. } => this.answer_secret(&card, true, cx),
            InputEvent::Change => {
                this.requests.errors.remove(&card.item_id);
                cx.notify();
            }
            _ => {}
        });
        self.requests.references.insert(id, (input.clone(), subscription));
        input
    }

    fn set_secret_source(&mut self, item_id: &SharedString, source: Option<SharedString>, cx: &mut Context<Self>) {
        match source {
            Some(source) => self.requests.sources.insert(item_id.clone(), source),
            None => self.requests.sources.remove(item_id),
        };
        self.requests.errors.remove(item_id);
        cx.notify();
    }

    /// `secrets.answerRequest`: save the typed value, read it from a secret provider, or decline.
    fn answer_secret(&mut self, card: &RequestCard, save: bool, cx: &mut Context<Self>) {
        let Some(client) = self.client() else { return };
        let id = card.item_id.clone();
        if self.requests.busy.contains(&id) {
            return;
        }
        let source = self
            .requests
            .sources
            .get(&id)
            .cloned()
            .filter(|source| self.secret_sources().iter().any(|candidate| &candidate.id == source));
        let answer = match (save, source) {
            (false, _) => json!({"type": "decline"}),
            (true, Some(source)) => {
                let Some((input, _)) = self.requests.references.get(&id) else { return };
                let text = input.read(cx).value().to_string();
                if text.trim().is_empty() {
                    return;
                }
                let Some(reference) = secret_reference(&source, &text) else {
                    self.requests.errors.insert(id, INVALID_REFERENCE.into());
                    cx.notify();
                    return;
                };
                json!({"type": "reference", "reference": reference})
            }
            (true, None) => {
                let Some((input, _)) = self.requests.secrets.get(&id) else { return };
                let secret = input.read(cx).value().trim().to_owned();
                if secret.is_empty() {
                    return;
                }
                json!({"type": "save", "secret": secret})
            }
        };
        let input = json!({"threadId": card.thread_id, "turnItemId": id, "answer": answer});
        self.requests.busy.insert(id.clone());
        self.requests.errors.remove(&id);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = client.request("secrets.answerRequest", input).await;
            let _ = this.update(cx, |this, cx| {
                this.requests.busy.remove(&id);
                match result {
                    // The row turns into its answered line once the item updates.
                    Ok(_) => {
                        this.requests.secrets.remove(&id);
                        this.requests.references.remove(&id);
                    }
                    Err(error) => {
                        this.requests.errors.insert(id, failure_message(&error, &SECRET_TAGS));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn decline_link(&mut self, card: &RequestCard, cx: &mut Context<Self>) {
        let Some(client) = self.client() else { return };
        let id = card.item_id.clone();
        if !self.requests.busy.insert(id.clone()) {
            return;
        }
        self.requests.errors.remove(&id);
        cx.notify();
        let input = json!({"threadId": card.thread_id, "turnItemId": id, "answer": {"type": "decline"}});
        cx.spawn(async move |this, cx| {
            let result = client.request("peerLinks.answerRequest", input).await;
            let _ = this.update(cx, |this, cx| {
                this.requests.busy.remove(&id);
                if let Err(error) = result {
                    this.requests.errors.insert(id, failure_message(&error, &LINK_TAGS));
                }
                cx.notify();
            });
        })
        .detach();
    }
}

/// The pending card for a secret or link request.
pub(super) fn request_row(
    card: &Rc<RequestCard>,
    this: &WeakEntity<Workspace>,
    t: &Tokens,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let Some(workspace) = this.upgrade() else { return div().into_any_element() };
    let body = match &card.kind {
        RequestKind::Secret { label, file, .. } => {
            let (input, sources) = workspace.update(cx, |w, cx| {
                let sources = w.secret_sources();
                let source = w.requests.sources.get(&card.item_id).filter(|id| sources.iter().any(|s| &s.id == *id));
                let input = match source {
                    Some(_) => w.reference_input(card, window, cx),
                    None => w.secret_input(card, window, cx),
                };
                (input, sources)
            });
            secret_card(card, label, *file, &input, &sources, &workspace, t, cx)
        }
        RequestKind::Link { .. } => link_card(card, &workspace, t, cx),
    };
    super::chat::lane(8., body).into_any_element()
}

fn card_frame(t: &Tokens) -> gpui_kit::Div {
    v_flex()
        .min_w_0()
        .gap(px(12.))
        .p(px(16.))
        .rounded(px(12.))
        .border_1()
        .border_color(t.border.opacity(0.6))
        .bg(t.card)
}

#[expect(clippy::too_many_arguments, reason = "one card's parts, passed once from request_row")]
fn secret_card(
    card: &Rc<RequestCard>,
    label: &SharedString,
    file: bool,
    input: &Entity<InputState>,
    sources: &[SecretSource],
    workspace: &Entity<Workspace>,
    t: &Tokens,
    cx: &mut App,
) -> AnyElement {
    let state = workspace.read(cx);
    let busy = state.requests.busy.contains(&card.item_id);
    let source = state.requests.sources.get(&card.item_id).and_then(|id| sources.iter().find(|s| &s.id == id));
    let picker = (!sources.is_empty()).then(|| source_picker(card, label, source, sources, workspace, t));
    let error = state.requests.errors.get(&card.item_id).cloned();
    let empty = input.read(cx).value().trim().is_empty();
    let privacy = if file {
        "Kept out of chat, in a private file this agent's commands can read for 24 hours"
    } else {
        "Stored securely, never shown to the agent"
    };
    let (save_card, decline_card) = (card.clone(), card.clone());
    let (save_view, decline_view) = (workspace.downgrade(), workspace.downgrade());
    card_frame(t)
        .child(
            v_flex()
                .min_w_0()
                .gap(px(4.))
                .child(div().text_sm().font_weight(FontWeight::MEDIUM).text_color(t.foreground).child(label.clone()))
                .when(!card.reason.trim().is_empty(), |el| {
                    el.child(div().text_sm().text_color(t.muted_foreground).child(card.reason.clone()))
                }),
        )
        .child(
            h_flex()
                .min_w_0()
                .gap(px(8.))
                .children(picker)
                .child(div().flex_1().min_w_0().child(Input::new(input).disabled(busy)))
                .child(Button::new("secret-save").primary().label("Save securely").disabled(busy || empty).on_click(
                    move |_, _, cx| {
                        let _ = save_view.update(cx, |w, cx| w.answer_secret(&save_card, true, cx));
                    },
                )),
        )
        .children(error.map(|e| div().text_xs().text_color(t.error).child(e)))
        .child(
            h_flex()
                .min_w_0()
                .justify_between()
                .gap(px(8.))
                .child(
                    h_flex()
                        .min_w_0()
                        .gap(px(6.))
                        .text_xs()
                        .text_color(t.muted_foreground)
                        .child(Icon::new(IconName::ShieldCheck).size(px(14.)).flex_none())
                        .child(privacy),
                )
                .child(
                    Button::new("secret-decline")
                        .ghost()
                        .xsmall()
                        .text_color(t.muted_foreground)
                        .label("Decline")
                        .disabled(busy)
                        .on_click(move |_, _, cx| {
                            let _ = decline_view.update(cx, |w, cx| w.answer_secret(&decline_card, false, cx));
                        }),
                ),
        )
        .into_any_element()
}

/// `SecretSourceSelect`: where the answer comes from, the typed value or a secret provider.
fn source_picker(
    card: &Rc<RequestCard>,
    label: &SharedString,
    source: Option<&SecretSource>,
    sources: &[SecretSource],
    workspace: &Entity<Workspace>,
    t: &Tokens,
) -> AnyElement {
    let icon = match source {
        Some(_) => Icon::empty().path("icons/app/onepassword.svg"),
        None => Icon::new(IconName::Type),
    };
    let title: SharedString = source.map_or_else(|| "Value".into(), |s| s.label.clone());
    let (view, item_id, sources) = (workspace.downgrade(), card.item_id.clone(), sources.to_vec());
    Button::new("secret-source")
        .outline()
        .small()
        .icon(icon.size(px(14.)).text_color(t.muted_foreground))
        .tooltip(format!("{label} source: {title}"))
        .dropdown_menu(move |menu, _, _| {
            let pick = |source: Option<SharedString>| {
                let (view, item_id) = (view.clone(), item_id.clone());
                move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
                    let _ = view.update(cx, |w, cx| w.set_secret_source(&item_id, source.clone(), cx));
                }
            };
            let entry = |icon: Icon, text: SharedString| {
                PopupMenuItem::element(move |_, _| {
                    h_flex().gap(px(8.)).child(icon.clone().size(px(14.))).child(text.clone())
                })
            };
            let mut menu = menu.item(entry(Icon::new(IconName::Type), "Value".into()).on_click(pick(None)));
            for source in &sources {
                let item = entry(Icon::empty().path("icons/app/onepassword.svg"), source.label.clone());
                menu = menu.item(item.on_click(pick(Some(source.id.clone()))));
            }
            menu
        })
        .into_any_element()
}

fn link_card(card: &Rc<RequestCard>, workspace: &Entity<Workspace>, t: &Tokens, cx: &mut App) -> AnyElement {
    let state = workspace.read(cx);
    let busy = state.requests.busy.contains(&card.item_id);
    let error = state.requests.errors.get(&card.item_id).cloned();
    let client = state.client();
    let RequestKind::Link { label, url, .. } = &card.kind else { return div().into_any_element() };
    let title = if card.picks_machine() {
        "Link a machine to this environment".to_owned()
    } else {
        format!("Link {} to this environment", card.link_target())
    };
    let (decline_card, review_card, view) = (card.clone(), card.clone(), workspace.downgrade());
    card_frame(t)
        .child(
            v_flex()
                .min_w_0()
                .gap(px(4.))
                .child(div().text_sm().font_weight(FontWeight::MEDIUM).text_color(t.foreground).child(title))
                .when(!card.reason.trim().is_empty(), |el| {
                    el.child(div().text_sm().text_color(t.muted_foreground).child(card.reason.clone()))
                })
                .when_some(url.clone().filter(|_| label.is_some()), |el, url| {
                    el.child(div().truncate().text_xs().text_color(t.muted_foreground.opacity(0.8)).child(url))
                }),
        )
        .children(error.map(|e| div().text_xs().text_color(t.error).child(e)))
        .child(
            h_flex()
                .min_w_0()
                .justify_end()
                .gap(px(8.))
                .child(
                    Button::new("link-decline")
                        .ghost()
                        .xsmall()
                        .text_color(t.muted_foreground)
                        .label("Decline")
                        .disabled(busy)
                        .on_click(move |_, _, cx| {
                            let _ = view.update(cx, |w, cx| w.decline_link(&decline_card, cx));
                        }),
                )
                .child(
                    Button::new("link-review")
                        .primary()
                        .xsmall()
                        .label(if card.picks_machine() { "Choose machine…" } else { "Review link…" })
                        .disabled(busy)
                        .on_click(move |_, window, cx| open_link_dialog(&review_card, client.clone(), window, cx)),
                ),
        )
        .into_any_element()
}

/// `LinkRequestDialog`, single-environment: this client knows no other machines, so the
/// link goes to the address the agent gave or one the user types, with a pairing code.
struct LinkDialog {
    card: Rc<RequestCard>,
    address: Entity<InputState>,
    code: Entity<InputState>,
    /// The address must be typed: the agent named no URL.
    asks_address: bool,
    access: &'static str,
    /// `peerLinks.list` says this environment already links to the machine named.
    linked: bool,
    busy: bool,
    error: Option<SharedString>,
    client: Option<RpcClient>,
    _subscriptions: [Subscription; 2],
}

impl LinkDialog {
    fn ready(&self, cx: &App) -> bool {
        self.linked
            || ((!self.asks_address || !self.address.read(cx).value().trim().is_empty())
                && !self.code.read(cx).value().trim().is_empty())
    }

    /// `linkRequestAnswerInput`: use the existing link, or link the address with the code.
    fn answer(&self, cx: &App) -> Option<Value> {
        let RequestKind::Link { url, environment_id, .. } = &self.card.kind else { return None };
        let base =
            |answer: Value| json!({"threadId": self.card.thread_id, "turnItemId": self.card.item_id, "answer": answer});
        if let (true, Some(id)) = (self.linked, environment_id) {
            return Some(base(json!({"type": "use-existing", "environmentId": id})));
        }
        let code = self.code.read(cx).value().trim().to_owned();
        let address = if self.asks_address {
            Some(self.address.read(cx).value().trim().to_owned())
        } else {
            url.as_ref().map(|u| u.trim().to_owned())
        };
        let address = address.filter(|a| !a.is_empty())?;
        if code.is_empty() {
            return None;
        }
        let mut answer = json!({"type": "link", "urls": [address], "access": self.access, "pairingCode": code});
        if let Some(id) = environment_id {
            answer["environmentId"] = id.to_string().into();
            answer["label"] = self.card.link_target().into();
        }
        Some(base(answer))
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(client), Some(input)) = (self.client.clone(), self.answer(cx)) else { return };
        if self.busy {
            return;
        }
        self.busy = true;
        self.error = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("peerLinks.answerRequest", input).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    // The card switches to its answered row once the item updates.
                    Ok(_) => window.close_dialog(cx),
                    Err(error) => this.error = Some(failure_message(&error, &LINK_TAGS)),
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for LinkDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *theme::tokens(cx);
        let busy = self.busy;
        let target = self.card.link_target();
        let field = |label: &'static str, input: &Entity<InputState>, help: Option<String>| {
            v_flex()
                .gap(px(6.))
                .child(div().text_xs().font_weight(FontWeight::MEDIUM).child(label))
                .child(Input::new(input).disabled(busy))
                .children(help.map(|h| div().text_xs().text_color(t.muted_foreground).child(h)))
        };
        let form = cx.entity().downgrade();
        let access_label = timeline::access_label(self.access);
        let picker = Button::new("link-request-access")
            .outline()
            .small()
            .w_full()
            .label(access_label)
            .disabled(busy)
            .dropdown_menu(move |mut menu, _, _| {
                for (value, description) in ACCESS {
                    let form = form.clone();
                    menu = menu.item(
                        PopupMenuItem::element(move |_, cx| {
                            let t = theme::tokens(cx);
                            v_flex()
                                .py(px(2.))
                                .max_w(px(360.))
                                .text_xs()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(FontWeight::MEDIUM)
                                        .child(timeline::access_label(value)),
                                )
                                .child(div().text_color(t.muted_foreground).child(description))
                        })
                        .on_click(move |_, _, cx| {
                            let _ = form.update(cx, |form, cx| {
                                form.access = value;
                                cx.notify();
                            });
                        }),
                    );
                }
                menu
            });
        let url = match &self.card.kind {
            RequestKind::Link { url: Some(url), .. } if !self.asks_address && !self.card.picks_machine() => {
                Some(url.clone())
            }
            _ => None,
        };
        v_flex()
            .gap(px(16.))
            .children(url.map(|url| div().truncate().text_xs().text_color(t.muted_foreground).child(url)))
            .when(self.linked, |el| {
                el.child(div().text_xs().text_color(t.muted_foreground).child(format!(
                    "This environment is already linked to {target}. The agent can use that link as it is; change its access in Settings → Connections."
                )))
            })
            .when(!self.linked, |el| {
                el.when(self.asks_address, |el| el.child(field("Address", &self.address, None)))
                    .child(field(
                        "Pairing code",
                        &self.code,
                        Some(format!("Create a link on {target}: Settings → Connections → Create link, then paste its code here.")),
                    ))
                    .child(
                        v_flex()
                            .gap(px(6.))
                            .child(div().text_xs().font_weight(FontWeight::MEDIUM).child("What agents here may do there"))
                            .child(picker),
                    )
            })
            .children(self.error.clone().map(|e| div().text_xs().text_color(t.error).child(e)))
            .child(
                h_flex()
                    .justify_end()
                    .gap(px(8.))
                    .child(Button::new("link-request-cancel").outline().label("Cancel").disabled(busy).on_click(|_, window, cx| window.close_dialog(cx)))
                    .child(
                        Button::new("link-request-submit")
                            .primary()
                            .label(if busy { "Linking…" } else if self.linked { "Use this link" } else { "Link" })
                            .disabled(busy || !self.ready(cx))
                            .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
                    ),
            )
    }
}

fn open_link_dialog(card: &Rc<RequestCard>, client: Option<RpcClient>, window: &mut Window, cx: &mut App) {
    let RequestKind::Link { url, environment_id, requested_access, .. } = &card.kind else { return };
    let address = cx.new(|cx| {
        let state = InputState::new(window, cx).placeholder("https://machine.tailnet.ts.net");
        match url {
            Some(url) => state.default_value(url.clone()),
            None => state,
        }
    });
    let code = cx.new(|cx| InputState::new(window, cx).placeholder("Paste the code"));
    let asks_address = url.is_none();
    if asks_address { &address } else { &code }.update(cx, |input, cx| input.focus(window, cx));
    let access = ACCESS
        .iter()
        .map(|(value, _)| *value)
        .find(|value| requested_access.as_deref() == Some(*value))
        .unwrap_or("approval-required");
    let dialog = cx.new(|cx| {
        let on_input = |this: &mut LinkDialog,
                        _: &Entity<InputState>,
                        event: &InputEvent,
                        window: &mut Window,
                        cx: &mut Context<LinkDialog>| match event {
            InputEvent::PressEnter { .. } if this.ready(cx) => this.submit(window, cx),
            InputEvent::Change => cx.notify(),
            _ => {}
        };
        LinkDialog {
            _subscriptions: [cx.subscribe_in(&address, window, on_input), cx.subscribe_in(&code, window, on_input)],
            card: card.clone(),
            address,
            code,
            asks_address,
            access,
            linked: false,
            busy: false,
            error: None,
            client: client.clone(),
        }
    });
    // A machine this environment already links to is answered with that link.
    if let (Some(client), Some(id)) = (client, environment_id.clone()) {
        let weak = dialog.downgrade();
        cx.spawn(async move |cx| {
            let Ok(links) = client.request("peerLinks.list", json!({})).await else { return };
            let linked = links["links"].as_array().or(links.as_array()).is_some_and(|links| {
                links.iter().any(|l| l["environmentId"].as_str() == Some(id.as_ref()) && l["status"] != "expired")
            });
            let _ = weak.update(cx, |dialog, cx| {
                dialog.linked = linked;
                cx.notify();
            });
        })
        .detach();
    }
    let title: SharedString =
        if card.picks_machine() { "Link a machine".into() } else { format!("Link {}", card.link_target()).into() };
    window.open_dialog(cx, move |d, _, cx| {
        let t = theme::tokens(cx);
        d.entrance(DialogEntrance::None)
            .title(title.clone())
            .w(px(448.))
            .child(
                div()
                    .mb(px(16.))
                    .text_sm()
                    .text_color(t.muted_foreground)
                    .child("Agents here can then launch, message and wait on threads there. Revoke it in that machine's Connections."),
            )
            .child(dialog.clone())
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_card_reads_only_from_one_password_providers_the_server_accepts() {
        let providers = json!({
            "work": {"kind": "one_password", "label": "Work", "config": {"account": "acme.1password.com"}},
            "broken": {"kind": "one_password", "label": "Broken", "config": {"account": "has space"}},
            "vault": {"kind": "vault", "label": "Vault", "config": {}},
        });
        assert_eq!(secret_sources(true, &providers), [SecretSource { id: "work".into(), label: "Work".into() }]);
        assert!(secret_sources(false, &providers).is_empty(), "older servers drop references");
    }

    #[test]
    fn references_follow_one_passwords_syntax() {
        for good in ["op://vault/item/field", " op://My Vault/item/section/field ", "op://v/i/f?attribute=otp"] {
            assert_eq!(
                secret_reference("work", good),
                Some(json!({"provider": "work", "reference": good.trim()})),
                "{good:?}"
            );
        }
        for bad in
            ["op://vault/item", "op://vault/ item/field", "vault/item/field", "op://v/i/f?Key=x", "op://a/b/c/d/e"]
        {
            assert_eq!(secret_reference("work", bad), None, "{bad:?}");
        }
    }
}
