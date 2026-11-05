//! The source editors: `DeviceHostEditor.tsx` (SSH), `EasDeviceHostEditor.tsx` and
//! `SimlockDeviceHostEditor.tsx`, each a dialog owning its draft. Saves patch the
//! environment's host list, read fresh from settings when Save is pressed.
//!
//! The web's secret-source picker for the Expo token only appears behind
//! `T3CODE_FEATURE_SECRETS`; without it the field is the plain secret input ported here,
//! and a saved secret reference is kept unless replaced or cleared.

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::{
    App, AppContext as _, Context, Entity, Font, FontWeight, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, StyledText, Subscription,
    TextRun, WeakEntity, Window, div, prelude::FluentBuilder as _, px,
};
use serde_json::{Value, json};

use super::hosts::{self, EasHost, SimlockHost, SshHost, TokenEntry};
use super::{failure_message, muted_text, notify_error, outline_button, test_host, update_settings};
use crate::app::Workspace;
use crate::device::PlatformAvailability;
use crate::rpc::RpcClient;
use crate::theme::{FONT_MONO, Tokens, tokens};

trait Editor: Render {
    fn busy(&self) -> bool;
}

fn new_host_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Opens `editor` as a modal that cannot be dismissed while it saves.
fn open_editor<E: Editor>(editor: Entity<E>, window: &mut Window, cx: &mut App) {
    window.open_dialog(cx, move |dialog, _, cx| {
        let busy = editor.read(cx).busy();
        let t = tokens(cx);
        dialog
            .p_0()
            .w(px(512.))
            .bg(t.popover)
            .rounded(px(16.))
            .close_button(!busy)
            .overlay_closable(!busy)
            .keyboard(!busy)
            .child(editor.clone())
    });
}

fn text_input(window: &mut Window, cx: &mut App, value: &str, placeholder: &str, masked: bool) -> Entity<InputState> {
    let (value, placeholder) = (value.to_owned(), placeholder.to_owned());
    cx.new(|cx| InputState::new(window, cx).masked(masked).placeholder(placeholder).default_value(value))
}

/// Re-renders on every edit and saves on Enter.
fn watch<E: Editor>(
    input: &Entity<InputState>,
    window: &mut Window,
    cx: &mut Context<E>,
    submit: fn(&mut E, &mut Window, &mut Context<E>),
) -> Subscription {
    cx.subscribe_in(input, window, move |this, _, event: &InputEvent, window, cx| match event {
        InputEvent::Change => cx.notify(),
        InputEvent::PressEnter { .. } => submit(this, window, cx),
        _ => {}
    })
}

fn value(input: &Entity<InputState>, cx: &App) -> String {
    input.read(cx).value().to_string()
}

/// A number field; anything that is not a whole number fails the range checks.
fn number(input: &Entity<InputState>, cx: &App) -> i64 {
    value(input, cx).trim().parse().unwrap_or(0)
}

fn header(title: &'static str, description: impl Into<SharedString>, t: &Tokens) -> impl IntoElement {
    v_flex()
        .gap(px(8.))
        .p(px(24.))
        .pb(px(12.))
        .child(div().text_xl().font_weight(FontWeight::SEMIBOLD).text_color(t.foreground).child(title))
        .child(div().text_sm().text_color(t.muted_foreground).child(description.into()))
}

fn field(label: impl Into<SharedString>, control: impl IntoElement) -> impl IntoElement {
    v_flex().gap(px(6.)).text_sm().child(label.into()).child(control)
}

fn input(state: &Entity<InputState>, busy: bool) -> Input {
    Input::new(state).small().disabled(busy)
}

fn footer<E: Editor>(
    can_save: bool,
    busy: bool,
    t: &Tokens,
    cx: &mut Context<E>,
    save: fn(&mut E, &mut Window, &mut Context<E>),
) -> impl IntoElement {
    h_flex()
        .justify_end()
        .gap(px(8.))
        .px(px(24.))
        .py(px(16.))
        .border_t_1()
        .border_color(t.border)
        .bg(t.muted.opacity(0.72))
        .rounded_b(px(15.))
        .child(
            Button::new("device-editor-cancel")
                .ghost()
                .small()
                .h(px(32.))
                .label("Cancel")
                .disabled(busy)
                .on_click(|_, window, cx| window.close_dialog(cx)),
        )
        .child(
            Button::new("device-editor-save")
                .primary()
                .small()
                .h(px(32.))
                .label("Save host")
                .loading(busy)
                .disabled(busy || !can_save)
                .on_click(cx.listener(move |this, _, window, cx| save(this, window, cx))),
        )
}

/// Writes `patch` and closes the dialog, or keeps it open and says why not.
fn save_patch<E: Editor>(
    client: RpcClient,
    patch: Value,
    failed: &'static str,
    environment: String,
    set_busy: fn(&mut E, bool),
    window: &mut Window,
    cx: &mut Context<E>,
) {
    cx.spawn_in(window, async move |this, cx| {
        let result = update_settings(&client, patch).await;
        let _ = this.update_in(cx, |this, window, cx| {
            set_busy(this, false);
            cx.notify();
            match result {
                Ok(()) => window.close_dialog(cx),
                Err(error) => notify_error(window, cx, failed, format!("{environment}: {error}")),
            }
        });
    })
    .detach();
}

