//! Device sources (`DeviceSourcesSettings.tsx`): local simulators and every SSH, EAS and
//! Simlock host the selected environments hold, one row each, plus the editors that add and
//! change them. A source held on several environments is one row; how it stands on each lives in
//! the row's status lines and platform breakdowns. Edits and removals write every environment that
//! holds the source, a new one every selected environment. Statuses come from each environment's
//! `subscribeDeviceState` stream.

mod editors;
mod hosts;

use std::collections::HashSet;
use std::time::Duration;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::hover_card::HoverCard;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::popover::Popover;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::{
    Anchor, AnyElement, App, Context, ElementId, Entity, FontWeight, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, Styled as _, Window, div, prelude::FluentBuilder as _, px, svg,
};
use serde_json::{Value, json};

use self::hosts::{EasHost, HostGroup, SimlockHost, SourceStatus, SshHost, Tone};
use crate::app::Workspace;
use crate::app::pages::settings::widgets::{SettingsView, section_with_action};
use crate::device::{DeviceState, PlatformAvailability};
use crate::rpc::{RpcClient, RpcError};
use crate::theme::{Tokens, tokens};

/// This list's own in-flight work, kept while the section renders.
#[derive(Default)]
pub(super) struct DeviceSources {
    /// A settings write from a row is in flight.
    busy: bool,
    /// SSH hosts whose "Test connection" from the row menu is running.
    testing: HashSet<String>,
}

/// A selected, connected environment as the device sources read it.
#[derive(Clone, Copy)]
pub(super) struct Member<'a> {
    pub id: &'a str,
    pub label: &'a str,
    pub settings: &'a Value,
    pub state: Option<&'a DeviceState>,
    /// This connection may write settings there.
    pub can_write: bool,
}

/// An environment a connection check runs on (`DeviceHostCheckTarget`), connected or not.
pub(super) struct CheckTarget {
    pub label: String,
    /// `None` while disconnected.
    pub client: Option<RpcClient>,
    pub can_write: bool,
}

/// One environment's answer to "Test connection" (`DeviceHostCheck`).
#[derive(Clone, Debug)]
pub(super) enum Check {
    Pending,
    Local,
    Connected(Vec<PlatformAvailability>),
    Failed(String),
}

/// A fix offered beside a status line, on the environment it names.
#[derive(Clone, Debug, PartialEq)]
enum StatusAction {
    Refresh,
    Retry(String),
}

/// A source on one active environment (`SourceEnvironment`).
#[derive(Clone, Debug)]
struct SourceEnvironment {
    id: String,
    label: String,
    added: bool,
    status: Option<SourceStatus>,
    action: Option<StatusAction>,
    /// Unknown until the environment's hub has reached the source.
    platforms: Option<Vec<PlatformAvailability>>,
}

#[derive(Clone, Debug)]
enum RowHost {
    Local,
    /// `holders` hold it; `only` keeps edits on them, for a host that differs between environments.
    Ssh {
        host: SshHost,
        holders: Vec<String>,
        only: bool,
    },
    /// `check_on`: a holder, where the editor checks the Expo account.
    Eas {
        host: EasHost,
        check_on: String,
    },
    Simlock(SimlockHost),
}

struct SourceRow {
    key: SharedString,
    title: SharedString,
    kind: Option<&'static str>,
    description: SharedString,
    /// One machine or account every environment reaches the same way, so its platforms are the
    /// same for all of them. Local simulators differ per machine.
    shared: bool,
    /// The active environments (devices on), whether each holds the source and how it stands.
    environments: Vec<SourceEnvironment>,
    host: RowHost,
}

fn enabled(settings: &Value, key: &str) -> bool {
    settings.get(key).and_then(Value::as_bool) == Some(true)
}

impl Workspace {
    /// The selected, connected environments, each with its settings, device state and grant.
    pub(super) fn device_members<'a>(&'a self, view: &'a SettingsView) -> Vec<Member<'a>> {
        view.selected_environments()
            .into_iter()
            .filter_map(|environment| {
                Some(Member {
                    id: environment.id,
                    label: environment.label,
                    settings: environment.settings?,
                    state: self.env_device_state(environment.id),
                    can_write: environment.can_write,
                })
            })
            .collect()
    }

    /// The connected ones among `environments` (id, label), read fresh whatever Settings now
    /// selects: an editor keeps acting where it opened.
    pub(super) fn members_among<'a>(&'a self, environments: &'a [(String, String)]) -> Vec<Member<'a>> {
        environments
            .iter()
            .filter_map(|(id, label)| {
                let session = self.session(id).filter(|session| session.is_connected())?;
                Some(Member {
                    id,
                    label,
                    settings: &session.server_info.settings,
                    state: session.device_state.as_ref(),
                    can_write: self.environment_writable(id),
                })
            })
            .collect()
    }

    /// [`Self::check_targets`] for `environments`, connected or not.
    pub(super) fn check_targets_among(&self, environments: &[(String, String)]) -> Vec<CheckTarget> {
        environments
            .iter()
            .map(|(id, label)| CheckTarget {
                label: label.clone(),
                client: self.session(id).filter(|session| session.is_connected()).and_then(|_| self.env_client(id)),
                can_write: self.environment_writable(id),
            })
            .collect()
    }

    fn environment_writable(&self, environment: &str) -> bool {
        use crate::app::pages::settings::connections::logic::SETTINGS_WRITE;
        self.settings_ui.grants.get(environment).is_some_and(|(_, session)| session.grants(SETTINGS_WRITE))
    }

    /// Every selected environment, for connection checks.
    pub(super) fn check_targets(&self, view: &SettingsView) -> Vec<CheckTarget> {
        view.selected_environments()
            .into_iter()
            .map(|environment| CheckTarget {
                label: environment.label.to_owned(),
                client: environment.settings.and_then(|_| self.env_client(environment.id)),
                can_write: environment.can_write,
            })
            .collect()
    }

