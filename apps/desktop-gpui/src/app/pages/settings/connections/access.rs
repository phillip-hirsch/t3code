//! Sharing & access (`ConnectionsSettings view="access"` with
//! `AuthorizedClientsSettings.tsx`): this environment's card, the session's
//! permissions, and its authorized clients and pairing links.

use std::collections::HashMap;
use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, ClipboardItem, Context, ElementId, Entity, FontWeight, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Task,
    WeakEntity, Window, div, px,
};
use serde_json::{Value, json};

use super::super::widgets::{RowFrame, card, hairlines, section_with_action};
use super::logic::{
    self, ACCESS_READ, ACCESS_WRITE, AccessSnapshot, ClientSession, ENVIRONMENT_MAINTAIN, PairingLink, SETTINGS_WRITE,
    Session,
};
use super::ui::{self, Size, Variant, WebButton, text_2xs, text_sm, text_xs};
use super::{auth_request, error_toast, success_toast, update_settings};
use crate::app::Workspace;
use crate::app::sidebar::now_ms;
use crate::backend::Endpoint;
use crate::rpc::{RpcClient, StreamEvent};
use crate::theme::{self, Tokens};

const SESSION_RETRY: Duration = Duration::from_secs(5);

/// The session check: pending, answered, or failed (and retrying).
#[derive(Default)]
enum SessionLoad {
    #[default]
    Pending,
    Ready(Session),
    Failed,
}

/// Everything the access page holds while it is open.
#[derive(Default)]
pub(super) struct AccessState {
    started: bool,
    session: SessionLoad,
    /// `None` until `subscribeAuthAccess` sends its snapshot.
    access: Option<AccessSnapshot>,
    access_error: Option<SharedString>,
    mutation_error: Option<SharedString>,
    /// Credentials of links created here, by link id; the server never sends them back.
    credentials: HashMap<String, String>,
    newest_link: Option<String>,
    revoking_link: Option<String>,
    revoking_session: Option<String>,
    revoking_others: bool,
    clients_open: bool,
    permissions_open: bool,
    _session_task: Option<Task<()>>,
    _stream_task: Option<Task<()>>,
    /// Repaints "Expires in …" once a second while a pairing link is live.
    _tick: Option<Task<()>>,
}

impl AccessState {
    /// Starts the session check, then the access stream, once there is a connection.
    fn start(&mut self, client: Option<RpcClient>, endpoint: Option<Endpoint>, cx: &mut Context<Self>) {
        let (Some(client), Some(endpoint)) = (client, endpoint) else { return };
        if self.started {
            return;
        }
        self.started = true;
        self._session_task = Some(cx.spawn(async move |this, cx| {
            loop {
                let result = auth_request(Some(endpoint.clone()), "/api/auth/session", None).await;
                let ready = result.is_ok();
                let Ok(()) = this.update(cx, |this, cx| {
                    match result {
                        Ok(value) => {
                            let session = Session::from_json(&value);
                            if session.grants(ACCESS_READ) {
                                this.subscribe(client.clone(), cx);
                            }
                            this.session = SessionLoad::Ready(session);
                        }
                        Err(_) => this.session = SessionLoad::Failed,
                    }
                    cx.notify();
                }) else {
                    return;
                };
                if ready {
                    return;
                }
                cx.background_executor().timer(SESSION_RETRY).await;
            }
        }));
    }

    fn subscribe(&mut self, client: RpcClient, cx: &mut Context<Self>) {
        let stream = client.stream("subscribeAuthAccess", json!({}));
        self._stream_task = Some(cx.spawn(async move |this, cx| {
            while let Some(event) = stream.next().await {
                let ended = this.update(cx, |this, cx| {
                    let ended = match event {
                        StreamEvent::Item(value) => {
                            this.access.get_or_insert_default().apply(&value);
                            this.access_error = None;
                            this.ensure_tick(cx);
                            false
                        }
                        StreamEvent::End(result) => {
                            if let Err(error) = result {
                                this.access_error = Some(error.to_string().into());
                            }
                            // Resubscribe on the next connection.
                            this.started = false;
                            true
                        }
                    };
                    cx.notify();
                    ended
                });
                if ended.unwrap_or(true) {
                    break;
                }
            }
        }));
    }

    fn live_links(&self, now: i64) -> bool {
        self.access.as_ref().is_some_and(|a| a.pairing_links.iter().any(|l| logic::ms(&l.expires_at) > now))
    }

