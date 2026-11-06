//! Saved remote environments (`SavedBackendListRow`, `ConnectionDiagnosisDetails.tsx`,
//! `AddEnvironmentDialog.tsx`) and the desktop's "This app" section (`LocalEnvironmentSetting`).
//! Only the "Remote link" route exists here: no SSH, T3 Connect, routes list or permissions.

use std::collections::HashSet;
use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, ClipboardItem, Context, ElementId, Entity, FontWeight, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription,
    Task, WeakEntity, Window, div, px,
};

use super::super::widgets::{RowFrame, section_with_action, settings_section, switch};
use super::logic;
use super::ui::{self, Size, Variant, WebButton, text_sm, text_xs};
use super::{error_toast, success_toast};
use crate::app::Workspace;
use crate::app::environments::SavedEnvironments;
use crate::app::sidebar::now_ms;
use crate::environments::{self, ConnectionState, HopResult, Phase, Presented, SavedEnvironment};
use crate::theme::{self, Tokens};

/// Per-section state: open diagnosis panels, and the once-a-second tick that keeps
/// "retrying in Ns" current while some environment waits to retry.
#[derive(Default)]
pub(super) struct SavedUi {
    details: HashSet<String>,
    _tick: Option<Task<()>>,
}

/// A save through a `SavedEnvironments` that was dropped did not happen.
fn gone(_: anyhow::Error) -> Result<(), String> {
    Err("The environment list is no longer available.".into())
}

impl Workspace {
    /// `<SettingsSection id="this-app">` with `LocalEnvironmentSetting`.
    pub(super) fn this_app_section(&self, cx: &App, t: &Tokens) -> AnyElement {
        let enabled = self.saved_environments.read(cx).local_environment_enabled();
        let saved = self.saved_environments.downgrade();
        let toggle = switch(&"local-environment".into(), enabled, false, t)
            .on_click(move |_, window, cx| confirm_local_environment(saved.clone(), enabled, window, cx));
        let description = if enabled {
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
        let action = {
            let saved = self.saved_environments.downgrade();
            WebButton::new("add-environment", "Add environment")
                .variant(Variant::GhostMuted)
                .icon(IconName::Plus)
                .on_click(move |_, window, cx| open_add_dialog(saved.clone(), primary_id.clone(), window, cx))
                .into_any_element()
        };
        let now = now_ms();
        let saved = self.saved_environments.read(cx);
        // The app's own environment saved here runs on the workspace's socket, so it shows that.
        let primary_phase = if self.sessions[0].is_connected() { Phase::Connected } else { Phase::Connecting };
        let rows: Vec<AnyElement> = if saved.environments().is_empty() {
            vec![empty_state(&t)]
        } else {
            let open = &ui.read(cx).details;
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
                    let expanded = open.contains(id);
                    saved_row(&self.saved_environments, &ui, saved, environment, state, expanded, now, &t)
                })
                .collect()
        };
        section_with_action("remote-environments", "Environments", Some(action), rows, &t)
    }
}

