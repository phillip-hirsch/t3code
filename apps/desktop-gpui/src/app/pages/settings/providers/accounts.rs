//! Provider sign-in on the Providers page: the managed Codex/ChatGPT setup
//! (`CodexSetupSection.tsx`, settings presentation), every other provider's
//! account row (`ProviderAuthenticationSection.tsx`, which is also Claude's),
//! the ChatGPT account picker, the add-ChatGPT-account wizard, the "Manage
//! usage" button and the one-time ChatGPT plan notice (`ChatGptWelcomeCoordinator`).
//!
//! Each section is a view keyed to its card, so its sign-in state and the
//! follow-up steps it started live and die with the card, like the web
//! component's state. The desktop shell catches ChatGPT's redirect itself
//! (`codex_callback`), as Electron does; the cross-environment handoff and
//! the remote-browser fallbacks are web-only and have no native counterpart.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::DialogEntrance;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::radio::Radio;
use gpui_kit::component::{Disableable as _, Icon, IconName, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, ClipboardItem, Context, Entity, FocusHandle, FontWeight, Global,
    InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, WeakEntity, Window, div, px,
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::codex_callback;

mod antigravity;
use crate::app::Workspace;
use crate::model::{Provider, ProviderAuth, ProviderSetup};
use crate::rpc::{RpcClient, RpcError, StreamEvent};
use crate::theme::{self, Tokens};

pub(in crate::app) const CHATGPT_USAGE_URL: &str = "https://chatgpt.com/#settings/Usage";
const MULTIPLE_ACCOUNTS_FAQ_URL: &str =
    "https://github.com/pingdotgg/t3code/blob/main/docs/user/faq.md#can-i-add-a-bunch-of-accounts-for-one-provider";
const CHANGE_ACCOUNT: &str = "chatgpt-change-account";
const TERMINAL_CHUNK: usize = 4_096;

// ---------------------------------------------------------------------------
// Wire shapes (`packages/contracts/src/providerSetup.ts`)

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct AuthState {
    phase: String,
    #[serde(default)]
    flow_id: Option<String>,
    #[serde(default)]
    authorization_url: Option<String>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    methods: Option<Vec<AuthMethod>>,
    #[serde(default)]
    interaction: Option<Interaction>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct AuthMethod {
    id: String,
    #[serde(default)]
    account_email: Option<String>,
    name: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
enum Interaction {
    Browser {
        id: String,
        url: String,
        requires_consent: bool,
        #[serde(default)]
        accepts_callback: Option<bool>,
    },
    DeviceCode {
        id: String,
        url: String,
        user_code: String,
    },
    Terminal {
        id: String,
        output: String,
    },
    Credentials {
        id: String,
        fields: Vec<CredentialField>,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
struct CredentialField {
    name: String,
    label: String,
    secret: bool,
}

impl Interaction {
    fn id(&self) -> &str {
        match self {
            Self::Browser { id, .. } | Self::DeviceCode { id, .. } | Self::Terminal { id, .. } => id,
            Self::Credentials { id, .. } => id,
            Self::Unknown => "",
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct InstallState {
    #[serde(default)]
    operation_id: Option<String>,
    phase: String,
    #[serde(default)]
    downloaded_bytes: u64,
    #[serde(default)]
    total_bytes: Option<u64>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    installed_version: Option<String>,
    #[serde(default)]
    executable_path: Option<String>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    can_remove: bool,
    #[serde(default)]
    message: Option<String>,
}

// ---------------------------------------------------------------------------
// Pure logic (`CodexSetupSection.logic.ts`)

/// `readCodexSetupMode`: only an explicit `"managed"` is managed.
pub(in crate::app) fn codex_setup_managed(config: &Value) -> bool {
    config.get("setupMode").and_then(Value::as_str) == Some("managed")
}

fn install_running(phase: &str) -> bool {
    matches!(phase, "downloading" | "extracting" | "verifying")
}

fn auth_running(phase: &str) -> bool {
    matches!(phase, "starting" | "waiting" | "verifying")
}

/// `settleProviderInstall`: whether the `started` install left a usable
/// runtime; `None` until settled. Only a state of the same operation settles a
/// running install, because the subscription can still carry the previous one.
fn settle_install(started: &InstallState, current: Option<&InstallState>) -> Option<bool> {
    let state = if install_running(&started.phase) {
        current.filter(|current| current.operation_id == started.operation_id)?
    } else {
        started
    };
    if install_running(&state.phase) {
        return None;
    }
    Some(state.phase != "failed" && state.phase != "cancelled" && state.installed_version.is_some())
}

/// `settleAuthorizationPage`: `Some(Some(url))` once `flow_id` waits on the
/// browser, `Some(None)` once it ends without one, `None` while it starts.
fn settle_authorization_page(state: Option<&AuthState>, flow_id: Option<&str>) -> Option<Option<String>> {
    let state = state?;
    let current = state.flow_id.as_deref()?;
    if flow_id.is_some_and(|flow_id| flow_id != current) {
        return None;
    }
    if matches!(state.phase.as_str(), "succeeded" | "failed" | "cancelled") {
        return Some(None);
    }
    if state.phase != "waiting" {
        return None;
    }
    browser_url(state).map(Some)
}

fn browser_url(state: &AuthState) -> Option<String> {
    match &state.interaction {
        Some(Interaction::Browser { url, .. }) => Some(url.clone()),
        _ => state.authorization_url.clone(),
    }
}

/// `usesChatGptSharing` in `packages/shared/src/usageLimits.ts`.
pub(in crate::app) fn uses_chatgpt_sharing(auth: &ProviderAuth) -> bool {
    auth.status == "authenticated" && auth.subscription_sharing == Some(true)
}

/// `redactedPlaceholder` in `RedactedSensitiveText.tsx`: a stable scramble that
/// keeps the address's shape.
fn redacted_placeholder(value: &str) -> String {
    const ALPHABET: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";
    let mut state: u32 = 0x811c_9dc5;
    for unit in value.encode_utf16() {
        state ^= u32::from(unit);
        state = state.wrapping_mul(0x0100_0193);
    }
    value
        .chars()
        .map(|c| {
            if matches!(c, '@' | '.' | '-' | '_') {
                return c;
            }
            state = (state ^ (state >> 13)).wrapping_mul(0x85eb_ca6b);
            state = (state ^ (state >> 16)).wrapping_mul(0xc2b2_ae35);
            ALPHABET[((state as i32).unsigned_abs() as usize) % ALPHABET.len()] as char
        })
        .collect()
}

/// Drops terminal control sequences so the sign-in output reads as text.
fn strip_ansi(output: &str) -> String {
    let mut text = String::with_capacity(output.len());
    let mut chars = output.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.next() {
                Some('[') => while chars.next().is_some_and(|c| !('@'..='~').contains(&c)) {},
                Some(']') => {
                    while let Some(c) = chars.next() {
                        if c == '\u{7}' || (c == '\u{1b}' && chars.next_if_eq(&'\\').is_some()) {
                            break;
                        }
                    }
                }
                _ => {}
            },
            '\r' if chars.peek() == Some(&'\n') => {}
            c if c.is_control() && c != '\n' && c != '\t' => {}
            c => text.push(c),
        }
    }
    text
}

/// What a key press sends to a terminal, as xterm encodes it.
fn terminal_input(event: &KeyDownEvent) -> Option<String> {
    let keystroke = &event.keystroke;
    let modifiers = &keystroke.modifiers;
    let named = match keystroke.key.as_str() {
        "enter" => Some("\r"),
        "backspace" => Some("\x7f"),
        "tab" => Some("\t"),
        "escape" => Some("\x1b"),
        "up" => Some("\x1b[A"),
        "down" => Some("\x1b[B"),
        "right" => Some("\x1b[C"),
        "left" => Some("\x1b[D"),
        "home" => Some("\x1b[H"),
        "end" => Some("\x1b[F"),
        "delete" => Some("\x1b[3~"),
        _ => None,
    };
    if let Some(named) = named {
        return Some(named.to_owned());
    }
    if modifiers.control && !modifiers.alt {
        let key = keystroke.key.chars().next().filter(|_| keystroke.key.len() == 1)?.to_ascii_lowercase();
        return key.is_ascii_lowercase().then(|| ((key as u8 - b'a' + 1) as char).to_string());
    }
    keystroke.key_char.clone()
}

// ---------------------------------------------------------------------------
// Shared presentation, a stand-in for the settings widgets until they land.

pub(super) fn settings_row(
    t: &Tokens,
    title: impl IntoElement,
    description: Option<AnyElement>,
    control: Option<AnyElement>,
) -> gpui_kit::Div {
    div().px_4().py_3().child(
        h_flex()
            .w_full()
            .gap_8()
            .items_center()
            .justify_between()
            .child(
                v_flex()
                    .min_w_0()
                    .flex_1()
                    .gap_1()
                    .child(
                        div()
                            .min_h(px(20.))
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(t.foreground)
                            .child(title),
                    )
                    .when_some(description, |el, description| {
                        el.child(div().text_xs().line_height(px(18.)).text_color(t.muted_foreground).child(description))
                    }),
            )
            .when_some(control, |el, control| el.child(h_flex().flex_none().gap_2().justify_end().child(control))),
    )
}

fn text(value: impl Into<SharedString>) -> AnyElement {
    div().child(value.into()).into_any_element()
}

pub(super) fn button(id: impl Into<gpui_kit::ElementId>, label: impl Into<SharedString>) -> Button {
    Button::new(id).small().label(label)
}

fn error_line(t: &Tokens, message: impl Into<SharedString>) -> gpui_kit::Div {
    div().px_4().py_2().text_xs().text_color(t.error_foreground).child(message.into())
}

/// The handler's message, the way `squashAtomCommandFailure(...).message` reads it.
pub(super) fn failure_message(error: &RpcError, fallback: &str) -> String {
    match error {
        RpcError::Failure(value) => value
            .get("detail")
            .or_else(|| value.get("message"))
            .and_then(Value::as_str)
            .filter(|message| !message.trim().is_empty())
            .unwrap_or(fallback)
            .to_owned(),
        _ => fallback.to_owned(),
    }
}

/// `ChatGptUsageButton`: opens ChatGPT's usage settings.
pub(in crate::app) fn chatgpt_usage_button(id: impl Into<gpui_kit::ElementId>, t: &Tokens) -> Button {
    Button::new(id)
        .ghost()
        .small()
        .text_color(t.muted_foreground)
        .child("Manage usage")
        .child(Icon::new(IconName::ExternalLink).size(px(14.)))
        .on_click(|_, _, cx| cx.open_url(CHATGPT_USAGE_URL))
}

/// `ChatGptConnectionButton`: the primary button with the OpenAI mark.
fn chatgpt_connection_button(id: impl Into<gpui_kit::ElementId>, label: impl Into<SharedString>) -> Button {
    Button::new(id)
        .primary()
        .small()
        .child(Icon::empty().path("icons/app/openai.svg").size(px(16.)))
        .child(label.into())
}

/// `RedactedSensitiveText`: the address scrambled until clicked.
fn redacted_email(id: &str, email: &str, revealed: bool, t: &Tokens) -> gpui_kit::Stateful<gpui_kit::Div> {
    div()
        .id(SharedString::from(format!("redacted-{id}")))
        .cursor_pointer()
        .font_family(t.fonts.mono)
        .text_size(px(10.))
        .text_color(if revealed { t.muted_foreground } else { t.muted_foreground.opacity(0.6) })
        .hover(|el| el.text_color(t.foreground))
        .child(if revealed { email.to_owned() } else { redacted_placeholder(email) })
        .tooltip(move |window, cx| {
            gpui_kit::component::tooltip::Tooltip::new(if revealed {
                "Click to hide email"
            } else {
                "Click to reveal email"
            })
            .build(window, cx)
        })
}

// ---------------------------------------------------------------------------
// Workspace entry points

impl Workspace {
    /// The account part of a provider instance's editor card: the managed Codex
    /// setup, the generic sign-in row, Cursor's API-key note, or nothing,
    /// as `ProviderSettingsPanel`'s `setup` slot picks them.
    pub(in crate::app) fn render_provider_account(
        &mut self,
        instance_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let provider = self.settings_info().provider(instance_id)?;
        let instance = &self.settings_info().settings["providerInstances"][instance_id];
        let driver = instance["driver"].as_str().unwrap_or(&provider.driver);
        let codex_managed = driver == "codex" && codex_setup_managed(&instance["config"]);
        let can_authenticate = provider.setup.as_ref().map(|setup| setup.can_authenticate);
        if !codex_managed && can_authenticate != Some(true) && !(provider.driver == "acpRegistry" && provider.installed)
        {
            if driver == "cursor" && can_authenticate == Some(false) {
                let t = theme::tokens(cx);
                return Some(
                    settings_row(
                        t,
                        "Cursor account",
                        Some(text(
                            "Using CURSOR_API_KEY. Remove it from this provider's environment to use browser sign-in.",
                        )),
                        None,
                    )
                    .into_any_element(),
                );
            }
            return None;
        }
        let (workspace, scope) = (cx.weak_entity(), self.settings_environment());
        let id = instance_id.to_owned();
        let key = format!("provider-account-{}-{instance_id}", scope.as_deref().unwrap_or_default());
        Some(
            window
                .use_keyed_state(SharedString::from(key), cx, move |_, _| {
                    AccountSection::new(workspace, scope, id, None)
                })
                .into_any_element(),
        )
    }

    /// `CodexManagedRuntimeFields`: the runtime paths of a managed Codex instance.
    pub(in crate::app) fn render_codex_runtime_fields(
        &mut self,
        instance_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (workspace, scope) = (cx.weak_entity(), self.settings_environment());
        let id = instance_id.to_owned();
        let key = format!("codex-runtime-{}-{instance_id}", scope.as_deref().unwrap_or_default());
        window
            .use_keyed_state(SharedString::from(key), cx, move |_, _| RuntimeFields {
                workspace,
                scope,
                instance_id: id,
                install: Live::default(),
            })
            .into_any_element()
    }

    /// `AddManagedCodexAccountDialog`: names a new managed Codex instance, then
    /// installs and signs it in. `on_created` takes over after creation instead.
    pub(in crate::app) fn open_add_codex_account_dialog(
        &mut self,
        on_created: Option<Rc<dyn Fn(String, String, &mut Window, &mut App)>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (workspace, scope) = (cx.weak_entity(), self.settings_environment());
        let dialog = cx.new(|cx| AddCodexAccount::new(workspace, scope, on_created, window, cx));
        window.open_dialog(cx, move |modal, window, cx| {
            let modal = modal.entrance(DialogEntrance::None);
            dialog.update(cx, |this, cx| this.render_dialog(modal, window, cx))
        });
    }

    /// Shows the ChatGPT plan notice once per connected sharing profile.
    pub(in crate::app) fn check_chatgpt_welcome(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let environment = self.settings_info().environment_label.clone().unwrap_or_else(|| "this environment".into());
        let next =
            self.settings_info().providers.iter().filter(|p| uses_chatgpt_sharing(&p.auth)).find_map(|provider| {
                let profile =
                    provider.auth.profile_id.as_deref().or(provider.auth.email.as_deref()).unwrap_or("default");
                let key = json!(["local", provider.instance_id, profile]).to_string();
                let welcome = cx.default_global::<Welcome>();
                welcome.load();
                (!welcome.acknowledged.contains(&key) && welcome.showing.is_none())
                    .then(|| (key, provider.display_name.clone().unwrap_or_else(|| "Codex".into())))
            });
        let Some((key, provider_name)) = next else { return };
        cx.global_mut::<Welcome>().showing = Some(key.clone());
        let detail: SharedString = format!("{provider_name} on {environment}").into();
        window.open_dialog(cx, move |modal, _, cx| {
            let modal = modal.entrance(DialogEntrance::None);
            let t = *theme::tokens(cx);
            let dismiss_key = key.clone();
            let close_key = key.clone();
            modal
                .w(px(448.))
                .on_close(move |_, _, cx| cx.global_mut::<Welcome>().acknowledge(&close_key))
                .child(
                    v_flex()
                        .gap_2()
                        .child(Icon::empty().path("icons/app/openai.svg").size(px(32.)).mb_2())
                        .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child("Your ChatGPT plan is connected"))
                        .child(div().text_sm().text_color(t.muted_foreground).child(
                            "Eligible usage in T3 Code uses your ChatGPT plan. Manage your shared usage and any credit settings in ChatGPT.",
                        ))
                        .child(div().text_xs().text_color(t.muted_foreground).child(detail.clone())),
                )
                .footer(h_flex().w_full().justify_end().gap_2().child(chatgpt_usage_button("welcome-usage", &t)).child(
                    button("welcome-continue", "Continue").primary().on_click(move |_, window, cx| {
                        cx.global_mut::<Welcome>().acknowledge(&dismiss_key);
                        window.close_dialog(cx);
                    }),
                ))
        });
    }
}

/// Acknowledged ChatGPT plan notices, kept beside the native client's other
/// files the way the web keeps them in `localStorage`.
#[derive(Default)]
struct Welcome {
    loaded: bool,
    acknowledged: Vec<String>,
    showing: Option<String>,
}

impl Global for Welcome {}

impl Welcome {
    fn path() -> Option<PathBuf> {
        let home = std::env::var_os("T3CODE_HOME").map(PathBuf::from).or_else(|| {
            std::env::var_os("USERPROFILE")
                .or_else(|| std::env::var_os("HOME"))
                .map(|home| PathBuf::from(home).join(".t3-gpui"))
        })?;
        Some(home.join("chatgpt-sharing-welcome.json"))
    }

    fn load(&mut self) {
        if std::mem::replace(&mut self.loaded, true) {
            return;
        }
        self.acknowledged = Self::path()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
    }

    fn acknowledge(&mut self, key: &str) {
        self.showing = None;
        if self.acknowledged.iter().any(|k| k == key) {
            return;
        }
        self.acknowledged.push(key.to_owned());
        if let Some(path) = Self::path() {
            let saved = path
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|()| std::fs::write(&path, serde_json::to_vec(&self.acknowledged).unwrap_or_default()));
            if let Err(error) = saved {
                log::warn!("could not save {}: {error}", path.display());
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Live server state

/// One `provider.*.subscribe` stream, like the web's query atom.
struct Live<T> {
    data: Option<T>,
    error: Option<String>,
    task: Option<Task<()>>,
}

impl<T> Default for Live<T> {
    fn default() -> Self {
        Self { data: None, error: None, task: None }
    }
}

/// Starts `tag`'s stream for `instance_id` unless one is running; a stream
/// the socket dropped restarts on the next render after reconnecting.
fn subscribe<V: 'static, T: for<'de> Deserialize<'de> + PartialEq + 'static>(
    view: &mut V,
    live: fn(&mut V) -> &mut Live<T>,
    client: Option<RpcClient>,
    tag: &'static str,
    instance_id: &str,
    on_update: fn(&mut V, &mut Context<V>),
    cx: &mut Context<V>,
) {
    let Some(client) = client else { return };
    if live(view).task.is_some() || live(view).error.is_some() {
        return;
    }
    let stream = client.stream(tag, json!({ "instanceId": instance_id }));
    live(view).task = Some(cx.spawn(async move |this, cx| {
        while let Some(event) = stream.next().await {
            let ended = this.update(cx, |view, cx| {
                let slot = live(view);
                let ended = match event {
                    StreamEvent::Item(value) => {
                        match T::deserialize(value) {
                            Ok(state) if slot.data.as_ref() != Some(&state) => slot.data = Some(state),
                            Ok(_) => return false,
                            Err(error) => log::warn!("bad {tag} event: {error}"),
                        }
                        false
                    }
                    StreamEvent::End(result) => {
                        if let Some(error) = result.as_ref().err().filter(|e| !matches!(e, RpcError::Disconnected(_))) {
                            slot.error = Some(failure_message(error, "Could not read setup status."));
                        }
                        slot.task = None;
                        true
                    }
                };
                on_update(view, cx);
                cx.notify();
                ended
            });
            if ended.unwrap_or(true) {
                break;
            }
        }
    }));
}

/// Aborts a task on the shared runtime when dropped, as unmounting aborts the web's work.
struct AbortOnDrop(tokio::task::AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

// ---------------------------------------------------------------------------
// The account section

/// A step this card started that waits for the server: sign in once the
/// install lands, or open the page once the flow waits on the browser.
/// Cancel, a new attempt, or unmounting drop it.
enum FollowUp {
    SignInAfterInstall { started: InstallState, method_id: String },
    OpenPage { flow_id: String },
}

/// Options of the managed setup shown inside the add-account wizard.
struct WizardSetup {
    on_cancelled: Rc<dyn Fn(&mut Window, &mut App)>,
}

pub(in crate::app) struct AccountSection {
    workspace: WeakEntity<Workspace>,
    /// The settings environment the section was made for; it reads and acts only there.
    scope: Option<SharedString>,
    instance_id: String,
    wizard: Option<WizardSetup>,
    auth: Live<AuthState>,
    install: Live<InstallState>,
    pending: bool,
    error: Option<SharedString>,
    /// Sign-in method picked in the generic row; empty means the provider default.
    method_id: String,
    /// Inputs of the current flow's interaction, keyed `flowId:interactionId`.
    draft_id: String,
    fields: HashMap<String, Entity<InputState>>,
    _field_subscriptions: Vec<Subscription>,
    // Managed Codex setup.
    awaiting_provider: bool,
    refreshed_flow: Option<String>,
    requested_method_id: String,
    callback_help_open: bool,
    picker_selection: Option<String>,
    follow_up: Option<FollowUp>,
    receiving: Option<(String, AbortOnDrop)>,
    auto_start_handled: bool,
    email_revealed: bool,
    terminal_focus: Option<FocusHandle>,
    terminal_queue: VecDeque<Value>,
    terminal_sending: bool,
    // Antigravity setup.
    antigravity: bool,
    setup_pending: Option<&'static str>,
    setup_error: Option<SharedString>,
}

/// The parts of the workspace a section renders from.
struct Snapshot {
    provider: Provider,
    instance: Value,
    environment: String,
    client: Option<RpcClient>,
}

impl AccountSection {
    fn new(
        workspace: WeakEntity<Workspace>,
        scope: Option<SharedString>,
        instance_id: String,
        wizard: Option<WizardSetup>,
    ) -> Self {
        Self {
            workspace,
            scope,
            instance_id,
            wizard,
            auth: Live::default(),
            install: Live::default(),
            pending: false,
            error: None,
            method_id: String::new(),
            draft_id: String::new(),
            fields: HashMap::new(),
            _field_subscriptions: Vec::new(),
            awaiting_provider: false,
            refreshed_flow: None,
            requested_method_id: "chatgpt".into(),
            callback_help_open: false,
            picker_selection: None,
            follow_up: None,
            receiving: None,
            auto_start_handled: false,
            email_revealed: false,
            terminal_focus: None,
            terminal_queue: VecDeque::new(),
            terminal_sending: false,
            antigravity: false,
            setup_pending: None,
            setup_error: None,
        }
    }

    fn snapshot(&self, cx: &App) -> Option<Snapshot> {
        let workspace = self.workspace.upgrade()?;
        let workspace = workspace.read(cx);
        let scope = self.scope.as_ref()?;
        let info = workspace.env_info(scope)?;
        Some(Snapshot {
            instance: info.settings["providerInstances"][self.instance_id.as_str()].clone(),
            environment: info.environment_label.clone().unwrap_or_else(|| "this environment".into()),
            client: workspace.env_client(scope),
            provider: info.provider(&self.instance_id)?.clone(),
        })
    }

    fn client(&self, cx: &App) -> Option<RpcClient> {
        self.workspace.upgrade()?.read(cx).env_client(self.scope.as_ref()?)
    }

    fn input(&self) -> Value {
        json!({ "instanceId": self.instance_id })
    }

    /// `run` in the web sections: one request at a time, its failure shown on the card.
    fn run(
        &mut self,
        tag: &'static str,
        payload: Value,
        fallback: &'static str,
        cx: &mut Context<Self>,
        on_success: impl FnOnce(&mut Self, Value, &mut Context<Self>) + 'static,
    ) {
        let Some(client) = self.client(cx).filter(|_| !self.pending) else { return };
        self.pending = true;
        self.error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = client.request(tag, payload).await;
            let _ = this.update(cx, |this, cx| {
                this.pending = false;
                match result {
                    Ok(value) => on_success(this, value, cx),
                    Err(RpcError::Disconnected(_)) => {}
                    Err(error) => this.error = Some(failure_message(&error, fallback).into()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn auth_updated(&mut self, cx: &mut Context<Self>) {
        let auth = self.auth.data.as_ref();
        // A sign-in page this card is listening for that is no longer the flow's.
        if self.receiving.as_ref().is_some_and(|(url, _)| auth.and_then(browser_url).as_deref() != Some(url)) {
            self.receiving = None;
        }
        if let Some(FollowUp::OpenPage { flow_id }) = &self.follow_up
            && let Some(page) = settle_authorization_page(auth, Some(flow_id))
        {
            let flow_id = flow_id.clone();
            self.follow_up = None;
            if let Some(url) = page {
                self.open_page(url, Some(flow_id), cx);
            }
        }
    }

    fn install_updated(&mut self, cx: &mut Context<Self>) {
        if let Some(FollowUp::SignInAfterInstall { started, method_id }) = &self.follow_up
            && let Some(ready) = settle_install(started, self.install.data.as_ref())
        {
            let method_id = method_id.clone();
            self.follow_up = None;
            if ready {
                self.sign_in(method_id, cx);
            }
        }
    }

    // --- Managed Codex actions -------------------------------------------

    fn return_url(&self) -> String {
        format!("t3code://app/settings/providers?instanceId={}", self.instance_id)
    }

    fn sign_in(&mut self, method_id: String, cx: &mut Context<Self>) {
        if self.pending {
            return;
        }
        self.requested_method_id = method_id.clone();
        let payload = json!({
            "instanceId": self.instance_id,
            "methodId": method_id,
            "returnUrl": self.return_url(),
            // The desktop catches the redirect itself, like Electron's bridge.
            "callbackMode": "client",
        });
        self.run("provider.auth.start", payload, "Codex setup failed. Try again.", cx, |this, value, cx| {
            this.awaiting_provider = true;
            let Ok(started) = AuthState::deserialize(value) else { return };
            let Some(flow_id) = started.flow_id.clone() else { return };
            match settle_authorization_page(Some(&started), Some(&flow_id)) {
                Some(Some(url)) => this.open_page(url, Some(flow_id), cx),
                Some(None) => {}
                None => {
                    this.follow_up = Some(FollowUp::OpenPage { flow_id });
                    this.auth_updated(cx);
                }
            }
        });
    }

    /// `setup`: installs or updates the managed runtime when needed, then signs in.
    fn setup(&mut self, method_id: &str, cx: &mut Context<Self>) {
        let Some(snapshot) = self.snapshot(cx) else { return };
        let view = CodexView::new(self, &snapshot);
        if view.unavailable || view.busy {
            return;
        }
        self.follow_up = None;
        let method_id = method_id.to_owned();
        if view.installed && !view.update_available {
            self.sign_in(method_id, cx);
            return;
        }
        self.run(
            "provider.install.start",
            self.input(),
            "Codex setup failed. Try again.",
            cx,
            move |this, value, cx| {
                let Ok(started) = InstallState::deserialize(value) else { return };
                this.follow_up = Some(FollowUp::SignInAfterInstall { started, method_id });
                this.install_updated(cx);
            },
        );
    }

    /// Opens the authorization page and, for this flow, catches its redirect on
    /// this computer and completes the sign-in with it.
    fn open_page(&mut self, url: String, flow_id: Option<String>, cx: &mut Context<Self>) {
        let Some(flow_id) = flow_id else {
            cx.open_url(&url);
            return;
        };
        if self.receiving.as_ref().is_some_and(|(receiving, _)| *receiving == url) {
            cx.open_url(&url);
            return;
        }
        let (bound_tx, bound_rx) = futures::channel::oneshot::channel();
        let authorization_url = url.clone();
        let job = crate::runtime().spawn(async move {
            let (listener, request) = codex_callback::listen(authorization_url).await?;
            let _ = bound_tx.send(());
            codex_callback::receive(listener, request).await
        });
        self.receiving = Some((url.clone(), AbortOnDrop(job.abort_handle())));
        cx.spawn(async move |this, cx| {
            if bound_rx.await.is_ok() {
                let opened = url.clone();
                let _ = this.update(cx, |_, cx| cx.open_url(&opened));
            }
            let result = job.await;
            let _ = this.update(cx, |this, cx| {
                if this.receiving.as_ref().is_none_or(|(receiving, _)| *receiving != url) {
                    return;
                }
                this.receiving = None;
                match result {
                    Ok(Ok(callback_url)) => {
                        let payload =
                            json!({ "instanceId": this.instance_id, "flowId": flow_id, "callbackUrl": callback_url });
                        this.run("provider.auth.complete", payload, "Codex setup failed. Try again.", cx, |_, _, _| {});
                    }
                    Ok(Err(error)) => {
                        log::warn!("ChatGPT callback: {error:#}");
                        this.error = Some(
                            "Could not finish sign-in on this computer. Try again or paste the redirect URL below."
                                .into(),
                        );
                    }
                    Err(_) => {}
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn cancel_sign_in(&mut self, cx: &mut Context<Self>) {
        self.awaiting_provider = false;
        self.follow_up = None;
        let Some(flow_id) = self.auth.data.as_ref().and_then(|auth| auth.flow_id.clone()) else { return };
        let payload = json!({ "instanceId": self.instance_id, "flowId": flow_id });
        self.run("provider.auth.cancel", payload, "Codex setup failed. Try again.", cx, |_, _, _| {});
    }

    fn cancel_install(&mut self, cx: &mut Context<Self>) {
        let Some(operation_id) = self.install.data.as_ref().and_then(|i| i.operation_id.clone()) else { return };
        self.follow_up = None;
        let payload = json!({ "instanceId": self.instance_id, "operationId": operation_id });
        self.run("provider.install.cancel", payload, "Codex setup failed. Try again.", cx, |_, _, _| {});
    }

    /// `onModeChange`: upserts the instance enabled, with the new setup mode.
    fn set_setup_mode(&mut self, mode: &str, instance: &Value, driver: &str, cx: &mut Context<Self>) {
        let mut next = if instance.is_object() { instance.clone() } else { json!({ "driver": driver }) };
        let mut config = next.get("config").filter(|c| c.is_object()).cloned().unwrap_or_else(|| json!({}));
        config["enabled"] = true.into();
        config["setupMode"] = mode.into();
        next["enabled"] = true.into();
        next["config"] = config;
        let payload = json!({
            "patch": {},
            "providerInstanceMutation": { "operation": "upsert", "instanceId": self.instance_id, "instance": next },
        });
        self.run("server.updateSettings", payload, "The settings update failed.", cx, |_, _, _| {});
    }

    fn open_account_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let section = cx.entity();
        window.open_dialog(cx, move |modal, _, cx| {
            let modal = modal.entrance(DialogEntrance::None);
            let t = *theme::tokens(cx);
            let this = section.read(cx);
            let profiles: Vec<AuthMethod> = this
                .auth
                .data
                .as_ref()
                .and_then(|auth| auth.methods.as_ref())
                .map(|methods| methods.iter().filter(|m| m.id.starts_with("chatgpt-profile:")).cloned().collect())
                .unwrap_or_default();
            let selected = match this.picker_selection.as_deref() {
                Some(CHANGE_ACCOUNT) => CHANGE_ACCOUNT.to_owned(),
                selection => profiles
                    .iter()
                    .find(|p| Some(p.id.as_str()) == selection)
                    .or(profiles.first())
                    .map_or(CHANGE_ACCOUNT.to_owned(), |p| p.id.clone()),
            };
            let option = |id: String, label: String, bold_break: bool| {
                let picked = section.clone();
                let value = id.clone();
                div()
                    .id(SharedString::from(format!("picker-{id}")))
                    .flex()
                    .items_center()
                    .gap_3()
                    .cursor_pointer()
                    .rounded_lg()
                    .border_1()
                    .border_color(t.border)
                    .px_3()
                    .py_3()
                    .text_sm()
                    .child(Radio::new(SharedString::from(format!("picker-radio-{id}"))).checked(selected == id))
                    .child(
                        div()
                            .min_w_0()
                            .font_weight(FontWeight::MEDIUM)
                            .when(bold_break, |el| el.truncate())
                            .child(label),
                    )
                    .on_click(move |_, _, cx| {
                        picked.update(cx, |this, cx| {
                            this.picker_selection = Some(value.clone());
                            cx.notify();
                        })
                    })
            };
            let confirm = section.clone();
            let chosen = selected.clone();
            modal
                .w(px(512.))
                .title("Reconnect ChatGPT")
                .child(
                    v_flex()
                        .gap_4()
                        .child(
                            div()
                                .text_sm()
                                .text_color(t.muted_foreground)
                                .child("On OpenAI, sign in with the account you choose here."),
                        )
                        .child(
                            v_flex()
                                .gap_2()
                                .children(profiles.iter().map(|p| option(p.id.clone(), p.name.clone(), true)))
                                .child(option(CHANGE_ACCOUNT.to_owned(), "Use a different account".into(), false)),
                        ),
                )
                .footer(h_flex().w_full().justify_end().child(
                    chatgpt_connection_button("picker-continue", "Continue with ChatGPT").on_click(
                        move |_, window, cx| {
                            window.close_dialog(cx);
                            let method = chosen.clone();
                            confirm.update(cx, |this, cx| {
                                let known = this
                                    .auth
                                    .data
                                    .as_ref()
                                    .and_then(|a| a.methods.as_ref())
                                    .is_some_and(|methods| methods.iter().any(|m| m.id == method));
                                this.setup(if known { &method } else { CHANGE_ACCOUNT }, cx);
                            });
                        },
                    ),
                ))
        });
    }

    // --- Generic sign-in actions ------------------------------------------

    fn respond(
        &mut self,
        response: Value,
        cx: &mut Context<Self>,
        on_sent: impl FnOnce(&mut Self, &mut Context<Self>) + 'static,
    ) {
        let Some(auth) = &self.auth.data else { return };
        let (Some(flow_id), Some(interaction)) = (&auth.flow_id, &auth.interaction) else { return };
        let payload = json!({
            "instanceId": self.instance_id,
            "flowId": flow_id,
            "interactionId": interaction.id(),
            "response": response,
        });
        self.run("provider.auth.respond", payload, "Provider sign-in failed. Try again.", cx, move |this, _, cx| {
            on_sent(this, cx)
        });
    }

    fn open_browser(&mut self, url: String, cx: &mut Context<Self>) {
        let consent = matches!(
            self.auth.data.as_ref().and_then(|a| a.interaction.as_ref()),
            Some(Interaction::Browser { requires_consent: true, .. })
        );
        if consent {
            self.respond(json!({ "type": "browser", "action": "accept" }), cx, move |this, cx| {
                this.error = None;
                cx.open_url(&url);
            });
        } else {
            cx.open_url(&url);
        }
    }

    fn copy_link(&mut self, url: &str, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(url.to_owned()));
        // Consent must still be recorded before the link is used.
        if matches!(
            self.auth.data.as_ref().and_then(|a| a.interaction.as_ref()),
            Some(Interaction::Browser { requires_consent: true, .. })
        ) {
            self.respond(json!({ "type": "browser", "action": "accept" }), cx, |_, _| {});
        }
    }

    fn start_generic(&mut self, cx: &mut Context<Self>) {
        let mut payload = self.input();
        let known = self
            .auth
            .data
            .as_ref()
            .and_then(|a| a.methods.as_ref())
            .is_some_and(|m| m.iter().any(|m| m.id == self.method_id));
        if !self.method_id.is_empty() && known {
            payload["methodId"] = self.method_id.clone().into();
        }
        self.run("provider.auth.start", payload, "Provider sign-in failed. Try again.", cx, |_, _, _| {});
    }

    fn confirm_logout(
        &mut self,
        provider_name: String,
        environment: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let section = cx.entity();
        let message = format!(
            "Sign out of {provider_name} on {environment}? This stops running threads that share this sign-in. Thread history is kept."
        );
        window.open_alert_dialog(cx, move |alert, _, _| {
            let alert = alert.entrance(DialogEntrance::None);
            let section = section.clone();
            alert.description(message.clone()).ok_text("Sign out").show_cancel(true).on_ok(move |_, _, cx| {
                section.update(cx, |this, cx| {
                    let payload = this.input();
                    this.run("provider.auth.logout", payload, "Provider sign-in failed. Try again.", cx, |_, _, _| {});
                });
                true
            })
        });
    }

    fn submit_credentials(&mut self, cx: &mut Context<Self>) {
        let Some(Interaction::Credentials { fields, .. }) = self.auth.data.as_ref().and_then(|a| a.interaction.clone())
        else {
            return;
        };
        let values: serde_json::Map<String, Value> = fields
            .iter()
            .map(|field| {
                let value =
                    self.fields.get(&field.name).map(|input| input.read(cx).value().to_string()).unwrap_or_default();
                (field.name.clone(), Value::String(value))
            })
            .collect();
        self.respond(json!({ "type": "credentials", "values": values }), cx, |this, cx| this.clear_drafts(cx));
    }

    fn submit_callback(&mut self, cx: &mut Context<Self>) {
        let value =
            self.fields.get("callback").map(|input| input.read(cx).value().trim().to_owned()).unwrap_or_default();
        let Some(flow_id) = self.auth.data.as_ref().and_then(|a| a.flow_id.clone()).filter(|_| !value.is_empty())
        else {
            return;
        };
        let payload = json!({ "instanceId": self.instance_id, "flowId": flow_id, "callbackUrl": value });
        let fallback = if self.wizard.is_some() || self.install.task.is_some() {
            "Codex setup failed. Try again."
        } else {
            "Provider sign-in failed. Try again."
        };
        self.run("provider.auth.complete", payload, fallback, cx, |this, _, cx| this.clear_drafts(cx));
    }

    fn clear_drafts(&mut self, cx: &mut Context<Self>) {
        self.draft_id.clear();
        self.fields.clear();
        self._field_subscriptions.clear();
        cx.notify();
    }

    /// The input for `name` in the current interaction's draft; Enter submits it.
    fn field(
        &mut self,
        draft_id: &str,
        name: &str,
        secret: bool,
        placeholder: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        if self.draft_id != draft_id {
            self.draft_id = draft_id.to_owned();
            self.fields.clear();
            self._field_subscriptions.clear();
        }
        if let Some(input) = self.fields.get(name) {
            return input.clone();
        }
        let placeholder = placeholder.to_owned();
        let input = cx.new(|cx| InputState::new(window, cx).masked(secret).placeholder(placeholder));
        let callback = name == "callback";
        self._field_subscriptions.push(cx.subscribe(&input, move |this, _, event: &InputEvent, cx| match event {
            InputEvent::PressEnter { .. } if callback => this.submit_callback(cx),
            InputEvent::PressEnter { .. } => this.submit_credentials(cx),
            InputEvent::Change => cx.notify(),
            _ => {}
        }));
        self.fields.insert(name.to_owned(), input.clone());
        input
    }

    fn send_terminal(&mut self, data: String, cx: &mut Context<Self>) {
        let Some(auth) = &self.auth.data else { return };
        let (Some(flow_id), Some(interaction)) = (&auth.flow_id, &auth.interaction) else { return };
        let chars: Vec<char> = data.chars().collect();
        for chunk in chars.chunks(TERMINAL_CHUNK) {
            self.terminal_queue.push_back(json!({
                "instanceId": self.instance_id,
                "flowId": flow_id,
                "interactionId": interaction.id(),
                "response": { "type": "terminal", "data": chunk.iter().collect::<String>() },
            }));
        }
        if std::mem::replace(&mut self.terminal_sending, true) {
            return;
        }
        let Some(client) = self.client(cx) else { return };
        cx.spawn(async move |this, cx| {
            loop {
                let Ok(Some(input)) = this.update(cx, |this, _| this.terminal_queue.pop_front()) else { break };
                if let Err(error) = client.request("provider.auth.respond", input).await {
                    let _ = this.update(cx, |this, cx| {
                        this.terminal_queue.clear();
                        if !matches!(error, RpcError::Disconnected(_)) {
                            this.error = Some("The provider sign-in terminal is no longer available.".into());
                        }
                        cx.notify();
                    });
                    break;
                }
            }
            let _ = this.update(cx, |this, _| this.terminal_sending = false);
        })
        .detach();
    }
}

/// Derived state of the managed Codex setup, named as in `ManagedCodexSetup`.
struct CodexView {
    installed: bool,
    authenticated: bool,
    update_available: bool,
    finishing_sign_in: bool,
    install_active: bool,
    auth_active: bool,
    unavailable: bool,
    busy: bool,
}

impl CodexView {
    fn new(section: &AccountSection, snapshot: &Snapshot) -> Self {
        let auth = section.auth.data.as_ref();
        let phase = auth.map_or("", |a| a.phase.as_str());
        let installation = section.install.data.as_ref();
        let installed = installation.is_some_and(|i| i.installed_version.is_some());
        let authenticated = snapshot.provider.auth.status == "authenticated";
        let update_available = installation.is_some_and(|i| {
            installed && i.source.as_deref() != Some("local") && i.version.is_some() && i.version != i.installed_version
        });
        let awaiting = section.awaiting_provider;
        let finishing_sign_in = awaiting && !authenticated && phase == "succeeded";
        let waiting_for_auth_state = awaiting && !authenticated && (auth.is_none() || phase == "idle");
        let auth_in_progress = finishing_sign_in || waiting_for_auth_state || auth_running(phase);
        let install_active = installation.is_some_and(|i| install_running(&i.phase));
        let starting_automatically = section.wizard.is_some() && !section.auto_start_handled;
        let auth_active = starting_automatically || auth_in_progress || (section.pending && !install_active);
        let unavailable = !instance_enabled(&snapshot.instance)
            || section.pending
            || section.auth.error.is_some()
            || section.install.error.is_some();
        let busy = section.pending || auth_in_progress || install_active;
        Self {
            installed,
            authenticated,
            update_available,
            finishing_sign_in,
            install_active,
            auth_active,
            unavailable,
            busy,
        }
    }
}

/// `resolveProviderInstanceEnabled` for a Codex instance, enabled by default.
fn instance_enabled(instance: &Value) -> bool {
    let own = instance.get("enabled").and_then(Value::as_bool);
    let config = instance.pointer("/config/enabled").and_then(Value::as_bool);
    own != Some(false) && config != Some(false)
}

impl Render for AccountSection {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.antigravity {
            return self.render_antigravity(window, cx);
        }
        let Some(snapshot) = self.snapshot(cx) else { return div().into_any_element() };
        let codex = self.wizard.is_some()
            || (snapshot.instance["driver"].as_str().unwrap_or(&snapshot.provider.driver) == "codex"
                && codex_setup_managed(&snapshot.instance["config"]));
        let instance_id = self.instance_id.clone();
        subscribe(
            self,
            |s| &mut s.auth,
            snapshot.client.clone(),
            "provider.auth.subscribe",
            &instance_id,
            Self::auth_updated,
            cx,
        );
        if codex {
            subscribe(
                self,
                |s| &mut s.install,
                snapshot.client.clone(),
                "provider.install.subscribe",
                &instance_id,
                Self::install_updated,
                cx,
            );
            self.render_codex(&snapshot, window, cx)
        } else {
            self.render_generic(&snapshot, window, cx)
        }
    }
}

impl AccountSection {
    fn render_codex(&mut self, snapshot: &Snapshot, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = *theme::tokens(cx);
        let provider = &snapshot.provider;
        let setup = provider.setup.clone();
        let view = CodexView::new(self, snapshot);
        let phase = self.auth.data.as_ref().map_or(String::new(), |a| a.phase.clone());
        if self.awaiting_provider && (view.authenticated || phase == "failed" || phase == "cancelled") {
            self.awaiting_provider = false;
        }
        // The server does not refresh the provider snapshot when a flow succeeds,
        // and it can finish elsewhere (callback, pasted redirect URL).
        let flow_id = self.auth.data.as_ref().and_then(|a| a.flow_id.clone());
        if self.awaiting_provider && phase == "succeeded" && self.refreshed_flow != flow_id {
            self.refreshed_flow = flow_id.clone();
            if let Some(client) = &snapshot.client {
                let client = client.clone();
                let payload = self.input();
                cx.background_spawn(async move { client.request("server.refreshProviders", payload).await }).detach();
            }
        }
        if self.wizard.is_some()
            && !self.auto_start_handled
            && !view.unavailable
            && !view.busy
            && self.install.data.is_some()
            && setup.as_ref().is_some_and(|s| s.can_install)
        {
            self.auto_start_handled = true;
            cx.defer_in(window, |this, _, cx| this.setup("chatgpt", cx));
        }
        if !setup.as_ref().is_some_and(|s| s.can_authenticate) {
            return settings_row(&t, "ChatGPT account", Some(text("Preparing sign-in.")), None).into_any_element();
        }
        let auth = self.auth.data.clone();
        let methods = auth.as_ref().and_then(|a| a.methods.clone()).unwrap_or_default();
        let email_of = |id: &str| methods.iter().find(|m| m.id == id).and_then(|m| m.account_email.clone());
        let reconnect_email = email_of("chatgpt");
        let requested_email = email_of(&self.requested_method_id);
        let has_saved_account = methods.iter().any(|m| m.id.starts_with("chatgpt-profile:"));
        let url = auth.as_ref().and_then(browser_url);
        let installation = self.install.data.clone();
        let allow_existing_cli = self.wizard.is_none();

        let runtime_description = match installation.as_ref() {
            Some(i) if i.phase == "downloading" => {
                let mb = |bytes: u64| format!("{:.1}", bytes as f64 / 1_000_000.);
                match i.total_bytes {
                    Some(total) => format!("Downloading {} of {} MB.", mb(i.downloaded_bytes), mb(total)),
                    None => format!("Downloading {} MB.", mb(i.downloaded_bytes)),
                }
            }
            Some(i) if i.phase == "extracting" => "Installing Codex.".into(),
            Some(i) if i.phase == "verifying" => "Checking Codex.".into(),
            Some(i) if view.installed => format!(
                "{}{}.",
                if i.source.as_deref() == Some("local") { "Using your installed Codex" } else { "Managed by T3 Code" },
                i.installed_version.as_ref().map(|v| format!(" · v{v}")).unwrap_or_default()
            ),
            i => i
                .and_then(|i| i.message.clone())
                .unwrap_or_else(|| "T3 Code downloads and manages Codex for you.".into()),
        };
        let email = provider.auth.email.as_deref().map(str::trim).filter(|e| !e.is_empty()).map(str::to_owned);
        let description: AnyElement = if view.finishing_sign_in {
            text("Finishing sign-in...")
        } else if view.install_active {
            text(runtime_description)
        } else if view.auth_active || phase == "failed" || phase == "cancelled" {
            text(match (phase.as_str(), &requested_email) {
                ("waiting", Some(email)) => format!("Continue as {email} on OpenAI."),
                _ => auth
                    .as_ref()
                    .and_then(|a| a.message.clone())
                    .unwrap_or_else(|| "Finish signing in in your browser.".into()),
            })
        } else if view.authenticated {
            match &email {
                Some(email) => h_flex()
                    .gap_1()
                    .child("Signed in as")
                    .child(redacted_email(&self.instance_id, email, self.email_revealed, &t).on_click(cx.listener(
                        |this, _, _, cx| {
                            this.email_revealed = !this.email_revealed;
                            cx.notify();
                        },
                    )))
                    .child(".")
                    .into_any_element(),
                None => text("Signed in with ChatGPT."),
            }
        } else {
            text(reconnect_email.unwrap_or_else(|| "Use your ChatGPT subscription.".into()))
        };

        let read_only = false;
        let pending = self.pending;
        let can_start = if view.installed {
            setup.as_ref().is_some_and(|s| s.can_authenticate)
        } else {
            setup.as_ref().is_some_and(|s| s.can_install)
        };
        let control = if view.auth_active {
            let waiting = phase == "waiting";
            let open_url = url.clone();
            let flow = flow_id.clone();
            h_flex()
                .gap_2()
                .child(
                    button(
                        "codex-open-page",
                        if view.finishing_sign_in {
                            "Finishing sign-in..."
                        } else if waiting {
                            "Open sign-in page"
                        } else {
                            "Signing in..."
                        },
                    )
                    .outline()
                    .disabled(url.is_none() || !waiting)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(url) = open_url.clone() {
                            this.open_page(url, flow.clone(), cx);
                        }
                    })),
                )
                .when(!view.finishing_sign_in, |el| {
                    el.child(button("codex-cancel-sign-in", "Cancel").ghost().disabled(pending || read_only).on_click(
                        cx.listener(|this, _, window, cx| {
                            if this.auth.data.as_ref().and_then(|a| a.flow_id.as_ref()).is_none() {
                                return;
                            }
                            this.cancel_sign_in(cx);
                            if let Some(wizard) = &this.wizard {
                                let on_cancelled = wizard.on_cancelled.clone();
                                cx.defer_in(window, move |_, window, cx| on_cancelled(window, cx));
                            }
                        }),
                    ))
                })
                .into_any_element()
        } else if view.install_active {
            button("codex-cancel-install", "Cancel")
                .ghost()
                .disabled(pending || read_only)
                .on_click(cx.listener(|this, _, _, cx| this.cancel_install(cx)))
                .into_any_element()
        } else if view.authenticated {
            h_flex()
                .gap_2()
                .child(
                    button("codex-change-account", "Change account")
                        .outline()
                        .disabled(view.unavailable || view.busy)
                        .on_click(cx.listener(|this, _, window, cx| this.open_account_picker(window, cx))),
                )
                .child(
                    button("codex-disconnect", "Disconnect").ghost().disabled(view.unavailable || view.busy).on_click(
                        cx.listener(|this, _, _, cx| {
                            let payload = this.input();
                            this.run(
                                "provider.auth.logout",
                                payload,
                                "Codex setup failed. Try again.",
                                cx,
                                |_, _, _| {},
                            );
                        }),
                    ),
                )
                .into_any_element()
        } else {
            let instance = snapshot.instance.clone();
            let driver = provider.driver.clone();
            h_flex()
                .gap_2()
                .flex_wrap()
                .justify_end()
                .max_w(px(352.))
                .child(
                    chatgpt_connection_button(
                        "codex-connect",
                        if pending {
                            "Setting up..."
                        } else if has_saved_account {
                            "Reconnect account"
                        } else {
                            "Continue with ChatGPT"
                        },
                    )
                    .disabled(view.unavailable || view.busy || !can_start)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if has_saved_account {
                            this.open_account_picker(window, cx);
                        } else {
                            this.setup("chatgpt", cx);
                        }
                    })),
                )
                .when(has_saved_account, |el| {
                    el.child(
                        button("codex-different-account", "Use a different account")
                            .ghost()
                            .disabled(view.unavailable || view.busy)
                            .on_click(cx.listener(|this, _, _, cx| this.setup(CHANGE_ACCOUNT, cx))),
                    )
                })
                .when(allow_existing_cli, |el| {
                    el.child(
                        button("codex-existing-cli", "Use existing CLI")
                            .ghost()
                            .disabled(read_only || view.busy)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.set_setup_mode("existing", &instance, &driver, cx)
                            })),
                    )
                })
                .into_any_element()
        };

        let logout_warning = auth
            .as_ref()
            .filter(|a| a.phase == "idle")
            .and_then(|a| a.message.clone())
            .filter(|message| message.starts_with("Signed out locally."));
        let callback_completion = (phase == "waiting" && url.is_some()).then(|| {
            let draft_id = format!("{}:callback", flow_id.clone().unwrap_or_default());
            let input = self.field(&draft_id, "callback", true, "Paste the URL from the sign-in tab", window, cx);
            let typed = !input.read(cx).value().trim().is_empty();
            let browser_url = url.clone().unwrap_or_default();
            v_flex()
                .w_full()
                .gap_3()
                .text_xs()
                .text_color(t.muted_foreground)
                .child("If sign-in doesn't return to T3 Code, paste the URL from the final localhost page.")
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            div().flex_1().min_w_0().child(Input::new(&input).small().disabled(pending || read_only)),
                        )
                        .child(
                            button("codex-callback-connect", "Connect")
                                .outline()
                                .disabled(pending || read_only || !typed)
                                .on_click(cx.listener(|this, _, _, cx| this.submit_callback(cx))),
                        ),
                )
                .child(
                    div().child(
                        Button::new("codex-try-browser")
                            .ghost()
                            .xsmall()
                            .text_color(t.muted_foreground)
                            .child("Try sign-in in your browser")
                            .child(Icon::new(IconName::ExternalLink).size(px(14.)))
                            .on_click(move |_, _, cx| cx.open_url(&browser_url)),
                    ),
                )
        });
        let help_open = self.callback_help_open;
        let callback_fallback = callback_completion.map(|completion| {
            v_flex()
                .gap_3()
                .text_xs()
                .text_color(t.muted_foreground)
                .child(
                    h_flex()
                        .id("codex-sign-in-help")
                        .gap_1p5()
                        .cursor_pointer()
                        .child("Having trouble signing in?")
                        .child(
                            Icon::new(if help_open { IconName::ChevronDown } else { IconName::ChevronRight })
                                .size(px(12.)),
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.callback_help_open = !this.callback_help_open;
                            cx.notify();
                        })),
                )
                .when(help_open, |el| {
                    el.child(div().w_full().border_t_1().border_color(t.border.opacity(0.5)).pt_3().child(completion))
                })
        });
        let error = self.error.clone().map(|e| e.to_string()).or_else(|| {
            if installation.as_ref().is_some_and(|i| i.phase == "failed") {
                Some(installation.as_ref().and_then(|i| i.message.clone()).unwrap_or_default())
            } else if self.auth.error.is_some() || self.install.error.is_some() {
                Some("Could not read Codex setup status. Reconnect and try again.".into())
            } else {
                None
            }
        });

        v_flex()
            .child(settings_row(&t, "ChatGPT account", Some(description), Some(control)))
            .when(view.authenticated, |el| {
                el.child(
                    h_flex()
                        .justify_end()
                        .px_4()
                        .py_1()
                        .border_t_1()
                        .border_color(t.border.opacity(0.5))
                        .child(chatgpt_usage_button("codex-manage-usage", &t)),
                )
            })
            .when_some(logout_warning, |el, warning| {
                el.child(
                    div()
                        .px_4()
                        .py_2()
                        .border_t_1()
                        .border_color(t.border.opacity(0.5))
                        .text_xs()
                        .text_color(t.muted_foreground)
                        .child(warning),
                )
            })
            .when_some(callback_fallback, |el, fallback| {
                el.child(div().px_4().py_3().border_t_1().border_color(t.border.opacity(0.5)).child(fallback))
            })
            .when_some(error, |el, error| {
                el.child(error_line(&t, error).border_t_1().border_color(t.border.opacity(0.5)))
            })
            .into_any_element()
    }

    fn render_generic(&mut self, snapshot: &Snapshot, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = *theme::tokens(cx);
        let provider = &snapshot.provider;
        let setup: Option<&ProviderSetup> = provider.setup.as_ref();
        let auth = self.auth.data.clone();
        let interaction = auth.as_ref().and_then(|a| a.interaction.clone());
        let phase = auth.as_ref().map_or("", |a| a.phase.as_str()).to_owned();
        let methods = auth.as_ref().and_then(|a| a.methods.clone());
        let active = auth_running(&phase);
        let signed_in =
            provider.auth.status == "authenticated" || (provider.auth.status == "unknown" && phase == "succeeded");
        let acp = provider.driver == "acpRegistry";
        let is_discovering = acp && !active && !signed_in && self.auth.error.is_none() && methods.is_none();
        let needs_external_setup = !active
            && !signed_in
            && (setup.is_some_and(|s| !s.can_authenticate) || (acp && methods.as_ref().is_some_and(Vec::is_empty)));
        let uses_credentials = provider.driver == "antigravity"
            && matches!(provider.auth.kind.as_deref(), Some("gemini-api-key" | "agent-platform"));
        let environment = &snapshot.environment;
        let account_description = if active {
            match (phase.as_str(), &interaction) {
                ("starting", _) => auth
                    .as_ref()
                    .and_then(|a| a.message.clone())
                    .unwrap_or_else(|| if uses_credentials { "Connecting…" } else { "Starting sign-in…" }.into()),
                ("verifying", _) if uses_credentials => "Checking credentials and loading models…".into(),
                ("verifying", _) => "Checking your account…".into(),
                (_, Some(Interaction::Terminal { .. })) => "Complete sign-in in the terminal below.".into(),
                (_, Some(Interaction::Credentials { .. })) => "Enter your credentials below.".into(),
                _ => "Finish signing in in your browser.".into(),
            }
        } else if signed_in {
            "Signed in.".into()
        } else if is_discovering {
            "Discovering sign-in methods…".into()
        } else if needs_external_setup {
            "No in-app sign-in advertised. Follow the provider's docs to finish setup.".to_owned()
        } else if uses_credentials {
            format!("Connect on {environment} to load models.")
        } else {
            format!("Sign in on {environment}.")
        };
        let email = provider.auth.email.as_deref().map(str::trim).filter(|e| !e.is_empty()).map(str::to_owned);
        let description = match email.filter(|_| signed_in && !active) {
            Some(email) => h_flex()
                .gap_1()
                .child("Signed in as")
                .child(redacted_email(&self.instance_id, &email, self.email_revealed, &t).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.email_revealed = !this.email_revealed;
                        cx.notify();
                    },
                )))
                .into_any_element(),
            None => text(account_description),
        };
        let status_message = auth.as_ref().filter(|a| a.phase == "failed").and_then(|a| a.message.clone());
        let disabled = self.pending || self.auth.error.is_some() || is_discovering;
        let draft_id = format!(
            "{}:{}",
            auth.as_ref().and_then(|a| a.flow_id.clone()).unwrap_or_default(),
            interaction.as_ref().map_or("", Interaction::id)
        );
        let url = match &interaction {
            Some(Interaction::Browser { url, .. } | Interaction::DeviceCode { url, .. }) => Some(url.clone()),
            _ => auth.as_ref().and_then(|a| a.authorization_url.clone()),
        };

        let mut control = h_flex().gap_1p5().flex_wrap();
        if !active && methods.as_ref().is_some_and(|m| m.len() > 1) {
            let methods = methods.clone().unwrap_or_default();
            let current = methods
                .iter()
                .find(|m| m.id == self.method_id)
                .map_or("Provider default".to_owned(), |m| m.name.clone());
            let section = cx.entity();
            let current_id = self.method_id.clone();
            control = control.child(
                Button::new("auth-method")
                    .outline()
                    .small()
                    .w(px(176.))
                    .justify_between()
                    .disabled(disabled)
                    .dropdown_caret(true)
                    .label(current)
                    .dropdown_menu(move |mut menu, _, _| {
                        for (id, name) in std::iter::once((String::new(), "Provider default".to_owned()))
                            .chain(methods.iter().map(|m| (m.id.clone(), m.name.clone())))
                        {
                            let section = section.clone();
                            let checked = id == current_id;
                            menu = menu.item(PopupMenuItem::new(name).checked(checked).on_click(move |_, _, cx| {
                                section.update(cx, |this, cx| {
                                    this.method_id = id.clone();
                                    cx.notify();
                                })
                            }));
                        }
                        menu
                    }),
            );
        }
        if let Some(url) = url.clone() {
            let open = url.clone();
            control = control
                .child(
                    button("auth-open-browser", "Open browser")
                        .outline()
                        .disabled(disabled)
                        .on_click(cx.listener(move |this, _, _, cx| this.open_browser(open.clone(), cx))),
                )
                .child(
                    Button::new("auth-copy-link")
                        .ghost()
                        .small()
                        .text_color(t.muted_foreground)
                        .icon(IconName::Copy)
                        .tooltip("Copy sign-in link")
                        .disabled(disabled)
                        .on_click(cx.listener(move |this, _, _, cx| this.copy_link(&url, cx))),
                );
        }
        let flow_id = auth.as_ref().and_then(|a| a.flow_id.clone());
        if let (true, Some(docs)) = (needs_external_setup, setup.and_then(|s| s.documentation_url.clone())) {
            control = control
                .child(button("auth-open-docs", "Open docs").outline().on_click(move |_, _, cx| cx.open_url(&docs)));
        } else if let (true, Some(flow_id)) = (active, flow_id) {
            control = control.child(
                button("auth-cancel", "Cancel").ghost().text_color(t.muted_foreground).disabled(disabled).on_click(
                    cx.listener(move |this, _, _, cx| {
                        let payload = json!({ "instanceId": this.instance_id, "flowId": flow_id });
                        this.run(
                            "provider.auth.cancel",
                            payload,
                            "Provider sign-in failed. Try again.",
                            cx,
                            |_, _, _| {},
                        );
                    }),
                ),
            );
        } else if !active && !needs_external_setup && setup.is_none_or(|s| s.can_authenticate) {
            let retry = phase == "failed" || phase == "cancelled";
            let label = match (signed_in, retry, uses_credentials) {
                (true, ..) => "Change account",
                (false, true, true) => "Retry connection",
                (false, true, false) => "Retry sign-in",
                (false, false, true) => "Connect",
                (false, false, false) => "Sign in",
            };
            control = control.child(
                button("auth-start", label)
                    .outline()
                    .disabled(disabled || !provider.enabled || !provider.installed || auth.is_none())
                    .on_click(cx.listener(|this, _, _, cx| this.start_generic(cx))),
            );
        }
        if !active && signed_in && provider.auth.can_logout.or(setup.map(|s| s.can_authenticate)) == Some(true) {
            let name = provider.display_name.clone().unwrap_or_else(|| provider.driver.clone());
            let environment = environment.clone();
            control = control.child(
                button("auth-sign-out", "Sign out").ghost().disabled(disabled || auth.is_none()).on_click(cx.listener(
                    move |this, _, window, cx| this.confirm_logout(name.clone(), environment.clone(), window, cx),
                )),
            );
        }

        let accepts_callback = match &interaction {
            Some(Interaction::Browser { accepts_callback, .. }) => accepts_callback.unwrap_or(false),
            Some(_) => false,
            None => true,
        } && url.is_some();
        let mut details = v_flex();
        if let Some(Interaction::DeviceCode { user_code, .. }) = &interaction {
            details = details.child(
                h_flex()
                    .py_2()
                    .gap_1()
                    .text_sm()
                    .text_color(t.muted_foreground)
                    .child("Enter code")
                    .child(div().font_family(t.fonts.mono).text_color(t.foreground).child(user_code.clone()))
                    .child("in your browser."),
            );
        }
        if let Some(Interaction::Terminal { output, .. }) = &interaction {
            let focus = self.terminal_focus.get_or_insert_with(|| cx.focus_handle()).clone();
            details = details.child(
                div().py_2().child(
                    div()
                        .id("auth-terminal")
                        .track_focus(&focus)
                        .h(px(256.))
                        .overflow_y_scroll()
                        .rounded_md()
                        .border_1()
                        .border_color(if focus.is_focused(window) { t.ring } else { t.border })
                        .bg(t.background)
                        .p_2()
                        .font_family(t.fonts.mono)
                        .text_size(px(13.))
                        .text_color(t.foreground)
                        .child(strip_ansi(output))
                        .on_click(cx.listener(|this, _, window, cx| {
                            if let Some(focus) = &this.terminal_focus {
                                window.focus(focus, cx);
                            }
                        }))
                        .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                            if let Some(data) = terminal_input(event) {
                                cx.stop_propagation();
                                this.send_terminal(data, cx);
                            }
                        })),
                ),
            );
        }
        if let Some(Interaction::Credentials { fields, .. }) = &interaction {
            let mut form = v_flex().gap_2().py_2().text_sm();
            for field in fields {
                let input = self.field(&draft_id, &field.name, field.secret, "", window, cx);
                form = form.child(
                    v_flex().gap_1().child(field.label.clone()).child(Input::new(&input).small().disabled(disabled)),
                );
            }
            details = details.child(
                form.child(
                    div().child(
                        button("auth-credentials-connect", "Connect")
                            .outline()
                            .disabled(disabled)
                            .on_click(cx.listener(|this, _, _, cx| this.submit_credentials(cx))),
                    ),
                ),
            );
        }
        if accepts_callback {
            let input = self.field(&draft_id, "callback", false, "", window, cx);
            let typed = !input.read(cx).value().trim().is_empty();
            details = details.child(
                v_flex()
                    .gap_2()
                    .py_2()
                    .text_sm()
                    .child(
                        v_flex()
                            .gap_1()
                            .child("If the final localhost page does not load, paste its full URL here.")
                            .child(Input::new(&input).small().disabled(disabled)),
                    )
                    .child(
                        div().child(
                            button("auth-callback-continue", "Continue")
                                .outline()
                                .disabled(disabled || !typed)
                                .on_click(cx.listener(|this, _, _, cx| this.submit_callback(cx))),
                        ),
                    ),
            );
        }
        let error = self.error.clone().map(|e| e.to_string()).or_else(|| self.auth.error.clone());
        if let Some(error) = &error {
            details = details.child(div().py_2().text_xs().text_color(t.error_foreground).child(error.clone()));
        }
        let has_details = matches!(
            interaction,
            Some(Interaction::Terminal { .. } | Interaction::Credentials { .. } | Interaction::DeviceCode { .. })
        ) || accepts_callback
            || error.is_some();

        settings_row(&t, "Account", Some(description), Some(control.into_any_element()))
            .when(has_details, |el| el.pb_1())
            .when_some(status_message, |el, status| {
                el.child(div().px_4().pt_0p5().text_xs().text_color(t.muted_foreground).child(status))
            })
            .when(has_details, |el| el.child(div().px_4().child(details)))
            .into_any_element()
    }
}

