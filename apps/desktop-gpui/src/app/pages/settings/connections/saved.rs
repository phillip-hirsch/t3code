//! Saved remote environments (`SavedBackendListRow`, `ConnectionDiagnosisDetails.tsx`) and the
//! desktop's "This app" section (`LocalEnvironmentSetting`). Each row opens its routes
//! (`routes.rs`); the add dialog is `add_environment.rs`. No T3 Connect or permissions here.

use std::collections::HashSet;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, ClipboardItem, Context, ElementId, Entity, FontWeight, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription,
    Task, WeakEntity, Window, div, px, svg,
};

use super::super::widgets::{RowFrame, section_with_action, settings_section, switch};
use super::add_environment::{self, RouteTarget};
use super::routes::{self, PendingOrders};
use super::ui::{self, Size, Variant, WebButton, text_sm, text_xs};
use super::{error_toast, logic, success_toast};
use crate::app::Workspace;
use crate::app::environments::{SavedEnvironments, Saving};
use crate::app::sidebar::now_ms;
use crate::environments::{self, ConnectionState, HopResult, Phase, Presented, SavedEnvironment};
use crate::theme::{self, Tokens};

/// Per-section state: open diagnosis panels and route lists, route orders being saved, and
/// the once-a-second tick that keeps "retrying in Ns" current while some environment waits.
#[derive(Default)]
pub(super) struct SavedUi {
    details: HashSet<String>,
    routes: HashSet<String>,
    pub(super) pending: PendingOrders,
    _tick: Option<Task<()>>,
}

/// Opens the add dialog for another route to a saved environment.
type AddRoute = Rc<dyn Fn(RouteTarget, &mut Window, &mut App)>;

/// Opens or folds one of a row's panels.
fn toggle(ui: &Entity<SavedUi>, id: &str, panel: fn(&mut SavedUi) -> &mut HashSet<String>, cx: &mut App) {
    ui.update(cx, |ui, cx| {
        let open = panel(ui);
        if !open.remove(id) {
            open.insert(id.to_owned());
        }
        cx.notify();
    });
}

/// Waits off the UI thread for an edit to be on disk; a dropped list saved nothing.
async fn finished(saving: anyhow::Result<Saving>) -> Result<(), String> {
    match saving {
        Ok(saving) => saving.finished().await,
        Err(_) => Err("The environment list is no longer available.".into()),
    }
}

/// Shows an edit's save failure as a toast titled `title`.
fn toast_failure(saving: anyhow::Result<Saving>, title: String, window: &mut Window, cx: &mut App) {
    window
        .spawn(cx, async move |cx| {
            if let Err(error) = finished(saving).await {
                drop(cx.update(|window, cx| error_toast(window, cx, title, error)));
            }
        })
        .detach();
}

impl Workspace {
    /// `<SettingsSection id="this-app">` with `LocalEnvironmentSetting`.
    pub(super) fn this_app_section(&self, cx: &App, t: &Tokens) -> AnyElement {
        let saved = self.saved_environments.read(cx);
        let (enabled, editable) = (saved.local_environment_enabled(), saved.editable());
        let saved = self.saved_environments.downgrade();
        // Until the catalog is read it is neither on nor off.
        let toggle = switch(&"local-environment".into(), enabled.unwrap_or(true), enabled.is_none())
            .accessibility_label("Local environment");
        let toggle = match enabled.filter(|_| editable) {
            Some(enabled) => {
                toggle.on_click(move |_, window, cx| confirm_local_environment(saved.clone(), enabled, window, cx))
            }
            None => toggle.disabled(true),
        };
        let description = if enabled.unwrap_or(true) {
            "Run agents on this computer. Turn off to use T3 Code only with remote environments."
        } else {
            "Turned off. Agents only run in remote environments."
        };
        let row = RowFrame::new("Local environment").description(description).control(toggle).render(t);
        settings_section("this-app", "This app", [row.into_any_element()], t)
    }