    /// `project_scope`: sources are environment-wide, so a project scope can't change them.
    pub(in crate::app) fn render_device_sources(
        &mut self,
        view: &SettingsView,
        project_scope: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let sources =
            window.use_keyed_state(self.settings_state_key("device-sources"), cx, |_, _| DeviceSources::default());
        let t = *tokens(cx);
        let state = self.settings_device_state();
        let members = self.device_members(view);
        let simlock_enabled = members.iter().any(|member| enabled(member.settings, "simlockDeviceHostsEnabled"));
        let supports_eas = state.is_some_and(|state| state.supports_eas_hosts);
        let supports_simlock = state.is_some_and(|state| state.supports_simlock_hosts) && simlock_enabled;
        let model = sources.read(cx);
        let disabled = project_scope || model.busy || members.is_empty();
        let primary = self.settings_environment();
        let rows = source_rows(&members, &model.testing, supports_eas, supports_simlock, primary.as_deref());
        let view_entity = cx.entity();

        let add = outline_button("device-sources-add")
            .disabled(disabled)
            .child(Icon::new(Lucide::Plus).with_size(px(14.)))
            .child("Add source")
            .child(Icon::new(Lucide::ChevronDown).with_size(px(14.)))
            .dropdown_menu_with_anchor(Anchor::TopRight, {
                let view = view_entity;
                move |menu, window, _| {
                    let mut menu = menu.item(PopupMenuItem::new("Machine over SSH").on_click(
                        window.listener_for(&view, |this, _, window, cx| this.open_ssh_editor(None, None, window, cx)),
                    ));
                    if supports_eas {
                        menu = menu.item(PopupMenuItem::new("EAS Simulator").on_click(window.listener_for(
                            &view,
                            |this, _, window, cx| {
                                this.open_eas_editor(None, None, window, cx);
                            },
                        )));
                    }
                    if supports_simlock {
                        menu = menu.item(PopupMenuItem::new("Simlock pool (alpha)").on_click(window.listener_for(
                            &view,
                            |this, _, window, cx| {
                                this.open_simlock_editor(None, window, cx);
                            },
                        )));
                    }
                    menu
                }
            });

        let rows: Vec<AnyElement> =
            rows.into_iter().map(|row| self.render_source_row(row, &sources, disabled, &t, cx)).collect();
        section_with_action("device-sources", "Device sources", Some(add.into_any_element()), rows, &t)
    }

    fn render_source_row(
        &self,
        row: SourceRow,
        sources: &Entity<DeviceSources>,
        disabled: bool,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let several = row.environments.len() > 1;
        let pending = row
            .environments
            .iter()
            .find_map(|environment| environment.status.as_ref().filter(|status| status.tone == Tone::Pending))
            .map(|status| status.label);
        let key = row.key.clone();
        let mut text = v_flex()
            .flex_1()
            .min_w_0()
            .child(
                h_flex()
                    .min_w_0()
                    .gap(px(6.))
                    .child(div().truncate().text_sm().font_weight(FontWeight::MEDIUM).child(row.title.clone()))
                    .when_some(row.kind, |this, kind| {
                        this.child(
                            div()
                                .flex_none()
                                .rounded(px(4.))
                                .border_1()
                                .border_color(t.border)
                                .px(px(4.))
                                .text_size(px(10.))
                                .line_height(px(14.))
                                .text_color(t.muted_foreground)
                                .child(kind.to_uppercase()),
                        )
                    }),
            )
            .child(div().truncate().text_xs().text_color(t.muted_foreground).child(row.description.clone()));
        let problems = row
            .environments
            .iter()
            .filter_map(|environment| Some((environment, environment.status.as_ref()?)))
            .filter(|(_, status)| status.tone != Tone::Pending);
        for (environment, problem) in problems {
            text = text.child(self.render_problem(&key, environment, problem, several, t, cx));
        }
        let side = match pending {
            Some(label) => h_flex()
                .flex_none()
                .gap(px(6.))
                .text_xs()
                .text_color(t.muted_foreground)
                .child(Spinner::new().xsmall().color(t.muted_foreground))
                .child(label)
                .into_any_element(),
            None => h_flex().flex_none().gap(px(2.)).children(platform_icons(&row, t)).into_any_element(),
        };
        h_flex()
            .id(ElementId::Name(format!("device-source-{key}").into()))
            .items_center()
            .gap(px(12.))
            .px(px(16.))
            .py(px(12.))
            .child(text)
            .child(side)
            .when_some(self.row_actions(&row, sources, disabled, t, cx), gpui_kit::ParentElement::child)
            .into_any_element()
    }