// ---------------------------------------------------------------------------
// Managed runtime paths

pub(in crate::app) struct RuntimeFields {
    workspace: WeakEntity<Workspace>,
    scope: Option<SharedString>,
    instance_id: String,
    install: Live<InstallState>,
}

impl Render for RuntimeFields {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *theme::tokens(cx);
        let (client, paths) = self
            .workspace
            .upgrade()
            .zip(self.scope.as_ref())
            .map(|(workspace, scope)| {
                let workspace = workspace.read(cx);
                let paths = workspace
                    .env_info(scope)
                    .and_then(|i| i.provider(&self.instance_id))
                    .map(|p| p.runtime_paths.clone());
                (workspace.env_client(scope), paths)
            })
            .unwrap_or_default();
        let instance_id = self.instance_id.clone();
        subscribe(self, |s| &mut s.install, client, "provider.install.subscribe", &instance_id, |_, _| {}, cx);
        let paths = paths.flatten();
        let field = |value: Option<String>, placeholder: &str| {
            div()
                .w(px(320.))
                .h(px(32.))
                .px_3()
                .flex()
                .items_center()
                .rounded_lg()
                .border_1()
                .border_color(t.input)
                .opacity(0.64)
                .text_sm()
                .overflow_hidden()
                .map(|el| match value.filter(|v| !v.is_empty()) {
                    Some(value) => el.text_color(t.foreground).child(div().truncate().child(value)),
                    None => el.text_color(t.muted_foreground).child(placeholder.to_owned()),
                })
                .into_any_element()
        };
        let executable = self.install.data.as_ref().and_then(|i| i.executable_path.clone());
        let binary_placeholder =
            if self.install.error.is_some() { "Could not read runtime path" } else { "Not installed" };
        let shadow = paths.as_ref().and_then(|p| p.shadow_home_path.clone());
        let rule = || div().border_t_1().border_color(t.border.opacity(0.5));
        v_flex()
            .child(settings_row(
                &t,
                "Binary path",
                Some(text("Selected by T3 Code.")),
                Some(field(executable, binary_placeholder)),
            ))
            .child(rule().child(settings_row(
                &t,
                "CODEX_HOME path",
                Some(text("Shared Codex config, sessions, and state.")),
                Some(field(paths.as_ref().map(|p| p.home_path.clone()), "Unavailable")),
            )))
            .child(rule().child(settings_row(
                &t,
                "Shadow home path",
                Some(text(if shadow.is_some() {
                    "Account-specific home sharing the Codex state above."
                } else {
                    "This instance uses the shared Codex home directly."
                })),
                Some(field(shadow, if paths.is_some() { "Not used" } else { "Unavailable" })),
            )))
    }
}

