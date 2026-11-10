//! The source editors: `DeviceHostEditor.tsx` (SSH), `EasDeviceHostEditor.tsx` and
//! `SimlockDeviceHostEditor.tsx`, each a dialog owning its draft. Each keeps the environments
//! selected when it opened: it tests on all of them and, when Save is pressed, rewrites the host
//! list of each one still connected, read fresh from its settings. Token fields offer the secret
//! providers their environment can read from (`SecretSourceInput`).

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, Font, FontWeight, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, StyledText, Subscription,
    TextRun, WeakEntity, Window, div, prelude::FluentBuilder as _, px,
};
use serde_json::{Value, json};

use super::hosts::{self, EasHost, SimlockHost, SshHost, TokenEntry};
use super::{
    Check, Member, Write, check_target, device_writes, eas_plan, failure_message, muted_text, notify_error,
    outline_button, report_failures, run_writes, simlock_save_plan, ssh_plan,
};
use crate::app::Workspace;
use crate::app::pages::settings::secret_source::{as_reference, secret_source_input};
use crate::app::request_cards::SecretSource;
use crate::device::PlatformAvailability;
use crate::rpc::RpcClient;
use crate::theme::{FONT_MONO, Tokens, tokens};

trait Editor: Render {
    fn busy(&self) -> bool;
    fn set_busy(&mut self, busy: bool);
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

/// The writes a save makes on the editor's environments still connected, planned from their
/// settings as they are now; an error says why nothing was written.
fn plan_writes(
    workspace: &WeakEntity<Workspace>,
    environments: &[(String, String)],
    cx: &App,
    plan: impl FnOnce(&[Member]) -> Result<Vec<(String, Value)>, String>,
) -> Result<Vec<Write>, String> {
    let workspace = workspace.upgrade().ok_or_else(|| "The window closed.".to_owned())?;
    let workspace = workspace.read(cx);
    let members = workspace.members_among(environments);
    if members.is_empty() {
        return Err("None of the environments this editor opened on is connected.".into());
    }
    let patches = plan(&members)?;
    device_writes(|id| workspace.env_client(id), &members, patches)
}

/// Sends `writes` and closes the dialog, or keeps it open and toasts the environments that failed.
fn save_writes<E: Editor>(writes: Vec<Write>, failed: &'static str, window: &mut Window, cx: &mut Context<E>) {
    cx.spawn_in(window, async move |this, cx| {
        let failures = run_writes(writes).await;
        let _ = this.update_in(cx, |this, window, cx| {
            this.set_busy(false);
            cx.notify();
            if report_failures(window, cx, failed, &failures) {
                window.close_dialog(cx);
            }
        });
    })
    .detach();
}

/// Plans, checks and sends a save; a refused plan is toasted under `refused`.
fn save<E: Editor>(
    editor: &mut E,
    workspace: &WeakEntity<Workspace>,
    environments: &[(String, String)],
    titles: (&'static str, &'static str),
    plan: impl FnOnce(&[Member]) -> Result<Vec<(String, Value)>, String>,
    window: &mut Window,
    cx: &mut Context<E>,
) {
    let (refused, failed) = titles;
    match plan_writes(workspace, environments, cx, plan) {
        Ok(writes) => {
            editor.set_busy(true);
            cx.notify();
            save_writes(writes, failed, window, cx);
        }
        Err(error) => notify_error(window, cx, refused, error),
    }
}

fn token_placeholder(configured: bool, otherwise: &'static str) -> &'static str {
    if configured { "Stored secret, enter a new value to replace" } else { otherwise }
}

fn clear_button(id: impl Into<gpui_kit::ElementId>, busy: bool, t: &Tokens) -> Button {
    Button::new(id)
        .ghost()
        .small()
        .size(px(28.))
        .text_color(t.muted_foreground)
        .icon(Icon::new(Lucide::X).with_size(px(14.)))
        .disabled(busy)
}

/// A token entry's secret reference: a new one, or the saved one while it is kept.
fn token_reference<'a>(token: &'a TokenEntry, saved: &'a Value) -> Option<&'a Value> {
    match token {
        TokenEntry::New(value) => as_reference(value),
        TokenEntry::Saved => as_reference(saved),
        TokenEntry::Cleared => None,
    }
}

impl Workspace {
    /// The environments Settings selects, `(id, label)`, for an editor to keep.
    fn editor_environments(&self) -> Vec<(String, String)> {
        let view = self.settings_view();
        view.selected_environments().iter().map(|e| (e.id.to_owned(), e.label.to_owned())).collect()
    }

    /// `only` limits the save to those environments, for a host that differs between them.
    pub(super) fn open_ssh_editor(
        &mut self,
        host: Option<SshHost>,
        only: Option<Vec<String>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (workspace, environments) = (cx.weak_entity(), self.editor_environments());
        let editor = cx.new(|cx| SshEditor::new(workspace, environments, only, host, window, cx));
        open_editor(editor, window, cx);
    }

    /// `check_on`: the holder whose account check the editor shows; a new host checks on the
    /// environment Settings reads.
    pub(super) fn open_eas_editor(
        &mut self,
        host: Option<EasHost>,
        check_on: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (workspace, environments) = (cx.weak_entity(), self.editor_environments());
        let check_on = check_on.or_else(|| self.settings_environment().map(|id| id.to_string()));
        let sources = check_on.as_deref().map(|id| self.secret_sources_on(id)).unwrap_or_default();
        let editor = cx.new(|cx| EasEditor::new(workspace, environments, check_on, sources, host, window, cx));
        open_editor(editor, window, cx);
    }

    pub(super) fn open_simlock_editor(
        &mut self,
        host: Option<SimlockHost>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (workspace, environments) = (cx.weak_entity(), self.editor_environments());
        let id = host.as_ref().map_or_else(new_host_id, |host| host.id.clone());
        let fields: Vec<TokenSeed> = self
            .members_among(&environments)
            .iter()
            .map(|member| TokenSeed {
                environment: member.id.to_owned(),
                label: member.label.to_owned(),
                sources: self.secret_sources_on(member.id),
                saved: hosts::readable_hosts::<SimlockHost>(member.settings, "simlockDeviceHosts")
                    .into_iter()
                    .find(|saved| saved.id == id)
                    .map_or_else(|| json!(""), |saved| saved.agent_token),
            })
            .collect();
        let editor = cx.new(|cx| SimlockEditor::new(workspace, environments, fields, host, id, window, cx));
        open_editor(editor, window, cx);
    }
}

type ConnectionKey = (String, Option<i64>, Option<String>);

struct SshEditor {
    workspace: WeakEntity<Workspace>,
    /// The environments selected at open: checked on, and saved to while connected.
    environments: Vec<(String, String)>,
    only: Option<Vec<String>>,
    id: String,
    /// The saved host, matched by id or destination on save; `None` adds one.
    original: Option<SshHost>,
    name: Entity<InputState>,
    target: Entity<InputState>,
    identity: Entity<InputState>,
    port: Entity<InputState>,
    options_open: bool,
    /// The latest checks, per environment, and the connection they answer for.
    checks: Option<(ConnectionKey, Vec<Check>)>,
    /// The per-environment results' fold, as toggled for one connection.
    checks_open: Option<(ConnectionKey, bool)>,
    /// Environments whose check error is shown.
    shown_errors: Vec<String>,
    busy: bool,
    _subscriptions: Vec<Subscription>,
}

impl Editor for SshEditor {
    fn busy(&self) -> bool {
        self.busy
    }

    fn set_busy(&mut self, busy: bool) {
        self.busy = busy;
    }
}

impl SshEditor {
    fn new(
        workspace: WeakEntity<Workspace>,
        environments: Vec<(String, String)>,
        only: Option<Vec<String>>,
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
            environments,
            only,
            id: host.as_ref().map_or_else(new_host_id, |host| host.id.clone()),
            options_open: host.as_ref().is_some_and(|host| host.port.is_some() || host.identity_file.is_some()),
            original,
            name,
            target,
            identity,
            port,
            checks: None,
            checks_open: None,
            shown_errors: Vec::new(),
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

    fn key(&self, cx: &App) -> Option<ConnectionKey> {
        Some(hosts::ssh_connection_key(&self.draft(cx)?))
    }

    fn current_checks(&self, cx: &App) -> Option<&[Check]> {
        let key = self.key(cx)?;
        self.checks.as_ref().filter(|(checked, _)| *checked == key).map(|(_, checks)| checks.as_slice())
    }

    fn checking(&self, cx: &App) -> bool {
        self.current_checks(cx).is_some_and(|checks| checks.iter().any(|check| matches!(check, Check::Pending)))
    }

    fn test(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(host), Some(key)) = (self.input(cx), self.key(cx)) else { return };
        let Some(workspace) = self.workspace.upgrade() else { return };
        let targets = workspace.read(cx).check_targets_among(&self.environments);
        self.checks = Some((key.clone(), vec![Check::Pending; targets.len()]));
        self.shown_errors.clear();
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let checks = futures::future::join_all(targets.iter().map(|target| check_target(target, &host))).await;
            let _ = this.update(cx, |this, cx| {
                if this.checks.as_ref().is_some_and(|(checked, _)| *checked == key) {
                    this.checks = Some((key, checks));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let named = self.draft(cx).is_some_and(|draft| !draft.label.trim().is_empty());
        let Some(host) = self.input(cx).filter(|_| named && !self.busy && !self.checking(cx)) else { return };
        let original = self.original.clone().unwrap_or_else(|| host.clone());
        let (workspace, environments, only) = (self.workspace.clone(), self.environments.clone(), self.only.clone());
        save(
            self,
            &workspace,
            &environments,
            ("Device host not saved", "Device host not saved everywhere"),
            |members| ssh_plan(members, &host, false, &original, only.as_deref()),
            window,
            cx,
        );
    }

    fn render_check(&self, t: &Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let checks = self.current_checks(cx).map(<[Check]>::to_vec);
        let key = self.key(cx);
        let valid = self.input(cx).is_some();
        let checking = self.checking(cx);
        let total = self.environments.len();
        let failed = checks.iter().flatten().filter(|check| matches!(check, Check::Failed(_))).count();
        let summary: SharedString = match &checks {
            _ if checking => "Checking environments…".into(),
            Some(_) if failed > 0 => format!("{failed} of {total} failed").into(),
            Some(_) => "Connection checks passed".into(),
            None => "Check access before saving".into(),
        };
        let open = total == 1
            || self.checks_open.as_ref().filter(|(seen, _)| Some(seen) == key.as_ref()).map_or(failed > 0, |(_, o)| *o);
        let summary = if total == 1 {
            muted_text(summary, t.muted_foreground).into_any_element()
        } else {
            // `CollapsibleTrigger`: the summary toggles the per-environment results once there are some.
            Button::new("device-host-checks-toggle")
                .text()
                .xsmall()
                .min_w_0()
                .gap(px(4.))
                .text_xs()
                .text_color(t.muted_foreground)
                .disabled(checks.is_none())
                .child(summary)
                .when(checks.is_some(), |el| {
                    el.child(Icon::new(if open { Lucide::ChevronUp } else { Lucide::ChevronDown }).with_size(px(12.)))
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(key) = this.key(cx) {
                        this.checks_open = Some((key, !open));
                        cx.notify();
                    }
                }))
                .into_any_element()
        };
        let results = checks.filter(|_| open).map(|checks| {
            v_flex().border_t_1().border_color(t.border.opacity(0.6)).children(
                self.environments
                    .iter()
                    .zip(checks)
                    .enumerate()
                    .map(|(index, ((id, label), check))| self.render_result(index, id, label, check, t, cx)),
            )
        });
        v_flex()
            .rounded(px(8.))
            .border_1()
            .border_color(t.border.opacity(0.6))
            .child(
                h_flex().justify_between().gap(px(12.)).px(px(12.)).py(px(10.)).child(summary).child(
                    outline_button("device-host-test")
                        .label("Test connection")
                        .loading(checking)
                        .disabled(self.busy || checking || !valid)
                        .on_click(cx.listener(|this, _, window, cx| this.test(window, cx))),
                ),
            )
            .children(results)
    }

    fn render_result(
        &self,
        index: usize,
        environment: &str,
        label: &str,
        check: Check,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (icon, text, color) = match &check {
            Check::Pending => (None, "Checking…", t.muted_foreground),
            Check::Local => (Some(Lucide::Monitor), "Already available locally", t.muted_foreground),
            Check::Failed(_) => (Some(Lucide::X), "Failed", t.error),
            Check::Connected(_) => (Some(Lucide::Check), "Connected", t.muted_foreground),
        };
        let shown = self.shown_errors.iter().any(|id| id == environment);
        let environment = environment.to_owned();
        v_flex()
            .px(px(12.))
            .py(px(10.))
            .text_xs()
            .when(index > 0, |el| el.border_t_1().border_color(t.border.opacity(0.4)))
            .child(
                h_flex()
                    .justify_between()
                    .gap(px(12.))
                    .child(div().truncate().font_weight(FontWeight::MEDIUM).child(label.to_owned()))
                    .child(
                        h_flex()
                            .flex_none()
                            .gap(px(6.))
                            .text_color(color)
                            .when(matches!(check, Check::Pending), |el| {
                                el.child(Spinner::new().xsmall().color(t.muted_foreground))
                            })
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
                        .child(
                            Button::new(SharedString::from(format!("device-host-show-error-{environment}")))
                                .text()
                                .xsmall()
                                .text_xs()
                                .text_color(t.muted_foreground)
                                .label("Show error")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if this.shown_errors.contains(&environment) {
                                        this.shown_errors.retain(|id| *id != environment);
                                    } else {
                                        this.shown_errors.push(environment.clone());
                                    }
                                    cx.notify();
                                })),
                        )
                        .when(shown, |this| {
                            this.child(
                                div()
                                    .id(SharedString::from(format!("device-host-error-{index}")))
                                    .mt(px(4.))
                                    .max_h(px(128.))
                                    .overflow_y_scroll()
                                    .child(error),
                            )
                        }),
                ),
                _ => this,
            })
            .into_any_element()
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
        let from = match self.environments.as_slice() {
            [(_, label)] => format!("Connect from {label}."),
            environments => format!("Connect from {} selected environments.", environments.len()),
        };
        v_flex()
            .child(header(
                if self.original.is_none() { "Add device host" } else { "Edit device host" },
                format!("{from} Hosts on the same machine are skipped."),
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

/// `environment`'s socket now: a reconnect while an editor is open replaces it.
fn live_client(workspace: &WeakEntity<Workspace>, environment: &str, cx: &App) -> Option<RpcClient> {
    workspace.upgrade()?.read(cx).env_client(environment)
}

struct EasEditor {
    workspace: WeakEntity<Workspace>,
    /// The environments selected at open, saved to while connected.
    environments: Vec<(String, String)>,
    /// The environment the account check runs on, picked at open; its socket is looked up at each check.
    check_on: Option<String>,
    /// The secret providers the token can read from there.
    sources: Vec<SecretSource>,
    host: EasHost,
    is_new: bool,
    name: Entity<InputState>,
    token_input: Entity<InputState>,
    /// A saved token arrives redacted; only a replacement or a clear changes it.
    token: TokenEntry,
    /// The token's secret source field accepts what it holds.
    token_valid: bool,
    /// Bumped by Clear, which starts the token's source field over.
    token_generation: u64,
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

    fn set_busy(&mut self, busy: bool) {
        self.busy = busy;
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
        environments: Vec<(String, String)>,
        check_on: Option<String>,
        sources: Vec<SecretSource>,
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
                        TokenEntry::New(typed.into())
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
        let editor = Self {
            workspace,
            environments,
            check_on,
            sources,
            app_id: host.app_id.clone(),
            token: if saved { TokenEntry::Saved } else { TokenEntry::Cleared },
            token_valid: true,
            token_generation: 0,
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
        // Deferred: the editor is built inside a Workspace update, and the check reads Workspace.
        cx.defer_in(window, move |this, window, cx| this.run_check(input, window, cx));
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
            TokenEntry::New(token) => input["accessToken"] = token.clone(),
            // A cleared token is sent empty, so the check does not use the saved one Save drops.
            TokenEntry::Cleared => input["accessToken"] = "".into(),
            TokenEntry::Saved => {}
        }
        self.run_check(input, window, cx);
    }

    fn run_check(&mut self, input: Value, window: &mut Window, cx: &mut Context<Self>) {
        let check_on = self.check_on.as_deref();
        let Some(client) = check_on.and_then(|id| live_client(&self.workspace, id, cx)) else { return };
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
            access_token: self.token.resolve(&self.host.access_token),
            max_concurrent_sessions: number(&self.sessions, cx),
            max_session_minutes: number(&self.session_minutes, cx),
            idle_minutes: number(&self.idle_minutes, cx),
            ..self.host.clone()
        }
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let next = self.next(cx);
        if self.busy || !self.token_valid || !hosts::valid_eas_draft(&next) {
            return;
        }
        let keep_token = !self.is_new && self.token == TokenEntry::Saved;
        let (workspace, environments) = (self.workspace.clone(), self.environments.clone());
        save(
            self,
            &workspace,
            &environments,
            ("EAS host not saved", "EAS host not saved everywhere"),
            |members| eas_plan(members, &next, false, keep_token),
            window,
            cx,
        );
    }

    fn clear_token(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.token = TokenEntry::Cleared;
        self.token_valid = true;
        self.token_generation += 1;
        self.token_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.set_placeholder("Optional", window, cx);
        });
        self.reset_account();
        cx.notify();
    }

    fn render_token(&mut self, window: &mut Window, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.busy;
        let editor = cx.entity().downgrade();
        let token_input = self.token_input.clone();
        let parts = secret_source_input(
            format!("eas-token-{}-{}", self.host.id, self.token_generation),
            "Expo access token",
            token_reference(&self.token, &self.host.access_token),
            &self.sources,
            busy,
            || input(&token_input, busy).into_any_element(),
            move |secret, _, cx| {
                let _ = editor.update(cx, |this, cx| {
                    this.token = secret.map_or(TokenEntry::Cleared, TokenEntry::New);
                    this.reset_account();
                    cx.notify();
                });
            },
            window,
            cx,
        );
        if parts.valid != self.token_valid {
            self.token_valid = parts.valid;
            self.reset_account();
        }
        field(
            "Expo access token",
            h_flex().items_start().gap(px(8.)).child(parts.row()).when(self.token != TokenEntry::Cleared, |this| {
                this.child(
                    clear_button("eas-token-clear", busy, t)
                        .on_click(cx.listener(|this, _, window, cx| this.clear_token(window, cx))),
                )
            }),
        )
        .into_any_element()
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *tokens(cx);
        let busy = self.busy;
        let token = self.render_token(window, &t, cx);
        let next = self.next(cx);
        let valid = self.token_valid && hosts::valid_eas_draft(&next);
        // Why Save is off, for the limits the fields' ranges do not show.
        let problem = (!valid).then_some(if !self.token_valid {
            "Enter a valid secret reference."
        } else if next.max_session_minutes > 0 && next.idle_minutes >= next.max_session_minutes {
            "Idle minutes must be smaller than session minutes."
        } else {
            "Give the host a label and limits within the ranges shown."
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
                    .child(token)
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
                                    .disabled(busy || checking || !self.token_valid)
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

/// The token check under a Simlock token field.
#[derive(Clone)]
enum Identity {
    Checking,
    Accepted { signed_in_as: String, from_machine: bool },
    Failed(String),
}

/// What an environment's Simlock token field starts from.
struct TokenSeed {
    environment: String,
    label: String,
    sources: Vec<SecretSource>,
    /// This environment's saved token: redacted when a literal, shown when a reference.
    saved: Value,
}

/// One environment's agent token (`SimlockTokenField`): Simlock counts leases per token.
struct TokenField {
    seed: TokenSeed,
    input: Entity<InputState>,
    token: TokenEntry,
    /// The token's secret source field accepts what it holds.
    valid: bool,
    /// Bumped by Clear, which starts the source field over.
    generation: u64,
    /// The check's answer and the input it was asked with.
    identity: Option<(Value, Identity)>,
}

impl TokenField {
    /// Emptying the field or leaving a reference returns to a saved literal, never to a saved reference.
    fn fallback(&self) -> TokenEntry {
        match &self.seed.saved {
            Value::String(saved) if !saved.is_empty() => TokenEntry::Saved,
            _ => TokenEntry::Cleared,
        }
    }
}

struct SimlockEditor {
    workspace: WeakEntity<Workspace>,
    /// The environments selected at open, saved to while connected.
    environments: Vec<(String, String)>,
    host: SimlockHost,
    is_new: bool,
    name: Entity<InputState>,
    gateway: Entity<InputState>,
    /// The gateway field was typed in, so a detected gateway no longer fills it.
    url_edited: bool,
    tokens: Vec<TokenField>,
    leases: Entity<InputState>,
    lease_minutes: Entity<InputState>,
    busy: bool,
    _subscriptions: Vec<Subscription>,
}

impl Editor for SimlockEditor {
    fn busy(&self) -> bool {
        self.busy
    }

    fn set_busy(&mut self, busy: bool) {
        self.busy = busy;
    }
}

impl SimlockEditor {
    fn new(
        workspace: WeakEntity<Workspace>,
        environments: Vec<(String, String)>,
        seeds: Vec<TokenSeed>,
        host: Option<SimlockHost>,
        id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let is_new = host.is_none();
        let host = host.unwrap_or_else(|| hosts::new_simlock_host(id));
        let name = text_input(window, cx, &host.label, "", false);
        let gateway = text_input(window, cx, &host.gateway_url, "https://gateway.example.ts.net:4700", false);
        let leases = text_input(window, cx, &host.max_leases.to_string(), "", false);
        let lease_minutes = text_input(window, cx, &host.max_lease_minutes.to_string(), "", false);
        name.update(cx, |input, cx| input.focus(window, cx));
        let mut subscriptions: Vec<_> =
            [&name, &leases, &lease_minutes].map(|input| watch(input, window, cx, Self::save)).into();
        subscriptions.push(cx.subscribe_in(&gateway, window, |this, _, event: &InputEvent, window, cx| match event {
            InputEvent::Change => {
                this.url_edited = true;
                this.check_identities(window, cx);
            }
            InputEvent::PressEnter { .. } => this.save(window, cx),
            _ => {}
        }));
        let mut tokens = Vec::new();
        for (index, seed) in seeds.into_iter().enumerate() {
            let configured = matches!(&seed.saved, Value::String(saved) if !saved.is_empty());
            let token = if has_token(&seed.saved) { TokenEntry::Saved } else { TokenEntry::Cleared };
            let input = text_input(window, cx, "", token_placeholder(configured, "Not set"), true);
            subscriptions.push(cx.subscribe_in(&input, window, move |this, input, event: &InputEvent, window, cx| {
                match event {
                    InputEvent::Change => {
                        let typed = value(input, cx);
                        let Some(field) = this.tokens.get_mut(index) else { return };
                        // Emptying the field is not clearing the saved token; the Clear button is.
                        field.token = if typed.is_empty() { field.fallback() } else { TokenEntry::New(typed.into()) };
                        this.check_identities(window, cx);
                    }
                    InputEvent::PressEnter { .. } => this.save(window, cx),
                    _ => {}
                }
            }));
            tokens.push(TokenField { seed, input, token, valid: true, generation: 0, identity: None });
        }
        let editor = Self {
            workspace,
            environments,
            host,
            is_new,
            name,
            gateway,
            url_edited: false,
            tokens,
            leases,
            lease_minutes,
            busy: false,
            _subscriptions: subscriptions,
        };
        // Deferred: the editor is built inside a Workspace update, and the checks read Workspace.
        cx.defer_in(window, move |this, window, cx| {
            if is_new {
                this.detect_gateway(window, cx);
            }
            this.check_identities(window, cx);
        });
        editor
    }

    /// A new host starts at the gateway an environment's own agent-device already uses.
    fn detect_gateway(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let clients: Vec<RpcClient> =
            self.tokens.iter().filter_map(|field| live_client(&self.workspace, &field.seed.environment, cx)).collect();
        cx.spawn_in(window, async move |this, cx| {
            let answers = futures::future::join_all(
                clients.iter().map(|client| client.request("device.simlockIdentity", json!({}))),
            )
            .await;
            let url = answers.into_iter().find_map(|answer| {
                answer.ok()?["gatewayUrl"].as_str().filter(|url| !url.is_empty()).map(str::to_owned)
            });
            let Some(url) = url else { return };
            let _ = this.update_in(cx, |this, window, cx| {
                if this.url_edited || !value(&this.gateway, cx).is_empty() {
                    return;
                }
                this.gateway.update(cx, |input, cx| input.set_value(url, window, cx));
                this.check_identities(window, cx);
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

    /// Asks the gateway about each environment's token once the shared fields are valid; a
    /// cleared field asks about the machine's own token, which a blank one falls back to.
    fn check_identities(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.notify();
        let next = self.next(cx);
        let valid = hosts::simlock_problem(&next).is_none();
        let clients: Vec<Option<RpcClient>> =
            self.tokens.iter().map(|field| live_client(&self.workspace, &field.seed.environment, cx)).collect();
        for (index, client) in clients.into_iter().enumerate() {
            let field = &mut self.tokens[index];
            if !valid {
                field.identity = None;
                continue;
            }
            let mut input = json!({"gatewayUrl": next.gateway_url});
            match &field.token {
                TokenEntry::New(Value::String(token)) if !token.trim().is_empty() => {
                    input["agentToken"] = token.trim().into();
                }
                TokenEntry::New(reference @ Value::Object(_)) => input["agentToken"] = reference.clone(),
                TokenEntry::Saved if !self.is_new => input["hostId"] = self.host.id.clone().into(),
                _ => {}
            }
            if field.identity.as_ref().is_some_and(|(asked, _)| *asked == input) {
                continue;
            }
            let Some(client) = client else { continue };
            field.identity = Some((input.clone(), Identity::Checking));
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
                    let Some(field) = this.tokens.get_mut(index) else { return };
                    if field.identity.as_ref().is_some_and(|(asked, _)| *asked == input) {
                        field.identity = Some((input, identity));
                        cx.notify();
                    }
                });
            })
            .detach();
        }
    }

    /// Why Save is off: the shared fields' problem, else a token reference that can't be saved.
    fn problem(&self, next: &SimlockHost) -> Option<&'static str> {
        hosts::simlock_problem(next)
            .or_else(|| self.tokens.iter().any(|field| !field.valid).then_some("Enter a valid secret reference."))
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let next = self.next(cx);
        if self.busy || self.problem(&next).is_some() {
            return;
        }
        let tokens: Vec<(String, TokenEntry)> =
            self.tokens.iter().map(|field| (field.seed.environment.clone(), field.token.clone())).collect();
        let (workspace, environments) = (self.workspace.clone(), self.environments.clone());
        save(
            self,
            &workspace,
            &environments,
            ("Simlock host not saved", "Simlock host not saved everywhere"),
            |members| simlock_save_plan(members, &next, &tokens),
            window,
            cx,
        );
    }

    fn clear_token(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(field) = self.tokens.get_mut(index) else { return };
        field.token = TokenEntry::Cleared;
        field.valid = true;
        field.generation += 1;
        field.input.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.set_placeholder("Not set", window, cx);
        });
        self.check_identities(window, cx);
    }

    fn render_token(&mut self, index: usize, several: bool, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = *tokens(cx);
        let busy = self.busy;
        let editor = cx.entity().downgrade();
        let field = &self.tokens[index];
        let label = if several { format!("Agent token · {}", field.seed.label) } else { "Agent token".to_owned() };
        let (token_input, cleared) = (field.input.clone(), field.token == TokenEntry::Cleared);
        let fallback = field.fallback();
        let status = field.identity.as_ref().map(|(_, identity)| match identity {
            Identity::Checking => muted_text("Checking…", t.muted_foreground),
            Identity::Accepted { signed_in_as, from_machine: true } => {
                muted_text(format!("Using this machine's agent-device token ({signed_in_as})."), t.muted_foreground)
            }
            Identity::Accepted { signed_in_as, .. } => {
                muted_text(format!("Accepted as {signed_in_as}."), t.muted_foreground)
            }
            Identity::Failed(error) => muted_text(error.clone(), if cleared { t.muted_foreground } else { t.error }),
        });
        let parts = secret_source_input(
            format!("simlock-token-{}-{}-{}", self.host.id, field.seed.environment, field.generation),
            "Agent token",
            token_reference(&field.token, &field.seed.saved),
            &field.seed.sources,
            busy,
            || input(&token_input, busy).into_any_element(),
            move |secret, window, cx| {
                let _ = editor.update(cx, |this, cx| {
                    let Some(field) = this.tokens.get_mut(index) else { return };
                    field.token = secret.map_or_else(|| fallback.clone(), TokenEntry::New);
                    this.check_identities(window, cx);
                });
            },
            window,
            cx,
        );
        if self.tokens[index].valid != parts.valid {
            self.tokens[index].valid = parts.valid;
        }
        v_flex()
            .gap(px(6.))
            .text_sm()
            .child(label)
            .child(h_flex().items_start().gap(px(8.)).child(parts.row()).when(!cleared, |this| {
                this.child(
                    clear_button(SharedString::from(format!("simlock-token-clear-{index}")), busy, &t)
                        .on_click(cx.listener(move |this, _, window, cx| this.clear_token(index, window, cx))),
                )
            }))
            .children(status)
            .into_any_element()
    }
}

impl Render for SimlockEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *tokens(cx);
        let busy = self.busy;
        let several = self.tokens.len() > 1;
        let token_fields: Vec<AnyElement> =
            (0..self.tokens.len()).map(|index| self.render_token(index, several, window, cx)).collect();
        let next = self.next(cx);
        let problem = self.problem(&next);
        let limit = |label: &'static str, input_state: &Entity<InputState>| {
            div().flex_1().min_w_0().child(field(label, input(input_state, busy)))
        };
        let description = if several {
            "Each selected environment's server leases devices from your Simlock gateway with its own agent token, and releases them when they close."
        } else {
            "The environment's server leases devices from your Simlock gateway and releases them when they close."
        };
        v_flex()
            .child(header(if self.is_new { "Add Simlock host" } else { "Edit Simlock host" }, description, &t))
            .child(
                v_flex()
                    .gap(px(16.))
                    .px(px(24.))
                    .pt(px(4.))
                    .pb(px(24.))
                    .child(field("Name", input(&self.name, busy)))
                    .child(field("Gateway URL", input(&self.gateway, busy)))
                    .child(v_flex().gap(px(12.)).children(token_fields).child(
                        div().text_xs().text_color(t.muted_foreground).child(with_code(
                            &[
                                (
                                    if several {
                                        "Leases, drives, and streams devices. Create one per environment with "
                                    } else {
                                        "Leases, drives, and streams devices. Create one with "
                                    },
                                    false,
                                ),
                                ("simlock --gateway token create --role agent", true),
                                (", or leave it blank to add it later. A blank token uses the machine's ", false),
                                ("AGENT_DEVICE_DAEMON_AUTH_TOKEN", true),
                                (" when its ", false),
                                ("AGENT_DEVICE_DAEMON_BASE_URL", true),
                                (" is this gateway.", false),
                            ],
                            t.muted_foreground,
                            window,
                        )),
                    ))
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
