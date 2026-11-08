//! `StorageCleanupSections` (`StorageSettings.tsx`) and `DatabaseStorageNotice` for the
//! selected, connected environments.

use crate::app::pages::settings::widgets::Switch;
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::NumberInput;
use gpui_kit::component::{Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, ClipboardItem, Context, ElementId, Entity, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Task, Window, div,
    prelude::FluentBuilder as _, px,
};
use serde_json::{Value, json};

use super::super::connections::logic::SETTINGS_WRITE;
use super::super::widgets::{RowFrame, card, loose_section, note, settings_section};
use super::{format_bytes, notify_error, now_ms, relative_label, text};
use crate::app::Workspace;
use crate::app::pages::settings::widgets::SettingsView;
use crate::app::sessions::EnvironmentSession;
use crate::rpc::{RpcClient, StreamEvent};
use crate::theme::{self, Tokens};

/// `DATABASE_COMPACTION_RECOMMENDED_BYTES`.
const COMPACTION_RECOMMENDED_BYTES: f64 = 1024. * 1024. * 1024.;
const RETENTION_MAX_DAYS: i64 = 3650;
/// The days a retention rule starts at when switched on.
const RETENTION_ON_DAYS: u64 = 8;

enum Report {
    Loading,
    Failed,
    /// The last run, if cleanup ever ran.
    Ready(Option<Value>),
}

/// What a selected, connected environment offers the storage sections.
pub(super) struct MachineSpec {
    id: String,
    label: SharedString,
    client: RpcClient,
    /// `storageCleanupRun`: it runs cleanup and reports the last run.
    runs_cleanup: bool,
    /// `databaseStorage`: it reports reclaimable database space.
    database: bool,
}

/// One environment's last run and database status.
struct Machine {
    id: String,
    label: SharedString,
    client: RpcClient,
    runs_cleanup: bool,
    report: Report,
    database: Option<Value>,
    /// The web keeps this per environment in local storage; this client keeps it while Settings is open.
    database_dismissed: bool,
}

pub(super) struct StorageState {
    machines: Vec<Machine>,
    running: bool,
    last_run_open: bool,
    _streams: Vec<Task<()>>,
}

impl StorageState {
    fn new(specs: Vec<MachineSpec>, cx: &mut Context<Self>) -> Self {
        let mut streams = Vec::new();
        for (index, spec) in specs.iter().enumerate() {
            if spec.runs_cleanup {
                streams.push(Self::follow(&spec.client, "server.getStorageCleanupReport", cx, move |this, event| {
                    let machine = &mut this.machines[index];
                    match event {
                        StreamEvent::Item(report) => {
                            machine.report = Report::Ready(Some(report).filter(|r| !r.is_null()));
                        }
                        StreamEvent::End(Err(_)) if matches!(machine.report, Report::Loading) => {
                            machine.report = Report::Failed;
                        }
                        StreamEvent::End(_) => {}
                    }
                }));
            }
            if spec.database {
                streams.push(Self::follow(&spec.client, "server.getDatabaseStorage", cx, move |this, event| {
                    if let StreamEvent::Item(status) = event {
                        this.machines[index].database = Some(status).filter(|s| !s.is_null());
                    }
                }));
            }
        }
        let machines = specs
            .into_iter()
            .map(|spec| Machine {
                id: spec.id,
                label: spec.label,
                client: spec.client,
                runs_cleanup: spec.runs_cleanup,
                report: Report::Loading,
                database: None,
                database_dismissed: false,
            })
            .collect();
        Self { machines, running: false, last_run_open: false, _streams: streams }
    }

    fn follow(
        client: &RpcClient,
        tag: &'static str,
        cx: &mut Context<Self>,
        apply: impl Fn(&mut Self, StreamEvent) + 'static,
    ) -> Task<()> {
        let stream = client.stream(tag, json!({}));
        cx.spawn(async move |this, cx| {
            while let Some(event) = stream.next().await {
                let ended = matches!(event, StreamEvent::End(_));
                if this
                    .update(cx, |this, cx| {
                        apply(this, event);
                        cx.notify();
                    })
                    .is_err()
                    || ended
                {
                    break;
                }
            }
        })
    }
}