/// The connection and settings an editor saves against, from the workspace.
fn context(workspace: &WeakEntity<Workspace>, cx: &App) -> Option<(RpcClient, Value, String)> {
    let workspace = workspace.upgrade()?;
    let workspace = workspace.read(cx);
    Some((workspace.client()?, (*workspace.settings_info().settings).clone(), workspace.environment_name()))
}

fn token_placeholder(configured: bool, otherwise: &'static str) -> &'static str {
    if configured { "Stored secret, enter a new value to replace" } else { otherwise }
}

fn clear_button(id: &'static str, busy: bool, t: &Tokens) -> Button {
    Button::new(id)
        .ghost()
        .small()
        .size(px(28.))
        .text_color(t.muted_foreground)
        .icon(Icon::new(Lucide::X).with_size(px(14.)))
        .disabled(busy)
}

impl Workspace {
    pub(super) fn open_ssh_editor(&mut self, host: Option<SshHost>, window: &mut Window, cx: &mut Context<Self>) {
        let workspace = cx.weak_entity();
        let environment = self.environment_name();
        let editor = cx.new(|cx| SshEditor::new(workspace, environment, host, window, cx));
        open_editor(editor, window, cx);
    }

    pub(super) fn open_eas_editor(&mut self, host: Option<EasHost>, window: &mut Window, cx: &mut Context<Self>) {
        let workspace = cx.weak_entity();
        let client = self.settings_client();
        let editor = cx.new(|cx| EasEditor::new(workspace, client, host, window, cx));
        open_editor(editor, window, cx);
    }

    pub(super) fn open_simlock_editor(
        &mut self,
        host: Option<SimlockHost>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let workspace = cx.weak_entity();
        let client = self.settings_client();
        let editor = cx.new(|cx| SimlockEditor::new(workspace, client, host, window, cx));
        open_editor(editor, window, cx);
    }
}

#[derive(Clone)]
enum Check {
    Pending,
    Local,
    Connected(Vec<PlatformAvailability>),
    Failed(String),
}

type ConnectionKey = (String, Option<i64>, Option<String>);

struct SshEditor {
    workspace: WeakEntity<Workspace>,
    environment: String,
    id: String,
    /// The saved host, matched by id or destination on save; `None` adds one.
    original: Option<SshHost>,
    name: Entity<InputState>,
    target: Entity<InputState>,
    identity: Entity<InputState>,
    port: Entity<InputState>,
    options_open: bool,
    /// The latest connection check and the connection it answers for.
    check: Option<(ConnectionKey, Check)>,
    show_error: bool,
    busy: bool,
    _subscriptions: Vec<Subscription>,
}

impl Editor for SshEditor {
    fn busy(&self) -> bool {
        self.busy
    }
}

impl SshEditor {
    fn new(
        workspace: WeakEntity<Workspace>,
        environment: String,
        original: Option<SshHost>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let host = original.clone();
        let text = |read: fn(&SshHost) -> String| host.as_ref().map(read).unwrap_or_default();
        let name = text_input(window, cx, &text(|h| h.label.clone()), "Mac mini", false);
        let target = text_input(window, cx, &text(|h| h.target.clone()), "user@host or SSH alias", false);
        let identity =
            text_input(window, cx, &text(|h| h.identity_file.clone().unwrap_or_default()), "SSH config default", false);
        let port =
            text_input(window, cx, &text(|h| h.port.map(|p| p.to_string()).unwrap_or_default()), "Default", false);
        name.update(cx, |input, cx| input.focus(window, cx));
        let subscriptions = [&name, &target, &identity, &port].map(|input| watch(input, window, cx, Self::save)).into();
        Self {
            workspace,
            environment,
            id: host.as_ref().map_or_else(new_host_id, |host| host.id.clone()),
            options_open: host.as_ref().is_some_and(|host| host.port.is_some() || host.identity_file.is_some()),
            original,
            name,
            target,
            identity,
            port,
            check: None,
            show_error: false,
            busy: false,
            _subscriptions: subscriptions,
        }
    }

    /// The fields as typed; `None` when the port is not a whole number.
    fn draft(&self, cx: &App) -> Option<SshHost> {
        let port = value(&self.port, cx);
        let port = match port.trim() {
            "" => None,
            port => Some(port.parse().ok()?),
        };
        let identity = value(&self.identity, cx);
        Some(SshHost {
            kind: "ssh".into(),
            id: self.id.clone(),
            label: value(&self.name, cx),
            target: value(&self.target, cx),
            identity_file: (!identity.is_empty()).then_some(identity),
            port,
            extra: serde_json::Map::new(),
        })
    }

    /// The host to test or save; a blank name tests under the target.
    fn input(&self, cx: &App) -> Option<SshHost> {
        let draft = self.draft(cx)?;
        let label = if draft.label.trim().is_empty() { draft.target.clone() } else { draft.label.clone() };
        hosts::parse_ssh_draft(&SshHost { label, ..draft })
    }

    fn current_check(&self, cx: &App) -> Option<&Check> {
        let key = hosts::ssh_connection_key(&self.draft(cx)?);
        self.check.as_ref().filter(|(checked, _)| *checked == key).map(|(_, check)| check)
    }

    fn checking(&self, cx: &App) -> bool {
        matches!(self.current_check(cx), Some(Check::Pending))
    }

    fn test(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(host) = self.input(cx) else { return };
        let Some((client, _, _)) = context(&self.workspace, cx) else { return };
        let key = hosts::ssh_connection_key(&host);
        self.check = Some((key.clone(), Check::Pending));
        self.show_error = false;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = test_host(&client, &host).await;
            let check = match result {
                Ok(summary) if summary.get("kind").and_then(Value::as_str) == Some("local") => Check::Local,
                Ok(summary) => {
                    Check::Connected(serde_json::from_value(summary["platforms"].clone()).unwrap_or_default())
                }
                Err(error) => Check::Failed(error),
            };
            let _ = this.update(cx, |this, cx| {
                if this.check.as_ref().is_some_and(|(checked, _)| *checked == key) {
                    this.check = Some((key, check));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let named = self.draft(cx).is_some_and(|draft| !draft.label.trim().is_empty());
        let Some(host) = self.input(cx).filter(|_| named && !self.busy && !self.checking(cx)) else { return };
        let Some((client, settings, environment)) = context(&self.workspace, cx) else { return };
        let original = self.original.clone().unwrap_or_else(|| host.clone());
        let next = hosts::host_list(&settings, "deviceHosts")
            .and_then(|saved| hosts::update_device_hosts(&saved, &host, false, &original).map_err(str::to_owned));
        let next = match next {
            Ok(next) => next,
            Err(error) => return notify_error(window, cx, "Device host not saved", error),
        };
        self.busy = true;
        cx.notify();
        let patch = json!({"deviceHosts": next});
        save_patch(
            client,
            patch,
            "Device host not saved everywhere",
            environment,
            |this: &mut Self, busy| this.busy = busy,
            window,
            cx,
        );
    }

    fn render_check(&self, t: &Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let check = self.current_check(cx).cloned();
        let valid = self.input(cx).is_some();
        let checking = matches!(check, Some(Check::Pending));
        let summary = match &check {
            Some(Check::Pending) => "Checking environments…",
            Some(Check::Failed(_)) => "1 of 1 failed",
            Some(_) => "Connection checks passed",
            None => "Check access before saving",
        };
        let result = check.map(|check| {
            let (icon, text, color) = match &check {
                Check::Pending => (None, "Checking…", t.muted_foreground),
                Check::Local => (Some(Lucide::Monitor), "Already available locally", t.muted_foreground),
                Check::Failed(_) => (Some(Lucide::X), "Failed", t.error),
                Check::Connected(_) => (Some(Lucide::Check), "Connected", t.muted_foreground),
            };
            v_flex()
                .px(px(12.))
                .py(px(10.))
                .text_xs()
                .border_t_1()
                .border_color(t.border.opacity(0.6))
                .child(
                    h_flex()
                        .justify_between()
                        .gap(px(12.))
                        .child(div().truncate().font_weight(FontWeight::MEDIUM).child(self.environment.clone()))
                        .child(
                            h_flex()
                                .flex_none()
                                .gap(px(6.))
                                .text_color(color)
                                .when_some(icon, |this, icon| this.child(Icon::new(icon).with_size(px(12.))))
                                .child(text),
                        ),
                )
                .map(|this| match check {
                    Check::Connected(platforms) => this.child(availability(&platforms, t)),
                    Check::Failed(error) => this.child(
                        v_flex()
                            .mt(px(6.))
                            .text_color(t.muted_foreground)
                            .child(div().id("device-host-show-error").cursor_pointer().child("Show error").on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.show_error = !this.show_error;
                                    cx.notify();
                                }),
                            ))
                            .when(self.show_error, |this| {
                                this.child(
                                    div()
                                        .id("device-host-error")
                                        .mt(px(4.))
                                        .max_h(px(128.))
                                        .overflow_y_scroll()
                                        .child(error),
                                )
                            }),
                    ),
                    _ => this,
                })
        });
        v_flex()
            .rounded(px(8.))
            .border_1()
            .border_color(t.border.opacity(0.6))
            .child(
                h_flex()
                    .justify_between()
                    .gap(px(12.))
                    .px(px(12.))
                    .py(px(10.))
                    .child(muted_text(summary, t.muted_foreground))
                    .child(
                        outline_button("device-host-test")
                            .label("Test connection")
                            .loading(checking)
                            .disabled(self.busy || checking || !valid)
                            .on_click(cx.listener(|this, _, window, cx| this.test(window, cx))),
                    ),
            )
            .children(result)
    }
}

/// One wrapping paragraph whose flagged parts are `<code>`.
fn with_code(parts: &[(&str, bool)], color: Hsla, window: &Window) -> StyledText {
    let text: String = parts.iter().map(|(part, _)| *part).collect();
    let base = window.text_style().font();
    let runs = parts
        .iter()
        .map(|(part, code)| TextRun {
            len: part.len(),
            font: if *code { Font { family: FONT_MONO.into(), ..base.clone() } } else { base.clone() },
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        })
        .collect();
    StyledText::new(text).with_runs(runs)
}

/// `DeviceHostAvailability`.
fn availability(platforms: &[PlatformAvailability], t: &Tokens) -> impl IntoElement {
    h_flex().mt(px(6.)).flex_wrap().gap_x(px(16.)).gap_y(px(4.)).text_color(t.muted_foreground).children(
        platforms.iter().map(|platform| {
            let name = if platform.platform == "ios" { "iOS" } else { "Android" };
            let state = if platform.available { "available" } else { "unavailable" };
            h_flex()
                .gap(px(4.))
                .child(Icon::new(if platform.available { Lucide::Check } else { Lucide::Minus }).with_size(px(12.)))
                .child(format!("{name} {state}"))
        }),
    )
}

impl Render for SshEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *tokens(cx);
        let busy = self.busy;
        let can_save = self.input(cx).is_some()
            && self.draft(cx).is_some_and(|draft| !draft.label.trim().is_empty())
            && !self.checking(cx);
        let options_open = self.options_open;
        v_flex()
            .child(header(
                if self.original.is_none() { "Add device host" } else { "Edit device host" },
                format!("Connect from {}. Hosts on the same machine are skipped.", self.environment),
                &t,
            ))
            .child(
                v_flex()
                    .gap(px(16.))
                    .px(px(24.))
                    .pt(px(4.))
                    .pb(px(24.))
                    .child(field("Name", input(&self.name, busy)))
                    .child(field("SSH target", input(&self.target, busy)))
                    .child(
                        v_flex()
                            .text_sm()
                            .child(
                                h_flex()
                                    .id("device-host-options")
                                    .gap(px(4.))
                                    .cursor_pointer()
                                    .text_color(t.muted_foreground)
                                    .child(
                                        Icon::new(if options_open {
                                            Lucide::ChevronDown
                                        } else {
                                            Lucide::ChevronRight
                                        })
                                        .with_size(px(12.)),
                                    )
                                    .child("SSH options")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.options_open = !this.options_open;
                                        cx.notify();
                                    })),
                            )
                            .when(options_open, |this| {
                                this.child(
                                    h_flex()
                                        .mt(px(12.))
                                        .gap(px(12.))
                                        .items_start()
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .child(field("Identity file", input(&self.identity, busy))),
                                        )
                                        .child(
                                            div().w(px(112.)).flex_none().child(field("Port", input(&self.port, busy))),
                                        ),
                                )
                                .child(
                                    muted_text(
                                        "Optional. Resolved separately on each environment.",
                                        t.muted_foreground,
                                    )
                                    .mt(px(8.)),
                                )
                            }),
                    )
                    .child(self.render_check(&t, cx)),
            )
            .child(footer(can_save, busy, &t, cx, Self::save))
    }
}

