//! `StorageCleanupSections` (`StorageSettings.tsx`) and `DatabaseStorageNotice` for the one
//! environment this client talks to.

use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::NumberInput;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, ClipboardItem, Context, ElementId, Entity, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Task, Window, div,
    prelude::FluentBuilder as _, px,
};
use serde_json::{Value, json};

use super::super::widgets::{RowFrame, card, loose_section, note, settings_section};
use super::{format_bytes, notify_error, now_ms, relative_label, text};
use crate::app::Workspace;
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

pub(super) struct StorageState {
    report: Report,
    database: Option<Value>,
    running: bool,
    last_run_open: bool,
    /// The web keeps this per environment in local storage; this client keeps it while Settings is open.
    database_dismissed: bool,
    _streams: Vec<Task<()>>,
}

impl StorageState {
    fn new(client: Option<RpcClient>, cx: &mut Context<Self>) -> Self {
        let streams = client
            .map(|client| {
                vec![
                    Self::follow(&client, "server.getStorageCleanupReport", cx, |this, event| match event {
                        StreamEvent::Item(report) => this.report = Report::Ready(Some(report).filter(|r| !r.is_null())),
                        StreamEvent::End(Err(_)) if matches!(this.report, Report::Loading) => {
                            this.report = Report::Failed
                        }
                        StreamEvent::End(_) => {}
                    }),
                    Self::follow(&client, "server.getDatabaseStorage", cx, |this, event| {
                        if let StreamEvent::Item(status) = event {
                            this.database = Some(status).filter(|s| !s.is_null());
                        }
                    }),
                ]
            })
            .unwrap_or_default();
        Self {
            report: Report::Loading,
            database: None,
            running: false,
            last_run_open: false,
            database_dismissed: false,
            _streams: streams,
        }
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
                path.and_then(|p| p.split(['/', '\\']).filter(|s| !s.is_empty()).next_back())
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
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

impl Workspace {
    pub(super) fn render_storage_sections(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let t = *theme::tokens(cx);
        let client = self.settings_client();
        let state = window.use_keyed_state("settings-storage", cx, |_, cx| StorageState::new(client, cx));
        let info = &self.settings_info();
        let (cleanup, cleanup_run, database) = (
            info.capability("storageCleanup"),
            info.capability("storageCleanupRun"),
            info.capability("databaseStorage"),
        );
        let mut sections = Vec::new();
        if database {
            sections.extend(self.render_database_notice(&state, &t, cx));
        }
        if !cleanup {
            let notice = note(
                "Update the selected environments to use storage cleanup, or choose a machine that supports it.",
                &t,
            );
            sections.push(loose_section("artifacts-and-logs", "Artifacts and logs", [notice]));
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
            let days = self.settings_json()["storageCleanup"][key].as_f64();
            let field = days.map(|days| {
                self.number_field(
                    key,
                    days.round() as i64,
                    (1, RETENTION_MAX_DAYS, 1),
                    window,
                    cx,
                    move |this, days, window, cx| {
                        this.update_settings(json!({ "storageCleanup": { key: days } }), window, cx)
                    },
                )
            });
            let control = self.retention_control(key, title, field.as_ref(), &t, cx);
            RowFrame::new(title).description(text(description)).control(control).render(&t).into_any_element()
        });
        sections.push(settings_section("artifacts-and-logs", "Artifacts and logs", rows, &t));
        if cleanup_run {
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
                            this.update_settings(json!({ "storageCleanup": { key: days } }), window, cx)
                        })
                    }),
            )
            .into_any_element()
    }

    /// `CleanupSection`: the manual run and the last run's results.
    fn render_cleanup(&self, state: &Entity<StorageState>, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let has_rules = has_rules(self.settings_json());
        let storage = state.read(cx);
        let report = match &storage.report {
            Report::Ready(report) => report.clone(),
            _ => None,
        };
        let description = if !has_rules {
            "Turn on a cleanup rule to use this.".to_owned()
        } else {
            match &storage.report {
                Report::Ready(Some(report)) => last_run_description(report, now_ms()),
                Report::Ready(None) => "Cleanup hasn't run yet.".into(),
                Report::Failed => "Could not load the last run.".into(),
                Report::Loading => "Loading last run…".into(),
            }
        };
        let running = storage.running;
        let last_run_open = storage.last_run_open;
        let label: SharedString =
            self.settings_info().environment_label.clone().unwrap_or_else(|| "this machine".into()).into();
        let client = self.settings_client();
        let run_state = state.clone();
        let run_label = label.clone();
        let delete_now = Button::new("storage-delete-now")
            .outline()
            .small()
            .label(if running { "Deleting…" } else { "Delete now" })
            .disabled(running || !has_rules || client.is_none())
            .on_click(move |_, window, cx| {
                let Some(client) = client.clone() else { return };
                run_storage_cleanup(client, run_state.clone(), run_label.clone(), window, cx);
            });
        let mut blocks = vec![card(
            [RowFrame::new("Delete now")
                .description(text(description))
                .control(delete_now)
                .render(t)
                .into_any_element()],
            t,
        )];
        if let Some(report) = report {
            let toggle = state.downgrade();
            let mut rows = vec![fold_header(
                "storage-last-run",
                "Last run",
                counts_label(&report).into(),
                last_run_open,
                t,
                move |_, cx| {
                    let _ = toggle.update(cx, |state, cx| {
                        state.last_run_open = !state.last_run_open;
                        cx.notify();
                    });
                },
            )];
            if last_run_open {
                rows.extend(cleanup_results(&report, &label, t));
            }
            blocks.push(card(rows, t));
        }
        loose_section("cleanup", "Cleanup", blocks)
    }

    /// `DatabaseStorageNotice`: suggests `t3 doctor` while the database holds a GiB or more of freed pages.
    fn render_database_notice(
        &self,
        state: &Entity<StorageState>,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let storage = state.read(cx);
        let status = storage.database.as_ref().filter(|_| !storage.database_dismissed)?;
        let reclaimable = status["reclaimableBytes"].as_f64().unwrap_or(0.);
        if status["incrementalVacuumEnabled"] == true || reclaimable < COMPACTION_RECOMMENDED_BYTES {
            return None;
        }
        let command = status["compactCommand"].as_str().unwrap_or_default().to_owned();
        let label = self.settings_info().environment_label.clone().unwrap_or_default();
        let dismiss = state.downgrade();
        let copied = command.clone();
        let controls = h_flex()
            .flex_wrap()
            .gap_1p5()
            .child(
                Button::new("database-copy")
                    .outline()
                    .xsmall()
                    .label("Copy command")
                    .on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(copied.clone()))),
            )
            .child(Button::new("database-dismiss").ghost().xsmall().label("Dismiss").on_click(move |_, _, cx| {
                let _ = dismiss.update(cx, |state, cx| {
                    state.database_dismissed = true;
                    cx.notify();
                });
            }));
        let rows = [
            RowFrame::new("Reclaim freed database space")
                .description(text(
                    "Old history is cleaned up automatically, but these databases still hold the freed space. Run the command on each machine to return it to disk; it pauses and restarts the background service. Quit the desktop app first if it hosts the environment.",
                ))
                .render(t)
                .into_any_element(),
            RowFrame::new(format!("{:.1} GiB on {label}", reclaimable / COMPACTION_RECOMMENDED_BYTES))
                .description(div().font_family(t.fonts.mono).child(command))
                .control(controls)
                .render(t)
                .into_any_element(),
        ];
        Some(settings_section("database", "Database", rows, t))
    }
}

/// `server.runStorageCleanup`; failures come back as a toast, as the web shows them.
fn run_storage_cleanup(
    client: RpcClient,
    state: Entity<StorageState>,
    label: SharedString,
    window: &mut Window,
    cx: &mut App,
) {
    if state.read(cx).running {
        return;
    }
    state.update(cx, |state, cx| {
        state.running = true;
        cx.notify();
    });
    window
        .spawn(cx, async move |cx| {
            let result = client.request("server.runStorageCleanup", json!({})).await;
            let _ = cx.update(|window, cx| {
                state.update(cx, |state, cx| {
                    state.running = false;
                    cx.notify();
                });
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
                        notify_error(window, cx, format!("Could not run cleanup on {label}"), error.to_string())
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
        let mut project = off.clone();
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
        let now = crate::inbox::parse_ms("2026-11-05T12:00:00Z").unwrap();
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