    /// One environment's problem line: its name when the row spans several, what is wrong, the
    /// cause behind "Details" for a warning, and a fix.
    fn render_problem(
        &self,
        key: &SharedString,
        environment: &SourceEnvironment,
        problem: &SourceStatus,
        several: bool,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let error = problem.tone == Tone::Error;
        let color = if error { t.error } else { t.warning };
        let id = format!("{key}-{}", environment.id);
        let message = match &problem.detail {
            Some(detail) if error && !several => detail.clone(),
            _ => problem.label.to_owned(),
        };
        let message = if several { format!("{}: {message}", environment.label) } else { message };
        let details = problem.detail.clone().filter(|_| !error).map(|detail| {
            let t = *t;
            Popover::new(ElementId::Name(format!("device-source-details-{id}").into()))
                .anchor(Anchor::TopLeft)
                .w(px(512.))
                .p(px(8.))
                .trigger(
                    Button::new(ElementId::Name(format!("device-source-details-trigger-{id}").into()))
                        .text()
                        .xsmall()
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(t.foreground)
                        .label("Details"),
                )
                .content(move |_, _, _| {
                    div()
                        .whitespace_normal()
                        .font_family(t.fonts.mono)
                        .text_xs()
                        .text_color(t.muted_foreground)
                        .child(detail.clone())
                })
        });
        let fix = environment.action.clone().map(|action| {
            let label = match action {
                StatusAction::Refresh => "Refresh",
                StatusAction::Retry(_) => "Retry",
            };
            let environment_id = environment.id.clone();
            Button::new(ElementId::Name(format!("device-source-fix-{id}").into()))
                .text()
                .xsmall()
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(t.foreground)
                .label(label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    let input = match &action {
                        StatusAction::Refresh => json!({}),
                        StatusAction::Retry(host_id) => json!({"retryHostId": host_id}),
                    };
                    this.list_device_hosts(&environment_id, input, cx);
                }))
        });
        h_flex()
            .mt(px(4.))
            .flex_wrap()
            .gap(px(6.))
            .text_xs()
            .text_color(color)
            .child(div().min_w_0().child(message))
            .children(details)
            .children(fix)
            .into_any_element()
    }

    /// Edit, and a menu with Remove (and Test connection for SSH hosts).
    fn row_actions(
        &self,
        row: &SourceRow,
        sources: &Entity<DeviceSources>,
        disabled: bool,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if matches!(row.host, RowHost::Local) {
            return None;
        }
        let key = row.key.clone();
        let host = row.host.clone();
        let edit = outline_button(ElementId::Name(format!("device-source-edit-{key}").into()))
            .label("Edit")
            .disabled(disabled)
            .on_click(cx.listener({
                let host = host.clone();
                move |this, _, window, cx| match &host {
                    RowHost::Ssh { host, holders, only } => {
                        let only = only.then(|| holders.clone());
                        this.open_ssh_editor(Some(host.clone()), only, window, cx);
                    }
                    RowHost::Eas { host, check_on } => {
                        this.open_eas_editor(Some(host.clone()), Some(check_on.clone()), window, cx);
                    }
                    RowHost::Simlock(host) => this.open_simlock_editor(Some(host.clone()), window, cx),
                    RowHost::Local => {}
                }
            }));
        let view = cx.entity();
        let sources = sources.clone();
        let error = t.error;
        let more = Button::new(ElementId::Name(format!("device-source-more-{key}").into()))
            .ghost()
            .small()
            .size(px(28.))
            .text_color(t.muted_foreground)
            .icon(Icon::new(Lucide::EllipsisVertical).with_size(px(14.)))
            .disabled(disabled)
            .dropdown_menu_with_anchor(Anchor::TopRight, move |mut menu, window, _| {
                if let RowHost::Ssh { host: ssh, .. } = &host {
                    let ssh = ssh.clone();
                    let sources = sources.clone();
                    menu = menu.item(PopupMenuItem::new("Test connection").on_click(window.listener_for(
                        &view,
                        move |this, _, window, cx| {
                            this.test_ssh_from_row(ssh.clone(), &sources, window, cx);
                        },
                    )));
                }
                let host = host.clone();
                let sources = sources.clone();
                menu.item(PopupMenuItem::element(move |_, _| div().text_color(error).child("Remove")).on_click(
                    window.listener_for(&view, move |this, _, window, cx| {
                        this.remove_source(&host, &sources, window, cx);
                    }),
                ))
            });
        Some(h_flex().flex_none().gap(px(4.)).child(edit).child(more).into_any_element())
    }

    /// `device.list` on one environment: a refresh, or a retry of one host.
    fn list_device_hosts(&mut self, environment: &str, input: Value, cx: &mut Context<Self>) {
        let Some(client) = self.env_client(environment) else { return };
        cx.spawn(async move |_, _| {
            if let Err(error) = client.request("device.list", input).await {
                log::warn!("device.list failed: {error}");
            }
        })
        .detach();
    }

    /// Removes the source from every environment that holds it.
    fn remove_source(
        &mut self,
        host: &RowHost,
        sources: &Entity<DeviceSources>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let view = self.settings_view();
        let members = self.device_members(&view);
        let (planned, failed) = match host {
            RowHost::Ssh { host, holders, .. } => {
                (ssh_plan(&members, host, true, host, Some(holders)), "Device host not removed everywhere")
            }
            RowHost::Eas { host, .. } => (eas_plan(&members, host, true, false), "EAS host not removed everywhere"),
            RowHost::Simlock(host) => (simlock_remove_plan(&members, host), "Simlock host not removed everywhere"),
            RowHost::Local => return,
        };
        let writes = planned.and_then(|patches| device_writes(|id| self.env_client(id), &members, patches));
        let writes = match writes {
            Ok(writes) => writes,
            Err(error) => return notify_error(window, cx, failed, error),
        };
        sources.update(cx, |sources, cx| {
            sources.busy = true;
            cx.notify();
        });
        let sources = sources.downgrade();
        cx.spawn_in(window, async move |_, cx| {
            let failures = run_writes(writes).await;
            let _ = cx.update(|window, cx| {
                let _ = sources.update(cx, |sources, cx| {
                    sources.busy = false;
                    cx.notify();
                });
                report_failures(window, cx, failed, &failures);
            });
        })
        .detach();
    }

    /// The row menu's "Test connection" on every selected environment: a status on the row, then
    /// a toast with each answer.
    fn test_ssh_from_row(
        &mut self,
        host: SshHost,
        sources: &Entity<DeviceSources>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let targets = self.check_targets(&self.settings_view());
        let host_id = host.id.clone();
        let started = sources.update(cx, |sources, cx| {
            cx.notify();
            sources.testing.insert(host_id.clone())
        });
        if !started {
            return;
        }
        let sources = sources.downgrade();
        cx.spawn_in(window, async move |_, cx| {
            let checks = futures::future::join_all(targets.iter().map(|target| check_target(target, &host))).await;
            let _ = cx.update(|window, cx| {
                let _ = sources.update(cx, |sources, cx| {
                    sources.testing.remove(&host_id);
                    cx.notify();
                });
                let failed: Vec<String> = targets
                    .iter()
                    .zip(&checks)
                    .filter_map(|(target, check)| match check {
                        Check::Failed(error) => Some(format!("{}: {error}", target.label)),
                        _ => None,
                    })
                    .collect();
                let note = if failed.is_empty() {
                    Notification::success("Connected or already available locally on each selected environment.")
                        .title(format!("{}: connection checks passed", host.label))
                } else {
                    Notification::error(failed.join("\n")).title(format!(
                        "{}: {} of {} environments failed",
                        host.label,
                        failed.len(),
                        targets.len()
                    ))
                };
                window.push_notification(note, cx);
            });
        })
        .detach();
    }
}