    fn ensure_tick(&mut self, cx: &mut Context<Self>) {
        if self._tick.is_some() || !self.live_links(now_ms()) {
            return;
        }
        self._tick = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let live = this.update(cx, |this, cx| {
                    cx.notify();
                    let live = this.live_links(now_ms());
                    if !live {
                        this._tick = None;
                    }
                    live
                });
                if !matches!(live, Ok(true)) {
                    return;
                }
            }
        }));
    }

    fn grants(&self, scope: &str) -> bool {
        matches!(&self.session, SessionLoad::Ready(session) if session.grants(scope))
    }
}

/// One of the three access writes, with its failure toast title and fallback message.
enum Revoke {
    Link(String),
    Client(String),
    Others,
}

fn revoke(state: &Entity<AccessState>, endpoint: Option<Endpoint>, target: Revoke, window: &mut Window, cx: &mut App) {
    let (path, body, title, fallback) = match &target {
        Revoke::Link(id) => (
            "/api/auth/pairing-links/revoke",
            json!({ "id": id }),
            "Could not revoke pairing link",
            "Failed to revoke pairing link.",
        ),
        Revoke::Client(id) => (
            "/api/auth/clients/revoke",
            json!({ "sessionId": id }),
            "Could not revoke client access",
            "Failed to revoke client access.",
        ),
        Revoke::Others => (
            "/api/auth/clients/revoke-others",
            json!({}),
            "Could not revoke other clients",
            "Failed to revoke other clients.",
        ),
    };
    state.update(cx, |state, cx| {
        match &target {
            Revoke::Link(id) => state.revoking_link = Some(id.clone()),
            Revoke::Client(id) => state.revoking_session = Some(id.clone()),
            Revoke::Others => state.revoking_others = true,
        }
        state.mutation_error = None;
        cx.notify();
    });
    let state = state.downgrade();
    window
        .spawn(cx, async move |cx| {
            let result = auth_request(endpoint, path, Some(body)).await;
            let _ = cx.update(|window, cx| {
                let _ = state.update(cx, |state, cx| {
                    match target {
                        Revoke::Link(_) => state.revoking_link = None,
                        Revoke::Client(_) => state.revoking_session = None,
                        Revoke::Others => state.revoking_others = false,
                    }
                    if let Err(error) = &result {
                        let message = if error.is_empty() { fallback.to_owned() } else { error.clone() };
                        state.mutation_error = Some(message.into());
                    }
                    cx.notify();
                });
                match result {
                    Ok(value) if path.ends_with("revoke-others") => {
                        let count = value["revokedCount"].as_u64().unwrap_or(0);
                        let title = if count == 1 {
                            "Revoked 1 other client".to_owned()
                        } else {
                            format!("Revoked {count} clients")
                        };
                        success_toast(
                            window,
                            cx,
                            title,
                            "Other paired clients will need a new pairing link before reconnecting.",
                        );
                    }
                    Ok(_) => {}
                    Err(error) => {
                        error_toast(window, cx, title, if error.is_empty() { fallback.to_owned() } else { error })
                    }
                }
            });
        })
        .detach();
}

impl Workspace {
    pub(super) fn render_access(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let state = window.use_keyed_state("settings-access", cx, |_, _| AccessState::default());
        let (client, endpoint) = (self.settings_client(), self.settings_endpoint().cloned());
        state.update(cx, |state, cx| state.start(client, endpoint, cx));
        let t = *theme::tokens(cx);
        let local_rows = self.local_backend_rows(window, cx, &t);
        let mut sections = vec![self.environment_card(&state, local_rows, window, cx, &t)];
        let s = state.read(cx);
        if s.grants(ACCESS_READ) || s.grants(ACCESS_WRITE) {
            sections.push(self.authorized_clients(&state, cx, &t));
        }
        let view = self.settings_view();
        self.settings_page("access", &view, sections, cx)
    }