    /// `<SettingsSection id="remote-environments">`: saved rows, or the empty state.
    pub(super) fn saved_environments_section(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let ui = window.use_keyed_state("settings-saved-environments", cx, |_, _| SavedUi::default());
        let t = *theme::tokens(cx);
        let waiting = {
            let saved = self.saved_environments.read(cx);
            saved
                .environments()
                .iter()
                .any(|e| e.enabled && saved.session(&e.environment_id).is_some_and(|s| s.state.phase == Phase::Backoff))
        };
        ui.update(cx, |ui, cx| match (waiting, ui._tick.is_some()) {
            (true, false) => {
                ui._tick = Some(cx.spawn(async move |this, cx| {
                    loop {
                        cx.background_executor().timer(Duration::from_secs(1)).await;
                        if this.update(cx, |_, cx| cx.notify()).is_err() {
                            return;
                        }
                    }
                }));
            }
            (false, true) => ui._tick = None,
            _ => {}
        });
        let primary_id = self.settings_info().environment_id.clone();
        let saved = self.saved_environments.read(cx);
        let add_route: AddRoute = {
            let (entity, primary_id) = (self.saved_environments.downgrade(), primary_id.clone());
            let keymap = self.keymap.clone();
            Rc::new(move |route, window, cx| {
                add_environment::open(entity.clone(), primary_id.clone(), Some(route), keymap.clone(), window, cx);
            })
        };
        let action = {
            let (entity, keymap) = (self.saved_environments.downgrade(), self.keymap.clone());
            WebButton::new("add-environment", "Add environment")
                .variant(Variant::GhostMuted)
                .icon(IconName::Plus)
                // A redeemed pairing code is spent, so pair only when the result can be saved.
                .disabled(!saved.editable())
                .on_click(move |_, window, cx| {
                    add_environment::open(entity.clone(), primary_id.clone(), None, keymap.clone(), window, cx);
                })
                .into_any_element()
        };
        let now = now_ms();
        // The app's own environment saved here runs on the workspace's socket, so it shows that.
        let primary_phase = if self.sessions[0].is_connected() { Phase::Connected } else { Phase::Connecting };
        let mut rows: Vec<AnyElement> = if saved.loading() {
            let loading =
                text_sm(div().px(px(16.)).py(px(10.)).text_color(t.muted_foreground)).child("Loading\u{2026}");
            vec![loading.into_any_element()]
        } else if saved.environments().is_empty() {
            vec![empty_state(&t)]
        } else {
            saved
                .environments()
                .iter()
                .map(|environment| {
                    let id = &environment.environment_id;
                    let state = match saved.session(id) {
                        _ if saved.is_primary(id) => ConnectionState { phase: primary_phase, ..Default::default() },
                        Some(session) => session.state.clone(),
                        None => ConnectionState::default(),
                    };
                    let row = Row { environment, state, add_route: add_route.clone(), now };
                    saved_row(&self.saved_environments, &ui, saved, row, &t, cx)
                })
                .collect()
        };
        if let Some(problem) = saved.problem() {
            let notice = text_xs(div().px(px(16.)).py(px(10.)).text_color(t.error)).child(problem.to_owned());
            rows.insert(0, notice.into_any_element());
        }
        section_with_action("remote-environments", "Environments", Some(action), rows, &t)
    }
}

/// `EmptyRemoteEnvironments cloudEnabled={false}`: the copy without T3 Connect, which this
/// client doesn't have.
fn empty_state(t: &Tokens) -> AnyElement {
    v_flex()
        .min_h(px(208.))
        .p(px(48.))
        .gap(px(24.))
        .items_center()
        .justify_center()
        .text_center()
        .child(empty_media("icons/chevrons-left-right-ellipsis.svg", t))
        .child(
            v_flex()
                .max_w(px(384.))
                .items_center()
                .child(
                    div()
                        .text_xl()
                        .line_height(px(28.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("No saved remote environments"),
                )
                .child(
                    text_sm(div().mt(px(4.)).text_color(t.muted_foreground))
                        .child("Click \u{201c}Add environment\u{201d} to pair another environment."),
                ),
        )
        .into_any_element()
}

/// `EmptyMedia variant="icon"`: a raised 36px card over two smaller ones tilted 10° out to
/// each side (drawn as one shape and its outline, since a div can't rotate), then `mb-6`.
fn empty_media(icon: &'static str, t: &Tokens) -> impl IntoElement {
    let back = |path: &'static str, color| {
        svg().path(path).absolute().left(px(-10.)).top(px(-10.)).w(px(56.)).h(px(50.)).text_color(color)
    };
    div()
        .relative()
        .mb(px(24.))
        .size(px(36.))
        .flex_none()
        .child(back("icons/app/empty-media-back.svg", t.card))
        .child(back("icons/app/empty-media-back-border.svg", t.border))
        .child(
            div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(6.))
                .border_1()
                .border_color(t.border)
                .bg(t.card)
                .shadow_sm()
                .child(ui::glyph(icon, 18., t.foreground)),
        )
}

