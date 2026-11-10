//! Environments (`ConnectionsSettings view="environments"`): this app's local
//! environment switch and saved remote environments (`saved.rs`), then each connected
//! host's linked environments (`LinkedEnvironmentsSettings.tsx`), then load balancing and
//! pull request sharing, which route work across them.

use std::collections::HashSet;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::Button;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, ElementId, Entity, FontWeight, IntoElement, ParentElement as _, Render,
    SharedString, Styled as _, Subscription, Task, WeakEntity, Window, div, px,
};
use serde_json::{Value, json};

use super::super::widgets::{RowFrame, section_with_action};
use super::linking::{ANOTHER_ADDRESS, LinkableMachine, machine_badge, mint_link_code};
use super::logic::{self, ACCESS_READ, LinkTone};
use super::ui::{self, Size, Variant, WebButton, text_sm, text_xs};
use super::{error_toast, success_toast};
use crate::app::Workspace;
use crate::app::pages::settings::widgets::SettingsView;
use crate::app::sidebar::now_ms;
use crate::backend::Endpoint;
use crate::rpc::{RpcClient, RpcError, SocketId};
use crate::theme::{self, Tokens};

#[derive(Default)]
pub(super) struct EnvironmentsState {
    /// The socket `peerLinks.list` was read on; a new connection reads again.
    loaded_on: Option<SocketId>,
    /// `peerLinks.list`: `None` while loading.
    links: Option<Result<Vec<Value>, String>>,
    forgetting: Option<String>,
    /// The latest listing; a newer one replaces it, so an older answer never lands.
    _load: Option<Task<()>>,
}

impl EnvironmentsState {
    /// Lists the links on `client`'s socket unless they were read there already.
    fn sync_links(&mut self, client: RpcClient, cx: &mut Context<Self>) {
        if self.loaded_on.as_ref() != Some(&client.socket_id()) {
            self.load_links(Some(client), cx);
        }
    }