/// `WorktreeCleanupRules` turns something on.
fn worktree_rules_on(rules: &Value) -> bool {
    !rules["worktreeAfterDays"].is_null()
        || !rules["worktreeSettledAfterDays"].is_null()
        || ["worktreeOnMerge", "worktreeOnDelete", "worktreeUnchanged"].iter().any(|key| rules[key] == true)
}

/// Any cleanup rule is on: retention, the global worktree rules, or a project's custom ones.
fn has_rules(settings: &Value) -> bool {
    let storage = &settings["storageCleanup"];
    !storage["browserArtifactsAfterDays"].is_null()
        || !storage["logsAfterDays"].is_null()
        || worktree_rules_on(storage)
        || settings["projectSettingsOverrides"].as_object().is_some_and(|overrides| {
            overrides
                .values()
                .any(|o| o["worktreeCleanup"]["mode"] == "custom" && worktree_rules_on(&o["worktreeCleanup"]["rules"]))
        })
}

fn counts_label(report: &Value) -> String {
    let count = |key: &str| report["counts"][key].as_u64().unwrap_or(0);
    let failed = count("failed");
    let failed = if failed > 0 { format!(", {failed} failed") } else { String::new() };
    format!("{} removed, {} kept{failed}", count("removed"), count("kept"))
}

fn last_run_description(report: &Value, now_ms: i64) -> String {
    let failed = report["counts"]["failed"].as_u64().unwrap_or(0);
    let failures = match failed {
        0 => String::new(),
        1 => " 1 item couldn't be removed.".into(),
        n => format!(" {n} items couldn't be removed."),
    };
    format!(
        "Last run {}. {} recovered.{failures}",
        relative_label(report["finishedAt"].as_str().unwrap_or_default(), now_ms),
        format_bytes(report["bytesFreed"].as_f64().unwrap_or(0.)),
    )
}

/// An entry's name in the last run (`CleanupResults`).
fn entry_name(entry: &Value) -> String {
    let path = entry["path"].as_str();
    match entry["kind"].as_str() {
        Some("worktree") => {
            let name = if entry["threadId"].is_null() {
                path
            } else {
                path.and_then(|p| p.split(['/', '\\']).rfind(|s| !s.is_empty()))
            };
            name.unwrap_or("Worktrees").to_owned()
        }
        Some("logs") => "Rotated logs".into(),
        _ => "Browser artifacts".into(),
    }
}

