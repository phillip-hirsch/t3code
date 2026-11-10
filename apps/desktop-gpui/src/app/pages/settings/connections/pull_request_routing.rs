//! Settings › Environments › Pull request sharing (`PullRequestRoutingSettings.tsx`): which
//! machines may read, or read and act on, pull requests through each other's source control
//! access. The grants live in this device's environment catalog; the router
//! ([`crate::pull_request_routing`]) reads them from what [`Workspace::publish_pull_request_routing`]
//! hands it.

use gpui_kit::assets::IconName;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{Disableable as _, Icon, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Context, ElementId, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};

use super::super::widgets::select_trigger;
use super::error_toast;
use super::ui::{fold_trigger, text_sm, text_xs};
use crate::app::Workspace;
use crate::environments::{Presented, Route};
use crate::pull_request_routing::{self as routing, PERMISSIONS, Permission};
use crate::theme::{self, Tokens};

const SECTION: &str = "pull-request-routing";

/// A machine the section lists and the router may use.
struct Machine {
    id: String,
    label: String,
    /// What a grant is keyed to; `None` until the primary's server is up, or for a saved
    /// route that cannot be keyed.
    connection_key: Option<String>,
    /// `isLocal`: reached over loopback.
    local: bool,
    /// The row's subtitle: the route the connection runs over, else the first saved one.
    address: Option<String>,
    primary: bool,
}

impl Workspace {
    /// The primary and every enabled saved environment, unsupported ones left out, as
    /// `loadBalancingEnvironments` lists them for both sections.
    fn routing_machines(&self, cx: &App) -> Vec<Machine> {
        let mut machines = Vec::new();
        if let Some(id) = self.primary_environment() {
            let http_base = self.sessions()[0].endpoint.as_ref().map(|endpoint| endpoint.http_base.as_str());
            machines.push(Machine {
                id: id.to_string(),
                label: self.primary_info().environment_label.clone().unwrap_or_else(|| id.to_string()),
                connection_key: http_base.and_then(|http_base| routing::connection_key(true, id, http_base)),
                local: http_base.is_some_and(routing::is_local),
                address: None,
                primary: true,
            });
        }
        let saved = self.saved_environments.read(cx);
        for environment in saved.environments() {
            let unsupported = saved
                .session(&environment.environment_id)
                .is_some_and(|session| session.state.presented() == Presented::Unsupported);
            if !environment.enabled || unsupported || saved.is_primary(&environment.environment_id) {
                continue;
            }
            let active = saved.session(&environment.environment_id).and_then(|session| session.active_route());
            let shown = active.and_then(|id| environment.route(id)).or_else(|| environment.routes.first());
            machines.push(Machine {
                id: environment.environment_id.clone(),
                label: environment.label.clone(),
                connection_key: routing::saved_connection_key(&environment.environment_id, &environment.routes),
                local: environment.routes.first().and_then(|route| route.http_base()).is_some_and(routing::is_local),
                address: shown.map(Route::address_text),
                primary: false,
            });
        }
        machines
    }

    /// Hands the router every connected machine with this device's grant for it. Runs whenever
    /// a connection, a server config or the grants change.
    pub(in crate::app) fn publish_pull_request_routing(&self, cx: &App) {
        let grants = self.saved_environments.read(cx).pull_request_routing();
        let members = self
            .routing_machines(cx)
            .into_iter()
            .filter_map(|machine| {
                let session = self.session(&machine.id)?;
                Some(routing::Member {
                    transport: session.client.clone()?,
                    local: machine.local,
                    permission: routing::permission_for(machine.connection_key.as_deref(), grants),
                    checks: session.server_info.capability("pullRequestChecks"),
                    environment_id: machine.id,
                })
            })
            .collect();
        routing::publish(members);
    }