const CREDENTIAL_SOURCES: [(&str, &str); 3] = [
    ("settings", "the access token saved here"),
    ("env", "EXPO_TOKEN on the server"),
    ("eas-login", "the server's eas login"),
];

#[derive(Clone)]
enum AccountCheck {
    Idle,
    Checking,
    SignedIn { signed_in_as: String, credential_source: String, apps: Vec<(String, String)> },
    Failed(String),
}

struct EasEditor {
    workspace: WeakEntity<Workspace>,
    /// Taken at open: the editor is built while the workspace is being updated.
    client: Option<RpcClient>,
    host: EasHost,
    is_new: bool,
    name: Entity<InputState>,
    token_input: Entity<InputState>,
    /// A saved token arrives redacted; only a replacement or a clear changes it.
    token: TokenEntry,
    app_id: Option<String>,
    sessions: Entity<InputState>,
    session_minutes: Entity<InputState>,
    idle_minutes: Entity<InputState>,
    account: AccountCheck,
    /// Bumped by every check and token edit, so a stale answer is dropped.
    check_run: u64,
    busy: bool,
    _subscriptions: Vec<Subscription>,
}

impl Editor for EasEditor {
    fn busy(&self) -> bool {
        self.busy
    }
}

fn has_token(token: &Value) -> bool {
    match token {
        Value::String(token) => !token.is_empty(),
        Value::Null => false,
        _ => true,
    }
}

