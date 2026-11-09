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
    AnyElement, App, AppContext as _, Context, Entity, FocusHandle, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, WeakEntity,
    Window, div, prelude::FluentBuilder as _, px, svg,
};
use serde_json::{Value, json};

use super::Workspace;
use super::pages::settings::connections::linking::{
    ANOTHER_ADDRESS, LinkableMachine, initial_machine, machine_badge, mint_link_code, rank_machines_by_hint,
};
use super::pages::settings::machine_icon;
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
        let Some(host) = self.thread_environment(&card.thread_id) else { return };
        let Some(client) = self.env_client(&host).filter(|_| self.manages_access(&host)) else { return };
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
    let host = state.thread_environment(&card.thread_id);
    let can_answer = host.as_deref().is_some_and(|host| state.manages_access(host));
    let RequestKind::Link { label, url, .. } = &card.kind else { return div().into_any_element() };
    let title = if card.picks_machine() {
        "Link a machine to this environment".to_owned()
    } else {
        format!("Link {} to this environment", card.link_target())
    };
    let (decline_card, review_card, view) = (card.clone(), card.clone(), workspace.downgrade());
    let review_view = view.clone();
    let actions = if can_answer {
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
                    .on_click(move |_, window, cx| open_link_dialog(&review_card, &review_view, window, cx)),
            )
            .into_any_element()
    } else {
        div()
            .text_xs()
            .text_color(t.muted_foreground)
            .child("Only a session that manages access here can answer this.")
            .into_any_element()
    };
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
        .child(actions)
        .into_any_element()
}

/// `DialogTarget`: a machine from this client's catalog, an address, or nothing picked yet.
/// A machine's `urls` is empty when this client knows no direct address for it.
#[derive(Clone, Debug, PartialEq, Eq)]
enum LinkTarget {
    None,
    Machine { environment_id: String, label: String, urls: Vec<String>, linked: bool },
    Address,
}

/// `resolveTarget`: what the dialog links, from the request and the user's pick.
fn resolve_target(card: &RequestCard, machines: &[LinkableMachine], choice: Option<&str>) -> LinkTarget {
    let RequestKind::Link { url, environment_id, .. } = &card.kind else { return LinkTarget::None };
    if environment_id.is_none() && url.is_some() {
        return LinkTarget::Address;
    }
    let Some(chosen) = environment_id.as_deref().or(choice) else { return LinkTarget::None };
    if chosen == ANOTHER_ADDRESS {
        return LinkTarget::Address;
    }
    let machine = machines.iter().find(|machine| machine.environment_id == chosen);
    let mut urls = machine.map(|m| m.urls.clone()).unwrap_or_default();
    if let Some(url) = url.as_deref().filter(|url| !urls.iter().any(|u| u == url)) {
        urls.push(url.to_owned());
    }
    LinkTarget::Machine {
        environment_id: machine.map_or_else(|| chosen.to_owned(), |m| m.environment_id.clone()),
        label: machine.map_or_else(|| card.link_target(), |m| m.label.clone()),
        urls,
        linked: machine.is_some_and(|m| m.linked),
    }
}

/// The dialog's steps: where to link, then what agents may do there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LinkStep {
    Machine,
    Access,
}

/// What the dialog shows and allows for a target, before any pairing code is known.
#[derive(Clone, Debug, PartialEq, Eq)]
struct LinkPlan {
    target: LinkTarget,
    /// The machine's own name, else the request's.
    target_name: String,
    needs_address: bool,
    /// The address must be typed: the agent gave none for the target.
    asks_address: bool,
    /// The environment links to the machine already, so that link is used as it is.
    use_existing: bool,
    /// The machine step comes first: the machine is the user's pick or needs an address.
    two_steps: bool,
    machine_ready: bool,
}