    fn load_links(&mut self, client: Option<RpcClient>, cx: &mut Context<Self>) {
        let Some(client) = client else { return };
        self.loaded_on = Some(client.socket_id());
        self._load = Some(cx.spawn(async move |this, cx| {
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
    pub(super) fn render_environments(
        &mut self,
        view: &SettingsView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = *theme::tokens(cx);
        let mut sections = vec![self.this_app_section(cx, &t), self.saved_environments_section(window, cx)];
        // `usePeerLinkHosts`: connected hosts offering links whose current session may read them.
        let hosts: Vec<(SharedString, String, RpcClient)> = self
            .sessions
            .iter()
            .filter(|s| {
                s.server_info.capability("peerLinks") && s.server_info.settings.bool("linkedEnvironmentsEnabled")
            })
            .filter_map(|s| {
                let id = s.id.clone()?;
                let label = s.server_info.environment_label.clone().unwrap_or_else(|| id.to_string());
                Some((id, label, s.client.clone()?))
            })
            .filter(|(id, ..)| self.env_grants(id, ACCESS_READ))
            .collect();
        let mut readable = Vec::new();
        for (id, label, client) in hosts {
            let key = SharedString::from(format!("settings-environments:{id}"));
            let state = window.use_keyed_state(key, cx, |_, _| EnvironmentsState::default());
            state.update(cx, |state, cx| state.sync_links(client, cx));
            readable.push((id, label, state));
        }
        let multiple = readable.len() > 1;
        let workspace = cx.entity().downgrade();
        for (index, (id, label, state)) in readable.into_iter().enumerate() {
            // The first is the search's "Linked environments" target; headings name their host
            // once there are several.
            let section_id: SharedString =
                if index == 0 { "linked-environments".into() } else { format!("linked-environments-{id}").into() };
            let title =
                if multiple { format!("Linked environments \u{b7} {label}") } else { "Linked environments".into() };
            let host = LinkHost { workspace: workspace.clone(), id };
            sections.push(self.linked_environments(&state, host, section_id, title, cx, &t));
        }
        sections.extend(self.load_balancing_section(cx));
        sections.extend(self.pull_request_routing_section(cx));
        self.settings_page("connections", view, sections, cx)
    }

    fn linked_environments(
        &self,
        state: &Entity<EnvironmentsState>,
        host: LinkHost,
        id: SharedString,
        title: String,
        cx: &App,
        t: &Tokens,
    ) -> AnyElement {
        let s = state.read(cx);
        let can_link = self.env_permits(&host.id, "peerLinks.link", &Value::Null);
        let can_unlink = self.env_permits(&host.id, "peerLinks.unlink", &Value::Null);
        let row_host = host.clone();
        let action = can_link.then(|| {
            let state = state.downgrade();
            WebButton::new("link-environment", "Link")
                .variant(Variant::Primary)
                .icon(IconName::Plus)
                .on_click(move |_, window, cx| {
                    let LinkHost { workspace, id } = host.clone();
                    open_link_dialog(workspace, id, state.clone(), window, cx);
                })
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
                links.iter().map(|link| linked_row(state, s, link, can_unlink, &row_host, now, t)).collect()
            }
        };
        section_with_action(id, title, action, rows, t)
    }
}

/// The environment a Link dialog links from, whose socket its requests read when they run.
#[derive(Clone)]
struct LinkHost {
    workspace: WeakEntity<Workspace>,
    id: SharedString,
}

impl LinkHost {
    /// The socket that sends `method` with `payload`, while this session may.
    fn client(&self, method: &str, payload: &Value, cx: &App) -> Option<RpcClient> {
        self.workspace.upgrade()?.read(cx).access_client(&self.id, method, payload)
    }

    /// The socket that lists the links.
    fn reader(&self, cx: &App) -> Option<RpcClient> {
        self.workspace.upgrade()?.read(cx).env_client(&self.id)
    }
}

fn linked_row(
    state: &Entity<EnvironmentsState>,
    s: &EnvironmentsState,
    link: &Value,
    can_unlink: bool,
    host: &LinkHost,
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
        let (state, host) = (state.downgrade(), host.clone());
        WebButton::new(
            ElementId::Name(format!("linked-forget-{id}").into()),
            if busy { "Forgetting\u{2026}" } else { "Forget" },
        )
        .variant(Variant::DestructiveOutline)
        .disabled(busy)
        .on_click(move |_, window, cx| {
            confirm_forget(state.clone(), host.clone(), id.clone(), label.clone(), window, cx);
        })
        .into_any_element()
    });
    ui::item_row(details, actions).into_any_element()
}

fn confirm_forget(
    state: WeakEntity<EnvironmentsState>,
    host: LinkHost,
    id: String,
    label: String,
    window: &mut Window,
    cx: &mut App,
) {
    window.open_alert_dialog(cx, move |dialog, _, _| {
        let (state, host, id, label) = (state.clone(), host.clone(), id.clone(), label.clone());
        dialog
            .title("Forget linked environment?")
            .description(format!(
                "Forget {label}? Agents here stop working there. Revoke \"T3 Code · \u{2026}\" in {label}'s Connections to end the session there too."
            ))
            .show_cancel(true)
            .ok_text("Forget")
            .ok_variant(gpui_kit::component::button::ButtonVariant::Danger)
            .on_ok(move |_, window, cx| {
                let (state, host, id) = (state.clone(), host.clone(), id.clone());
                let input = json!({ "environmentId": id });
                let Some(rpc) = host.client("peerLinks.unlink", &input, cx) else { return true };
                let _ = state.update(cx, |s, cx| {
                    s.forgetting = Some(id.clone());
                    cx.notify();
                });
                window
                    .spawn(cx, async move |cx| {
                        let result = rpc.request("peerLinks.unlink", input).await;
                        let _ = cx.update(|window, cx| {
                            let client = host.reader(cx);
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

/// What "Link an environment" asks for, given the pick (`LinkEnvironmentAction`).
#[derive(Debug, PartialEq, Eq)]
struct LinkFormPlan {
    /// `None` until the user picks; with no other machines known, only an address is left.
    selected: Option<String>,
    machine: Option<LinkableMachine>,
    needs_address: bool,
}

fn link_form_plan(machines: &[LinkableMachine], choice: Option<&str>) -> LinkFormPlan {
    let selected = choice.map(str::to_owned).or_else(|| machines.is_empty().then(|| ANOTHER_ADDRESS.to_owned()));
    let machine = selected.as_deref().and_then(|id| machines.iter().find(|m| m.environment_id == id)).cloned();
    let needs_address =
        selected.as_deref() == Some(ANOTHER_ADDRESS) || machine.as_ref().is_some_and(|m| m.urls.is_empty());
    LinkFormPlan { selected, machine, needs_address }
}

/// `peerLinks.link`'s input: the machine's addresses with its expected identity, or the
/// address typed in. `None` while it is incomplete.
fn link_input(plan: &LinkFormPlan, address: &str, code: &str, access: &str) -> Option<Value> {
    let code = code.trim();
    let urls: Vec<String> = match &plan.machine {
        Some(machine) if !plan.needs_address => machine.urls.clone(),
        _ => vec![address.trim().to_owned()],
    };
    let (url, alternates) = urls.split_first()?;
    if plan.selected.is_none() || url.is_empty() || code.is_empty() {
        return None;
    }
    let mut input = json!({ "url": url, "pairingCode": code, "access": access });
    if !alternates.is_empty() {
        input["alternateUrls"] = json!(alternates);
    }
    if let Some(machine) = &plan.machine {
        input["expectedEnvironmentId"] = json!(machine.environment_id);
        input["expectedLabel"] = json!(machine.label);
    }
    Some(input)
}

/// "Link an environment" (`LinkEnvironmentAction`): one of this client's other machines, or
/// an address, with a pairing code minted there when this client may, else one pasted in.
struct LinkForm {
    workspace: WeakEntity<Workspace>,
    /// The environment the link is made from.
    host: SharedString,
    choice: Option<String>,
    address: Entity<InputState>,
    code: Entity<InputState>,
    access: &'static str,
    busy: bool,
    /// Machines whose minting already failed, so the dialog asks for a code instead.
    mint_failed: HashSet<String>,
    list: WeakEntity<EnvironmentsState>,
    _subscriptions: Vec<Subscription>,
}

impl LinkForm {
    fn machines(&self, cx: &App) -> Vec<LinkableMachine> {
        let links = self.list.upgrade().and_then(|list| match &list.read(cx).links {
            Some(Ok(links)) => Some(links.clone()),
            _ => None,
        });
        let Some(workspace) = self.workspace.upgrade() else { return Vec::new() };
        workspace.read(cx).linkable_machines(&self.host, links.as_deref().unwrap_or_default(), cx)
    }

    /// Where a code can be minted for the picked machine, unless minting there failed already.
    fn mint_endpoint(&self, plan: &LinkFormPlan, cx: &App) -> Option<Endpoint> {
        let machine = plan.machine.as_ref().filter(|m| !self.mint_failed.contains(&m.environment_id))?;
        self.workspace.upgrade()?.read(cx).mint_endpoint(&machine.environment_id)
    }

    fn host(&self) -> LinkHost {
        LinkHost { workspace: self.workspace.clone(), id: self.host.clone() }
    }

    fn ready(&self, plan: &LinkFormPlan, cx: &App) -> bool {
        plan.selected.is_some()
            && (!plan.needs_address || !self.address.read(cx).value().trim().is_empty())
            && (self.mint_endpoint(plan, cx).is_some() || !self.code.read(cx).value().trim().is_empty())
    }

    fn pick(&mut self, choice: String, window: &mut Window, cx: &mut Context<Self>) {
        let placeholder = if choice == ANOTHER_ADDRESS {
            "From that environment's Connections, or t3 pair there"
        } else {
            "Paste the code"
        };
        self.code.update(cx, |input, cx| input.set_placeholder(placeholder, window, cx));
        self.choice = Some(choice);
        cx.notify();
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let plan = link_form_plan(&self.machines(cx), self.choice.as_deref());
        let host = self.host();
        if self.busy || !self.ready(&plan, cx) || host.client("peerLinks.link", &Value::Null, cx).is_none() {
            return;
        }
        let mint = self.mint_endpoint(&plan, cx);
        let (address, typed) = (self.address.read(cx).value().to_string(), self.code.read(cx).value().to_string());
        let access = self.access;
        self.busy = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            // Settles to the code to link with, or reports why there is none.
            let code = match mint {
                Some(endpoint) => match mint_link_code(endpoint, access).await {
                    Ok(code) => code,
                    Err(message) => {
                        let _ = this.update_in(cx, |this, window, cx| {
                            this.busy = false;
                            this.mint_failed.extend(plan.machine.as_ref().map(|m| m.environment_id.clone()));
                            error_toast(window, cx, "Could not link the environment", message);
                            cx.notify();
                        });
                        return;
                    }
                },
                None => typed,
            };
            let Some(input) = link_input(&plan, &address, &code, access) else {
                let _ = this.update(cx, |this, cx| {
                    this.busy = false;
                    cx.notify();
                });
                return;
            };
            // Minting can outlast the connection the dialog opened on.
            let Ok(client) = this.update(cx, |_, cx| host.client("peerLinks.link", &input, cx)) else { return };
            let result = match client {
                Some(client) => client.request("peerLinks.link", input).await,
                None => Err(RpcError::Disconnected("no connection that manages access on the host".into())),
            };
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                cx.notify();
                match result {
                    Ok(link) => {
                        let label = link["label"].as_str().unwrap_or_default();
                        success_toast(window, cx, format!("Linked {label}"), "");
                        let client = host.reader(cx);
                        let _ = this.list.update(cx, |list, cx| list.load_links(client, cx));
                        window.close_dialog(cx);
                    }
                    Err(error) => error_toast(window, cx, "Could not link the environment", error.to_string()),
                }
            });
        })
        .detach();
    }

    /// `LinkMachineSelect`: the other machines this client knows, then an address.
    fn machine_select(&self, machines: &[LinkableMachine], plan: &LinkFormPlan, cx: &mut Context<Self>) -> AnyElement {
        let t = *theme::tokens(cx);
        let trigger: AnyElement = match (plan.selected.as_deref(), &plan.machine) {
            (Some(ANOTHER_ADDRESS), _) => div().child("Another address\u{2026}").into_any_element(),
            (_, Some(machine)) => machine_name(machine, &t),
            _ => div().text_color(t.muted_foreground).child("Choose a machine").into_any_element(),
        };
        let trigger =
            h_flex().w_full().min_w_0().justify_between().gap(px(8.)).child(trigger).child(
                gpui_kit::component::Icon::new(IconName::ChevronDown).size(px(14.)).text_color(t.muted_foreground),
            );
        let form = cx.entity().downgrade();
        let machines = machines.to_vec();
        let select =
            Button::new("link-machine").outline().small().w_full().child(trigger).disabled(self.busy).dropdown_menu(
                move |mut menu, _, _| {
                    let choices = machines.iter().map(|m| Some(m.clone())).chain([None]);
                    for machine in choices {
                        let id =
                            machine.as_ref().map_or_else(|| ANOTHER_ADDRESS.to_owned(), |m| m.environment_id.clone());
                        let form = form.clone();
                        menu = menu.item(
                            PopupMenuItem::element(move |_, cx| {
                                let t = *theme::tokens(cx);
                                match &machine {
                                    Some(machine) => h_flex()
                                        .min_w(px(240.))
                                        .justify_between()
                                        .gap(px(12.))
                                        .child(machine_name(machine, &t))
                                        .children(machine_badge(machine, &t)),
                                    None => h_flex().child("Another address\u{2026}"),
                                }
                            })
                            .on_click(move |_, window, cx| {
                                let _ = form.update(cx, |form, cx| form.pick(id.clone(), window, cx));
                            }),
                        );
                    }
                    menu
                },
            );
        v_flex()
            .gap(px(6.))
            .child(text_xs(div().font_weight(FontWeight::MEDIUM)).child("Machine"))
            .child(select)
            .into_any_element()
    }
}

fn machine_name(machine: &LinkableMachine, t: &Tokens) -> AnyElement {
    h_flex()
        .min_w_0()
        .gap(px(8.))
        .child(ui::glyph(logic::machine_icon(&machine.machine), 16., t.foreground.opacity(0.8)))
        .child(div().min_w_0().truncate().child(machine.label.clone()))
        .into_any_element()
}

impl Render for LinkForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *theme::tokens(cx);
        let busy = self.busy;
        let machines = self.machines(cx);
        let plan = link_form_plan(&machines, self.choice.as_deref());
        let can_mint = self.mint_endpoint(&plan, cx).is_some();
        let ready = self.ready(&plan, cx);
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
        let field = |label: &'static str, input: &Entity<InputState>, help: Option<String>| {
            v_flex()
                .gap(px(6.))
                .child(text_xs(div().font_weight(FontWeight::MEDIUM)).child(label))
                .child(Input::new(input).disabled(busy))
                .children(help.map(|help| text_xs(div().text_color(t.muted_foreground)).child(help)))
        };
        let machine = plan.machine.as_ref();
        let address_help = machine.map(|m| {
            format!(
                "This app has no address for {} that another machine can reach. Enter its https or Tailscale address.",
                m.label
            )
        });
        let code_help = machine.map(|m| {
            format!("Create a link on {}: Settings → Connections → Create link, then paste its code here.", m.label)
        });
        v_flex()
            .gap(px(16.))
            .when(!machines.is_empty(), |el| el.child(self.machine_select(&machines, &plan, cx)))
            .when(machine.is_some_and(|m| m.linked), |el| {
                el.child(
                    text_xs(div().text_color(t.muted_foreground))
                        .child("Already linked. Linking again renews it with the access you pick."),
                )
            })
            .when(plan.needs_address, |el| el.child(field("Address", &self.address, address_help)))
            .when(plan.selected.is_some() && !can_mint, |el| el.child(field("Pairing code", &self.code, code_help)))
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
                            .disabled(busy || !ready)
                            .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
                    ),
            )
    }
}

fn open_link_dialog(
    workspace: WeakEntity<Workspace>,
    host: SharedString,
    list: WeakEntity<EnvironmentsState>,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(entity) = workspace.upgrade() else { return };
    let address = cx.new(|cx| InputState::new(window, cx).placeholder("https://box.example.ts.net"));
    let code =
        cx.new(|cx| InputState::new(window, cx).placeholder("From that environment's Connections, or t3 pair there"));
    let form = cx.new(|cx| {
        let on_input = |this: &mut LinkForm,
                        input: &Entity<InputState>,
                        event: &InputEvent,
                        window: &mut Window,
                        cx: &mut Context<LinkForm>| {
            match event {
                InputEvent::PressEnter { .. } => this.submit(window, cx),
                InputEvent::Change => {
                    // Machines found after typing began must not hide the address being typed.
                    if *input == this.address && this.choice.is_none() {
                        this.choice = Some(ANOTHER_ADDRESS.to_owned());
                    }
                    cx.notify();
                }
                _ => {}
            }
        };
        LinkForm {
            _subscriptions: vec![
                cx.subscribe_in(&address, window, on_input),
                cx.subscribe_in(&code, window, on_input),
                // Machines connect, disconnect and change their grants while the dialog is open.
                cx.observe(&entity, |_, _, cx| cx.notify()),
            ],
            workspace,
            host,
            choice: None,
            address,
            code,
            access: "approval-required",
            busy: false,
            mint_failed: HashSet::new(),
            list,
        }
    });
    form.update(cx, |form, cx| {
        if form.machines(cx).is_empty() {
            form.address.update(cx, |input, cx| input.focus(window, cx));
        }
    });
    let description: SharedString = "Agents here can then launch, message and wait on threads there. It appears in that environment's Connections, where it can be revoked.".into();
    window.open_dialog(cx, move |dialog, _, cx| {
        let t = theme::tokens(cx);
        // The web keeps it open while linking, so a result is never lost with the form.
        let busy = form.read(cx).busy;
        dialog
            .title("Link an environment")
            .w(px(448.))
            .close_button(!busy)
            .keyboard(!busy)
            .overlay_closable(!busy)
            .child(text_sm(div().mb(px(16.)).text_color(t.muted_foreground)).child(description.clone()))
            .child(form.clone())
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn machine(id: &str, urls: &[&str]) -> LinkableMachine {
        LinkableMachine {
            environment_id: id.into(),
            label: format!("Machine {id}"),
            machine: "server".into(),
            connected: true,
            linked: false,
            urls: urls.iter().map(|u| (*u).to_owned()).collect(),
        }
    }

    #[test]
    fn with_no_other_machines_only_an_address_is_left() {
        let plan = link_form_plan(&[], None);
        assert_eq!((plan.selected.as_deref(), plan.needs_address), (Some(ANOTHER_ADDRESS), true));
        assert_eq!(
            link_input(&plan, " https://box.ts.net ", " CODE ", "auto"),
            Some(json!({"url": "https://box.ts.net", "pairingCode": "CODE", "access": "auto"}))
        );
        assert_eq!(link_input(&plan, "", "CODE", "auto"), None, "an address is required");
    }

    #[test]
    fn a_known_machine_links_over_its_addresses_with_its_identity() {
        let machines = [machine("vps", &["https://vps.ts.net/", "http://10.0.0.2:3773"]), machine("ssh", &[])];
        let unpicked = link_form_plan(&machines, None);
        assert_eq!((unpicked.selected, unpicked.needs_address), (None, false), "nothing is asked before a pick");
        let plan = link_form_plan(&machines, Some("vps"));
        assert_eq!(
            link_input(&plan, "", "CODE", "read-only"),
            Some(json!({
                "url": "https://vps.ts.net/", "alternateUrls": ["http://10.0.0.2:3773"], "pairingCode": "CODE",
                "access": "read-only", "expectedEnvironmentId": "vps", "expectedLabel": "Machine vps",
            }))
        );
        let ssh = link_form_plan(&machines, Some("ssh"));
        assert!(ssh.needs_address, "an SSH-only machine needs an address another machine can reach");
        let input = link_input(&ssh, "https://ssh.ts.net", "CODE", "auto").expect("complete");
        assert_eq!(
            (input["url"].clone(), input["expectedEnvironmentId"].clone()),
            (json!("https://ssh.ts.net"), json!("ssh"))
        );
    }
}
