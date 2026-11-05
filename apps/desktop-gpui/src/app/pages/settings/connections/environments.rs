//! Environments (`ConnectionsSettings view="environments"`): this app's local
//! environment switch and saved remote environments (`saved.rs`), then this
//! environment's linked environments (`LinkedEnvironmentsSettings.tsx`). Load
//! balancing and pull request sharing route work across environments, which this
//! client does not do yet, so it never shows them.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::Button;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, ElementId, Entity, FontWeight, IntoElement, ParentElement as _, Render,
    SharedString, Styled as _, Subscription, Task, WeakEntity, Window, div, px,
};
use serde_json::{Value, json};

use super::super::widgets::{RowFrame, section_with_action};
use super::logic::{self, ACCESS_READ, ACCESS_WRITE, LinkTone, Session};
use super::ui::{self, Size, Variant, WebButton, text_sm, text_xs};
use super::{auth_request, error_toast, success_toast};
use crate::app::Workspace;
use crate::app::sidebar::now_ms;
use crate::rpc::RpcClient;
use crate::theme::{self, Tokens};

#[derive(Default)]
pub(super) struct EnvironmentsState {
    started: bool,
    session: Option<Session>,
    /// `peerLinks.list`: `None` while loading.
    links: Option<Result<Vec<Value>, String>>,
    forgetting: Option<String>,
    _tasks: Vec<Task<()>>,
}

impl EnvironmentsState {
    fn load_links(&mut self, client: Option<RpcClient>, cx: &mut Context<Self>) {
        let Some(client) = client else { return };
        self._tasks.push(cx.spawn(async move |this, cx| {
            let result = client.request("peerLinks.list", json!({})).await;
            let _ = this.update(cx, |this, cx| {
                this.links = Some(match result {
                    Ok(value) => Ok(value["links"].as_array().cloned().unwrap_or_default()),
                    Err(error) => Err(error.to_string()),
                });
                cx.notify();
            });
        }));
    }
}

impl Workspace {
    pub(super) fn render_environments(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let state = window.use_keyed_state("settings-environments", cx, |_, _| EnvironmentsState::default());
        let (client, endpoint) = (self.client(), self.endpoint.clone());
        if client.is_some() && !state.read(cx).started {
            state.update(cx, |state, cx| {
                state.started = true;
                let client = client.clone();
                state._tasks.push(cx.spawn(async move |this, cx| {
                    let session = auth_request(endpoint, "/api/auth/session", None).await;
                    let _ = this.update(cx, |this, cx| {
                        let session = session.map(|v| Session::from_json(&v)).unwrap_or_default();
                        if session.grants(ACCESS_READ) {
                            this.load_links(client, cx);
                        }
                        this.session = Some(session);
                        cx.notify();
                    });
                }));
            });
        }
        let t = *theme::tokens(cx);
        let mut sections = vec![self.this_app_section(cx, &t), self.saved_environments_section(window, cx)];
        let info = &self.server_info;
        let offers_links = info.capability("peerLinks") && info.settings.bool("linkedEnvironmentsEnabled");
        let readable = state.read(cx).session.as_ref().is_some_and(|s| s.grants(ACCESS_READ));
        if offers_links && readable && self.client().is_some() {
            sections.push(self.linked_environments(&state, cx, &t));
        }
        let view = self.settings_view();
        self.settings_page("connections", &view, sections, cx)
    }

    fn linked_environments(&self, state: &Entity<EnvironmentsState>, cx: &App, t: &Tokens) -> AnyElement {
        let s = state.read(cx);
        let session = s.session.as_ref();
        let can_link = session.is_some_and(|s| s.grants(ACCESS_WRITE));
        let client = self.client();
        let action = can_link.then(|| {
            let (client, state) = (client.clone(), state.downgrade());
            WebButton::new("link-environment", "Link")
                .variant(Variant::Primary)
                .icon(IconName::Plus)
                .on_click(move |_, window, cx| open_link_dialog(client.clone(), state.clone(), window, cx))
                .into_any_element()
        });
        let rows = match &s.links {
            Some(Err(error)) => {
                vec![RowFrame::new("Could not load linked environments").description(error.clone()).render(t).into_any_element()]
            }
            None => vec![RowFrame::new("Loading linked environments\u{2026}").render(t).into_any_element()],
            Some(Ok(links)) if links.is_empty() => vec![
                RowFrame::new("No linked environments")
                    .description("Link another machine running T3 Code, such as a VPS, so agents here can launch and drive threads there.")
                    .render(t)
                    .into_any_element(),
            ],
            Some(Ok(links)) => {
                let now = now_ms();
                links.iter().map(|link| linked_row(state, s, link, can_link, client.clone(), now, t)).collect()
            }
        };
        section_with_action("linked-environments", "Linked environments", action, rows, t)
    }
}