fn link_plan(card: &RequestCard, machines: &[LinkableMachine], selected: Option<&str>, address: &str) -> LinkPlan {
    let target = resolve_target(card, machines, selected);
    let given_url = matches!(&card.kind, RequestKind::Link { url: Some(_), environment_id: None, .. });
    let target_name = match &target {
        LinkTarget::Machine { label, .. } => label.clone(),
        _ => card.link_target(),
    };
    let needs_address = match &target {
        LinkTarget::Address => true,
        LinkTarget::Machine { urls, .. } => urls.is_empty(),
        LinkTarget::None => false,
    };
    let asks_address = needs_address && !given_url;
    let use_existing = matches!(target, LinkTarget::Machine { linked: true, .. });
    let machine_ready = target != LinkTarget::None && (use_existing || !needs_address || !address.trim().is_empty());
    LinkPlan {
        two_steps: card.picks_machine() || asks_address,
        target,
        target_name,
        needs_address,
        asks_address,
        use_existing,
        machine_ready,
    }
}

/// `linkRequestAnswerInput`: use the existing link, or link the target with the code. `None`
/// while the answer is incomplete, since the server rejects it.
fn link_answer(card: &RequestCard, plan: &LinkPlan, address: &str, access: &str, code: &str) -> Option<Value> {
    let base = |answer: Value| json!({"threadId": card.thread_id, "turnItemId": card.item_id, "answer": answer});
    if let LinkTarget::Machine { environment_id, linked: true, .. } = &plan.target {
        return Some(base(json!({"type": "use-existing", "environmentId": environment_id})));
    }
    let code = code.trim();
    let address = address.trim();
    if code.is_empty() {
        return None;
    }
    let answer = match &plan.target {
        LinkTarget::None => return None,
        LinkTarget::Address if address.is_empty() => return None,
        LinkTarget::Address => json!({"type": "link", "urls": [address], "access": access, "pairingCode": code}),
        LinkTarget::Machine { environment_id, label, urls, .. } => {
            let urls: Vec<&str> =
                if plan.needs_address { vec![address] } else { urls.iter().map(String::as_str).collect() };
            if urls.first().is_none_or(|url| url.is_empty()) {
                return None;
            }
            json!({
                "type": "link",
                "environmentId": environment_id,
                "label": label,
                "urls": urls,
                "access": access,
                "pairingCode": code,
            })
        }
    };
    Some(base(answer))
}

/// `LinkRequestDialog`: the machine (picked, or the one the agent named), its address when
/// this client knows none, then the access and, unless one can be minted there, a pairing code.
struct LinkDialog {
    card: Rc<RequestCard>,
    workspace: WeakEntity<Workspace>,
    /// The environment the request was made in, which the link is made from.
    host: SharedString,
    client: Option<RpcClient>,
    /// `peerLinks.list` on the host, for the machines it links to already.
    links: Vec<Value>,
    /// `None` until the user picks; until then the hint's best match is picked.
    choice: Option<String>,
    address: Entity<InputState>,
    code: Entity<InputState>,
    access: &'static str,
    step: LinkStep,
    busy: bool,
    error: Option<SharedString>,
    /// Machines whose minting failed, so the dialog asks for a code instead.
    mint_failed: HashSet<String>,
    /// Each machine option's focus, for the arrow keys.
    options: HashMap<String, FocusHandle>,
    _subscriptions: Vec<Subscription>,
}

impl LinkDialog {
    fn machines(&self, cx: &App) -> Vec<LinkableMachine> {
        let Some(workspace) = self.workspace.upgrade() else { return Vec::new() };
        let RequestKind::Link { hint, .. } = &self.card.kind else { return Vec::new() };
        let listed = workspace.read(cx).linkable_machines(&self.host, &self.links, cx);
        rank_machines_by_hint(listed, hint.as_deref())
    }

    fn selected(&self, machines: &[LinkableMachine]) -> Option<String> {
        let RequestKind::Link { environment_id, hint, .. } = &self.card.kind else { return None };
        self.choice.clone().or_else(|| initial_machine(environment_id.as_deref(), hint.as_deref(), machines))
    }

    fn plan(&self, machines: &[LinkableMachine], cx: &App) -> LinkPlan {
        let selected = self.selected(machines);
        link_plan(&self.card, machines, selected.as_deref(), &self.address.read(cx).value())
    }

    /// Where a code can be minted for the target, unless minting there failed already.
    fn mint_endpoint(&self, plan: &LinkPlan, cx: &App) -> Option<crate::backend::Endpoint> {
        let LinkTarget::Machine { environment_id, .. } = &plan.target else { return None };
        if self.mint_failed.contains(environment_id) {
            return None;
        }
        self.workspace.upgrade()?.read(cx).mint_endpoint(environment_id)
    }