impl EasEditor {
    fn new(
        workspace: WeakEntity<Workspace>,
        client: Option<RpcClient>,
        host: Option<EasHost>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let is_new = host.is_none();
        let host = host.unwrap_or_else(|| hosts::new_eas_host(new_host_id()));
        let saved = has_token(&host.access_token);
        let configured = saved && host.access_token.is_string();
        let name = text_input(window, cx, &host.label, "", false);
        let token_input = text_input(window, cx, "", token_placeholder(configured, "Optional"), true);
        let sessions = text_input(window, cx, &host.max_concurrent_sessions.to_string(), "", false);
        let session_minutes = text_input(window, cx, &host.max_session_minutes.to_string(), "", false);
        let idle_minutes = text_input(window, cx, &host.idle_minutes.to_string(), "", false);
        name.update(cx, |input, cx| input.focus(window, cx));
        let mut subscriptions: Vec<_> = [&name, &sessions, &session_minutes, &idle_minutes]
            .map(|input| watch(input, window, cx, Self::save))
            .into();
        subscriptions.push(cx.subscribe_in(&token_input, window, |this, input, event: &InputEvent, window, cx| {
            match event {
                InputEvent::Change => {
                    // Emptying the field is not clearing the saved token; the Clear button is.
                    let typed = value(input, cx);
                    this.token = if !typed.is_empty() {
                        TokenEntry::New(typed)
                    } else if has_token(&this.host.access_token) {
                        TokenEntry::Saved
                    } else {
                        TokenEntry::Cleared
                    };
                    this.reset_account();
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => this.save(window, cx),
                _ => {}
            }
        }));
        let mut editor = Self {
            workspace,
            client,
            app_id: host.app_id.clone(),
            token: if saved { TokenEntry::Saved } else { TokenEntry::Cleared },
            host,
            is_new,
            name,
            token_input,
            sessions,
            session_minutes,
            idle_minutes,
            account: AccountCheck::Idle,
            check_run: 0,
            busy: false,
            _subscriptions: subscriptions,
        };
        // Checking on open names the saved project and shows a rejected credential.
        let input = if !saved {
            json!({"accessToken": ""})
        } else if is_new {
            json!({})
        } else {
            json!({"hostId": editor.host.id})
        };
        editor.run_check(input, window, cx);
        editor
    }

    fn reset_account(&mut self) {
        self.check_run += 1;
        self.account = AccountCheck::Idle;
    }

    fn check_account(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut input = json!({});
        if self.token == TokenEntry::Saved && !self.is_new {
            input["hostId"] = self.host.id.clone().into();
        }
        match &self.token {
            TokenEntry::New(token) => input["accessToken"] = token.clone().into(),
            // A cleared token is sent empty, so the check does not use the saved one Save drops.
            TokenEntry::Cleared => input["accessToken"] = "".into(),
            TokenEntry::Saved => {}
        }
        self.run_check(input, window, cx);
    }

    fn run_check(&mut self, input: Value, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else { return };
        self.check_run += 1;
        let run = self.check_run;
        self.account = AccountCheck::Checking;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("device.easAccount", input).await;
            let account = match result {
                Ok(account) => AccountCheck::SignedIn {
                    signed_in_as: account["signedInAs"].as_str().unwrap_or_default().to_owned(),
                    credential_source: account["credentialSource"].as_str().unwrap_or_default().to_owned(),
                    apps: account["apps"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|app| Some((app["id"].as_str()?.to_owned(), app["fullName"].as_str()?.to_owned())))
                        .collect(),
                },
                Err(error) => AccountCheck::Failed(failure_message(&error)),
            };
            let _ = this.update(cx, |this, cx| {
                if this.check_run == run {
                    this.account = account;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn next(&self, cx: &App) -> EasHost {
        EasHost {
            label: value(&self.name, cx).trim().to_owned(),
            app_id: self.app_id.clone(),
            access_token: match &self.token {
                TokenEntry::Saved => self.host.access_token.clone(),
                TokenEntry::New(token) => token.clone().into(),
                TokenEntry::Cleared => "".into(),
            },
            max_concurrent_sessions: number(&self.sessions, cx),
            max_session_minutes: number(&self.session_minutes, cx),
            idle_minutes: number(&self.idle_minutes, cx),
            ..self.host.clone()
        }
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let next = self.next(cx);
        if self.busy || !hosts::valid_eas_draft(&next) {
            return;
        }
        let Some((client, settings, environment)) = context(&self.workspace, cx) else { return };
        let keep_token = !self.is_new && self.token == TokenEntry::Saved;
        let saved = match hosts::host_list(&settings, "easDeviceHosts") {
            Ok(saved) => saved,
            Err(error) => return notify_error(window, cx, "EAS host not saved", error),
        };
        let hosts = hosts::update_eas_hosts(&saved, &next, false, keep_token);
        self.busy = true;
        cx.notify();
        let patch = json!({"easDeviceHosts": hosts});
        save_patch(
            client,
            patch,
            "EAS host not saved everywhere",
            environment,
            |this: &mut Self, busy| this.busy = busy,
            window,
            cx,
        );
    }

    fn clear_token(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.token = TokenEntry::Cleared;
        self.token_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.set_placeholder("Optional", window, cx);
        });
        self.reset_account();
        cx.notify();
    }

    fn render_project(&self, t: &Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        const FROM_THREAD: &str = "From the thread's project";
        let apps = match &self.account {
            AccountCheck::SignedIn { apps, .. } => apps.clone(),
            _ => Vec::new(),
        };
        let selected: SharedString = match &self.app_id {
            None => FROM_THREAD.into(),
            Some(id) => apps.iter().find(|(app, _)| app == id).map_or(id.clone(), |(_, name)| name.clone()).into(),
        };
        let mut options: Vec<(Option<String>, String)> = vec![(None, FROM_THREAD.into())];
        if let Some(id) = self.app_id.as_ref().filter(|id| !apps.iter().any(|(app, _)| app == *id)) {
            options.push((Some(id.clone()), id.clone()));
        }
        options.extend(apps.into_iter().map(|(id, name)| (Some(id), name)));
        let current = self.app_id.clone();
        let view = cx.entity();
        v_flex()
            .gap(px(6.))
            .text_sm()
            .child("EAS project")
            .child(
                outline_button("eas-project")
                    .w_full()
                    .justify_between()
                    .disabled(self.busy)
                    .child(div().flex_1().truncate().child(selected))
                    .child(Icon::new(Lucide::ChevronDown).with_size(px(14.)).text_color(t.muted_foreground))
                    .dropdown_menu(move |mut menu, window, _| {
                        for (id, label) in &options {
                            let id = id.clone();
                            menu = menu.item(PopupMenuItem::new(label.clone()).checked(id == current).on_click(
                                window.listener_for(&view, move |this: &mut Self, _, _, cx| {
                                    this.app_id = id.clone();
                                    cx.notify();
                                }),
                            ));
                        }
                        menu
                    }),
            )
            .child(muted_text(
                "Sessions bill the project's Expo account. \"From the thread's project\" reads extra.eas.projectId from the project's app config.",
                t.muted_foreground,
            ))
    }
}

impl Render for EasEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *tokens(cx);
        let busy = self.busy;
        let next = self.next(cx);
        let valid = hosts::valid_eas_draft(&next);
        // Why Save is off, for the limits the fields' ranges do not show.
        let problem = (!valid).then(|| {
            if next.max_session_minutes > 0 && next.idle_minutes >= next.max_session_minutes {
                "Idle minutes must be smaller than session minutes."
            } else {
                "Give the host a label and limits within the ranges shown."
            }
        });
        let status: SharedString = match &self.account {
            AccountCheck::SignedIn { signed_in_as, credential_source, .. } => {
                let source = CREDENTIAL_SOURCES
                    .iter()
                    .find(|(key, _)| key == credential_source)
                    .map_or(credential_source.as_str(), |(_, text)| text);
                format!("Signed in as {signed_in_as} with {source}.").into()
            }
            AccountCheck::Failed(error) => error.clone().into(),
            AccountCheck::Checking => "Checking the Expo account…".into(),
            AccountCheck::Idle => "Check the Expo account before choosing a project.".into(),
        };
        let checking = matches!(self.account, AccountCheck::Checking);
        let limit = |label: &'static str, input_state: &Entity<InputState>| {
            div().flex_1().min_w_0().child(field(label, input(input_state, busy)))
        };
        v_flex()
            .child(header(
                if self.is_new { "Add EAS host" } else { "Edit EAS host" },
                "The environment's server signs in to Expo and starts the sessions. Without a token here it uses EXPO_TOKEN, then its eas login.",
                &t,
            ))
            .child(
                v_flex()
                    .gap(px(16.))
                    .px(px(24.))
                    .pt(px(4.))
                    .pb(px(24.))
                    .child(field("Name", input(&self.name, busy)))
                    .child(field(
                        "Expo access token",
                        h_flex()
                            .gap(px(8.))
                            .child(div().flex_1().min_w_0().child(input(&self.token_input, busy)))
                            .when(self.token != TokenEntry::Cleared, |this| {
                                this.child(
                                    clear_button("eas-token-clear", busy, &t)
                                        .on_click(cx.listener(|this, _, window, cx| this.clear_token(window, cx))),
                                )
                            }),
                    ))
                    .child(
                        h_flex()
                            .justify_between()
                            .gap(px(12.))
                            .rounded(px(8.))
                            .border_1()
                            .border_color(t.border.opacity(0.6))
                            .px(px(12.))
                            .py(px(10.))
                            .child(muted_text(status, t.muted_foreground).min_w_0())
                            .child(
                                outline_button("eas-check-account")
                                    .label("Check account")
                                    .loading(checking)
                                    .disabled(busy || checking)
                                    .on_click(cx.listener(|this, _, window, cx| this.check_account(window, cx))),
                            ),
                    )
                    .child(self.render_project(&t, cx))
                    .child(
                        h_flex()
                            .gap(px(12.))
                            .items_start()
                            .child(limit("Sessions at once", &self.sessions))
                            .child(limit("Session minutes", &self.session_minutes))
                            .child(limit("Idle minutes", &self.idle_minutes)),
                    )
                    .child(muted_text(
                        format!(
                            "Expo allows sessions of up to 40 minutes on free accounts and 115 on paid ones, and ends one after the idle minutes without activity, so idle minutes must be smaller than session minutes.{}",
                            if self.is_new { "" } else { " Saving restarts this host, which stops its running sessions." }
                        ),
                        t.muted_foreground,
                    ))
                    .when_some(problem, |this, problem| this.child(muted_text(problem, t.error))),
            )
            .child(footer(valid, busy, &t, cx, Self::save))
    }
}