/// The size column: files and bytes for removed entries, and for kept ones that were measured.
fn entry_size(entry: &Value) -> Option<String> {
    let outcome = entry["outcome"].as_str();
    let bytes = entry["bytes"].as_f64();
    if !(outcome == Some("removed") || (outcome == Some("kept") && bytes.is_some())) {
        return None;
    }
    let files = entry["files"]
        .as_u64()
        .filter(|_| entry["kind"] != "worktree")
        .map(|files| format!("{} {}", group_digits(files), if files == 1 { "file" } else { "files" }));
    let parts: Vec<String> = files.into_iter().chain(bytes.map(format_bytes)).collect();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// `Number.toLocaleString()` in an English locale.
fn group_digits(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::new();
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

impl Workspace {
    pub(super) fn render_storage_sections(
        &mut self,
        view: &SettingsView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let t = *theme::tokens(cx);
        let connected: Vec<&EnvironmentSession> =
            view.connected_environment_ids().iter().filter_map(|id| self.session(id)).collect();
        let cleanup = connected.iter().all(|s| s.server_info.capability("storageCleanup"));
        let key = connected.iter().filter_map(|s| s.id.as_deref()).collect::<Vec<_>>().join("\u{1f}");
        let specs: Vec<MachineSpec> = connected
            .iter()
            .filter_map(|s| {
                let id = s.id.as_deref()?.to_owned();
                Some(MachineSpec {
                    label: s.server_info.environment_label.clone().unwrap_or_else(|| id.clone()).into(),
                    client: s.client.clone()?,
                    runs_cleanup: s.server_info.capability("storageCleanupRun"),
                    database: s.server_info.capability("databaseStorage"),
                    id,
                })
            })
            .collect();
        let state = window.use_keyed_state(SharedString::from(format!("settings-storage:{key}")), cx, |_, cx| {
            StorageState::new(specs, cx)
        });
        let mut sections = Vec::new();
        sections.extend(render_database_notice(&state, &t, cx));
        if !cleanup {
            let notice = note(
                "Update the selected environments to use storage cleanup, or choose a machine that supports it.",
                &t,
            );
            sections.push(loose_section("storage-artifacts", "Artifacts and logs", [notice]));
            return sections;
        }
        let rows = [
            (
                "Delete old browser artifacts",
                "Delete saved browser captures after this many days. Older capture links will no longer open.",
                "browserArtifactsAfterDays",
            ),
            (
                "Delete old rotated logs",
                "Delete inactive rotated log files after this many days. Current logs are kept.",
                "logsAfterDays",
            ),
        ]
        .map(|(title, description, key)| {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "rounded first, and `as` saturates out-of-range values"
            )]
            let days = self.settings_json()["storageCleanup"][key].as_f64().map(|days| days.round() as i64);
            let field = days.map(|days| {
                self.number_field(key, days, (1, RETENTION_MAX_DAYS, 1), window, cx, move |this, days, window, cx| {
                    this.update_settings(json!({ "storageCleanup": { key: days } }), window, cx);
                })
            });
            let control = self.retention_control(key, title, field.as_ref(), &t, cx);
            RowFrame::new(title).description(text(description)).control(control).render(&t).into_any_element()
        });
        sections.push(settings_section("storage-artifacts", "Artifacts and logs", rows, &t));
        if state.read(cx).machines.iter().any(|m| m.runs_cleanup) {
            sections.push(self.render_cleanup(&state, &t, cx));
        }
        sections
    }

    /// `RetentionControl`: days and a switch; off is `null`.
    fn retention_control(
        &self,
        key: &'static str,
        label: &'static str,
        input: Option<&Entity<gpui_kit::component::input::InputState>>,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let view = cx.entity();
        let connected = self.settings_client().is_some();
        h_flex()
            .gap_3()
            .child(match input {
                Some(input) => NumberInput::new(input)
                    .small()
                    .w(px(136.))
                    .accessibility_label(format!("{label} in days"))
                    .suffix(div().text_xs().child("days"))
                    .into_any_element(),
                None => div().text_xs().text_color(t.muted_foreground).child("Off").into_any_element(),
            })
            .child(
                Switch::new(SharedString::from(format!("retention-{key}")))
                    .checked(input.is_some())
                    .accessibility_label(label)
                    .disabled(!connected)
                    .on_click(move |enabled, window, cx| {
                        let days = if *enabled { json!(RETENTION_ON_DAYS) } else { Value::Null };
                        view.update(cx, |this, cx| {
                            this.update_settings(json!({ "storageCleanup": { key: days } }), window, cx);
                        });
                    }),
            )
            .into_any_element()
    }

    /// `CleanupSection`: the manual run on every machine that may run it, and each one's last run.
    fn render_cleanup(&self, state: &Entity<StorageState>, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let storage = state.read(cx);
        let supported: Vec<&Machine> = storage.machines.iter().filter(|m| m.runs_cleanup).collect();
        let has_rules =
            supported.iter().any(|m| self.session(&m.id).is_some_and(|s| has_rules(&s.server_info.settings)));
        let allowed: Vec<(SharedString, RpcClient)> = supported
            .iter()
            .filter(|m| self.environment_grants(&m.id, SETTINGS_WRITE))
            .map(|m| (m.label.clone(), m.client.clone()))
            .collect();
        let now = now_ms();
        let description = if has_rules {
            let named = supported.len() > 1;
            let lines = supported.iter().map(|m| {
                let line = machine_description(&m.report, now);
                text(if named { format!("{}: {line}", m.label) } else { line })
            });
            v_flex().children(lines).into_any_element()
        } else {
            text("Turn on a cleanup rule to use this.")
        };
        let running = storage.running;
        let run_state = state.clone();
        let delete_now = Button::new("storage-delete-now")
            .outline()
            .small()
            .label(if running { "Deleting…" } else { "Delete now" })
            .disabled(running || !has_rules || allowed.is_empty())
            .on_click(move |_, window, cx| run_storage_cleanup(allowed.clone(), run_state.clone(), window, cx));
        let mut blocks = vec![card(
            [RowFrame::new("Delete now").description(description).control(delete_now).render(t).into_any_element()],
            t,
        )];
        let reports: Vec<(&SharedString, &Value)> = supported
            .iter()
            .filter_map(|m| match &m.report {
                Report::Ready(Some(report)) => Some((&m.label, report)),
                _ => None,
            })
            .collect();
        if !reports.is_empty() {
            let counts: Vec<(&str, String)> =
                reports.iter().map(|(label, report)| (label.as_ref(), counts_label(report))).collect();
            let summary = last_run_summary(&counts, storage.machines.len());
            let toggle = state.downgrade();
            let open = storage.last_run_open;
            let mut rows = vec![fold_header("storage-last-run", "Last run", summary.into(), open, t, move |_, cx| {
                let _ = toggle.update(cx, |state, cx| {
                    state.last_run_open = !state.last_run_open;
                    cx.notify();
                });
            })];
            if open {
                for (label, report) in &reports {
                    rows.extend(cleanup_results(report, label, t));
                }
            }
            blocks.push(card(rows, t));
        }
        loose_section("cleanup", "Cleanup", blocks)
    }
}