    /// The `connections-environment` section: name, icon menu, policy, version, network access, permissions.
    fn environment_card(
        &self,
        state: &Entity<AccessState>,
        local_rows: Option<Vec<AnyElement>>,
        window: &mut Window,
        cx: &App,
        t: &Tokens,
    ) -> AnyElement {
        let s = state.read(cx);
        let config = &self.settings_info().config;
        let has_config = config.is_object();
        let label: SharedString = self.environment_label().unwrap_or("This machine").to_owned().into();
        let mut rows = Vec::new();
        if let Some(organization) = config.pointer("/managedPolicy/organization").and_then(Value::as_str) {
            let support = config.pointer("/managedPolicy/supportUrl").and_then(Value::as_str).map(str::to_owned);
            rows.push(managed_notice(organization, support, t));
        }
        if s.grants(ENVIRONMENT_MAINTAIN) {
            let version = config.pointer("/environment/serverVersion").and_then(Value::as_str);
            let url = self.http_base();
            let parts: Vec<&str> = [version, url].into_iter().flatten().collect();
            let description = if version.is_none() { "Loading\u{2026}".to_owned() } else { parts.join(" · ") };
            let row = RowFrame::new("Version").description(description).status(has_config.then_some("Up to date"));
            rows.push(row.render(t).into_any_element());
            // A server this app started is exposed from here (`renderNetworkAccessRow`).
            match local_rows {
                Some(local) => rows.extend(local),
                None => rows.push(network_access_row(s, t)),
            }
        }
        rows.push(permissions_details(state, s, self.settings_client().is_some(), t));
        let more = self.icon_menu(s, window).into_any_element();
        let icon = ui::glyph(logic::machine_icon(self.settings_info().machine_kind()), 16., t.foreground);
        section_with_action(
            "connections-environment",
            h_flex().gap(px(8.)).child(icon).child(label),
            Some(more),
            rows,
            t,
        )
    }

    /// The "…" menu with its Icon submenu (`EnvironmentIconMenu`).
    fn icon_menu(&self, s: &AccessState, _: &mut Window) -> impl IntoElement {
        let config = &self.settings_info().config;
        let lock = if !config.is_object() {
            Some("Connect to this environment to change its icon.")
        } else if !s.grants(SETTINGS_WRITE) {
            Some("Your session on this environment cannot change its settings.")
        } else {
            None
        };
        let machine = config.pointer("/environment/platform/machine").and_then(Value::as_str).map(str::to_owned);
        let resolved = self.settings_info().machine_kind().to_owned();
        let client = self.settings_client();
        Button::new("settings-access-more")
            .ghost()
            .xsmall()
            .icon(IconName::Ellipsis)
            .tooltip("More actions for this machine")
            .dropdown_menu_with_anchor(gpui_kit::Anchor::TopRight, move |menu, window, cx| {
                let (machine, resolved, client) = (machine.clone(), resolved.clone(), client.clone());
                let icon = gpui_kit::component::Icon::default().path(logic::machine_icon(&resolved));
                menu.submenu_with_icon(Some(icon), "Icon", window, cx, move |mut menu, _, _| {
                    if let Some(lock) = lock {
                        menu = menu.item(PopupMenuItem::new(lock).disabled(true)).separator();
                    }
                    let detected = machine.clone().unwrap_or_else(|| "server".into());
                    for (kind, kind_label) in logic::MACHINE_KINDS {
                        let note = (*kind == detected).then(|| if machine.is_some() { "detected" } else { "default" });
                        let patch =
                            json!({ "environmentIcon": if *kind == detected { Value::Null } else { json!(kind) } });
                        let client = client.clone();
                        menu = menu.item(
                            PopupMenuItem::element(move |_, cx| {
                                let t = theme::tokens(cx);
                                h_flex()
                                    .gap(px(8.))
                                    .min_w(px(160.))
                                    .child(ui::glyph(logic::machine_icon(kind), 14., t.foreground))
                                    .child(div().flex_1().child(*kind_label))
                                    .when_some(note, |el, note| {
                                        el.child(text_xs(div().text_color(t.muted_foreground)).child(note))
                                    })
                            })
                            .checked(resolved == *kind)
                            .disabled(lock.is_some())
                            .on_click(move |_, window, cx| {
                                let (client, patch) = (client.clone(), patch.clone());
                                window
                                    .spawn(cx, async move |cx| {
                                        if let Err(error) = update_settings(client, patch).await {
                                            let _ = cx.update(|window, cx| {
                                                error_toast(window, cx, "Could not update settings", error)
                                            });
                                        }
                                    })
                                    .detach();
                            }),
                        );
                    }
                    menu
                })
            })
    }