    /// Shown from two machines, after load balancing.
    pub(super) fn pull_request_routing_section(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let machines = self.routing_machines(cx);
        if machines.len() < 2 {
            return None;
        }
        let t = *theme::tokens(cx);
        let saved = self.saved_environments.read(cx);
        let grants = saved.pull_request_routing();
        let editable = saved.editable() && !self.settings_ui.pull_request_routing_saving;
        let rows: Vec<(Machine, Option<String>, Permission)> = machines
            .into_iter()
            .map(|machine| {
                let key = machine.connection_key.clone();
                let permission = routing::permission_for(key.as_deref(), grants);
                (machine, key, permission)
            })
            .collect();
        let entries: Vec<(&str, Permission)> = rows.iter().map(|(m, _, p)| (m.label.as_str(), *p)).collect();
        let summary = routing::summarize(&entries).unwrap_or_else(|| "Off".into());
        let open = self.settings_ui.advanced_open.contains(SECTION);
        let header = h_flex()
            .id("pull-request-routing-fold")
            .px(px(16.))
            .min_h(px(44.))
            .py(px(8.))
            .gap(px(8.))
            .cursor_pointer()
            .map(|el| fold_trigger(el, t.ring))
            .on_click(cx.listener(|this, _, _, cx| {
                if !this.settings_ui.advanced_open.remove(SECTION) {
                    this.settings_ui.advanced_open.insert(SECTION);
                }
                cx.notify();
            }))
            .child(
                Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight })
                    .size(px(16.))
                    .text_color(t.muted_foreground),
            )
            .child(text_sm(div().flex_none().font_weight(FontWeight::MEDIUM)).child("Pull request sharing"))
            .child(text_xs(div().min_w_0().truncate().text_color(t.muted_foreground)).child(summary));
        let body = open.then(|| {
            v_flex()
                .child(
                    text_xs(div().px(px(16.)).py(px(10.)).text_color(t.muted_foreground)).child(
                        "Machines you trust here can read PR data through each other's source control access. Enable both machines. Read and act may use broader permissions than the machine that owns them. This applies only to this device.",
                    ),
                )
                .children(rows.into_iter().map(|(machine, key, permission)| {
                    self.routing_row(machine, key, permission, editable, &t, cx)
                }))
        });
        Some(
            v_flex()
                .id(SECTION)
                .rounded(px(12.))
                .border_1()
                .border_color(t.border)
                .bg(t.card)
                .child(header)
                .children(body)
                .into_any_element(),
        )
    }

    fn routing_row(
        &self,
        machine: Machine,
        key: Option<String>,
        permission: Permission,
        editable: bool,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let trigger_id = SharedString::from(format!("pull-request-routing-{}", machine.id));
        let subtitle = if machine.primary { Some("This machine".to_owned()) } else { machine.address.clone() };
        let view = cx.entity();
        let disabled = !editable || key.is_none();
        let id = machine.id.clone();
        let trigger = select_trigger(&trigger_id, permission.label(), 128., t)
            .disabled(disabled)
            .dropdown_menu_with_anchor(gpui_kit::Anchor::TopRight, move |mut menu, _, _| {
                for (value, label) in PERMISSIONS {
                    let (view, id) = (view.clone(), id.clone());
                    menu = menu.item(PopupMenuItem::new(label).checked(value == permission).on_click(
                        move |_, window, cx| {
                            view.update(cx, |this, cx| {
                                // The machine's routes now: the menu can outlive an edit to them.
                                let machine = this.routing_machines(cx).into_iter().find(|m| m.id == id);
                                let Some(key) = machine.and_then(|m| m.connection_key) else { return };
                                this.set_pull_request_routing(id.clone(), key, value, window, cx);
                            });
                        },
                    ));
                }
                menu
            });
        h_flex()
            .id(ElementId::Name(format!("pull-request-routing-row-{}", machine.id).into()))
            .min_h(px(52.))
            .px(px(16.))
            .py(px(8.))
            .gap(px(12.))
            .border_t_1()
            .border_color(t.border.opacity(0.5))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(text_sm(div().truncate().font_weight(FontWeight::MEDIUM)).child(machine.label))
                    .children(
                        subtitle
                            .map(|subtitle| text_xs(div().truncate().text_color(t.muted_foreground)).child(subtitle)),
                    ),
            )
            .child(trigger)
            .into_any_element()
    }

    /// Saves one machine's grant; the pickers wait until it is on disk.
    fn set_pull_request_routing(
        &mut self,
        environment_id: String,
        connection_key: String,
        permission: Permission,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.settings_ui.pull_request_routing_saving || !self.saved_environments.read(cx).editable() {
            return;
        }
        self.settings_ui.pull_request_routing_saving = true;
        let saving = self
            .saved_environments
            .update(cx, |saved, cx| saved.set_pull_request_routing(&environment_id, connection_key, permission, cx));
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = saving.finished().await;
            drop(this.update_in(cx, |this, window, cx| {
                this.settings_ui.pull_request_routing_saving = false;
                cx.notify();
                if let Err(error) = result {
                    error_toast(window, cx, "Could not save pull request routing permission", error);
                }
            }));
        })
        .detach();
    }
}