/// One saved environment's row, as the section lists it.
struct Row<'a> {
    environment: &'a SavedEnvironment,
    state: ConnectionState,
    add_route: AddRoute,
    now: i64,
}

fn saved_row(
    entity: &Entity<SavedEnvironments>,
    ui: &Entity<SavedUi>,
    saved: &SavedEnvironments,
    row: Row<'_>,
    t: &Tokens,
    cx: &App,
) -> AnyElement {
    let Row { environment, state, add_route, now } = row;
    let id = environment.environment_id.clone();
    let (expanded, routes_open) = {
        let ui = ui.read(cx);
        (ui.details.contains(&id), ui.routes.contains(&id))
    };
    let session = saved.session(&id);
    let active_route = session.and_then(|s| s.active_route());
    let unsupported = state.presented() == Presented::Unsupported;
    let enabled = environment.enabled && !unsupported;
    let label = environment.label.clone();
    let server_version = session.and_then(|s| s.server_version.clone());
    let diagnosis = (environment.enabled
        && !matches!(state.presented(), Presented::Connected | Presented::Unsupported))
    .then(|| environments::diagnose(&label, &state, server_version.as_deref()))
    .flatten();
    let (mut status, error_tone) = state.saved_status(environment.enabled);
    // Switched off, it makes no attempt that would say so.
    let locked =
        environment.routes.iter().any(|route| matches!(route.credential, environments::Credential::Locked { .. }));
    if !environment.enabled && locked {
        status.push_str(" \u{b7} Credential locked");
    }
    // The headline below names the failure in full.
    let transport = environment.transport_label(active_route);
    let subtitle = match &diagnosis {
        Some(_) => transport,
        None => format!("{transport} \u{b7} {status}"),
    };
    let tooltip = if unsupported {
        state.last_failure.as_ref().map_or_else(|| state.status_text(), |f| f.detail.clone())
    } else if environment.enabled {
        state.status_text()
    } else {
        "Switched off".to_owned()
    };
    let dim = if enabled { 1. } else { 0.6 };
    let machine = session.and_then(|s| s.machine.clone()).unwrap_or_else(|| "server".into());
    let element_id = |part: &str| ElementId::Name(format!("saved-env-{part}-{id}").into());

    let route_count = environment.routes.len();
    let routes_toggle = {
        let (ui, id, ring, foreground) = (ui.clone(), id.clone(), t.ring, t.foreground);
        h_flex()
            .id(element_id("routes"))
            .flex_none()
            .gap(px(2.))
            .rounded(px(2.))
            .text_color(t.muted_foreground)
            .cursor_pointer()
            .hover(move |style| style.text_color(foreground))
            .tab_index(0)
            .focus_visible(move |style| {
                style.shadow(vec![gpui_kit::BoxShadow::new(px(0.), px(0.), ring).spread_radius(px(1.)).inset()])
            })
            .on_click(move |_, _, cx| toggle(&ui, &id, |ui| &mut ui.routes, cx))
            .child(if route_count == 1 { "Routes".to_owned() } else { format!("{route_count} routes") })
            .child(ui::chevron(routes_open, 12., t.muted_foreground))
    };
    let status_color = if enabled && error_tone && diagnosis.is_none() { t.error } else { t.muted_foreground };
    let mut details = v_flex()
        .min_w_0()
        .opacity(dim)
        .child(text_sm(div().truncate().font_weight(FontWeight::MEDIUM).text_color(t.foreground)).child(label.clone()))
        .child(
            text_xs(h_flex().min_w_0().gap(px(4.)))
                .child(
                    div()
                        .id(element_id("status"))
                        .min_w_0()
                        .truncate()
                        .text_color(status_color)
                        .tooltip(ui::tooltip(tooltip))
                        .child(subtitle),
                )
                .child(div().flex_none().text_color(t.muted_foreground).child("\u{b7}"))
                .child(routes_toggle),
        );
    if let Some(diagnosis) = &diagnosis {
        let retry = state.retry_at.map(|at| match (at - now + 999) / 1_000 {
            seconds if seconds > 0 => format!("retrying in {seconds}s"),
            _ => "retrying".to_owned(),
        });
        let mut line: Vec<String> = Vec::new();
        line.extend(retry);
        if state.attempt > 0 {
            line.push(format!("attempt {}", state.attempt));
        }
        let toggle = {
            let (ui, id) = (ui.clone(), id.clone());
            h_flex()
                .id(element_id("details"))
                .gap(px(2.))
                .cursor_pointer()
                .hover(|style| style.text_color(t.foreground))
                .child("Details")
                .child(ui::chevron(expanded, 12., t.muted_foreground))
                .on_click(move |_, _, cx| toggle(&ui, &id, |ui| &mut ui.details, cx))
        };
        details = details.child(
            text_xs(v_flex()).child(div().text_color(t.error).child(diagnosis.headline.clone())).child(
                h_flex()
                    .flex_wrap()
                    .gap(px(4.))
                    .text_color(t.muted_foreground)
                    .children(line.iter().map(|part| format!("{part} \u{b7}")))
                    .child(toggle),
            ),
        );
    }

    let toggle = {
        let (entity, id) = (entity.downgrade(), id.clone());
        let tooltip = if enabled { "Switch off" } else { "Switch on" };
        let switch = switch(&format!("saved-env-{id}").into(), enabled, false)
            .accessibility_label(format!("{tooltip} {label}"))
            .on_click(move |_, window, cx| {
                let saving = entity.update(cx, |saved, cx| saved.set_enabled(&id, !enabled, cx));
                let title = format!("Failed to switch the backend {}.", if enabled { "off" } else { "on" });
                toast_failure(saving, title, window, cx);
            });
        div().id(element_id("switch-tooltip")).tooltip(ui::tooltip(tooltip)).child(switch)
    };
    let menu = row_menu(entity.downgrade(), ui.downgrade(), environment, active_route, routes_open, element_id("menu"));
    let mut row = v_flex().px(px(16.)).py(px(10.)).child(
        h_flex()
            .gap(px(12.))
            .child(div().opacity(dim).child(ui::glyph(logic::machine_icon(&machine), 16., t.muted_foreground)))
            .child(details.flex_1())
            .child(h_flex().flex_none().gap(px(4.)).child(toggle).child(menu)),
    );
    if let (Some(diagnosis), true) = (diagnosis, expanded) {
        row = row.child(div().pl(px(28.)).child(diagnosis_panel(entity, &id, &label, diagnosis, t)));
    }
    if routes_open {
        let route = RouteTarget { environment_id: id.clone(), label };
        let on_add_route: routes::OnAddRoute = Rc::new(move |window, cx| add_route(route.clone(), window, cx));
        let list = routes::list(entity, ui, environment, active_route, on_add_route, t, cx);
        row = row.child(div().pl(px(28.)).child(list));
    }
    row.into_any_element()
}