    fn authorized_clients(&self, state: &Entity<AccessState>, cx: &App, t: &Tokens) -> AnyElement {
        let s = state.read(cx);
        let (can_read, can_write) = (s.grants(ACCESS_READ), s.grants(ACCESS_WRITE));
        let snapshot = s.access.clone().unwrap_or_default();
        let now = now_ms();
        let links: Vec<&PairingLink> =
            snapshot.sorted_links().into_iter().filter(|l| logic::ms(&l.expires_at) > now).collect();
        let sessions = snapshot.sorted_sessions();
        let open = s.clients_open;
        let endpoint = self.settings_endpoint().cloned();
        let link_row = |link: &PairingLink| pairing_link_row(state, s, link, can_write, endpoint.clone(), now, t);

        let toggle = {
            let state = state.clone();
            move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
                state.update(cx, |s, cx| {
                    s.clients_open = !s.clients_open;
                    cx.notify();
                })
            }
        };
        let header = h_flex()
            .px(px(16.))
            .gap(px(16.))
            .child(
                h_flex()
                    .id("authorized-clients-toggle")
                    .flex_1()
                    .min_w_0()
                    .min_h(px(44.))
                    .py(px(8.))
                    .gap(px(8.))
                    .cursor_pointer()
                    .on_click(toggle)
                    .child(ui::chevron(open, 16., t.muted_foreground))
                    .child(text_sm(div().flex_none().font_weight(FontWeight::MEDIUM)).child("Authorized clients"))
                    .child(
                        text_xs(div().min_w_0().truncate().text_color(t.muted_foreground)).child(snapshot.summary()),
                    ),
            )
            .when(can_write, |el| el.child(self.authorized_clients_actions(state, s, can_read, t)));

        let mut body: Vec<AnyElement> = Vec::new();
        if !can_read {
            body.push(
                text_xs(div().px(px(16.)).py(px(12.)).text_color(t.muted_foreground))
                    .child("This connection can create access links but cannot view authorized clients.")
                    .into_any_element(),
            );
        } else {
            if let Some(error) = s.mutation_error.clone().or(s.access_error.clone()) {
                body.push(
                    ui::item_row(text_xs(div().text_color(t.error_foreground)).child(error), None).into_any_element(),
                );
            }
            body.extend(links.iter().map(|link| link_row(link)));
            body.extend(
                sessions
                    .iter()
                    .map(|session| client_session_row(state, s, session, can_write, endpoint.clone(), now, t)),
            );
            if links.is_empty() && sessions.is_empty() && s.access.is_some() {
                body.push(
                    ui::item_row(
                        text_xs(div().text_color(t.muted_foreground.opacity(0.6)))
                            .child("No pairing links or client sessions."),
                        None,
                    )
                    .into_any_element(),
                );
            }
        }
        let collapsed = (!open && can_read)
            .then(|| {
                s.newest_link.as_ref().and_then(|id| links.iter().find(|l| &l.id == id)).map(|link| link_row(link))
            })
            .flatten();
        let mut rows = vec![header.into_any_element()];
        rows.extend(collapsed.map(IntoElement::into_any_element));
        if open {
            rows.push(
                div()
                    .id("authorized-clients-scroll")
                    .max_h(px(360.))
                    .overflow_y_scroll()
                    .child(hairlines(body, t))
                    .into_any_element(),
            );
        }
        card(rows, t)
    }

    /// `AuthorizedClientsHeaderAction`: "Revoke others" and "Create link".
    fn authorized_clients_actions(
        &self,
        state: &Entity<AccessState>,
        s: &AccessState,
        can_read: bool,
        t: &Tokens,
    ) -> AnyElement {
        let endpoint = self.settings_endpoint().cloned();
        // Unknown sessions keep the button on, as the web does without a snapshot.
        let others =
            !can_read || s.access_error.is_some() || s.access.as_ref().is_none_or(AccessSnapshot::has_other_clients);
        let revoke_others = WebButton::new(
            "revoke-other-clients",
            if s.revoking_others { "Revoking\u{2026}" } else { "Revoke others" },
        )
        .variant(Variant::DestructiveOutline)
        .disabled(s.revoking_others || !others)
        .on_click({
            let (state, endpoint) = (state.clone(), endpoint.clone());
            move |_, window, cx| revoke(&state, endpoint.clone(), Revoke::Others, window, cx)
        });
        let denial = logic::remote_clients_denial(&self.settings_info().managed_policy);
        let delegatable: Vec<String> = match &s.session {
            SessionLoad::Ready(session) => session.delegatable().to_vec(),
            _ => Vec::new(),
        };
        let create =
            WebButton::new("create-pairing-link", "Create link").variant(Variant::Primary).icon(IconName::Plus);
        let create = match denial {
            Some(reason) => div()
                .id("create-pairing-link-managed")
                .opacity(0.5)
                .tooltip(ui::tooltip(reason))
                .child(create.disabled(true))
                .into_any_element(),
            None => {
                let state = state.downgrade();
                create
                    .on_click(move |_, window, cx| {
                        open_create_link_dialog(state.clone(), endpoint.clone(), delegatable.clone(), window, cx)
                    })
                    .into_any_element()
            }
        };
        let _ = t;
        h_flex().gap(px(8.)).child(revoke_others).child(create).into_any_element()
    }
}