/// The token check under the Simlock token field.
#[derive(Clone)]
enum Identity {
    Checking,
    Accepted { signed_in_as: String, from_machine: bool },
    Failed(String),
}

struct SimlockEditor {
    workspace: WeakEntity<Workspace>,
    /// Taken at open: the editor is built while the workspace is being updated.
    client: Option<RpcClient>,
    host: SimlockHost,
    is_new: bool,
    name: Entity<InputState>,
    gateway: Entity<InputState>,
    /// The gateway field was typed in, so a detected gateway no longer fills it.
    url_edited: bool,
    token_input: Entity<InputState>,
    token: TokenEntry,
    leases: Entity<InputState>,
    lease_minutes: Entity<InputState>,
    /// The check's answer and the (gateway, token) it was asked about.
    identity: Option<(Value, Identity)>,
    busy: bool,
    _subscriptions: Vec<Subscription>,
}

impl Editor for SimlockEditor {
    fn busy(&self) -> bool {
        self.busy
    }
}

impl SimlockEditor {
    fn new(
        workspace: WeakEntity<Workspace>,
        client: Option<RpcClient>,
        host: Option<SimlockHost>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let is_new = host.is_none();
        let host = host.unwrap_or_else(|| hosts::new_simlock_host(new_host_id()));
        let saved = !host.agent_token.is_empty();
        let name = text_input(window, cx, &host.label, "", false);
        let gateway = text_input(window, cx, &host.gateway_url, "https://gateway.example.ts.net:4700", false);
        let token_input = text_input(window, cx, "", token_placeholder(saved, "Not set"), true);
        let leases = text_input(window, cx, &host.max_leases.to_string(), "", false);
        let lease_minutes = text_input(window, cx, &host.max_lease_minutes.to_string(), "", false);
        name.update(cx, |input, cx| input.focus(window, cx));
        let mut subscriptions: Vec<_> =
            [&name, &leases, &lease_minutes].map(|input| watch(input, window, cx, Self::save)).into();
        subscriptions.push(cx.subscribe_in(&gateway, window, |this, _, event: &InputEvent, window, cx| match event {
            InputEvent::Change => {
                this.url_edited = true;
                this.check_identity(window, cx);
            }
            InputEvent::PressEnter { .. } => this.save(window, cx),
            _ => {}
        }));
        subscriptions.push(cx.subscribe_in(&token_input, window, |this, input, event: &InputEvent, window, cx| {
            match event {
                InputEvent::Change => {
                    // Emptying the field is not clearing the saved token; the Clear button is.
                    let typed = value(input, cx);
                    this.token = if !typed.is_empty() {
                        TokenEntry::New(typed)
                    } else if !this.host.agent_token.is_empty() {
                        TokenEntry::Saved
                    } else {
                        TokenEntry::Cleared
                    };
                    this.check_identity(window, cx);
                }
                InputEvent::PressEnter { .. } => this.save(window, cx),
                _ => {}
            }
        }));
        let mut editor = Self {
            workspace,
            client,
            token: if saved { TokenEntry::Saved } else { TokenEntry::Cleared },
            host,
            is_new,
            name,
            gateway,
            url_edited: false,
            token_input,
            leases,
            lease_minutes,
            identity: None,
            busy: false,
            _subscriptions: subscriptions,
        };
        if is_new {
            editor.detect_gateway(window, cx);
        }
        editor.check_identity(window, cx);
        editor
    }