/// A machine's line under Delete now.
fn machine_description(report: &Report, now_ms: i64) -> String {
    match report {
        Report::Ready(Some(report)) => last_run_description(report, now_ms),
        Report::Ready(None) => "Cleanup hasn't run yet.".into(),
        Report::Failed => "Could not load the last run.".into(),
        Report::Loading => "Loading last run…".into(),
    }
}

/// `lastRunSummary`: one machine's counts, "… on every machine" when every machine agrees, else
/// each machine's counts by name.
fn last_run_summary(entries: &[(&str, String)], machine_count: usize) -> String {
    let Some((_, first)) = entries.first() else { return String::new() };
    if machine_count <= 1 {
        return first.clone();
    }
    if entries.len() == machine_count && entries.iter().all(|(_, counts)| counts == first) {
        return format!("{first} on every machine");
    }
    entries.iter().map(|(label, counts)| format!("{label}: {counts}")).collect::<Vec<_>>().join(" · ")
}

/// `DatabaseStorageNotice`: suggests `t3 doctor` on each machine whose database holds a GiB or
/// more of freed pages.
fn render_database_notice(state: &Entity<StorageState>, t: &Tokens, cx: &App) -> Option<AnyElement> {
    let storage = state.read(cx);
    let visible: Vec<(usize, &Machine, &Value)> = storage
        .machines
        .iter()
        .enumerate()
        .filter(|(_, machine)| !machine.database_dismissed)
        .filter_map(|(index, machine)| {
            let status = machine.database.as_ref()?;
            let reclaimable = status["reclaimableBytes"].as_f64().unwrap_or(0.);
            let recommended = status["incrementalVacuumEnabled"] != true && reclaimable >= COMPACTION_RECOMMENDED_BYTES;
            recommended.then_some((index, machine, status))
        })
        .collect();
    if visible.is_empty() {
        return None;
    }
    let mut rows = vec![
        RowFrame::new("Reclaim freed database space")
            .description(text(
                "These databases still hold space freed by history cleanup. Run the command on each machine to reclaim it. It restarts the background service. Quit the desktop app first if it hosts the environment.",
            ))
            .render(t)
            .into_any_element(),
    ];
    for (index, machine, status) in visible {
        let reclaimable = status["reclaimableBytes"].as_f64().unwrap_or(0.);
        let command = status["compactCommand"].as_str().unwrap_or_default().to_owned();
        let dismiss = state.downgrade();
        let copied = command.clone();
        let controls = h_flex()
            .flex_wrap()
            .gap_1p5()
            .child(
                Button::new(SharedString::from(format!("database-copy-{index}")))
                    .outline()
                    .xsmall()
                    .label("Copy command")
                    .on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(copied.clone()))),
            )
            .child(
                Button::new(SharedString::from(format!("database-dismiss-{index}")))
                    .ghost()
                    .xsmall()
                    .label("Dismiss")
                    .on_click(move |_, _, cx| {
                        let _ = dismiss.update(cx, |state, cx| {
                            state.machines[index].database_dismissed = true;
                            cx.notify();
                        });
                    }),
            );
        rows.push(
            RowFrame::new(format!("{:.1} GiB on {}", reclaimable / COMPACTION_RECOMMENDED_BYTES, machine.label))
                .description(div().font_family(t.fonts.mono).child(command))
                .control(controls)
                .render(t)
                .into_any_element(),
        );
    }
    Some(settings_section("database", "Database", rows, t))
}