/// `ManagedByOrganization`.
fn managed_notice(organization: &str, support: Option<String>, t: &Tokens) -> AnyElement {
    div()
        .px(px(16.))
        .py(px(8.))
        .child(
            h_flex()
                .items_start()
                .gap(px(8.))
                .text_color(t.muted_foreground)
                .child(div().mt(px(2.)).child(ui::glyph("icons/building.svg", 14., t.muted_foreground)))
                .child(
                    text_xs(h_flex().flex_wrap().gap(px(4.)))
                        .child(format!("This environment is managed by {organization}."))
                        .when_some(support, |el, url| {
                            el.child(
                                div()
                                    .id("managed-support")
                                    .text_color(t.foreground)
                                    .underline()
                                    .cursor_pointer()
                                    .on_click(move |_, _, cx| cx.open_url(&url))
                                    .child("Get help"),
                            )
                        }),
                ),
        )
        .into_any_element()
}

/// `renderDisabledNetworkAccessRow`: exposure is decided where the server starts.
fn network_access_row(s: &AccessState, t: &Tokens) -> AnyElement {
    let policy = match &s.session {
        SessionLoad::Ready(session) => session.policy.as_deref(),
        _ => None,
    };
    let description = match policy {
        Some("remote-reachable") => {
            "Remote access is already configured. Change network exposure where the server starts."
        }
        Some("loopback-browser") => {
            "Only this machine can connect. Restart with a non-loopback host for remote pairing."
        }
        _ => "Network exposure information is unavailable.",
    };
    let control = matches!(policy, Some("remote-reachable" | "loopback-browser")).then(|| {
        div()
            .id("network-access-switch")
            .tooltip(ui::tooltip(
                "Network exposure changes restart the backend and must be controlled where the server process is launched.",
            ))
            .child(Switch::new("network-access").checked(policy == Some("remote-reachable")).disabled(true))
            .into_any_element()
    });
    RowFrame::new("Network access")
        .description(description)
        .control(div().children(control))
        .render(t)
        .into_any_element()
}

/// The "Permissions" disclosure (`SessionPermissions`).
fn permissions_details(state: &Entity<AccessState>, s: &AccessState, connected: bool, t: &Tokens) -> AnyElement {
    let open = s.permissions_open;
    let toggle = {
        let state = state.clone();
        move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
            state.update(cx, |s, cx| {
                s.permissions_open = !s.permissions_open;
                cx.notify();
            })
        }
    };
    let summary = h_flex()
        .id("permissions-toggle")
        .min_h(px(44.))
        .gap(px(8.))
        .cursor_pointer()
        .on_click(toggle)
        .child(text_sm(div().font_weight(FontWeight::MEDIUM)).child("Permissions"))
        .child(ui::chevron(open, 12., t.muted_foreground));
    let body = open.then(|| match (&s.session, connected) {
        (SessionLoad::Ready(session), true) if session.authenticated => text_xs(v_flex().gap(px(12.)).py(px(12.)))
            .child(
                div()
                    .text_color(t.muted_foreground)
                    .child("These permissions apply to this client\u{2019}s current connection."),
            )
            .child(div().flex().flex_wrap().gap_y(px(8.)).children(logic::SCOPE_OPTIONS.iter().map(
                |(scope, title, _)| {
                    let granted = session.grants(scope);
                    // Two columns 24px apart (`sm:grid-cols-2 gap-x-6`).
                    h_flex().w(gpui_kit::relative(0.5)).pr(px(12.)).justify_between().gap(px(12.)).child(*title).child(
                        div().flex_none().text_color(t.muted_foreground).child(if granted {
                            "Allowed"
                        } else {
                            "Not granted"
                        }),
                    )
                },
            )))
            .into_any_element(),
        (session, connected) => {
            let failed = matches!(session, SessionLoad::Failed);
            let rejected = matches!(session, SessionLoad::Ready(s) if !s.authenticated);
            text_xs(div().py(px(12.)).text_color(t.muted_foreground))
                .child(logic::permissions_unavailable_message(connected, failed, rejected))
                .into_any_element()
        }
    });
    div().px(px(16.)).child(summary).children(body).into_any_element()
}