/// `checkDeviceHostConnections` for one target.
async fn check_target(target: &CheckTarget, host: &SshHost) -> Check {
    let Some(client) = &target.client else { return Check::Failed("Environment disconnected".into()) };
    if !target.can_write {
        return Check::Failed("This connection cannot test device hosts.".into());
    }
    match test_host(client, host).await {
        Ok(summary) if summary.get("kind").and_then(Value::as_str) == Some("local") => Check::Local,
        Ok(summary) => Check::Connected(serde_json::from_value(summary["platforms"].clone()).unwrap_or_default()),
        Err(error) => Check::Failed(error),
    }
}

/// A settings patch on its way to one environment.
pub(super) struct Write {
    label: String,
    client: RpcClient,
    patch: Value,
}

/// `write`'s check: every patched environment must be connected and writable from here, else
/// nothing is written and the error names the ones that are not.
pub(super) fn device_writes(
    client: impl Fn(&str) -> Option<RpcClient>,
    members: &[Member],
    patches: Vec<(String, Value)>,
) -> Result<Vec<Write>, String> {
    let mut writes = Vec::new();
    let mut refused = Vec::new();
    for (environment, patch) in patches {
        let member = members.iter().find(|member| member.id == environment);
        let label = member.map_or_else(|| environment.clone(), |member| member.label.to_owned());
        match (member.filter(|member| member.can_write), client(&environment)) {
            (Some(_), Some(client)) => writes.push(Write { label, client, patch }),
            _ => refused.push(label),
        }
    }
    if refused.is_empty() {
        Ok(writes)
    } else {
        Err(format!("This connection cannot change settings on {}.", refused.join(", ")))
    }
}

/// Sends every write at once; the failures, each named by its environment.
pub(super) async fn run_writes(writes: Vec<Write>) -> Vec<String> {
    let results =
        futures::future::join_all(writes.iter().map(|write| update_settings(&write.client, write.patch.clone()))).await;
    writes
        .iter()
        .zip(results)
        .filter_map(|(write, result)| result.err().map(|error| format!("{}: {error}", write.label)))
        .collect()
}

/// One toast for the environments a write could not change; `true` when there were none.
pub(super) fn report_failures(window: &mut Window, cx: &mut App, title: &'static str, failures: &[String]) -> bool {
    if failures.is_empty() {
        return true;
    }
    notify_error(window, cx, title, failures.join("\n"));
    false
}

/// `saveSsh`: the SSH host list each environment saves, on `only`'s environments when set.
fn ssh_plan(
    members: &[Member],
    host: &SshHost,
    remove: bool,
    original: &SshHost,
    only: Option<&[String]>,
) -> Result<Vec<(String, Value)>, String> {
    members
        .iter()
        .filter(|member| only.is_none_or(|only| only.iter().any(|id| id == member.id)))
        .map(|member| {
            let saved = hosts::host_list(member.settings, "deviceHosts")?;
            let next = hosts::update_device_hosts(&saved, host, remove, original)?;
            Ok((member.id.to_owned(), json!({"deviceHosts": next})))
        })
        .collect()
}

/// `saveEas`: every environment's EAS host list; `keep_token` keeps each environment's own token.
fn eas_plan(
    members: &[Member],
    host: &EasHost,
    remove: bool,
    keep_token: bool,
) -> Result<Vec<(String, Value)>, String> {
    members
        .iter()
        .map(|member| {
            let saved = hosts::host_list(member.settings, "easDeviceHosts")?;
            let next = hosts::update_eas_hosts(&saved, host, remove, keep_token);
            Ok((member.id.to_owned(), json!({"easDeviceHosts": next})))
        })
        .collect()
}

/// `removeSimlock`: the host goes from the environments that hold it.
fn simlock_remove_plan(members: &[Member], host: &SimlockHost) -> Result<Vec<(String, Value)>, String> {
    let mut patches = Vec::new();
    for member in members {
        let saved: Vec<SimlockHost> = hosts::host_list(member.settings, "simlockDeviceHosts")?;
        if saved.iter().any(|candidate| candidate.id == host.id) {
            let next = hosts::update_simlock_hosts(&saved, host, true);
            patches.push((member.id.to_owned(), json!({"simlockDeviceHosts": next})));
        }
    }
    Ok(patches)
}

/// `saveSimlock`: the shared fields everywhere and each environment's own token.
fn simlock_save_plan(
    members: &[Member],
    draft: &SimlockHost,
    tokens: &[(String, hosts::TokenEntry)],
) -> Result<Vec<(String, Value)>, String> {
    let environments = members
        .iter()
        .map(|member| Ok((member.id.to_owned(), hosts::host_list(member.settings, "simlockDeviceHosts")?)))
        .collect::<Result<Vec<_>, String>>()?;
    let plans = hosts::plan_simlock_saves(&environments, draft, tokens).map_err(str::to_owned)?;
    Ok(plans.into_iter().map(|(id, hosts)| (id, json!({"simlockDeviceHosts": hosts}))).collect())
}