/// `ConnectionDiagnosisPanel`: hops in order, what to try, the facts, and the actions.
fn diagnosis_panel(
    entity: &Entity<SavedEnvironments>,
    id: &str,
    host: &str,
    diagnosis: environments::Diagnosis,
    t: &Tokens,
) -> AnyElement {
    let hops = diagnosis.hops.iter().map(|(label, result)| {
        let (mark, note) = match result {
            HopResult::Ok => ("\u{2713}", ""),
            HopResult::Failed => ("\u{2715}", "Failed"),
            HopResult::Unchecked => ("\u{2013}", "Not checked"),
        };
        let color = if *result == HopResult::Failed { t.error } else { t.muted_foreground };
        h_flex()
            .gap(px(8.))
            .text_color(color)
            .child(div().w(px(12.)).flex().justify_center().child(mark))
            .child(div().when(*result == HopResult::Ok, |el| el.text_color(t.foreground)).child(label.clone()))
            .child(note)
    });
    let steps =
        diagnosis.steps.iter().enumerate().map(|(index, step)| {
            h_flex().items_start().gap(px(4.)).child(format!("{}.", index + 1)).child(step.clone())
        });
    let facts = diagnosis.facts.iter().map(|(label, value)| {
        h_flex()
            .items_start()
            .gap(px(12.))
            .child(div().w(px(96.)).flex_none().child(*label))
            .child(div().flex_1().min_w_0().text_color(t.foreground).child(value.clone()))
    });
    let text = environments::diagnosis_text(host, &diagnosis);
    let retry = {
        let (entity, id) = (entity.downgrade(), id.to_owned());
        WebButton::new(ElementId::Name(format!("saved-env-retry-{id}").into()), "Retry now")
            .on_click(move |_, _, cx| drop(entity.update(cx, |saved, cx| saved.retry(&id, cx))))
    };
    let copy = WebButton::new(ElementId::Name(format!("saved-env-copy-{id}").into()), "Copy diagnostics").on_click(
        move |_, window, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
            success_toast(window, cx, "Diagnostics copied", "");
        },
    );
    text_xs(v_flex())
        .mt(px(8.))
        .pt(px(8.))
        .gap(px(12.))
        .border_t_1()
        .border_color(t.border.opacity(0.5))
        .child(v_flex().gap(px(2.)).children(hops))
        .child(
            v_flex()
                .child(div().font_weight(FontWeight::MEDIUM).text_color(t.foreground).child("What to try"))
                .child(v_flex().pl(px(4.)).text_color(t.muted_foreground).children(steps)),
        )
        .child(v_flex().font_family(t.fonts.mono).text_color(t.muted_foreground).children(facts))
        .child(h_flex().flex_wrap().gap(px(8.)).child(retry).child(copy))
        .into_any_element()
}