// ---------------------------------------------------------------------------
// Add a ChatGPT account

struct AddCodexAccount {
    workspace: WeakEntity<Workspace>,
    /// The settings environment at open: the account is made there.
    scope: Option<SharedString>,
    name: Entity<InputState>,
    pending: bool,
    created: Option<(String, Entity<AccountSection>)>,
    on_created: Option<Rc<dyn Fn(String, String, &mut Window, &mut App)>>,
    _subscriptions: Vec<Subscription>,
}

impl AddCodexAccount {
    fn new(
        workspace: WeakEntity<Workspace>,
        scope: Option<SharedString>,
        on_created: Option<Rc<dyn Fn(String, String, &mut Window, &mut App)>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let name =
            cx.new(|cx| InputState::new(window, cx).default_value("Personal").placeholder("e.g. Personal or Work"));
        let subscription = cx.subscribe_in(&name, window, |this, _, event: &InputEvent, window, cx| match event {
            InputEvent::PressEnter { .. } => this.create(window, cx),
            InputEvent::Change => cx.notify(),
            _ => {}
        });
        Self { workspace, scope, name, pending: false, created: None, on_created, _subscriptions: vec![subscription] }
    }

    fn display_name(&self, cx: &App) -> String {
        format!("ChatGPT - {}", self.name.read(cx).value().trim())
    }