    fn ready(&self, plan: &LinkPlan, cx: &App) -> bool {
        plan.machine_ready
            && (plan.use_existing
                || self.mint_endpoint(plan, cx).is_some()
                || !self.code.read(cx).value().trim().is_empty())
    }

    fn current_step(&self, plan: &LinkPlan) -> LinkStep {
        if plan.two_steps { self.step } else { LinkStep::Access }
    }

    /// The machine step ends the dialog when the machine is linked here already.
    fn finishes_here(&self, plan: &LinkPlan) -> bool {
        self.current_step(plan) == LinkStep::Access || plan.use_existing
    }

    fn pick(&mut self, choice: String, cx: &mut Context<Self>) {
        self.choice = Some(choice);
        self.error = None;
        cx.notify();
    }

    fn set_step(&mut self, step: LinkStep, window: &mut Window, cx: &mut Context<Self>) {
        self.step = step;
        self.error = None;
        if step == LinkStep::Access {
            self.code.update(cx, |input, cx| input.focus(window, cx));
        }
        cx.notify();
    }

    /// Enter or the primary button: the next step, or the answer.
    fn advance(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let machines = self.machines(cx);
        let plan = self.plan(&machines, cx);
        if self.finishes_here(&plan) {
            self.submit(&plan, window, cx);
        } else if plan.machine_ready {
            self.set_step(LinkStep::Access, window, cx);
        }
    }