/// The row's "More actions" menu: Routes, Copy MCP URL, Rename…, Remove from this device….
fn row_menu(
    entity: WeakEntity<SavedEnvironments>,
    ui: WeakEntity<SavedUi>,
    environment: &SavedEnvironment,
    active_route: Option<&str>,
    routes_open: bool,
    id: ElementId,
) -> impl IntoElement {
    let (environment_id, label, mcp_url) =
        (environment.environment_id.clone(), environment.label.clone(), environment.mcp_url(active_route));
    Button::new(id)
        .ghost()
        .xsmall()
        .icon(IconName::Ellipsis)
        .tooltip(format!("More actions for {label}"))
        .dropdown_menu_with_anchor(gpui_kit::Anchor::TopRight, move |menu, _, _| {
            let (rename_entity, remove_entity) = (entity.clone(), entity.clone());
            let (rename_id, rename_label) = (environment_id.clone(), label.clone());
            let (remove_id, remove_label) = (environment_id.clone(), label.clone());
            let (routes_ui, routes_id) = (ui.clone(), environment_id.clone());
            let routes = PopupMenuItem::new(if routes_open { "Hide routes" } else { "Routes" })
                .icon(Icon::empty().path("icons/route.svg"))
                .on_click(move |_, _, cx| {
                    if let Some(ui) = routes_ui.upgrade() {
                        toggle(&ui, &routes_id, |ui| &mut ui.routes, cx);
                    }
                });
            let menu = menu.item(routes);
            // An SSH forward is a port of this run, so an SSH-only environment has no MCP URL.
            let menu = match mcp_url.clone() {
                Some(copy_url) => menu.item(PopupMenuItem::new("Copy MCP URL").on_click(move |_, window, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(copy_url.clone()));
                    success_toast(
                        window,
                        cx,
                        "MCP URL copied",
                        format!("Add it to an agent, e.g. claude mcp add --transport http t3 {copy_url}"),
                    );
                })),
                None => menu,
            };
            menu.item(PopupMenuItem::new("Rename\u{2026}").on_click(move |_, window, cx| {
                open_rename_dialog(rename_entity.clone(), rename_id.clone(), rename_label.clone(), window, cx);
            }))
            .separator()
            .item(
                PopupMenuItem::element(|_, cx| {
                    div().text_color(theme::tokens(cx).error_foreground).child("Remove from this device\u{2026}")
                })
                .on_click(move |_, window, cx| {
                    confirm_remove(remove_entity.clone(), remove_id.clone(), remove_label.clone(), window, cx);
                }),
            )
        })
}

fn confirm_remove(entity: WeakEntity<SavedEnvironments>, id: String, label: String, window: &mut Window, cx: &mut App) {
    window.open_alert_dialog(cx, move |dialog, _, _| {
        let (entity, id) = (entity.clone(), id.clone());
        dialog
            .title(format!("Remove {label} from this device?"))
            .description(
                "This forgets its pairing, credentials, and cached threads here. Switch it off instead to keep it saved.",
            )
            .show_cancel(true)
            .ok_text("Remove")
            .ok_variant(gpui_kit::component::button::ButtonVariant::Danger)
            .on_ok(move |_, window, cx| {
                let saving = entity.update(cx, |saved, cx| saved.remove(&id, cx));
                toast_failure(saving, "Failed to remove backend.".into(), window, cx);
                true
            })
    });
}