    /// A new host starts at the gateway the machine's own agent-device already uses.
    fn detect_gateway(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else { return };
        cx.spawn_in(window, async move |this, cx| {
            let Ok(identity) = client.request("device.simlockIdentity", json!({})).await else { return };
            let Some(url) = identity["gatewayUrl"].as_str().filter(|url| !url.is_empty()).map(str::to_owned) else {
                return;
            };
            let _ = this.update_in(cx, |this, window, cx| {
                if this.url_edited || !value(&this.gateway, cx).is_empty() {
                    return;
                }
                this.gateway.update(cx, |input, cx| input.set_value(url, window, cx));
                this.check_identity(window, cx);
            });
        })
        .detach();
    }

    fn next(&self, cx: &App) -> SimlockHost {
        SimlockHost {
            label: value(&self.name, cx).trim().to_owned(),
            gateway_url: value(&self.gateway, cx).trim().to_owned(),
            max_leases: number(&self.leases, cx),
            max_lease_minutes: number(&self.lease_minutes, cx),
            ..self.host.clone()
        }
    }

    /// Asks the gateway about the token once the shared fields are valid; a cleared
    /// field asks about the machine's own token, which a blank one falls back to.
    fn check_identity(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.notify();
        let next = self.next(cx);
        if hosts::simlock_problem(&next).is_some() {
            self.identity = None;
            return;
        }
        let mut input = json!({"gatewayUrl": next.gateway_url});
        match &self.token {
            TokenEntry::New(token) if !token.trim().is_empty() => input["agentToken"] = token.trim().into(),
            TokenEntry::Saved if !self.is_new => input["hostId"] = self.host.id.clone().into(),
            _ => {}
        }
        if self.identity.as_ref().is_some_and(|(asked, _)| *asked == input) {
            return;
        }
        let Some(client) = self.client.clone() else { return };
        self.identity = Some((input.clone(), Identity::Checking));
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("device.simlockIdentity", input.clone()).await;
            let identity = match result {
                Ok(identity) => Identity::Accepted {
                    signed_in_as: identity["signedInAs"].as_str().unwrap_or_default().to_owned(),
                    from_machine: identity["tokenSource"].as_str() == Some("env"),
                },
                Err(error) => Identity::Failed(failure_message(&error)),
            };
            let _ = this.update(cx, |this, cx| {
                if this.identity.as_ref().is_some_and(|(asked, _)| *asked == input) {
                    this.identity = Some((input, identity));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some((client, settings, environment)) = context(&self.workspace, cx) else { return };
        let saved = match hosts::host_list(&settings, "simlockDeviceHosts") {
            Ok(saved) => saved,
            Err(error) => return notify_error(window, cx, "Simlock host not saved", error),
        };
        let Ok(hosts) = hosts::plan_simlock_save(&saved, &self.next(cx), &self.token) else { return };
        self.busy = true;
        cx.notify();
        let patch = json!({"simlockDeviceHosts": hosts});
        save_patch(
            client,
            patch,
            "Simlock host not saved everywhere",
            environment,
            |this: &mut Self, busy| this.busy = busy,
            window,
            cx,
        );
    }

    fn clear_token(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.token = TokenEntry::Cleared;
        self.token_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.set_placeholder("Not set", window, cx);
        });
        self.check_identity(window, cx);
    }
}

impl Render for SimlockEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *tokens(cx);
        let busy = self.busy;
        let next = self.next(cx);
        let problem = hosts::simlock_problem(&next);
        let status = self.identity.as_ref().map(|(_, identity)| match identity {
            Identity::Checking => muted_text("Checking…", t.muted_foreground),
            Identity::Accepted { signed_in_as, from_machine: true } => {
                muted_text(format!("Using this machine's agent-device token ({signed_in_as})."), t.muted_foreground)
            }
            Identity::Accepted { signed_in_as, .. } => {
                muted_text(format!("Accepted as {signed_in_as}."), t.muted_foreground)
            }
            Identity::Failed(error) => {
                muted_text(error.clone(), if self.token == TokenEntry::Cleared { t.muted_foreground } else { t.error })
            }
        });
        let limit = |label: &'static str, input_state: &Entity<InputState>| {
            div().flex_1().min_w_0().child(field(label, input(input_state, busy)))
        };
        v_flex()
            .child(header(
                if self.is_new { "Add Simlock host" } else { "Edit Simlock host" },
                "The environment's server leases devices from your Simlock gateway and releases them when they close.",
                &t,
            ))
            .child(
                v_flex()
                    .gap(px(16.))
                    .px(px(24.))
                    .pt(px(4.))
                    .pb(px(24.))
                    .child(field("Name", input(&self.name, busy)))
                    .child(field("Gateway URL", input(&self.gateway, busy)))
                    .child(
                        v_flex()
                            .gap(px(12.))
                            .child(
                                v_flex()
                                    .gap(px(6.))
                                    .text_sm()
                                    .child("Agent token")
                                    .child(
                                        h_flex()
                                            .gap(px(8.))
                                            .child(div().flex_1().min_w_0().child(input(&self.token_input, busy)))
                                            .when(self.token != TokenEntry::Cleared, |this| {
                                                this.child(clear_button("simlock-token-clear", busy, &t).on_click(
                                                    cx.listener(|this, _, window, cx| this.clear_token(window, cx)),
                                                ))
                                            }),
                                    )
                                    .children(status),
                            )
                            .child(div().text_xs().text_color(t.muted_foreground).child(with_code(
                                &[
                                    ("Leases, drives, and streams devices. Create one with ", false),
                                    ("simlock --gateway token create --role agent", true),
                                    (", or leave it blank to add it later. A blank token uses the machine's ", false),
                                    ("AGENT_DEVICE_DAEMON_AUTH_TOKEN", true),
                                    (" when its ", false),
                                    ("AGENT_DEVICE_DAEMON_BASE_URL", true),
                                    (" is this gateway.", false),
                                ],
                                t.muted_foreground,
                                window,
                            ))),
                    )
                    .child(
                        h_flex()
                            .gap(px(12.))
                            .items_start()
                            .child(limit("Devices at once", &self.leases))
                            .child(limit("Lease minutes", &self.lease_minutes)),
                    )
                    .child(muted_text(
                        format!(
                            "A lease ends after the lease minutes even while in use.{}",
                            if self.is_new { "" } else { " Saving restarts this host, which releases its devices." }
                        ),
                        t.muted_foreground,
                    ))
                    .when_some(problem.filter(|_| !next.gateway_url.is_empty()), |this, problem| {
                        this.child(muted_text(problem, t.error))
                    }),
            )
            .child(footer(problem.is_none(), busy, &t, cx, Self::save))
    }
}