    fn create(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace.upgrade() else { return };
        if self.pending || self.name.read(cx).value().trim().is_empty() || self.created.is_some() {
            return;
        }
        let Some(scope) = self.scope.as_ref() else { return };
        let (client, mut instances) = {
            let workspace = workspace.read(cx);
            let instances = workspace.env_info(scope).map(|i| i.settings["providerInstances"].clone());
            (workspace.env_client(scope), instances.unwrap_or_default())
        };
        let Some(client) = client else { return };
        let id = format!("codex_{}", uuid::Uuid::new_v4());
        let display_name = self.display_name(cx);
        if !instances.is_object() {
            instances = json!({});
        }
        instances[id.as_str()] = json!({
            "driver": "codex",
            "displayName": display_name,
            "enabled": true,
            "config": { "enabled": true, "setupMode": "managed" },
        });
        self.pending = true;
        cx.notify();
        let payload = json!({ "patch": { "providerInstances": instances } });
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("server.updateSettings", payload).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.pending = false;
                match result {
                    Ok(_) => match this.on_created.clone() {
                        Some(on_created) => {
                            on_created(id, display_name, window, cx);
                            window.close_dialog(cx);
                        }
                        None => {
                            let close: Rc<dyn Fn(&mut Window, &mut App)> =
                                Rc::new(|window, cx| window.close_dialog(cx));
                            let (workspace, scope) = (this.workspace.clone(), this.scope.clone());
                            let section_id = id.clone();
                            let wizard = Some(WizardSetup { on_cancelled: close });
                            let section = cx.new(|_| AccountSection::new(workspace, scope, section_id, wizard));
                            this.created = Some((id, section));
                        }
                    },
                    Err(error) => window.push_notification(
                        gpui_kit::component::notification::Notification::error(failure_message(
                            &error,
                            "Could not save the setting.",
                        ))
                        .title("Add ChatGPT account"),
                        cx,
                    ),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn render_dialog(
        &mut self,
        modal: gpui_kit::component::dialog::Dialog,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::component::dialog::Dialog {
        let t = *theme::tokens(cx);
        let providers = self
            .workspace
            .upgrade()
            .zip(self.scope.as_ref())
            .and_then(|(w, scope)| Some(w.read(cx).env_info(scope)?.providers.clone()))
            .unwrap_or_default();
        let created = self.created.clone();
        let provider = created.as_ref().and_then(|(id, _)| providers.iter().find(|p| p.instance_id == *id));
        // Closes itself once the new account connects.
        if provider.is_some_and(|p| uses_chatgpt_sharing(&p.auth)) {
            cx.defer_in(window, |_, window, cx| window.close_dialog(cx));
        }
        let name_empty = self.name.read(cx).value().trim().is_empty();
        let title = if created.is_some() { self.display_name(cx) } else { "Add ChatGPT account".into() };
        let body: AnyElement = match (&created, provider) {
            (Some((_, section)), Some(p)) if p.setup.is_some() => section.clone().into_any_element(),
            (Some(_), _) => {
                settings_row(&t, "Codex runtime", Some(text("Preparing managed setup.")), None).into_any_element()
            }
            (None, _) => v_flex()
                .gap_3()
                .child(settings_row(
                    &t,
                    "Account name",
                    Some(text("Shown in the provider list and model picker.")),
                    Some(div().w(px(240.)).child(Input::new(&self.name).disabled(self.pending)).into_any_element()),
                ))
                .when(providers.iter().any(|p| p.driver == "codex" && p.auth.status == "authenticated"), |el| {
                    el.child(multiple_accounts_note(&t))
                })
                .into_any_element(),
        };
        let footer = match created {
            Some(_) => h_flex().gap_2().justify_end().child(
                button("add-codex-finish-later", "Finish later")
                    .outline()
                    .on_click(|_, window, cx| window.close_dialog(cx)),
            ),
            None => h_flex()
                .gap_2()
                .justify_end()
                .child(
                    button("add-codex-cancel", "Cancel")
                        .outline()
                        .disabled(self.pending)
                        .on_click(|_, window, cx| window.close_dialog(cx)),
                )
                .child(
                    button("add-codex-continue", if self.pending { "Adding account…" } else { "Continue" })
                        .primary()
                        .disabled(self.pending || name_empty)
                        .on_click(cx.listener(|this, _, window, cx| this.create(window, cx))),
                ),
        };
        modal
            .w(px(640.))
            .title(title)
            .child(
                v_flex()
                    .gap_4()
                    .child(div().text_sm().text_color(t.muted_foreground).child(
                        "Each account has its own Codex instance and sign-in. Choose the other account on the sign-in page.",
                    ))
                    .child(body),
            )
            .footer(footer.w_full())
    }
}

/// `MultipleAccountsNote` for Codex.
fn multiple_accounts_note(t: &Tokens) -> AnyElement {
    h_flex()
        .gap_2()
        .items_start()
        .rounded_lg()
        .border_1()
        .border_color(t.info.opacity(0.32))
        .bg(t.info.opacity(0.04))
        .px_3p5()
        .py_3()
        .text_sm()
        .child(Icon::new(IconName::Info).size(px(16.)).text_color(t.info_foreground))
        .child(
            h_flex()
                .flex_wrap()
                .gap_1()
                .text_color(t.muted_foreground)
                .child("We do not recommend managing multiple Codex accounts in T3 Code.")
                .child(
                    div()
                        .id("multiple-accounts-more-info")
                        .cursor_pointer()
                        .underline()
                        .hover(|el| el.text_color(t.foreground))
                        .child("More info")
                        .on_click(|_, _, cx| cx.open_url(MULTIPLE_ACCOUNTS_FAQ_URL)),
                ),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn install(operation: Option<&str>, phase: &str, installed: Option<&str>) -> InstallState {
        InstallState {
            operation_id: operation.map(Into::into),
            phase: phase.into(),
            version: Some("0.2.0".into()),
            installed_version: installed.map(Into::into),
            ..Default::default()
        }
    }

    fn auth(flow: &str, phase: &str, url: Option<&str>) -> AuthState {
        AuthState {
            phase: phase.into(),
            flow_id: Some(flow.into()),
            authorization_url: url.map(Into::into),
            ..Default::default()
        }
    }

    #[test]
    fn an_install_settles_on_its_own_operation() {
        let started = install(Some("op-2"), "downloading", Some("0.1.0"));
        assert_eq!(settle_install(&started, Some(&install(Some("op-1"), "succeeded", Some("0.1.0")))), None);
        assert_eq!(settle_install(&started, None), None);
        assert_eq!(settle_install(&started, Some(&install(Some("op-2"), "extracting", None))), None);
        assert_eq!(settle_install(&started, Some(&install(Some("op-2"), "succeeded", Some("0.2.0")))), Some(true));
        assert_eq!(settle_install(&started, Some(&install(Some("op-2"), "failed", Some("0.1.0")))), Some(false));
        assert_eq!(settle_install(&started, Some(&install(Some("op-2"), "cancelled", Some("0.1.0")))), Some(false));
        // Start reused an existing runtime.
        assert_eq!(settle_install(&install(None, "succeeded", Some("0.1.0")), None), Some(true));
        assert_eq!(settle_install(&install(None, "idle", None), None), Some(false));
    }

    #[test]
    fn the_sign_in_page_waits_for_its_own_flow() {
        assert_eq!(settle_authorization_page(None, Some("flow-1")), None);
        assert_eq!(settle_authorization_page(Some(&auth("flow-1", "starting", None)), Some("flow-1")), None);
        let waiting = auth("flow-1", "waiting", Some("https://a"));
        assert_eq!(settle_authorization_page(Some(&waiting), Some("flow-1")), Some(Some("https://a".into())));
        let browser = AuthState {
            interaction: Some(Interaction::Browser {
                id: "interaction-1".into(),
                url: "https://b".into(),
                requires_consent: false,
                accepts_callback: None,
            }),
            ..waiting
        };
        assert_eq!(settle_authorization_page(Some(&browser), Some("flow-1")), Some(Some("https://b".into())));
        let other = auth("flow-2", "waiting", Some("https://a"));
        assert_eq!(settle_authorization_page(Some(&other), Some("flow-1")), None);
        for phase in ["failed", "cancelled", "succeeded"] {
            assert_eq!(settle_authorization_page(Some(&auth("flow-1", phase, None)), Some("flow-1")), Some(None));
        }
    }

    #[test]
    fn interactions_decode_and_unknown_ones_are_kept_out_of_the_way() {
        let state: AuthState = serde_json::from_value(json!({
            "instanceId": "codex", "phase": "waiting", "flowId": "f", "authorizationUrl": null,
            "expiresAt": null, "message": null,
            "interaction": { "type": "deviceCode", "id": "i", "url": "https://d", "userCode": "ABCD" },
        }))
        .unwrap();
        assert_eq!(
            state.interaction,
            Some(Interaction::DeviceCode { id: "i".into(), url: "https://d".into(), user_code: "ABCD".into() })
        );
        let future: AuthState =
            serde_json::from_value(json!({ "phase": "waiting", "interaction": { "type": "hologram", "id": "i" } }))
                .unwrap();
        assert_eq!(future.interaction, Some(Interaction::Unknown));
    }

    #[test]
    fn redaction_keeps_the_address_shape_and_is_stable() {
        let redacted = redacted_placeholder("jane.doe@example.com");
        assert_eq!(redacted.len(), "jane.doe@example.com".len());
        assert_eq!(redacted.find('@'), Some(8));
        assert_eq!(redacted.matches('.').count(), 2);
        assert_ne!(redacted, "jane.doe@example.com");
        assert_eq!(redacted, redacted_placeholder("jane.doe@example.com"));
    }

    #[test]
    fn terminal_output_drops_control_sequences() {
        assert_eq!(
            strip_ansi("\x1b[1;32mSign in\x1b[0m\r\nCode: \x1b]8;;https://x\x07link\x1b]8;;\x07"),
            "Sign in\nCode: link"
        );
    }
}