/// `EmptyRemoteEnvironments cloudEnabled={false}`.
fn empty_state(t: &Tokens) -> AnyElement {
    v_flex()
        .min_h(px(208.))
        .p(px(48.))
        .items_center()
        .justify_center()
        .child(
            div()
                .mb(px(24.))
                .size(px(36.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(6.))
                .border_1()
                .border_color(t.border)
                .bg(t.card)
                .child(ui::glyph("icons/chevrons-left-right-ellipsis.svg", 18., t.foreground)),
        )
        .child(
            div()
                .text_xl()
                .line_height(px(28.))
                .font_weight(FontWeight::SEMIBOLD)
                .child("No saved remote environments"),
        )
        .child(
            text_sm(div().mt(px(8.)).text_color(t.muted_foreground))
                .child("Click \u{201c}Add environment\u{201d} to pair another environment."),
        )
        .into_any_element()
}

fn saved_row(
    entity: &Entity<SavedEnvironments>,
    ui: &Entity<SavedUi>,
    saved: &SavedEnvironments,
    environment: &SavedEnvironment,
    state: ConnectionState,
    expanded: bool,
    now: i64,
    t: &Tokens,
) -> AnyElement {
    let id = environment.environment_id.clone();
    let session = saved.session(&id);
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
    if !environment.enabled && matches!(environment.credential, environments::Credential::Locked { .. }) {
        status.push_str(" \u{b7} Credential locked");
    }
    // The headline below names the failure in full.
    let subtitle = match &diagnosis {
        Some(_) => environment.http_base.clone(),
        None => format!("{} \u{b7} {status}", environment.http_base),
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

    let mut details = v_flex()
        .min_w_0()
        .opacity(dim)
        .child(text_sm(div().truncate().font_weight(FontWeight::MEDIUM).text_color(t.foreground)).child(label.clone()))
        .child(
            text_xs(div().id(element_id("status")).truncate())
                .text_color(if enabled && error_tone && diagnosis.is_none() { t.error } else { t.muted_foreground })
                .tooltip(ui::tooltip(tooltip))
                .child(subtitle),
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
                .on_click(move |_, _, cx| {
                    ui.update(cx, |ui, cx| {
                        if !ui.details.remove(&id) {
                            ui.details.insert(id.clone());
                        }
                        cx.notify();
                    });
                })
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
        switch(&format!("saved-env-{id}").into(), enabled, false, t)
            .tooltip(ui::tooltip(if enabled { "Switch off" } else { "Switch on" }))
            .on_click(move |_, window, cx| {
                let result = entity.update(cx, |saved, cx| saved.set_enabled(&id, !enabled, cx));
                if let Ok(Err(error)) = result {
                    let title = format!("Failed to switch the backend {}.", if enabled { "off" } else { "on" });
                    error_toast(window, cx, title, error);
                }
            })
    };
    let menu = row_menu(entity.downgrade(), environment, element_id("menu"));
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

/// The row's "More actions" menu: Copy MCP URL, Rename…, Remove from this device….
fn row_menu(entity: WeakEntity<SavedEnvironments>, environment: &SavedEnvironment, id: ElementId) -> impl IntoElement {
    let (environment_id, label, mcp_url) =
        (environment.environment_id.clone(), environment.label.clone(), environment.mcp_url());
    Button::new(id)
        .ghost()
        .xsmall()
        .icon(IconName::Ellipsis)
        .tooltip(format!("More actions for {label}"))
        .dropdown_menu_with_anchor(gpui_kit::Anchor::TopRight, move |menu, _, _| {
            let (copy_url, rename_entity, remove_entity) = (mcp_url.clone(), entity.clone(), entity.clone());
            let (rename_id, rename_label) = (environment_id.clone(), label.clone());
            let (remove_id, remove_label) = (environment_id.clone(), label.clone());
            menu.item(PopupMenuItem::new("Copy MCP URL").on_click(move |_, window, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(copy_url.clone()));
                success_toast(
                    window,
                    cx,
                    "MCP URL copied",
                    format!("Add it to an agent, e.g. claude mcp add --transport http t3 {copy_url}"),
                );
            }))
            .item(PopupMenuItem::new("Rename\u{2026}").on_click(move |_, window, cx| {
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
                if let Ok(Err(error)) = entity.update(cx, |saved, cx| saved.remove(&id, cx)) {
                    error_toast(window, cx, "Failed to remove backend.", error);
                }
                true
            })
    });
}

/// `setLocalEnvironmentEnabled`: saves the choice and relaunches, as the desktop app does.
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
                let saved = saved.update(cx, |saved, _| saved.set_local_environment_enabled(!enabled)).unwrap_or_else(gone);
                match saved.and_then(|()| relaunch()) {
                    Ok(()) => cx.quit(),
                    Err(error) => error_toast(window, cx, "Couldn't change this setting.", error),
                }
                true
            })
    });
}

/// Starts this app again with the same arguments; the caller quits.
fn relaunch() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|error| error.to_string())?;
    std::process::Command::new(exe).args(std::env::args_os().skip(1)).spawn().map(drop).map_err(|e| e.to_string())
}

/// `AddEnvironmentDialog` without routes, SSH or T3 Connect: a "Remote link" host and code.
struct AddForm {
    host: Entity<InputState>,
    code: Entity<InputState>,
    busy: bool,
    error: Option<SharedString>,
    saved: WeakEntity<SavedEnvironments>,
    /// The workspace's own environment, which is never saved again.
    primary_id: Option<String>,
    /// A pairing that could not be saved, with the host and code it used. The code is spent,
    /// so submitting the same fields again retries only the save.
    unsaved: Option<(String, String, SavedEnvironment)>,
    _subscriptions: [Subscription; 2],
}

impl AddForm {
    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let fields =
            environments::parse_remote_pairing_fields(&self.host.read(cx).value(), &self.code.read(cx).value());
        let (host, code) = match fields {
            Ok(fields) => fields,
            Err(message) => return self.fail(message.to_owned(), window, cx),
        };
        if let Some((_, _, paired)) = self.unsaved.take().filter(|(h, c, _)| (h, c) == (&host, &code)) {
            return self.save(host, code, paired, window, cx);
        }
        let Some(catalog) = self.saved.read_with(cx, |saved, _| saved.environments().to_vec()).ok() else { return };
        self.busy = true;
        self.error = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = crate::runtime().spawn(environments::pair(host.clone(), code.clone(), catalog)).await;
            let result = result.unwrap_or_else(|error| Err(error.to_string()));
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok(paired) => this.save(host, code, paired, window, cx),
                Err(error) => this.fail(error, window, cx),
            });
        })
        .detach();
    }

    /// Saves a redeemed pairing; on failure the form stays open with the error, ready to retry.
    fn save(
        &mut self,
        host: String,
        code: String,
        paired: SavedEnvironment,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let saved = if self.primary_id.as_deref() == Some(&paired.environment_id) {
            // The workspace's own environment is already here (`register` skips platform entries).
            Ok(())
        } else {
            self.saved.update(cx, |saved, cx| saved.add(paired.clone(), cx)).unwrap_or_else(gone)
        };
        match saved {
            Ok(()) => {
                self.busy = false;
                window.close_dialog(cx);
                success_toast(
                    window,
                    cx,
                    "Backend added",
                    "The environment is saved and will reconnect on app startup.",
                );
            }
            Err(error) => {
                self.unsaved = Some((host, code, paired));
                self.fail(error, window, cx);
            }
        }
    }

    fn fail(&mut self, message: String, window: &mut Window, cx: &mut Context<Self>) {
        self.busy = false;
        self.error = Some(message.clone().into());
        error_toast(window, cx, "Could not add backend", message);
        cx.notify();
    }
}