/// "N scopes", listing them on hover (`AccessScopeSummary`).
fn scope_summary(id: ElementId, scopes: &[String], t: &Tokens) -> AnyElement {
    let count = scopes.len();
    let label = format!("{count} {}", if count == 1 { "scope" } else { "scopes" });
    let scopes = scopes.to_vec();
    let fg = t.foreground;
    div()
        .id(id)
        .underline()
        .text_decoration_color(t.border)
        .hover(move |style| style.text_color(fg))
        .tooltip(move |window, cx| {
            let scopes = scopes.clone();
            Tooltip::element(move |_, cx| {
                let t = theme::tokens(cx);
                text_xs(v_flex().max_w(px(320.)))
                    .child(div().mb(px(4.)).font_weight(FontWeight::MEDIUM).child("Granted scopes"))
                    .children(scopes.iter().map(|scope| {
                        div().font_family(t.fonts.mono).text_color(t.foreground.opacity(0.85)).child(scope.clone())
                    }))
            })
            .build(window, cx)
        })
        .child(label)
        .into_any_element()
}

fn pairing_link_row(
    state: &Entity<AccessState>,
    s: &AccessState,
    link: &PairingLink,
    can_revoke: bool,
    endpoint: Option<Endpoint>,
    now: i64,
    t: &Tokens,
) -> AnyElement {
    let credential = s.credentials.get(&link.id).cloned();
    let id = &link.id;
    let created = SharedString::from(format!("Link created at {}", logic::format_timestamp(&link.created_at)));
    let expires_tip = logic::format_timestamp(&link.expires_at);
    let details = v_flex()
        .gap(px(4.))
        .child(
            h_flex()
                .min_h(px(20.))
                .gap(px(6.))
                .child(ui::status_dot(ElementId::Name(format!("link-dot-{id}").into()), t.warning, Some(created)))
                .child(
                    text_sm(div().font_weight(FontWeight::MEDIUM))
                        .child(link.label.clone().unwrap_or_else(|| "Pairing link".into())),
                ),
        )
        .child(
            text_xs(h_flex().text_color(t.muted_foreground))
                .child(
                    div()
                        .id(ElementId::Name(format!("link-expires-{id}").into()))
                        .tooltip(ui::tooltip(expires_tip))
                        .child(logic::expires_in_label(logic::ms(&link.expires_at), now)),
                )
                .child("\u{a0}·\u{a0}")
                .child(scope_summary(ElementId::Name(format!("link-scopes-{id}").into()), &link.scopes, t)),
        )
        .child(text_2xs(div().text_color(t.muted_foreground.opacity(0.7))).child(if credential.is_none() {
            "Create a new link to share from this client."
        } else {
            "Copy the token and pair from another client using this backend\u{2019}s reachable host."
        }));
    let revoking = s.revoking_link.as_ref() == Some(id);
    let actions = h_flex()
        .gap(px(8.))
        .when_some(credential, |el, credential| {
            el.child(WebButton::new(ElementId::Name(format!("link-copy-{id}").into()), "Copy code").on_click(
                move |_, window, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(credential.clone()));
                    success_toast(window, cx, "Pairing code copied", "Paste it into another client to finish pairing.");
                },
            ))
        })
        .child(
            WebButton::new(
                ElementId::Name(format!("link-revoke-{id}").into()),
                if revoking { "Revoking\u{2026}" } else { "Revoke" },
            )
            .variant(Variant::DestructiveOutline)
            .disabled(!can_revoke || revoking)
            .on_click({
                let (state, id) = (state.clone(), id.clone());
                move |_, window, cx| revoke(&state, endpoint.clone(), Revoke::Link(id.clone()), window, cx)
            }),
        );
    ui::item_row(details, Some(actions.into_any_element())).into_any_element()
}