/// `server.runStorageCleanup` on each target at once; failures come back as toasts, as the web
/// shows them.
fn run_storage_cleanup(
    targets: Vec<(SharedString, RpcClient)>,
    state: Entity<StorageState>,
    window: &mut Window,
    cx: &mut App,
) {
    if state.read(cx).running || targets.is_empty() {
        return;
    }
    state.update(cx, |state, cx| {
        state.running = true;
        cx.notify();
    });
    window
        .spawn(cx, async move |cx| {
            let runs = targets.into_iter().map(|(label, client)| async move {
                (label, client.request("server.runStorageCleanup", json!({})).await)
            });
            let results = futures::future::join_all(runs).await;
            let _ = cx.update(|window, cx| {
                state.update(cx, |state, cx| {
                    state.running = false;
                    cx.notify();
                });
                for (label, result) in results {
                    match result {
                        Ok(report) => {
                            let failed = report["entries"]
                                .as_array()
                                .and_then(|entries| entries.iter().find(|entry| entry["outcome"] == "failed"));
                            if let Some(entry) = failed {
                                let reason = entry["reason"].as_str().unwrap_or_default().to_owned();
                                notify_error(window, cx, format!("Cleanup failed on {label}"), reason);
                            }
                        }
                        Err(error) => {
                            notify_error(window, cx, format!("Could not run cleanup on {label}"), error.to_string());
                        }
                    }
                }
            });
        })
        .detach();
}

/// `CleanupResults`: one row per entry, then how many more were left out.
fn cleanup_results(report: &Value, machine: &str, t: &Tokens) -> Vec<AnyElement> {
    let entries = report["entries"].as_array().map(Vec::as_slice).unwrap_or_default();
    let mut rows: Vec<AnyElement> = entries
        .iter()
        .map(|entry| {
            let failed = entry["outcome"] == "failed";
            let detail = |label: &'static str, value: &str, mono: bool| {
                h_flex().gap_3().items_start().child(div().w(px(56.)).flex_none().child(label)).child(
                    div()
                        .min_w_0()
                        .text_color(t.foreground.opacity(0.8))
                        .when(mono, |el| el.font_family(t.fonts.mono))
                        .child(value.to_owned()),
                )
            };
            let mut details = v_flex().gap_1().pl_3().border_l_1().border_color(t.border.opacity(0.6));
            details = details.child(detail("Machine", machine, false));
            for (label, key, mono) in
                [("Project", "projectPath", true), ("Folder", "path", true), ("Thread", "threadTitle", false)]
            {
                if let Some(value) = entry[key].as_str().filter(|v| !v.is_empty()) {
                    details = details.child(detail(label, value, mono));
                }
            }
            let description = v_flex()
                .gap_2()
                .child(
                    div()
                        .when(failed, |el| el.text_color(t.error_foreground))
                        .child(entry["reason"].as_str().unwrap_or_default().to_owned()),
                )
                .child(details);
            let size = entry_size(entry)
                .map(|size| div().text_xs().text_color(t.muted_foreground).child(size).into_any_element());
            RowFrame::new(entry_name(entry))
                .description(description)
                .control(div().children(size))
                .render(t)
                .into_any_element()
        })
        .collect();
    let omitted = report["omittedCount"].as_u64().unwrap_or(0);
    if omitted > 0 {
        rows.push(note(format!("{machine}: {} more results.", group_digits(omitted)), t));
    }
    if entries.is_empty() && omitted == 0 {
        rows.push(note(format!("{machine}: Nothing needed cleanup."), t));
    }
    rows
}