    fn submit(&mut self, plan: &LinkPlan, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else { return };
        if self.busy || !self.ready(plan, cx) {
            return;
        }
        let mint = (!plan.use_existing).then(|| self.mint_endpoint(plan, cx)).flatten();
        let target_id = match &plan.target {
            LinkTarget::Machine { environment_id, .. } => Some(environment_id.clone()),
            _ => None,
        };
        let address = self.address.read(cx).value().to_string();
        let typed = self.code.read(cx).value().to_string();
        let access = self.access;
        let card = self.card.clone();
        let plan = plan.clone();
        self.busy = true;
        self.error = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let code = match mint {
                Some(endpoint) => match mint_link_code(endpoint, access).await {
                    Ok(code) => code,
                    Err(message) => {
                        let _ = this.update(cx, |this, cx| {
                            this.busy = false;
                            this.mint_failed.extend(target_id);
                            this.error = Some(message.into());
                            cx.notify();
                        });
                        return;
                    }
                },
                None => typed,
            };
            let Some(input) = link_answer(&card, &plan, &address, access, &code) else {
                let _ = this.update(cx, |this, cx| {
                    this.busy = false;
                    cx.notify();
                });
                return;
            };
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

    fn title(&self, cx: &App) -> SharedString {
        if self.card.picks_machine() {
            return "Link a machine".into();
        }
        let machines = self.machines(cx);
        format!("Link {}", self.plan(&machines, cx).target_name).into()
    }

    /// Up and Down move the machine pick and its focus, as a radio group's arrows do.
    fn move_pick(&mut self, ids: &[String], forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let machines = self.machines(cx);
        let current = self.selected(&machines);
        let len = ids.len();
        let next = match current.and_then(|id| ids.iter().position(|candidate| *candidate == id)) {
            Some(i) if forward => (i + 1) % len,
            Some(i) => (i + len - 1) % len,
            None => 0,
        };
        let Some(id) = ids.get(next).cloned() else { return };
        if let Some(handle) = self.options.get(&id) {
            handle.focus(window, cx);
        }
        self.pick(id, cx);
    }

    fn option_focus(&mut self, id: &str, cx: &mut Context<Self>) -> FocusHandle {
        self.options.entry(id.to_owned()).or_insert_with(|| cx.focus_handle().tab_stop(true)).clone()
    }

    /// `MachinePicker`: the user's other machines, best match for the hint first, then an address.
    fn machine_picker(
        &mut self,
        machines: &[LinkableMachine],
        selected: Option<&str>,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let busy = self.busy;
        let ids: Rc<[String]> =
            machines.iter().map(|m| m.environment_id.clone()).chain([ANOTHER_ADDRESS.to_owned()]).collect();
        let mut rows = Vec::new();
        for (index, id) in ids.iter().enumerate() {
            let machine = machines.get(index);
            let checked = selected == Some(id.as_str());
            let focus = self.option_focus(id, cx);
            let ring = t.ring;
            let radio = div()
                .size(px(16.))
                .flex_none()
                .rounded_full()
                .border_1()
                .border_color(if checked { t.primary } else { t.input })
                .flex()
                .items_center()
                .justify_center()
                .when(checked, |el| el.bg(t.primary).child(div().size(px(6.)).rounded_full().bg(t.primary_foreground)));
            let (pick_id, move_ids) = (id.clone(), ids.clone());
            let row = h_flex()
                .id(SharedString::from(format!("link-machine-{id}")))
                .track_focus(&focus)
                .min_w_0()
                .gap(px(10.))
                .px(px(12.))
                .py(px(8.))
                .rounded(px(8.))
                .border_1()
                .border_color(t.border)
                .text_sm()
                .focus_visible(move |style| {
                    style.shadow(vec![gpui_kit::BoxShadow::new(px(0.), px(0.), ring).spread_radius(px(2.))])
                })
                .when(!busy, |el| {
                    el.cursor_pointer()
                        .on_click(cx.listener(move |this, _, _, cx| this.pick(pick_id.clone(), cx)))
                        .on_key_down(cx.listener(move |this, event: &gpui_kit::KeyDownEvent, window, cx| {
                            let forward = match event.keystroke.key.as_str() {
                                "down" | "right" => true,
                                "up" | "left" => false,
                                _ => return,
                            };
                            cx.stop_propagation();
                            this.move_pick(&move_ids, forward, window, cx);
                        }))
                })
                .when(busy, |el| el.opacity(0.64))
                .child(radio)
                .map(|el| match machine {
                    Some(machine) => el
                        .child(
                            svg()
                                .path(machine_icon(&machine.machine))
                                .size(px(16.))
                                .flex_none()
                                .text_color(t.muted_foreground),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .font_weight(FontWeight::MEDIUM)
                                .child(machine.label.clone()),
                        )
                        .children(machine_badge(machine, t)),
                    None => {
                        el.child(div().flex_1().min_w_0().font_weight(FontWeight::MEDIUM).child("Another address…"))
                    }
                });
            rows.push(row);
        }
        v_flex()
            .gap(px(8.))
            .child(div().text_xs().font_weight(FontWeight::MEDIUM).child("Machine"))
            .children(rows)
            .into_any_element()
    }

    fn steps(&self, plan: &LinkPlan, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let current = self.current_step(plan);
        let summary = (plan.target != LinkTarget::None).then(|| plan.target_name.clone());
        let busy = self.busy;
        let machine_ready = plan.machine_ready;
        let frame = if t.dark { gpui_kit::hsla(0., 0., 1., 0.05) } else { gpui_kit::hsla(0., 0., 0., 0.05) };
        let ring = t.ring;
        h_flex()
            .gap(px(4.))
            .p(px(4.))
            .rounded(px(12.))
            .bg(if t.dark { gpui_kit::hsla(0., 0., 1., 0.04) } else { t.background })
            .border_1()
            .border_color(frame)
            .children([(LinkStep::Machine, "Machine", 0usize), (LinkStep::Access, "Access", 1)].map(
                |(step, label, index)| {
                    let is_current = step == current;
                    let done = current == LinkStep::Access && step == LinkStep::Machine;
                    let disabled = busy || (step == LinkStep::Access && !machine_ready);
                    let text = match (&summary, done) {
                        (Some(summary), true) => format!("{label}: {summary}"),
                        _ => label.to_owned(),
                    };
                    let badge = div()
                        .size(px(20.))
                        .flex_none()
                        .rounded_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .border_1()
                        .map(|badge| {
                            if done {
                                badge
                                    .bg(t.primary)
                                    .border_color(t.primary)
                                    .child(Icon::new(IconName::Check).size(px(14.)).text_color(t.primary_foreground))
                            } else if is_current {
                                badge
                                    .bg(t.primary.opacity(0.1))
                                    .border_color(t.primary.opacity(0.3))
                                    .text_color(t.primary)
                                    .child((index + 1).to_string())
                            } else {
                                badge
                                    .bg(t.card)
                                    .border_color(frame)
                                    .text_color(t.muted_foreground)
                                    .child((index + 1).to_string())
                            }
                        });
                    h_flex()
                        .id(SharedString::from(format!("link-step-{label}")))
                        .flex_1()
                        .min_w_0()
                        .gap(px(8.))
                        .px(px(10.))
                        .py(px(8.))
                        .rounded(px(8.))
                        .when(is_current, |el| el.bg(t.card).border_1().border_color(frame))
                        .when(!disabled, |el| {
                            el.cursor_pointer()
                                .when(!is_current, |el| el.hover(|style| style.bg(t.card)))
                                .tab_index(0)
                                .focus_visible(move |style| {
                                    style.shadow(vec![
                                        gpui_kit::BoxShadow::new(px(0.), px(0.), ring).spread_radius(px(2.)),
                                    ])
                                })
                                .on_click(cx.listener(move |this, _, window, cx| this.set_step(step, window, cx)))
                        })
                        .child(badge)
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(if is_current { t.foreground } else { t.muted_foreground })
                                .child(text),
                        )
                },
            ))
            .into_any_element()
    }