fn client_session_row(
    state: &Entity<AccessState>,
    s: &AccessState,
    session: &ClientSession,
    can_revoke: bool,
    endpoint: Option<Endpoint>,
    now: i64,
    t: &Tokens,
) -> AnyElement {
    let id = &session.session_id;
    let live = session.current || session.connected;
    let status = match (&session.last_connected_at, live) {
        (Some(at), true) => format!("Connected for {}", logic::elapsed_label(logic::ms(at), now)),
        (None, true) => "Connected".into(),
        (Some(at), false) => format!("Last connected at {}", logic::format_timestamp(at)),
        (None, false) => "Not connected yet.".into(),
    };
    let dot = if live { ui::success() } else { t.muted_foreground.opacity(0.3) };
    let bits = session.device_bits();
    let details = v_flex()
        .gap(px(4.))
        .child(
            h_flex()
                .min_h(px(20.))
                .gap(px(6.))
                .child(ui::status_dot(ElementId::Name(format!("client-dot-{id}").into()), dot, Some(status.into())))
                .child(text_sm(div().font_weight(FontWeight::MEDIUM)).child(session.title()))
                .when(session.current, |el| {
                    el.child(
                        div()
                            .px(px(4.))
                            .py(px(2.))
                            .rounded(px(6.))
                            .border_1()
                            .border_color(t.border.opacity(0.5))
                            .bg(t.muted.opacity(0.5))
                            .text_size(px(10.))
                            .line_height(px(14.))
                            .text_color(t.muted_foreground.opacity(0.8))
                            .child("This device"),
                    )
                }),
        )
        .child(
            text_xs(h_flex().text_color(t.muted_foreground))
                .when(!bits.is_empty(), |el| el.child(format!("{}\u{a0}·\u{a0}", bits.join(" · "))))
                .child(scope_summary(ElementId::Name(format!("client-scopes-{id}").into()), &session.scopes, t)),
        );
    let revoking = s.revoking_session.as_ref() == Some(id);
    let actions = (!session.current).then(|| {
        WebButton::new(
            ElementId::Name(format!("client-revoke-{id}").into()),
            if revoking { "Revoking\u{2026}" } else { "Revoke" },
        )
        .variant(Variant::DestructiveOutline)
        .disabled(!can_revoke || revoking)
        .on_click({
            let (state, id) = (state.clone(), id.clone());
            move |_, window, cx| revoke(&state, endpoint.clone(), Revoke::Client(id.clone()), window, cx)
        })
        .into_any_element()
    });
    ui::item_row(details, actions).into_any_element()
}

// ---------------------------------------------------------------------------
// Create pairing link

/// The "Create pairing link" dialog's form.
struct CreateLinkForm {
    label: Entity<InputState>,
    scopes: Vec<String>,
    delegatable: Vec<String>,
    busy: bool,
    access: WeakEntity<AccessState>,
    endpoint: Option<Endpoint>,
}

impl CreateLinkForm {
    fn selected(&self) -> Vec<String> {
        self.scopes.iter().filter(|s| self.delegatable.contains(s)).cloned().collect()
    }

    fn preset(&mut self, scopes: &[&str], cx: &mut Context<Self>) {
        self.scopes = scopes.iter().map(|s| s.to_string()).filter(|s| self.delegatable.contains(s)).collect();
        cx.notify();
    }

    fn create(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let scopes = self.selected();
        if self.busy || scopes.is_empty() || !scopes.iter().all(|s| self.delegatable.contains(s)) {
            return;
        }
        self.busy = true;
        cx.notify();
        let label = self.label.read(cx).value().trim().to_owned();
        let mut body = json!({ "scopes": scopes });
        if !label.is_empty() {
            body["label"] = label.into();
        }
        let endpoint = self.endpoint.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = auth_request(endpoint, "/api/auth/pairing-token", Some(body)).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                cx.notify();
                match result {
                    Ok(created) => {
                        let (Some(id), Some(credential)) = (created["id"].as_str(), created["credential"].as_str())
                        else {
                            return;
                        };
                        let _ = this.access.update(cx, |access, cx| {
                            access.credentials.insert(id.to_owned(), credential.to_owned());
                            access.newest_link = Some(id.to_owned());
                            cx.notify();
                        });
                        window.close_dialog(cx);
                    }
                    Err(error) => {
                        let message = if error.is_empty() { "Failed to create pairing URL.".to_owned() } else { error };
                        error_toast(window, cx, "Could not create pairing URL", message);
                    }
                }
            });
        })
        .detach();
    }
}

