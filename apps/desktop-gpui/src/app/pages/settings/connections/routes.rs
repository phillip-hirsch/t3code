//! `EnvironmentRoutesList`: a saved environment's routes, preferred first. Drag a route's grip,
//! or focus it and press the arrow keys, to reorder; remove one while another remains.

use std::collections::HashMap;
use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::component::{Icon, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, ElementId, Entity, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, WeakEntity, Window, div,
    px,
};

use super::ui::{self, Variant, WebButton, text_2xs, text_xs};
use super::{error_toast, saved::SavedUi};
use crate::app::environments::SavedEnvironments;
use crate::environments::{self, SavedEnvironment};
use crate::theme::Tokens;

/// Route orders still being saved, by environment, shown in place of the saved one meanwhile.
#[derive(Default)]
pub(super) struct PendingOrders(HashMap<String, Vec<String>>);

/// Opens the add dialog for another route to this environment.
pub(super) type OnAddRoute = Rc<dyn Fn(&mut Window, &mut App)>;

/// A route's grip being dragged.
struct RouteDrag {
    environment_id: String,
    from: usize,
}

struct RouteDragPreview(SharedString);

impl Render for RouteDragPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = crate::theme::tokens(cx);
        text_xs(div())
            .px(px(8.))
            .py(px(4.))
            .rounded(px(6.))
            .border_1()
            .border_color(t.border)
            .bg(t.background)
            .shadow_md()
            .font_weight(FontWeight::MEDIUM)
            .text_color(t.foreground)
            .child(self.0.clone())
    }
}

/// Saves `order` for `environment_id`, showing it until the save lands; a failed save puts the
/// saved order back.
fn reorder(
    saved: &WeakEntity<SavedEnvironments>,
    ui: &WeakEntity<SavedUi>,
    environment_id: &str,
    order: Vec<String>,
    window: &mut Window,
    cx: &mut App,
) {
    let id = environment_id.to_owned();
    drop(ui.update(cx, |ui, cx| {
        ui.pending.0.insert(id.clone(), order.clone());
        cx.notify();
    }));
    let saving = saved.update(cx, |saved, cx| saved.reorder_routes(&id, order, cx));
    let ui = ui.clone();
    window
        .spawn(cx, async move |cx| {
            let result = match saving {
                Ok(saving) => saving.finished().await,
                Err(_) => Err("The environment list is no longer available.".into()),
            };
            drop(cx.update(|window, cx| {
                drop(ui.update(cx, |ui, cx| {
                    ui.pending.0.remove(&id);
                    cx.notify();
                }));
                if let Err(error) = result {
                    error_toast(window, cx, "Reorder routes", error);
                }
            }));
        })
        .detach();
}

/// `confirmRemove`: asks, then forgets the route.
fn confirm_remove(
    saved: WeakEntity<SavedEnvironments>,
    environment_id: String,
    route_id: String,
    label: String,
    address: String,
    window: &mut Window,
    cx: &mut App,
) {
    let confirmed = crate::app::confirm::confirm(&format!("Remove {label} route?\n{address}"), true, window, cx);
    window
        .spawn(cx, async move |cx| {
            if !confirmed.await.unwrap_or(false) {
                return;
            }
            let Ok(saving) = saved.update(cx, |saved, cx| saved.remove_route(&environment_id, &route_id, cx)) else {
                return;
            };
            if let Err(error) = saving.finished().await {
                drop(cx.update(|window, cx| error_toast(window, cx, "Remove route", error)));
            }
        })
        .detach();
}