    fn access_picker(&self, cx: &mut Context<Self>) -> AnyElement {
        let form = cx.entity().downgrade();
        let t = theme::tokens(cx);
        let description = ACCESS.iter().find(|(value, _)| *value == self.access).map_or("", |(_, d)| *d);
        let picker = Button::new("link-request-access")
            .outline()
            .small()
            .w_full()
            .label(timeline::access_label(self.access))
            .disabled(self.busy)
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
        v_flex()
            .gap(px(6.))
            .child(div().text_xs().font_weight(FontWeight::MEDIUM).child("What agents here may do there"))
            .child(picker)
            .child(div().text_xs().text_color(t.muted_foreground).child(format!(
                "{description} An agent never gets more access there than its own mode here. Below full access, it can change only threads this link started, not that machine's own threads, projects or settings."
            )))
            .into_any_element()
    }
}

impl Render for LinkDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *theme::tokens(cx);
        let busy = self.busy;
        let machines = self.machines(cx);
        let selected = self.selected(&machines);
        let plan = self.plan(&machines, cx);
        let current = self.current_step(&plan);
        let can_mint = self.mint_endpoint(&plan, cx).is_some();
        let ready = self.ready(&plan, cx);
        let finishes_here = self.finishes_here(&plan);
        let field = |label: &'static str, input: &Entity<InputState>, help: Option<String>| {
            v_flex()
                .gap(px(6.))
                .child(div().text_xs().font_weight(FontWeight::MEDIUM).child(label))
                .child(Input::new(input).disabled(busy))
                .children(help.map(|h| div().text_xs().text_color(t.muted_foreground).child(h)))
        };
        let mut body: Vec<AnyElement> = Vec::new();
        if current == LinkStep::Machine {
            if self.card.picks_machine() {
                body.push(self.machine_picker(&machines, selected.as_deref(), &t, cx));
            }
            if plan.asks_address {
                let help = match &plan.target {
                    LinkTarget::Machine { label, .. } => Some(format!(
                        "This app has no address for {label} that another machine can reach. Enter its https or Tailscale address."
                    )),
                    _ => None,
                };
                body.push(field("Address", &self.address, help).into_any_element());
            }
        } else {
            if let RequestKind::Link { url: Some(url), .. } = &self.card.kind
                && !self.card.picks_machine()
            {
                body.push(
                    div().truncate().text_xs().text_color(t.muted_foreground).child(url.clone()).into_any_element(),
                );
            }
            if !plan.use_existing {
                if !can_mint {
                    let help = format!(
                        "Create a link on {}: Settings → Connections → Create link, then paste its code here.",
                        plan.target_name
                    );
                    body.push(field("Pairing code", &self.code, Some(help)).into_any_element());
                }
                body.push(self.access_picker(cx));
            }
        }
        if plan.use_existing {
            body.push(
                div()
                    .text_xs()
                    .text_color(t.muted_foreground)
                    .child(format!(
                        "This environment is already linked to {}. The agent can use that link as it is; change its access in Settings → Connections.",
                        plan.target_name
                    ))
                    .into_any_element(),
            );
        }
        if let Some(error) = self.error.clone() {
            body.push(div().text_xs().text_color(t.error).child(error).into_any_element());
        }
        let back = current == LinkStep::Access && plan.two_steps;
        let secondary = if back {
            Button::new("link-request-back")
                .outline()
                .label("Back")
                .disabled(busy)
                .on_click(cx.listener(|this, _, window, cx| this.set_step(LinkStep::Machine, window, cx)))
        } else {
            Button::new("link-request-cancel")
                .outline()
                .label("Cancel")
                .disabled(busy)
                .on_click(|_, window, cx| window.close_dialog(cx))
        };
        let label = if busy {
            "Linking…"
        } else if plan.use_existing {
            "Use this link"
        } else if finishes_here {
            "Link"
        } else {
            "Continue"
        };
        let enabled = if finishes_here { ready } else { plan.machine_ready };
        v_flex()
            .gap(px(16.))
            .when(plan.two_steps && !plan.use_existing, |el| el.child(self.steps(&plan, &t, cx)))
            .children(body)
            .child(
                h_flex().justify_end().gap(px(8.)).child(secondary).child(
                    Button::new("link-request-submit")
                        .primary()
                        .label(label)
                        .disabled(busy || !enabled)
                        .on_click(cx.listener(|this, _, window, cx| this.advance(window, cx))),
                ),
            )
    }
}