/// Every device source the members can open, one row each (`DeviceSourcesSettings`' rows).
fn source_rows(
    members: &[Member],
    testing: &HashSet<String>,
    supports_eas: bool,
    supports_simlock: bool,
    primary: Option<&str>,
) -> Vec<SourceRow> {
    // Environments with devices off are left out: the switch above already says so.
    let active: Vec<&Member> =
        members.iter().filter(|member| enabled(member.settings, "enableDeviceSupport")).collect();
    let platforms = |state: Option<&DeviceState>, id: &str| state?.platforms(id).map(<[_]>::to_vec);
    let not_added = |member: &Member| SourceEnvironment {
        id: member.id.to_owned(),
        label: member.label.to_owned(),
        added: false,
        status: None,
        action: None,
        platforms: None,
    };
    let held =
        |member: &Member, id: &str, status: Option<SourceStatus>, action: Option<StatusAction>| SourceEnvironment {
            id: member.id.to_owned(),
            label: member.label.to_owned(),
            added: true,
            status,
            action,
            platforms: platforms(member.state, id),
        };

    let mut rows = vec![SourceRow {
        key: "local".into(),
        title: "Local simulators".into(),
        kind: None,
        description: if members.len() > 1 {
            "Simulators and emulators installed on each environment's own machine.".into()
        } else {
            "Simulators and emulators installed on this environment's machine.".into()
        },
        shared: false,
        environments: active
            .iter()
            .map(|member| {
                let id = member.state.and_then(DeviceState::local_host).map_or("", |local| local.id.as_str());
                let status = hosts::host_status(member.state, id);
                let action = status.as_ref().filter(|s| s.tone == Tone::Error).map(|_| StatusAction::Refresh);
                held(member, id, status, action)
            })
            .collect(),
        host: RowHost::Local,
    }];

    let lists: Vec<(&str, Vec<SshHost>)> =
        members.iter().map(|member| (member.id, hosts::readable_hosts(member.settings, "deviceHosts"))).collect();
    let grouped = hosts::group_device_hosts(&lists);
    let only = grouped.is_none();
    // Hosts that cannot be matched across environments stay one row per environment.
    let groups = grouped.unwrap_or_else(|| {
        lists
            .iter()
            .flat_map(|(id, hosts)| {
                hosts
                    .iter()
                    .map(|host| HostGroup { host: host.clone(), members: vec![((*id).to_owned(), host.clone())] })
            })
            .collect()
    });
    for group in groups {
        let host = group.host;
        let holders: Vec<String> = group.members.iter().map(|(id, _)| id.clone()).collect();
        let environments = active
            .iter()
            .map(|member| {
                let Some((_, entry)) = group.members.iter().find(|(id, _)| id == member.id) else {
                    return not_added(member);
                };
                let mut status = if testing.contains(&host.id) {
                    Some(SourceStatus { tone: Tone::Pending, label: "Testing connection", detail: None })
                } else {
                    hosts::host_status(member.state, &entry.id)
                };
                let retry = member.state.is_some_and(|state| state.supports_host_retry);
                let mut action = None;
                if let Some(failed) = status.as_mut().filter(|status| status.tone == Tone::Error && retry) {
                    failed.label = "Can't connect";
                    action = Some(StatusAction::Retry(entry.id.clone()));
                }
                held(member, &entry.id, status, action)
            })
            .collect();
        rows.push(SourceRow {
            key: format!("ssh:{}:{}", host.id, holders.join(",")).into(),
            title: host.label.clone().into(),
            kind: Some("SSH"),
            description: host.target.clone().into(),
            shared: true,
            environments,
            host: RowHost::Ssh { host, holders, only },
        });
    }

    if supports_eas {
        let lists: Vec<Vec<EasHost>> =
            members.iter().map(|member| hosts::readable_hosts(member.settings, "easDeviceHosts")).collect();
        let holds = |member: &Member, id: &str| {
            members.iter().position(|m| m.id == member.id).is_some_and(|i| lists[i].iter().any(|h| h.id == id))
        };
        for host in first_by_id(lists.iter().flatten(), |host: &EasHost| host.id.as_str()) {
            let holders: Vec<&Member> = members.iter().filter(|member| holds(member, &host.id)).collect();
            let Some(first) = holders.first() else { continue };
            let check_on = holders.iter().find(|member| Some(member.id) == primary).unwrap_or(first).id.to_owned();
            let environments = active
                .iter()
                .map(|member| {
                    if holds(member, &host.id) {
                        held(member, &host.id, hosts::host_status(member.state, &host.id), None)
                    } else {
                        not_added(member)
                    }
                })
                .collect();
            rows.push(SourceRow {
                key: format!("eas:{}", host.id).into(),
                title: host.label.clone().into(),
                kind: Some("EAS"),
                description: hosts::eas_session_limits(&host).into(),
                shared: true,
                environments,
                host: RowHost::Eas { host, check_on },
            });
        }
    }

    if supports_simlock {
        let lists: Vec<Vec<SimlockHost>> =
            members.iter().map(|member| hosts::readable_hosts(member.settings, "simlockDeviceHosts")).collect();
        for host in first_by_id(lists.iter().flatten(), |host: &SimlockHost| host.id.as_str()) {
            let environments = active
                .iter()
                .map(|member| {
                    let index = members.iter().position(|m| m.id == member.id);
                    let holds = index.is_some_and(|i| lists[i].iter().any(|h| h.id == host.id));
                    // A blank token can still resolve to the machine's own, so the host's answer decides.
                    if holds {
                        held(member, &host.id, hosts::host_status(member.state, &host.id), None)
                    } else {
                        not_added(member)
                    }
                })
                .collect();
            let origin = host.gateway_url.trim_start_matches("https://").trim_start_matches("http://");
            rows.push(SourceRow {
                key: format!("simlock:{}", host.id).into(),
                title: host.label.clone().into(),
                kind: Some("Simlock"),
                description: format!("{origin} · {}", hosts::simlock_lease_limits(&host)).into(),
                shared: true,
                environments,
                host: RowHost::Simlock(host),
            });
        }
    }
    rows
}

/// The first host seen under each id, in order.
fn first_by_id<'a, T: Clone + 'a>(hosts: impl Iterator<Item = &'a T>, id: fn(&T) -> &str) -> Vec<T> {
    let mut seen: Vec<T> = Vec::new();
    for host in hosts {
        if !seen.iter().any(|candidate| id(candidate) == id(host)) {
            seen.push(host.clone());
        }
    }
    seen
}

const PLATFORMS: [(&str, &str, &str); 2] =
    [("ios", "iOS", "icons/app/apple.svg"), ("android", "Android", "icons/app/android.svg")];

/// How one platform stands across a row's environments (`PlatformIcon`).
#[derive(Clone, Debug, PartialEq)]
struct PlatformSummary {
    available: bool,
    /// Available on some environments that answered, not all.
    partial: bool,
    /// Every environment that answered gives the same answer.
    uniform: bool,
    /// The first answer's reason, given once for a shared source.
    reason: Option<String>,
    available_on: Vec<String>,
    unavailable_on: Vec<(String, Option<String>)>,
    connected: Vec<String>,
    not_added: Vec<String>,
    /// Each answering environment's own answer, for local simulators.
    per_machine: Vec<(String, bool, Option<String>)>,
}