/// The routes of `environment`, in the order being saved or the saved one. `active` is the
/// route the open connection runs over.
pub(super) fn list(
    saved: &Entity<SavedEnvironments>,
    ui: &Entity<SavedUi>,
    environment: &SavedEnvironment,
    active: Option<&str>,
    on_add_route: OnAddRoute,
    t: &Tokens,
    cx: &App,
) -> AnyElement {
    let environment_id = environment.environment_id.clone();
    let saved_ids = environment.route_ids();
    let pending = ui.read(cx).pending.0.get(&environment_id).map(Vec::as_slice);
    let order: Vec<String> = environments::pending_route_order(pending, &saved_ids).unwrap_or(&saved_ids).to_vec();
    let order = Rc::new(order);
    let removable = environment.routes.len() > 1;
    let rows = order.iter().enumerate().filter_map(|(position, route_id)| {
        let route = environment.route(route_id)?;
        let (label, address) = (route.label(), route.address_text());
        let in_use = active == Some(route_id.as_str());
        let element_id = |part: &str| ElementId::Name(format!("route-{part}-{environment_id}-{route_id}").into());
        let move_to = {
            let (saved, ui, order, environment_id) =
                (saved.downgrade(), ui.downgrade(), order.clone(), environment_id.clone());
            move |to: usize, window: &mut Window, cx: &mut App| {
                if to != position && to < order.len() {
                    let next = environments::move_route(&order, position, to);
                    reorder(&saved, &ui, &environment_id, next, window, cx);
                }
            }
        };
        let grip = {
            let (ring, muted, foreground) = (t.ring, t.muted_foreground.opacity(0.7), t.foreground);
            let count = order.len();
            div()
                .id(element_id("grip"))
                .size(px(24.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(4.))
                .text_color(muted)
                .cursor(gpui_kit::CursorStyle::OpenHand)
                .hover(move |style| style.text_color(foreground))
                .tab_index(0)
                .focus_visible(move |style| {
                    style.shadow(vec![gpui_kit::BoxShadow::new(px(0.), px(0.), ring).spread_radius(px(2.)).inset()])
                })
                .tooltip(ui::tooltip(format!("Reorder {label}, position {}", position + 1)))
                .on_key_down(move |event, window, cx| {
                    let to = match event.keystroke.key.as_str() {
                        "up" if position > 0 => position - 1,
                        "down" if position + 1 < count => position + 1,
                        _ => return,
                    };
                    cx.stop_propagation();
                    move_to(to, window, cx);
                })
                .on_drag(RouteDrag { environment_id: environment_id.clone(), from: position }, {
                    let label = SharedString::from(label.clone());
                    move |_, _, _, cx| cx.new(|_| RouteDragPreview(label.clone()))
                })
                .child(Icon::empty().path("icons/grip-vertical.svg").size(px(14.)))
        };
        let remove = removable.then(|| {
            let (saved, environment_id, route_id) = (saved.downgrade(), environment_id.clone(), route_id.clone());
            let (label, address) = (label.clone(), address.clone());
            let accent = t.accent;
            div()
                .id(element_id("remove"))
                .size(px(24.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(6.))
                .text_color(t.muted_foreground)
                .cursor_pointer()
                .hover(move |style| style.bg(accent))
                .tab_index(0)
                .focus_visible({
                    let ring = t.ring;
                    move |style| {
                        style.shadow(vec![gpui_kit::BoxShadow::new(px(0.), px(0.), ring).spread_radius(px(2.))])
                    }
                })
                .tooltip(ui::tooltip("Remove route"))
                .on_click(move |_, window, cx| {
                    let (saved, environment_id, route_id) = (saved.clone(), environment_id.clone(), route_id.clone());
                    confirm_remove(saved, environment_id, route_id, label.clone(), address.clone(), window, cx);
                })
                .child(Icon::new(IconName::X).size(px(12.)))
        });
        let drop_environment = environment_id.clone();
        let (saved_entity, ui_entity, drop_order) = (saved.downgrade(), ui.downgrade(), order.clone());
        let hover_bg = t.accent.opacity(0.5);
        let success = ui::success();
        Some(
            h_flex()
                .id(element_id("row"))
                .gap(px(8.))
                .py(px(8.))
                .rounded(px(6.))
                .drag_over::<RouteDrag>(move |style, _, _, _| style.bg(hover_bg))
                .on_drop(move |drag: &RouteDrag, window, cx| {
                    if drag.environment_id == drop_environment && drag.from != position {
                        let next = environments::move_route(&drop_order, drag.from, position);
                        reorder(&saved_entity, &ui_entity, &drop_environment, next, window, cx);
                    }
                })
                .child(
                    v_flex()
                        .min_w_0()
                        .flex_1()
                        .child(
                            text_xs(h_flex().gap(px(6.)).font_weight(FontWeight::MEDIUM).text_color(t.foreground))
                                .child(label)
                                .when(in_use, |el| {
                                    el.child(
                                        text_2xs(div())
                                            .px(px(4.))
                                            .rounded(px(2.))
                                            .bg(success.opacity(0.12))
                                            .font_weight(FontWeight::NORMAL)
                                            .text_color(t.success_foreground)
                                            .child("In use"),
                                    )
                                }),
                        )
                        .child(text_2xs(div().truncate().text_color(t.muted_foreground)).child(address)),
                )
                .child(grip)
                .children(remove)
                .into_any_element(),
        )
    });
    v_flex()
        .mt(px(8.))
        .py(px(8.))
        .border_t_1()
        .border_color(t.border.opacity(0.5))
        .children(rows)
        .child(
            div().ml(px(-6.)).pt(px(4.)).child(
                WebButton::new(ElementId::Name(format!("route-add-{environment_id}").into()), "Add route")
                    .variant(Variant::GhostMuted)
                    .icon(IconName::Plus)
                    .on_click(move |_, window, cx| on_add_route(window, cx)),
            ),
        )
        .into_any_element()
}