/// `setLocalEnvironmentEnabled`: saves the choice and relaunches, as the desktop app does. The
/// new app starts once this one has exited, so it does not hand itself to this one.
fn confirm_local_environment(saved: WeakEntity<SavedEnvironments>, enabled: bool, window: &mut Window, cx: &mut App) {
    window.open_alert_dialog(cx, move |dialog, _, _| {
        let saved = saved.clone();
        dialog
            .title(if enabled { "Turn off local environment?" } else { "Turn on local environment?" })
            .description(if enabled {
                "T3 Code will restart without running a server on this computer. Any agents and terminals running here will stop, and other devices will no longer be able to connect to this computer. Your projects, history, and remote environments are unaffected."
            } else {
                "T3 Code will restart and start running a server on this computer again."
            })
            .show_cancel(true)
            .ok_text(if enabled { "Restart and turn off" } else { "Restart and turn on" })
            .when(enabled, |dialog| dialog.ok_variant(gpui_kit::component::button::ButtonVariant::Danger))
            .on_ok(move |_, window, cx| {
                let saving = saved.update(cx, |saved, cx| saved.set_local_environment_enabled(!enabled, cx));
                window
                    .spawn(cx, async move |cx| {
                        // The new run reads the choice, so restart once it is on disk.
                        let result = finished(saving).await;
                        drop(cx.update(|window, cx| match result {
                            Ok(()) => cx.restart(),
                            Err(error) => error_toast(window, cx, "Couldn't change this setting.", error),
                        }));
                    })
                    .detach();
                true
            })
    });
}

/// Rename a saved environment (`updateBearer` with the same address, as mobile's editor does).
struct RenameForm {
    label: Entity<InputState>,
    error: Option<SharedString>,
    saved: WeakEntity<SavedEnvironments>,
    id: String,
    _subscription: Subscription,
}

impl RenameForm {
    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let label = self.label.read(cx).value().to_string();
        let saving = self.saved.update(cx, |saved, cx| saved.rename(&self.id, &label, cx));
        cx.spawn_in(window, async move |this, cx| {
            let result = finished(saving).await;
            drop(this.update_in(cx, |this, window, cx| match result {
                Ok(()) => window.close_dialog(cx),
                Err(error) => {
                    this.error = Some(error.into());
                    cx.notify();
                }
            }));
        })
        .detach();
    }
}

impl Render for RenameForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *theme::tokens(cx);
        v_flex()
            .gap(px(16.))
            .child(
                v_flex()
                    .gap(px(6.))
                    .child(text_xs(div().font_weight(FontWeight::MEDIUM)).child("Name"))
                    .child(Input::new(&self.label)),
            )
            .when_some(self.error.clone(), |el, error| el.child(text_xs(div().text_color(t.error)).child(error)))
            .child(
                h_flex()
                    .justify_end()
                    .gap(px(8.))
                    .child(
                        WebButton::new("rename-environment-cancel", "Cancel")
                            .size(Size::Default)
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        WebButton::new("rename-environment-save", "Save")
                            .size(Size::Default)
                            .variant(Variant::Primary)
                            .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
                    ),
            )
    }
}

fn open_rename_dialog(
    saved: WeakEntity<SavedEnvironments>,
    id: String,
    label: String,
    window: &mut Window,
    cx: &mut App,
) {
    let input = cx.new(|cx| InputState::new(window, cx).default_value(label.clone()));
    input.update(cx, |input, cx| input.focus(window, cx));
    let form = cx.new(|cx| RenameForm {
        _subscription: cx.subscribe_in(&input, window, |this: &mut RenameForm, _, event: &InputEvent, window, cx| {
            if let InputEvent::PressEnter { .. } = event {
                this.submit(window, cx);
            }
        }),
        label: input,
        error: None,
        saved,
        id,
    });
    window.open_dialog(cx, move |dialog, _, _| dialog.title(format!("Rename {label}")).w(px(448.)).child(form.clone()));
}