/// `None` when no environment that holds the source offers `platform`.
fn platform_summary(row: &SourceRow, platform: &str) -> Option<PlatformSummary> {
    let known: Vec<&SourceEnvironment> = row.environments.iter().filter(|e| e.added && e.platforms.is_some()).collect();
    let availability = |e: &SourceEnvironment| e.platforms.as_ref()?.iter().find(|p| p.platform == platform).cloned();
    if !known.iter().any(|e| availability(e).is_some()) {
        return None;
    }
    let answer = |e: &SourceEnvironment| availability(e).map(|p| (p.available, p.reason));
    let available_on: Vec<String> =
        known.iter().filter(|e| availability(e).is_some_and(|p| p.available)).map(|e| e.label.clone()).collect();
    let shared = known.iter().find_map(|e| answer(e));
    Some(PlatformSummary {
        available: !available_on.is_empty(),
        partial: !available_on.is_empty() && available_on.len() < known.len(),
        uniform: known.iter().all(|e| answer(e) == shared),
        reason: shared.and_then(|(_, reason)| reason),
        unavailable_on: known
            .iter()
            .filter(|e| !availability(e).is_some_and(|p| p.available))
            .map(|e| (e.label.clone(), availability(e).and_then(|p| p.reason)))
            .collect(),
        available_on,
        connected: row.environments.iter().filter(|e| e.added).map(|e| e.label.clone()).collect(),
        not_added: row.environments.iter().filter(|e| !e.added).map(|e| e.label.clone()).collect(),
        per_machine: known
            .iter()
            .map(|e| {
                let entry = availability(e);
                (e.label.clone(), entry.as_ref().is_some_and(|p| p.available), entry.and_then(|p| p.reason))
            })
            .collect(),
    })
}

/// `SourcePlatforms`: one icon per platform the source offers, solid where available, muted where
/// not, dotted where only some environments have it; hovering explains per environment.
fn platform_icons(row: &SourceRow, t: &Tokens) -> Vec<AnyElement> {
    let several = row.environments.len() > 1;
    PLATFORMS
        .iter()
        .filter_map(|&(platform, label, icon)| {
            let summary = platform_summary(row, platform)?;
            let state = match (summary.available, summary.partial) {
                (true, true) => "available on some",
                (true, false) => "available",
                (false, _) => "not available",
            };
            let title: SharedString = format!("{label} {state}").into();
            let (foreground, muted) = (t.foreground, t.muted_foreground);
            let trigger = div()
                .id(ElementId::Name(format!("device-platform-{}-{platform}", row.key).into()))
                .relative()
                .size(px(24.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(6.))
                .hover(|style| style.bg(t.accent))
                .child(
                    svg()
                        .path(icon)
                        .size(px(14.))
                        .when(platform == "ios", |this| this.mt(px(-1.)))
                        .text_color(if summary.available { foreground } else { muted.opacity(0.45) }),
                )
                .when(summary.partial, |this| {
                    this.child(
                        div()
                            .absolute()
                            .top(px(2.))
                            .right(px(2.))
                            .size(px(6.))
                            .rounded_full()
                            .bg(t.warning)
                            .border_2()
                            .border_color(t.background),
                    )
                });
            let (shared, t) = (row.shared, *t);
            Some(
                HoverCard::new(ElementId::Name(format!("device-platform-card-{}-{platform}", row.key).into()))
                    .anchor(Anchor::BottomRight)
                    .open_delay(Duration::from_millis(150))
                    .close_delay(Duration::from_millis(100))
                    .trigger(trigger)
                    .content(move |_, _, _| {
                        v_flex()
                            .w(px(256.))
                            .gap(px(4.))
                            .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(title.clone()))
                            .children(platform_breakdown(&summary, shared, several, &t))
                    })
                    .into_any_element(),
            )
        })
        .collect()
}

/// An environment's label and, where it has one, why the platform is missing there.
type Labeled = (String, Option<String>);

/// The hover card's body under its title: a shared source's one reason and its environments
/// grouped by answer, or each machine's own answer for local simulators.
fn platform_breakdown(summary: &PlatformSummary, shared: bool, several: bool, t: &Tokens) -> Vec<gpui_kit::Div> {
    let muted = t.muted_foreground;
    let note = |text: String| div().text_xs().text_color(muted).child(text);
    if shared {
        let mut body = Vec::new();
        if summary.uniform && !summary.available {
            body.extend(summary.reason.clone().map(note));
        }
        if several {
            let mut groups: Vec<(&str, Vec<Labeled>)> = if summary.uniform {
                vec![("Connected", summary.connected.iter().map(|l| (l.clone(), None)).collect())]
            } else {
                vec![
                    ("Available", summary.available_on.iter().map(|l| (l.clone(), None)).collect()),
                    ("Not available", summary.unavailable_on.clone()),
                ]
            };
            groups.push(("Not added", summary.not_added.iter().map(|l| (l.clone(), None)).collect()));
            let list = v_flex().mt(px(4.)).gap(px(6.)).text_xs().children(
                groups.into_iter().filter(|(_, environments)| !environments.is_empty()).map(
                    |(heading, environments)| {
                        v_flex().child(div().text_color(muted).child(heading)).children(environments.into_iter().map(
                            |(label, reason)| {
                                v_flex()
                                    .child(div().truncate().child(label))
                                    .children(reason.map(|reason| div().text_color(muted).child(reason)))
                            },
                        ))
                    },
                ),
            );
            body.push(list);
        }
        return body;
    }
    if several {
        return vec![v_flex().mt(px(4.)).gap(px(4.)).text_xs().children(summary.per_machine.iter().map(
            |(label, available, reason)| {
                let answer = if *available {
                    "Available".to_owned()
                } else {
                    reason.clone().unwrap_or_else(|| "Not available".into())
                };
                h_flex()
                    .items_start()
                    .gap(px(16.))
                    .child(div().flex_none().text_color(muted).child(label.clone()))
                    .child(div().flex_1().min_w_0().when(!available, |el| el.text_color(muted)).child(answer))
            },
        ))];
    }
    if summary.available {
        return Vec::new();
    }
    let reason = summary.per_machine.first().and_then(|(_, _, reason)| reason.clone());
    vec![note(reason.unwrap_or_else(|| "Not set up on this machine.".into()))]
}