impl Render for AddForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *theme::tokens(cx);
        let busy = self.busy;
        let field = |label: &'static str, input: &Entity<InputState>| {
            v_flex()
                .gap(px(6.))
                .child(text_xs(div().font_weight(FontWeight::MEDIUM)).child(label))
                .child(Input::new(input).disabled(busy))
        };
        // "Remote link" mode card, selected; SSH needs the desktop bridge's tunnels.
        let mode = h_flex()
            .min_h(px(96.))
            .items_start()
            .gap(px(12.))
            .p(px(16.))
            .rounded(px(8.))
            .border_1()
            .border_color(t.primary.opacity(0.5))
            .bg(t.primary.opacity(0.05))
            .child(
                div()
                    .mt(px(2.))
                    .size(px(32.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(t.primary.opacity(0.3))
                    .bg(t.primary.opacity(0.1))
                    .child(ui::glyph("icons/chevrons-left-right-ellipsis.svg", 16., t.primary)),
            )
            .child(
                v_flex().min_w_0().child(text_sm(div().font_weight(FontWeight::MEDIUM)).child("Remote link")).child(
                    text_xs(div().mt(px(4.)).text_color(t.muted_foreground))
                        .child("Enter a backend host and pairing code."),
                ),
            );
        v_flex()
            .gap(px(16.))
            .child(h_flex().gap(px(12.)).child(mode.flex_1()).child(div().flex_1()))
            .child(
                v_flex()
                    .gap(px(12.))
                    .child(
                        h_flex()
                            .items_start()
                            .gap(px(12.))
                            .child(field("Host", &self.host).flex_1().min_w_0())
                            .child(field("Pairing code", &self.code).w(px(160.)).flex_none()),
                    )
                    .child(
                        ui::text_2xs(div().mt(px(4.)).text_color(t.muted_foreground))
                            .child("Paste a full pairing URL here to fill both fields automatically."),
                    ),
            )
            .when_some(self.error.clone(), |el, error| el.child(text_xs(div().text_color(t.error)).child(error)))
            .child(
                h_flex().w_full().child(
                    WebButton::new("add-environment-submit", if busy { "Adding\u{2026}" } else { "Add environment" })
                        .size(Size::Default)
                        .icon(IconName::Plus)
                        .disabled(busy)
                        .full_width()
                        .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
                ),
            )
    }
}

fn open_add_dialog(
    saved: WeakEntity<SavedEnvironments>,
    primary_id: Option<String>,
    window: &mut Window,
    cx: &mut App,
) {
    let host = cx.new(|cx| InputState::new(window, cx).placeholder("backend.example.com"));
    let code = cx.new(|cx| InputState::new(window, cx).placeholder("PAIRCODE"));
    host.update(cx, |input, cx| input.focus(window, cx));
    let form = cx.new(|cx| {
        let on_host = |this: &mut AddForm,
                       _: &Entity<InputState>,
                       event: &InputEvent,
                       window: &mut Window,
                       cx: &mut Context<AddForm>| {
            if let InputEvent::Change = event {
                // `handleSavedBackendHostChange`: a pasted pairing URL fills both fields.
                if let Some((host, code)) = environments::parse_pairing_url(&this.host.read(cx).value()) {
                    this.host.update(cx, |input, cx| input.set_value(host, window, cx));
                    this.code.update(cx, |input, cx| input.set_value(code, window, cx));
                }
                cx.notify();
            }
        };
        let on_code =
            |_: &mut AddForm, _: &Entity<InputState>, event: &InputEvent, _: &mut Window, cx: &mut Context<AddForm>| {
                if let InputEvent::Change = event {
                    cx.notify();
                }
            };
        AddForm {
            _subscriptions: [cx.subscribe_in(&host, window, on_host), cx.subscribe_in(&code, window, on_code)],
            host,
            code,
            busy: false,
            error: None,
            saved,
            primary_id,
            unsaved: None,
        }
    });
    window.open_dialog(cx, move |dialog, _, cx| {
        let t = theme::tokens(cx);
        dialog
            .title("Add Environment")
            .w(px(768.))
            .child(
                text_sm(div().mb(px(16.)).text_color(t.muted_foreground))
                    .child("Pair another environment to this client."),
            )
            .child(form.clone())
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
        let result = self.saved.update(cx, |saved, cx| saved.rename(&self.id, &label, cx)).unwrap_or_else(gone);
        match result {
            Ok(()) => window.close_dialog(cx),
            Err(error) => {
                self.error = Some(error.into());
                cx.notify();
            }
        }
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