/// The header of a fold inside a card (`FoldedSettingsSection`, header inside).
fn fold_header(
    id: impl Into<ElementId>,
    title: &'static str,
    summary: SharedString,
    open: bool,
    t: &Tokens,
    on_toggle: impl Fn(&mut Window, &mut App) + 'static,
) -> AnyElement {
    h_flex()
        .id(id)
        .min_h(px(44.))
        .px_4()
        .py_2()
        .gap_2()
        .cursor_pointer()
        .on_click(move |_, window, cx| on_toggle(window, cx))
        .child(
            Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight })
                .size(px(16.))
                .text_color(t.muted_foreground),
        )
        .child(div().flex_none().text_sm().font_weight(FontWeight::MEDIUM).child(title))
        .when(!summary.is_empty(), |el| {
            el.child(div().min_w_0().truncate().text_xs().text_color(t.muted_foreground).child(summary))
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rule_is_on_from_retention_global_worktree_rules_or_a_custom_project() {
        let off = json!({"storageCleanup": {"browserArtifactsAfterDays": null, "logsAfterDays": null,
            "worktreeAfterDays": null, "worktreeSettledAfterDays": null, "worktreeOnMerge": false,
            "worktreeOnDelete": false, "worktreeUnchanged": false}, "projectSettingsOverrides": {}});
        assert!(!has_rules(&off));
        let mut logs = off.clone();
        logs["storageCleanup"]["logsAfterDays"] = json!(8);
        assert!(has_rules(&logs));
        let mut project = off;
        project["projectSettingsOverrides"] =
            json!({"p": {"worktreeCleanup": {"mode": "custom", "rules": {"worktreeOnMerge": true}}}});
        assert!(has_rules(&project));
        project["projectSettingsOverrides"]["p"]["worktreeCleanup"]["mode"] = json!("off");
        assert!(!has_rules(&project), "only custom project rules count");
    }

    #[test]
    fn describes_last_run_entries_like_the_web() {
        let report = json!({"finishedAt": "2026-11-05T11:00:00Z", "bytesFreed": 2048,
            "counts": {"removed": 3, "kept": 1, "failed": 1}});
        assert_eq!(counts_label(&report), "3 removed, 1 kept, 1 failed");
        let (one, other) = ("1 removed, 0 kept".to_owned(), "2 removed, 0 kept".to_owned());
        assert_eq!(last_run_summary(&[("Laptop", one.clone())], 1), one);
        assert_eq!(
            last_run_summary(&[("Laptop", one.clone()), ("Desk", one.clone())], 2),
            "1 removed, 0 kept on every machine"
        );
        assert_eq!(
            last_run_summary(&[("Laptop", one.clone())], 2),
            "Laptop: 1 removed, 0 kept",
            "a machine that never ran keeps the summary per machine"
        );
        assert_eq!(
            last_run_summary(&[("Laptop", one), ("Desk", other)], 2),
            "Laptop: 1 removed, 0 kept · Desk: 2 removed, 0 kept"
        );
        let now = crate::timestamp::parse_ms("2026-11-05T12:00:00Z").unwrap();
        assert_eq!(
            last_run_description(&report, now),
            "Last run 1h ago. 2.00 KB recovered. 1 item couldn't be removed."
        );

        let worktree = json!({"kind": "worktree", "outcome": "removed", "path": "C:\\wt\\feature\\", "threadId": "t", "bytes": 1024, "files": 9});
        assert_eq!(entry_name(&worktree), "feature");
        assert_eq!(entry_size(&worktree).as_deref(), Some("1.00 KB"), "worktrees show no file count");
        let logs =
            json!({"kind": "logs", "outcome": "kept", "path": null, "threadId": null, "bytes": null, "files": 1200});
        assert_eq!((entry_name(&logs).as_str(), entry_size(&logs)), ("Rotated logs", None));
        let artifacts = json!({"kind": "browser-artifacts", "outcome": "removed", "bytes": null, "files": 1200});
        assert_eq!(entry_size(&artifacts).as_deref(), Some("1,200 files"));
    }
}