/// The web's `failureMessage`: the error's `message`, which for the device errors is a
/// getter in `packages/contracts/src/device.ts` and so is rebuilt here from its fields.
fn failure_message(error: &RpcError) -> String {
    let RpcError::Failure(failure) = error else { return error.to_string() };
    let text = |key: &str| failure.get(key).and_then(Value::as_str).unwrap_or_default();
    if let Some(message) = failure.get("message").and_then(Value::as_str) {
        return message.to_owned();
    }
    let reason = text("reason");
    match text("_tag") {
        "DeviceHostUnavailableError" => {
            let host = failure
                .get("hostLabel")
                .and_then(Value::as_str)
                .map_or_else(|| format!("Device host {}", text("hostId")), str::to_owned);
            format!("{host} is unavailable: {reason}")
        }
        "DevicePlatformUnavailableError" => {
            format!("{} devices are unavailable on host {}: {reason}", text("platform"), text("hostId"))
        }
        "DeviceNotFoundError" => format!("Device {} was not found on host {}.", text("deviceId"), text("hostId")),
        "DeviceOperationError" => {
            let explanation = match reason {
                "command_failed" => match failure.get("exitCode").and_then(Value::as_i64) {
                    Some(code) => format!("The device command failed (exit code {code})."),
                    None => "The device command failed.".to_owned(),
                },
                "request_failed" => "Could not communicate with device support. Try refreshing devices.".to_owned(),
                "invalid_payload" => "The device request could not be encoded.".to_owned(),
                "settings_failed" => "Could not read or save device settings.".to_owned(),
                _ => "The device hub could not complete the request.".to_owned(),
            };
            format!("Device {} failed: {explanation}", text("operation"))
        }
        _ if !reason.is_empty() => reason.to_owned(),
        _ => error.to_string(),
    }
}

/// `server.updateSettings` with one patch.
async fn update_settings(client: &RpcClient, patch: Value) -> Result<(), String> {
    client.request("server.updateSettings", json!({"patch": patch})).await.map(drop).map_err(|e| failure_message(&e))
}

/// `device.testHost`; a host on this same machine answers `kind: "local"`.
async fn test_host(client: &RpcClient, host: &SshHost) -> Result<Value, String> {
    let payload = serde_json::to_value(host).map_err(|error| error.to_string())?;
    client.request("device.testHost", payload).await.map_err(|e| failure_message(&e))
}

fn notify_error(window: &mut Window, cx: &mut App, title: &'static str, message: String) {
    window.push_notification(Notification::error(message).title(title), cx);
}

/// A web `outline` `sm` button.
fn outline_button(id: impl Into<ElementId>) -> Button {
    Button::new(id).outline().small().h(px(28.)).px(px(9.)).gap(px(6.)).text_sm().font_weight(FontWeight::MEDIUM)
}