impl Render for CreateLinkForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *theme::tokens(cx);
        let busy = self.busy;
        let selected = self.selected();
        let options = logic::SCOPE_OPTIONS.iter().filter(|(scope, ..)| self.delegatable.iter().any(|d| d == scope));
        let warning = if selected.is_empty() {
            Some((t.error_foreground, "Select at least one permission."))
        } else if selected.iter().any(|s| s == ACCESS_WRITE) {
            Some((t.warning_foreground, "This client can create or revoke access for other devices."))
        } else {
            None
        };
        let hairline = t.border.opacity(0.6);
        v_flex()
            .gap(px(16.))
            .child(
                text_sm(div().text_color(t.muted_foreground))
                    .child("Generate a one-time link that another device can use to pair with this backend as an authorized client."),
            )
            .child(
                v_flex()
                    .gap(px(6.))
                    .child(text_xs(div().font_weight(FontWeight::MEDIUM)).child("Client label (optional)"))
                    .child(Input::new(&self.label).disabled(busy)),
            )
            .child(
                v_flex()
                    .gap(px(12.))
                    .child(
                        h_flex()
                            .justify_between()
                            .gap(px(12.))
                            .child(
                                text_xs(v_flex())
                                    .child(div().font_weight(FontWeight::MEDIUM).child("Permissions"))
                                    .child(div().text_color(t.muted_foreground).child("Limit what the paired client can do.")),
                            )
                            .child(
                                h_flex()
                                    .gap(px(4.))
                                    .child(WebButton::new("scopes-read-only", "Read only").disabled(busy).on_click(
                                        cx.listener(|this, _, _, cx| this.preset(logic::READ_ONLY_SCOPES, cx)),
                                    ))
                                    .child(WebButton::new("scopes-standard", "Standard").disabled(busy).on_click(
                                        cx.listener(|this, _, _, cx| this.preset(logic::STANDARD_SCOPES, cx)),
                                    )),
                            ),
                    )
                    .child(
                        v_flex()
                            .id("pairing-scopes")
                            .max_h(px(280.))
                            .overflow_y_scroll()
                            .rounded(px(8.))
                            .border_1()
                            .border_color(t.input)
                            .bg(t.muted.opacity(0.25))
                            .children(options.enumerate().map(|(index, (scope, title, description))| {
                                let checked = self.scopes.iter().any(|s| s == scope);
                                h_flex()
                                    .id(ElementId::Name(format!("pairing-scope-{scope}").into()))
                                    .items_start()
                                    .gap(px(12.))
                                    .px(px(12.))
                                    .py(px(10.))
                                    .when(index > 0, |el| el.border_t_1().border_color(hairline))
                                    .hover(|style| style.bg(t.muted.opacity(0.4)))
                                    .when(!busy, |el| {
                                        el.cursor_pointer().on_click(cx.listener(move |this, _, _, cx| {
                                            this.scopes = logic::toggle_scope(&this.scopes, scope, !checked);
                                            cx.notify();
                                        }))
                                    })
                                    .child(div().mt(px(2.)).child(Checkbox::new(*scope).checked(checked).disabled(busy)))
                                    .child(
                                        text_xs(v_flex().min_w_0())
                                            .child(div().font_weight(FontWeight::MEDIUM).child(*title))
                                            .child(div().text_color(t.muted_foreground).child(*description)),
                                    )
                            })),
                    )
                    .when_some(warning, |el, (color, text)| el.child(text_xs(div().text_color(color)).child(text))),
            )
            .child(
                h_flex()
                    .justify_end()
                    .gap(px(8.))
                    .child(
                        WebButton::new("create-link-cancel", "Cancel")
                            .size(Size::Default)
                            .disabled(busy)
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        WebButton::new("create-link-submit", if busy { "Creating\u{2026}" } else { "Create link" })
                            .size(Size::Default)
                            .variant(Variant::Primary)
                            .disabled(busy || selected.is_empty())
                            .on_click(cx.listener(|this, _, window, cx| this.create(window, cx))),
                    ),
            )
    }
}

fn open_create_link_dialog(
    access: WeakEntity<AccessState>,
    endpoint: Option<Endpoint>,
    delegatable: Vec<String>,
    window: &mut Window,
    cx: &mut App,
) {
    let label = cx.new(|cx| InputState::new(window, cx).placeholder("e.g. Living room iPad"));
    label.update(cx, |input, cx| input.focus(window, cx));
    let scopes = logic::STANDARD_SCOPES.iter().map(|s| s.to_string()).collect();
    let form = cx.new(|_| CreateLinkForm { label, scopes, delegatable, busy: false, access, endpoint });
    window.open_dialog(cx, move |dialog, _, _| dialog.title("Create pairing link").w(px(448.)).child(form.clone()));
}
