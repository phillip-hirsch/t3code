//! Settings › Environments › Load balancing (`LoadBalancingSettings.tsx`): the switch and each
//! machine's preference, folded like `FoldedSettingsSection`.

use gpui_kit::assets::IconName;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{Icon, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, ElementId, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use serde_json::{Map, Value, json};

use super::super::widgets::{ROW_SELECT_MIN_WIDTH, select_trigger, switch};
use super::ui::{text_sm, text_xs};
use crate::app::Workspace;
use crate::load_balancing::{self, PREFERENCES};
use crate::theme::{self, Tokens};

const SECTION: &str = "load-balancing";

/// A machine the section lists.
struct Machine {
    id: String,
    label: String,
    address: Option<String>,
}

impl Workspace {
    /// The primary and every enabled saved environment, unsupported ones left out
    /// (`loadBalancingEnvironments`); the section shows from two of them.
    fn load_balancing_machines(&self, cx: &Context<Self>) -> Vec<Machine> {
        let mut machines = Vec::new();
        if let Some(id) = self.primary_environment() {
            let info = self.primary_info();
            machines.push(Machine {
                id: id.to_string(),
                label: info.environment_label.clone().unwrap_or_else(|| id.to_string()),
                address: self.sessions()[0].endpoint.as_ref().map(|e| e.http_base.clone()),
            });
        }
        let saved = self.saved_environments.read(cx);
        for environment in saved.environments() {
            let unsupported = saved
                .session(&environment.environment_id)
                .is_some_and(|s| s.state.presented() == crate::environments::Presented::Unsupported);
            if !environment.enabled || unsupported || saved.is_primary(&environment.environment_id) {
                continue;
            }
            machines.push(Machine {
                id: environment.environment_id.clone(),
                label: environment.label.clone(),
                address: Some(environment.http_base.clone()),
            });
        }
        machines
    }

    pub(super) fn load_balancing_section(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let machines = self.load_balancing_machines(cx);
        if machines.len() < 2 {
            return None;
        }
        let t = *theme::tokens(cx);
        let values = &self.client_settings.values;
        let enabled = values.bool("loadBalancingEnabled");
        let weights = values.get("loadBalancingWeights").clone();
        let labels: Vec<(&str, &str)> = machines.iter().map(|m| (m.id.as_str(), m.label.as_str())).collect();
        let summary = if enabled { load_balancing::summarize(&labels, &weights) } else { Some("Off".to_owned()) };
        let open = self.settings_ui.advanced_open.contains(SECTION);
        let toggle_id = SharedString::from(SECTION);
        let header = h_flex()
            .px(px(16.))
            .gap(px(16.))
            .child(
                h_flex()
                    .id("load-balancing-fold")
                    .flex_1()
                    .min_w_0()
                    .min_h(px(44.))
                    .py(px(8.))
                    .gap(px(8.))
                    .cursor_pointer()
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
                    .child(text_sm(div().flex_none().font_weight(FontWeight::MEDIUM)).child("Load balancing"))
                    .children(summary.map(|summary| {
                        text_xs(div().min_w_0().truncate().text_color(t.muted_foreground)).child(summary)
                    })),
            )
            .child(switch(&toggle_id, enabled, false, &t).on_click(cx.listener(move |this, _, window, cx| {
                this.update_settings(json!({"loadBalancingEnabled": !enabled}), window, cx);
            })));
        let body = open.then(|| {
            v_flex()
                .child(
                    text_xs(div().px(px(16.)).py(px(10.)).text_color(t.muted_foreground)).child(
                        "New threads in shared projects start on the machine with the most free CPU and memory, weighted by each machine's preference.",
                    ),
                )
                .children(machines.iter().map(|machine| self.load_preference_row(machine, &weights, enabled, &t, cx)))
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

    fn load_preference_row(
        &self,
        machine: &Machine,
        weights: &Value,
        enabled: bool,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let preference = load_balancing::preference_for_weight(weights.get(&machine.id).and_then(Value::as_f64));
        let trigger_id = SharedString::from(format!("load-preference-{}", machine.id));
        let view = cx.entity();
        let (id, current) = (machine.id.clone(), weights.as_object().cloned().unwrap_or_default());
        let trigger = select_trigger(&trigger_id, load_balancing::preference_label(preference), 128., t)
            .min_w(px(ROW_SELECT_MIN_WIDTH))
            .when(!enabled, |button| button.opacity(0.5))
            .dropdown_menu_with_anchor(gpui_kit::Anchor::TopRight, move |mut menu, _, _| {
                for (value, label) in PREFERENCES {
                    let (view, id, current) = (view.clone(), id.clone(), current.clone());
                    menu =
                        menu.item(PopupMenuItem::new(label).checked(value == preference).disabled(!enabled).on_click(
                            move |_, window, cx| {
                                let mut weights: Map<String, Value> = current.clone();
                                weights.insert(id.clone(), value.into());
                                view.update(cx, |this, cx| {
                                    this.update_settings(json!({"loadBalancingWeights": weights}), window, cx);
                                });
                            },
                        ));
                }
                menu
            });
        h_flex()
            .id(ElementId::Name(format!("load-row-{}", machine.id).into()))
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
                    .child(text_sm(div().truncate().font_weight(FontWeight::MEDIUM)).child(machine.label.clone()))
                    .children(
                        machine
                            .address
                            .clone()
                            .map(|address| text_xs(div().truncate().text_color(t.muted_foreground)).child(address)),
                    ),
            )
            .child(trigger)
            .into_any_element()
    }
}