fn open_link_dialog(card: &Rc<RequestCard>, workspace: &WeakEntity<Workspace>, window: &mut Window, cx: &mut App) {
    let Some(entity) = workspace.upgrade() else { return };
    let RequestKind::Link { url, requested_access, .. } = &card.kind else { return };
    let (host, client) = {
        let state = entity.read(cx);
        let Some(host) = state.thread_environment(&card.thread_id) else { return };
        let client = state.env_client(&host);
        (host, client)
    };
    let address = cx.new(|cx| {
        let state = InputState::new(window, cx).placeholder("https://machine.tailnet.ts.net");
        match url {
            Some(url) => state.default_value(url.clone()),
            None => state,
        }
    });
    let code = cx.new(|cx| InputState::new(window, cx).placeholder("Paste the code"));
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
            InputEvent::PressEnter { .. } => this.advance(window, cx),
            InputEvent::Change => cx.notify(),
            _ => {}
        };
        LinkDialog {
            _subscriptions: vec![
                cx.subscribe_in(&address, window, on_input),
                cx.subscribe_in(&code, window, on_input),
                // Machines connect, disconnect and change their grants while the dialog is open.
                cx.observe(&entity, |_, _, cx| cx.notify()),
            ],
            card: card.clone(),
            workspace: workspace.clone(),
            host: host.clone(),
            client: client.clone(),
            links: Vec::new(),
            choice: None,
            address,
            code,
            access,
            step: LinkStep::Machine,
            busy: false,
            error: None,
            mint_failed: HashSet::new(),
            options: HashMap::new(),
        }
    });
    // The machines the host links to already are answered with that link.
    if let Some(client) = client {
        let weak = dialog.downgrade();
        cx.spawn(async move |cx| {
            let Ok(links) = client.request("peerLinks.list", json!({})).await else { return };
            let links = links["links"].as_array().cloned().unwrap_or_default();
            let _ = weak.update(cx, |dialog, cx| {
                dialog.links = links;
                cx.notify();
            });
        })
        .detach();
    }
    // The first field to fill: the address when it must be typed, else the code.
    dialog.update(cx, |dialog, cx| {
        let machines = dialog.machines(cx);
        let plan = dialog.plan(&machines, cx);
        if plan.asks_address && !dialog.card.picks_machine() {
            dialog.address.update(cx, |input, cx| input.focus(window, cx));
        } else if !plan.two_steps {
            dialog.code.update(cx, |input, cx| input.focus(window, cx));
        }
    });
    window.open_dialog(cx, move |d, _, cx| {
        let t = theme::tokens(cx);
        let title = dialog.read(cx).title(cx);
        d.entrance(DialogEntrance::None)
            .title(title)
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

    fn link_request(environment_id: Option<&str>, url: Option<&str>, label: Option<&str>) -> RequestCard {
        RequestCard {
            item_id: "item".into(),
            thread_id: "thread".into(),
            reason: SharedString::default(),
            kind: RequestKind::Link {
                label: label.map(SharedString::from),
                url: url.map(SharedString::from),
                environment_id: environment_id.map(SharedString::from),
                requested_access: None,
                hint: None,
            },
        }
    }

    fn machine(id: &str, urls: &[&str], linked: bool) -> LinkableMachine {
        LinkableMachine {
            environment_id: id.into(),
            label: format!("Machine {id}"),
            machine: "server".into(),
            connected: true,
            linked,
            urls: urls.iter().map(|u| (*u).to_owned()).collect(),
        }
    }

    #[test]
    fn a_picked_machine_with_an_address_links_over_its_urls() {
        let card = link_request(None, None, None);
        let machines = [machine("vps", &["https://vps.ts.net/", "http://10.0.0.2:3773"], false)];
        let none = link_plan(&card, &machines, None, "");
        assert_eq!((none.target, none.two_steps, none.machine_ready), (LinkTarget::None, true, false));
        let plan = link_plan(&card, &machines, Some("vps"), "");
        assert!(plan.machine_ready && !plan.needs_address && plan.two_steps);
        assert_eq!(plan.target_name, "Machine vps");
        assert_eq!(link_answer(&card, &plan, "", "auto", "  "), None, "a blank code is never sent");
        assert_eq!(
            link_answer(&card, &plan, "", "auto", " CODE "),
            Some(json!({"threadId": "thread", "turnItemId": "item", "answer": {
                "type": "link", "environmentId": "vps", "label": "Machine vps",
                "urls": ["https://vps.ts.net/", "http://10.0.0.2:3773"], "access": "auto", "pairingCode": "CODE",
            }}))
        );
    }

    #[test]
    fn a_machine_without_an_address_asks_for_one_and_a_linked_one_is_reused() {
        let card = link_request(None, None, None);
        let machines = [machine("ssh-only", &[], false), machine("linked", &["https://l/"], true)];
        let plan = link_plan(&card, &machines, Some("ssh-only"), " ");
        assert!(plan.asks_address && !plan.machine_ready, "an SSH-only machine needs an address");
        let typed = link_plan(&card, &machines, Some("ssh-only"), "https://box.ts.net");
        let answer = link_answer(&card, &typed, "https://box.ts.net", "read-only", "CODE").expect("complete");
        assert_eq!(answer["answer"]["urls"], json!(["https://box.ts.net"]));
        let reused = link_plan(&card, &machines, Some("linked"), "");
        assert!(reused.use_existing && reused.machine_ready);
        assert_eq!(
            link_answer(&card, &reused, "", "auto", "").map(|a| a["answer"].clone()),
            Some(json!({"type": "use-existing", "environmentId": "linked"}))
        );
    }

    #[test]
    fn a_named_machine_or_address_skips_the_machine_step() {
        let machines = [machine("vps", &["https://vps.ts.net/"], false)];
        let named = link_request(Some("vps"), Some("https://agent.example/"), Some("VPS"));
        let plan = link_plan(&named, &machines, None, "https://agent.example/");
        assert!(!plan.two_steps, "a named machine with an address has nothing to pick");
        assert!(matches!(&plan.target, LinkTarget::Machine { urls, .. } if urls.len() == 2));
        let unknown = link_plan(&link_request(Some("gone"), None, Some("Old box")), &machines, None, "");
        assert_eq!(unknown.target_name, "Old box");
        assert!(unknown.asks_address && unknown.two_steps, "this client knows no address for it");
        let address = link_request(None, Some("https://agent.example/"), None);
        let plan = link_plan(&address, &machines, None, "https://agent.example/");
        assert_eq!(plan.target, LinkTarget::Address);
        assert!(!plan.asks_address && !plan.two_steps, "the agent's address is already filled in");
        let picked = link_plan(&link_request(None, None, None), &machines, Some(ANOTHER_ADDRESS), "");
        assert!(picked.target == LinkTarget::Address && picked.asks_address && !picked.machine_ready);
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