fn linked_row(
    state: &Entity<EnvironmentsState>,
    s: &EnvironmentsState,
    link: &Value,
    can_unlink: bool,
    client: Option<RpcClient>,
    now: i64,
    t: &Tokens,
) -> AnyElement {
    let id = link["environmentId"].as_str().unwrap_or_default().to_owned();
    let label = link["label"].as_str().unwrap_or_default().to_owned();
    let (tone, status) = logic::link_health(link, now);
    let color = match tone {
        LinkTone::Reachable => ui::success(),
        LinkTone::Expiring => t.warning,
        LinkTone::Broken => t.error,
        LinkTone::Idle => t.muted_foreground.opacity(0.3),
    };
    let (access, _) = logic::link_access(link["access"].as_str().unwrap_or_default());
    let urls: Vec<&str> =
        link["urls"].as_array().map(|u| u.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
    let details = v_flex()
        .gap(px(4.))
        .child(
            h_flex()
                .min_h(px(20.))
                .gap(px(6.))
                .child(ui::status_dot(
                    ElementId::Name(format!("linked-dot-{id}").into()),
                    color,
                    Some(status.clone().into()),
                ))
                .child(text_sm(div().font_weight(FontWeight::MEDIUM)).child(label.clone())),
        )
        .child(text_xs(div().text_color(t.muted_foreground)).child(format!("{access} · {status}")))
        .child(text_xs(div().truncate().text_color(t.muted_foreground.opacity(0.8))).child(urls.join(", ")));
    let busy = s.forgetting.as_ref() == Some(&id);
    let actions = can_unlink.then(|| {
        let state = state.downgrade();
        WebButton::new(
            ElementId::Name(format!("linked-forget-{id}").into()),
            if busy { "Forgetting\u{2026}" } else { "Forget" },
        )
        .variant(Variant::DestructiveOutline)
        .disabled(busy)
        .on_click(move |_, window, cx| {
            confirm_forget(state.clone(), client.clone(), id.clone(), label.clone(), window, cx)
        })
        .into_any_element()
    });
    ui::item_row(details, actions).into_any_element()
}

fn confirm_forget(
    state: WeakEntity<EnvironmentsState>,
    client: Option<RpcClient>,
    id: String,
    label: String,
    window: &mut Window,
    cx: &mut App,
) {
    window.open_alert_dialog(cx, move |dialog, _, _| {
        let (state, client, id, label) = (state.clone(), client.clone(), id.clone(), label.clone());
        dialog
            .title("Forget linked environment?")
            .description(format!(
                "Forget {label}? Agents here stop working there. Revoke \"T3 Code · \u{2026}\" in {label}'s Connections to end the session there too."
            ))
            .show_cancel(true)
            .ok_text("Forget")
            .ok_variant(gpui_kit::component::button::ButtonVariant::Danger)
            .on_ok(move |_, window, cx| {
                let (state, client, id) = (state.clone(), client.clone(), id.clone());
                let Some(rpc) = client.clone() else { return true };
                let _ = state.update(cx, |s, cx| {
                    s.forgetting = Some(id.clone());
                    cx.notify();
                });
                window
                    .spawn(cx, async move |cx| {
                        let result = rpc.request("peerLinks.unlink", json!({ "environmentId": id })).await;
                        let _ = cx.update(|window, cx| {
                            let _ = state.update(cx, |s, cx| {
                                s.forgetting = None;
                                s.load_links(client, cx);
                            });
                            if let Err(error) = result {
                                error_toast(window, cx, "Could not forget the link", error.to_string());
                            }
                        });
                    })
                    .detach();
                true
            })
    });
}

/// "Link an environment" (`LinkEnvironmentAction`). This client knows no other
/// machines, so the link always goes to a typed address with a pairing code.
struct LinkForm {
    address: Entity<InputState>,
    code: Entity<InputState>,
    access: &'static str,
    busy: bool,
    client: Option<RpcClient>,
    list: WeakEntity<EnvironmentsState>,
    _subscriptions: [Subscription; 2],
}

impl LinkForm {
    fn ready(&self, cx: &App) -> bool {
        !self.address.read(cx).value().trim().is_empty() && !self.code.read(cx).value().trim().is_empty()
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else { return };
        if self.busy || !self.ready(cx) {
            return;
        }
        self.busy = true;
        cx.notify();
        let input = json!({
            "url": self.address.read(cx).value().trim(),
            "pairingCode": self.code.read(cx).value().trim(),
            "access": self.access,
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("peerLinks.link", input).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                cx.notify();
                match result {
                    Ok(link) => {
                        let label = link["label"].as_str().unwrap_or_default();
                        success_toast(window, cx, format!("Linked {label}"), "");
                        let client = this.client.clone();
                        let _ = this.list.update(cx, |list, cx| list.load_links(client, cx));
                        window.close_dialog(cx);
                    }
                    Err(error) => error_toast(window, cx, "Could not link the environment", error.to_string()),
                }
            });
        })
        .detach();
    }
}