fn muted_text(text: impl Into<SharedString>, color: Hsla) -> gpui_kit::Div {
    div().text_xs().text_color(color).child(text.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_messages_read_like_the_errors_own() {
        let message = |error: Value| failure_message(&RpcError::Failure(error));
        assert_eq!(
            message(json!({"_tag": "DeviceOperationError", "operation": "test host", "reason": "request_failed"})),
            "Device test host failed: Could not communicate with device support. Try refreshing devices."
        );
        assert_eq!(
            message(json!({"_tag": "DeviceHostUnavailableError", "hostId": "mac", "reason": "ssh: timeout"})),
            "Device host mac is unavailable: ssh: timeout"
        );
        assert_eq!(
            message(json!({"_tag": "DeviceEasAccountError", "reason": "No Expo credential"})),
            "No Expo credential"
        );
        assert_eq!(message(json!({"_tag": "EnvironmentAuthorizationError", "message": "Forbidden"})), "Forbidden");
    }

    fn mac() -> Value {
        json!({"kind": "ssh", "id": "mac", "label": "Mac mini", "target": "me@mac"})
    }

    fn eas() -> Value {
        json!({"kind": "eas", "id": "eas", "label": "EAS", "accessToken": ""})
    }

    fn member<'a>(id: &'a str, settings: &'a Value, state: Option<&'a DeviceState>) -> Member<'a> {
        Member { id, label: id, settings, state, can_write: true }
    }

    fn state(value: Value) -> DeviceState {
        DeviceState::decode(&value).unwrap()
    }

    #[test]
    fn a_source_on_several_environments_is_one_row_with_each_ones_standing() {
        let laptop = json!({"enableDeviceSupport": true, "deviceHosts": [mac()], "easDeviceHosts": [eas()]});
        let server = json!({"enableDeviceSupport": true, "deviceHosts": [mac()]});
        let off = json!({"enableDeviceSupport": false, "easDeviceHosts": [eas()]});
        let laptop_state = state(json!({
            "supportsHostRetry": true,
            "hosts": [
                {"id": "local", "kind": "local", "label": "This Mac", "platforms": [{"platform": "ios", "available": true}]},
                {"id": "mac", "kind": "ssh", "label": "Mac mini", "platforms": [{"platform": "ios", "available": true}]},
            ],
            "hostStatuses": {"mac": {"status": "failed", "detail": "ssh: timeout"}},
        }));
        let members = [
            member("laptop", &laptop, Some(&laptop_state)),
            member("server", &server, None),
            member("off", &off, None),
        ];
        let rows = source_rows(&members, &HashSet::new(), true, false, Some("server"));
        let keys: Vec<&str> = rows.iter().map(|row| row.key.as_ref()).collect();
        assert_eq!(keys, ["local", "ssh:mac:laptop,server", "eas:eas"]);
        assert_eq!(
            rows[0].description.as_ref(),
            "Simulators and emulators installed on each environment's own machine."
        );

        // Devices off on an environment leave it out of every row.
        let ssh = &rows[1];
        let standing: Vec<(&str, bool, Option<&str>)> = ssh
            .environments
            .iter()
            .map(|e| (e.id.as_str(), e.added, e.status.as_ref().map(|status| status.label)))
            .collect();
        assert_eq!(standing, [("laptop", true, Some("Can't connect")), ("server", true, Some("Loading"))]);
        assert_eq!(ssh.environments[0].action, Some(StatusAction::Retry("mac".into())));
        assert!(matches!(&ssh.host, RowHost::Ssh { holders, only: false, .. } if holders == &["laptop", "server"]));

        // EAS checks the account on a holder: the primary is not one, so the first holder.
        let eas_row = &rows[2];
        assert!(matches!(&eas_row.host, RowHost::Eas { check_on, .. } if check_on == "laptop"));
        let added: Vec<(&str, bool)> = eas_row.environments.iter().map(|e| (e.id.as_str(), e.added)).collect();
        assert_eq!(added, [("laptop", true), ("server", false)]);
    }

    #[test]
    fn hosts_that_differ_between_environments_keep_a_row_each() {
        let laptop = json!({"enableDeviceSupport": true, "deviceHosts": [mac()]});
        let renamed = json!({"enableDeviceSupport": true, "deviceHosts": [{"kind": "ssh", "id": "mac", "label": "Studio", "target": "me@mac"}]});
        let members = [member("laptop", &laptop, None), member("server", &renamed, None)];
        let rows = source_rows(&members, &HashSet::new(), false, false, None);
        let keys: Vec<&str> = rows.iter().map(|row| row.key.as_ref()).collect();
        assert_eq!(keys, ["local", "ssh:mac:laptop", "ssh:mac:server"]);
        assert!(matches!(&rows[1].host, RowHost::Ssh { only: true, .. }));
        assert!(!rows[2].environments[0].added && rows[2].environments[1].added);
    }

    #[test]
    fn writes_go_to_the_holders_and_refuse_environments_this_connection_cannot_change() {
        let settings = json!({});
        let guest = Member { can_write: false, ..member("guest", &settings, None) };
        let members = [member("laptop", &settings, None), guest, member("server", &settings, None)];
        let patch = |id: &str| (id.to_owned(), json!({}));
        let connected = |id: &str| (id != "server").then(RpcClient::unopened);
        let writes = device_writes(connected, &members, vec![patch("laptop")]).unwrap();
        assert_eq!(writes.iter().map(|write| write.label.as_str()).collect::<Vec<_>>(), ["laptop"]);
        let refused = device_writes(connected, &members, vec![patch("laptop"), patch("guest"), patch("server")]);
        assert_eq!(refused.err().as_deref(), Some("This connection cannot change settings on guest, server."));

        let laptop = json!({"deviceHosts": [mac()], "simlockDeviceHosts": []});
        let server = json!({"deviceHosts": [], "simlockDeviceHosts": [{"kind": "simlock", "id": "pool", "label": "Pool", "gatewayUrl": "https://gw:4700"}]});
        let members = [member("laptop", &laptop, None), member("server", &server, None)];
        let host: SshHost = serde_json::from_value(mac()).unwrap();
        let holders = ["laptop".to_owned()];
        let removed = ssh_plan(&members, &host, true, &host, Some(&holders)).unwrap();
        assert_eq!(removed, [("laptop".to_owned(), json!({"deviceHosts": []}))]);
        let added = ssh_plan(&members, &host, false, &host, None).unwrap();
        assert_eq!(added[1], ("server".to_owned(), json!({"deviceHosts": [mac()]})));

        let pool: SimlockHost = serde_json::from_value(server["simlockDeviceHosts"][0].clone()).unwrap();
        let removed = simlock_remove_plan(&members, &pool).unwrap();
        assert_eq!(removed, [("server".to_owned(), json!({"simlockDeviceHosts": []}))]);
        let saved = simlock_save_plan(&members, &pool, &[]).unwrap();
        assert_eq!(saved.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(), ["laptop", "server"]);

        let eas: EasHost = serde_json::from_value(eas()).unwrap();
        let added = eas_plan(&members, &eas, false, false).unwrap();
        assert!(added.iter().all(|(_, patch)| patch["easDeviceHosts"][0]["id"] == "eas"));

        let unreadable = json!({"deviceHosts": [{"id": "broken"}]});
        let members = [member("laptop", &unreadable, None)];
        assert!(ssh_plan(&members, &host, false, &host, None).is_err());
    }

    fn platform_row(answers: &[(&str, Option<bool>, Option<&str>)], shared: bool) -> SourceRow {
        SourceRow {
            key: "row".into(),
            title: "Row".into(),
            kind: None,
            description: "".into(),
            shared,
            environments: answers
                .iter()
                .map(|(id, available, reason)| SourceEnvironment {
                    id: (*id).into(),
                    label: (*id).into(),
                    added: available.is_some(),
                    status: None,
                    action: None,
                    platforms: available.map(|available| {
                        vec![PlatformAvailability { platform: "ios".into(), available, reason: reason.map(Into::into) }]
                    }),
                })
                .collect(),
            host: RowHost::Local,
        }
    }

    #[test]
    fn platform_breakdowns_mark_partial_availability_and_name_each_environment() {
        let row = platform_row(
            &[("laptop", Some(true), None), ("server", Some(false), Some("No Xcode")), ("pi", None, None)],
            true,
        );
        let ios = platform_summary(&row, "ios").unwrap();
        assert!(ios.available && ios.partial && !ios.uniform);
        assert_eq!(ios.available_on, ["laptop"]);
        assert_eq!(ios.unavailable_on, [("server".to_owned(), Some("No Xcode".to_owned()))]);
        assert_eq!(ios.not_added, ["pi"]);
        assert_eq!(platform_summary(&row, "android"), None, "a platform no one offers has no icon");

        let row =
            platform_row(&[("laptop", Some(false), Some("Sign in")), ("server", Some(false), Some("Sign in"))], true);
        let ios = platform_summary(&row, "ios").unwrap();
        assert!(!ios.available && !ios.partial && ios.uniform);
        assert_eq!(ios.reason.as_deref(), Some("Sign in"));
        assert_eq!(ios.connected, ["laptop", "server"]);
    }
}