impl Render for LinkForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *theme::tokens(cx);
        let busy = self.busy;
        let (access_label, access_description) = logic::link_access(self.access);
        let form = cx.entity().downgrade();
        let picker =
            Button::new("link-access").outline().small().w_full().label(access_label).disabled(busy).dropdown_menu(
                move |mut menu, _, _| {
                    for (value, label, description) in logic::LINK_ACCESS {
                        let form = form.clone();
                        menu = menu.item(
                            PopupMenuItem::element(move |_, cx| {
                                let t = theme::tokens(cx);
                                text_xs(v_flex().py(px(2.)).max_w(px(360.)))
                                    .child(text_sm(div().font_weight(FontWeight::MEDIUM)).child(*label))
                                    .child(div().text_color(t.muted_foreground).child(*description))
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
                },
            );
        let field = |label: &'static str, input: &Entity<InputState>| {
            v_flex()
                .gap(px(6.))
                .child(text_xs(div().font_weight(FontWeight::MEDIUM)).child(label))
                .child(Input::new(input).disabled(busy))
        };
        v_flex()
            .gap(px(16.))
            .child(field("Address", &self.address))
            .child(field("Pairing code", &self.code))
            .child(
                v_flex()
                    .gap(px(6.))
                    .child(text_xs(div().font_weight(FontWeight::MEDIUM)).child("What agents here may do there"))
                    .child(picker)
                    .child(text_xs(div().text_color(t.muted_foreground)).child(format!(
                        "{access_description} An agent never gets more access there than its own mode here. Below full access, it can change only threads this link started, not that machine's own threads, projects or settings."
                    ))),
            )
            .child(
                h_flex()
                    .justify_end()
                    .gap(px(8.))
                    .child(
                        WebButton::new("link-cancel", "Cancel")
                            .size(Size::Default)
                            .disabled(busy)
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        WebButton::new("link-submit", if busy { "Linking\u{2026}" } else { "Link" })
                            .size(Size::Default)
                            .variant(Variant::Primary)
                            .disabled(busy || !self.ready(cx))
                            .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
                    ),
            )
    }
}

fn open_link_dialog(client: Option<RpcClient>, list: WeakEntity<EnvironmentsState>, window: &mut Window, cx: &mut App) {
    let address = cx.new(|cx| InputState::new(window, cx).placeholder("https://box.example.ts.net"));
    let code =
        cx.new(|cx| InputState::new(window, cx).placeholder("From that environment's Connections, or t3 pair there"));
    address.update(cx, |input, cx| input.focus(window, cx));
    let form = cx.new(|cx| {
        let on_input = |this: &mut LinkForm,
                        _: &Entity<InputState>,
                        event: &InputEvent,
                        window: &mut Window,
                        cx: &mut Context<LinkForm>| {
            match event {
                InputEvent::PressEnter { .. } => this.submit(window, cx),
                InputEvent::Change => cx.notify(),
                _ => {}
            }
        };
        LinkForm {
            _subscriptions: [cx.subscribe_in(&address, window, on_input), cx.subscribe_in(&code, window, on_input)],
            address,
            code,
            access: "approval-required",
            busy: false,
            client,
            list,
        }
    });
    let description: SharedString = "Agents here can then launch, message and wait on threads there. It appears in that environment's Connections, where it can be revoked.".into();
    window.open_dialog(cx, move |dialog, _, cx| {
        let t = theme::tokens(cx);
        dialog
            .title("Link an environment")
            .w(px(448.))
            .child(text_sm(div().mb(px(16.)).text_color(t.muted_foreground)).child(description.clone()))
            .child(form.clone())
    });
}
