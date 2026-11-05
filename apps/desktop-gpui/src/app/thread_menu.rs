//! The thread title's actions menu and inline rename (`ChatHeader.tsx`,
//! `useThreadActionMenu.ts`, `threadActionMenu.logic.ts`). Electron shows this
//! menu natively; here it is a popup anchored under the title.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use chrono::{DateTime, Datelike, Local, TimeZone, Timelike};
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::{
    AnyElement, App, AppContext as _, Bounds, ClipboardItem, Context, DismissEvent, Entity, Focusable as _,
    IntoElement, ParentElement as _, Pixels, SharedString, Subscription, Task, Window, anchored, deferred, point, px,
};
use serde_json::{Value, json};

use super::titlebar::icon;
use super::{Target, Workspace, new_id};
use crate::inbox;

/// Electron delays the menu through the double-click interval, so a double click renames instead.
const TITLE_MENU_DELAY: Duration = Duration::from_millis(500);

#[derive(Default)]
pub(super) struct TitleState {
    menu: Option<(Entity<PopupMenu>, Subscription)>,
    pending: Option<Task<()>>,
    pub rename: Option<Rename>,
    /// The title's bounds from the last paint; the menu opens under its left edge.
    pub bounds: Rc<Cell<Bounds<Pixels>>>,
}

pub(super) struct Rename {
    pub thread_id: String,
    pub original: String,
    pub input: Entity<InputState>,
    _subscription: Subscription,
}

/// One row of the snooze menu (`SnoozePreset`).
#[derive(Clone, Debug, PartialEq)]
pub(super) struct SnoozePreset {
    pub label: String,
    pub when: String,
    pub until: String,
}

const DEFAULT_SNOOZE_PRESETS: &str = r#"[
    {"kind": "after", "minutes": 60},
    {"kind": "after", "minutes": 180},
    {"kind": "at", "day": "today", "time": "18:00"},
    {"kind": "at", "day": "tomorrow", "time": "09:00"},
    {"kind": "at", "day": "monday", "time": "09:00"}
]"#;

const WEEKDAYS: [&str; 7] = ["sunday", "monday", "tuesday", "wednesday", "thursday", "friday", "saturday"];

fn pluralize(count: i64, unit: &str) -> String {
    format!("{count} {unit}{}", if count == 1 { "" } else { "s" })
}

fn capitalize(word: &str) -> String {
    let mut chars = word.chars();
    chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

fn time_label<Tz: TimeZone>(date: &DateTime<Tz>) -> String {
    let (pm, hour) = date.hour12();
    format!("{hour}:{:02} {}", date.minute(), if pm { "PM" } else { "AM" })
}

/// `resolveSnoozePresets`: soonest first, one row per wake minute.
pub(super) fn snooze_presets<Tz: TimeZone>(now: DateTime<Tz>, settings: Option<&Value>) -> Vec<SnoozePreset> {
    let defaults: Value = serde_json::from_str(DEFAULT_SNOOZE_PRESETS).unwrap_or_default();
    let settings = settings.and_then(Value::as_array).unwrap_or_else(|| defaults.as_array().unwrap());
    let mut presets: Vec<(i64, SnoozePreset)> = settings
        .iter()
        .filter_map(|setting| {
            let (wake, label, names_day) = match setting["kind"].as_str()? {
                "after" => {
                    let minutes = setting["minutes"].as_i64()?;
                    let duration = if minutes % (24 * 60) == 0 {
                        pluralize(minutes / (24 * 60), "day")
                    } else if minutes % 60 == 0 {
                        pluralize(minutes / 60, "hour")
                    } else {
                        pluralize(minutes, "minute")
                    };
                    (now.clone() + chrono::Duration::minutes(minutes), format!("In {duration}"), false)
                }
                "at" => {
                    let day = setting["day"].as_str()?;
                    let (hour, minute) = setting["time"].as_str()?.split_once(':')?;
                    let (hour, minute) = (hour.parse::<u32>().ok()?, minute.parse::<u32>().ok()?);
                    let today = now.weekday().num_days_from_sunday() as i64;
                    let days_ahead = match day {
                        "today" => 0,
                        "tomorrow" => 1,
                        weekday => {
                            let index = WEEKDAYS.iter().position(|w| *w == weekday)? as i64;
                            match (index - today + 7) % 7 {
                                0 => 7,
                                n => n,
                            }
                        }
                    };
                    let date = now.date_naive() + chrono::Duration::days(days_ahead);
                    let wake = now.timezone().from_local_datetime(&date.and_hms_opt(hour, minute, 0)?).earliest()?;
                    // A today option whose time has passed is left out until tomorrow.
                    if wake <= now {
                        return None;
                    }
                    let label = match days_ahead {
                        0 if hour < 12 => "This morning".to_owned(),
                        0 if hour < 17 => "This afternoon".to_owned(),
                        0 => "This evening".to_owned(),
                        1 => "Tomorrow".to_owned(),
                        _ if day == "monday" => "Next week".to_owned(),
                        7 => format!("Next {}", capitalize(day)),
                        _ => capitalize(day),
                    };
                    let names_day = label != "Next week";
                    (wake, label, names_day)
                }
                _ => return None,
            };
            let time = time_label(&wake);
            let when = if names_day || wake.date_naive() == now.date_naive() {
                time
            } else {
                let day =
                    ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"][wake.weekday().num_days_from_sunday() as usize];
                format!("{day} {time}")
            };
            let minute = wake.timestamp() / 60;
            Some((
                minute,
                SnoozePreset { label, when, until: wake.to_utc().to_rfc3339_opts(chrono::SecondsFormat::Millis, true) },
            ))
        })
        .collect();
    presets.sort_by_key(|(minute, _)| *minute);
    presets.dedup_by_key(|(minute, _)| *minute);
    presets.into_iter().map(|(_, preset)| preset).collect()
}

/// `hostedThreadLink`: every character outside `[A-Za-z0-9_-]` percent-encoded.
pub(super) fn hosted_thread_link(environment_id: &str, thread_id: &str) -> String {
    let segment = |id: &str| {
        id.bytes()
            .map(|byte| match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-' => (byte as char).to_string(),
                _ => format!("%{byte:02X}"),
            })
            .collect::<String>()
    };
    format!("https://app.t3.codes/threads/{}/{}", segment(environment_id), segment(thread_id))
}

impl Workspace {
    fn menu_thread(&self) -> Option<&crate::model::ThreadShell> {
        let Some(Target::Thread(id)) = &self.target else { return None };
        self.shell.threads.iter().find(|thread| &thread.id == id)
    }

    /// A click on the title: the menu after the double-click interval; the chevron opens it at once.
    pub(super) fn click_title(&mut self, click_count: usize, now: bool, window: &mut Window, cx: &mut Context<Self>) {
        if click_count >= 2 {
            self.title.pending = None;
            self.start_rename(window, cx);
            return;
        }
        if now {
            self.open_thread_menu(window, cx);
            return;
        }
        self.title.pending = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(TITLE_MENU_DELAY).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.title.pending = None;
                this.open_thread_menu(window, cx);
            });
        }));
    }

    fn open_thread_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread) = self.menu_thread().cloned() else { return };
        let view = cx.entity();
        let info = &self.server_info;
        let supports = |name: &str| info.capability(name);
        let (pinning, settlement, snooze) =
            (supports("threadPinning"), supports("threadSettlement"), supports("threadSnooze"));
        let (regenerate, auto_settle) = (supports("threadTitleRegeneration"), supports("threadAutoSettleOptOut"));
        let now = super::sidebar::now_ms();
        let requests = inbox::child_requests(&self.shell.threads);
        let facts = inbox::Facts::new(&thread, &requests);
        let snoozed = snooze && facts.is_snoozed(now);
        let can_snooze = thread.pending_runtime_request.is_none() && thread.pending_guest_message_count == 0;
        let running = thread.active_run_id.is_some();
        let presets = snooze_presets(Local::now(), info.snooze_presets.as_ref());
        let workspace_path = thread.worktree_path.clone().or_else(|| {
            self.shell.projects.iter().find(|p| p.id == thread.project_id).map(|p| p.workspace_root.clone())
        });
        let link = info.environment_id.as_deref().map(|env| hosted_thread_link(env, &thread.id));
        let regenerating = thread.title_regeneration.is_some();
        let id = thread.id.clone();
        let command = move |kind: &'static str, extra: Value, _: &mut Window| {
            let (id, view) = (id.clone(), view.clone());
            move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
                let mut command = json!({"type": kind, "commandId": new_id(), "threadId": id});
                if let (Some(command), Value::Object(extra)) = (command.as_object_mut(), extra.clone()) {
                    command.extend(extra);
                }
                view.update(cx, |this, cx| this.dispatch(command, cx));
            }
        };
        let copy = |text: String| {
            move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
                cx.write_to_clipboard(ClipboardItem::new_string(text.clone()))
            }
        };
        let view = cx.entity();
        let branch = thread.branch.clone();
        let menu = PopupMenu::build(window, cx, move |mut menu, window, cx| {
            if pinning {
                menu = match thread.pinned_at {
                    Some(_) => {
                        menu.item(PopupMenuItem::new("Unpin thread").icon(icon("icons/pin-off.svg")).on_click(command(
                            "thread.unpin",
                            json!({}),
                            window,
                        )))
                    }
                    None => menu.item(PopupMenuItem::new("Pin thread").icon(icon("icons/pin.svg")).on_click(command(
                        "thread.pin",
                        json!({}),
                        window,
                    ))),
                };
            }
            if settlement {
                menu =
                    if thread.settled_override.as_deref() == Some("settled") {
                        menu.item(
                            PopupMenuItem::new("Un-settle thread")
                                .icon(icon("icons/circle-check.svg"))
                                .on_click(command("thread.unsettle", json!({"reason": "user"}), window)),
                        )
                    } else {
                        menu.item(
                            PopupMenuItem::new("Settle thread").icon(icon("icons/circle-check.svg")).on_click(command(
                                "thread.settle",
                                json!({}),
                                window,
                            )),
                        )
                    };
            }
            if snooze {
                menu = if snoozed {
                    menu.item(PopupMenuItem::new("Wake thread").icon(icon("icons/clock.svg")).on_click(command(
                        "thread.unsnooze",
                        json!({"reason": "user"}),
                        window,
                    )))
                } else if !can_snooze {
                    menu.item(PopupMenuItem::new("Snooze").icon(icon("icons/clock.svg")).disabled(true))
                } else {
                    let presets = presets.clone();
                    let command = command.clone();
                    menu.submenu_with_icon(
                        Some(icon("icons/clock.svg").into()),
                        "Snooze",
                        window,
                        cx,
                        move |mut menu, window, _| {
                            for preset in &presets {
                                let label = format!("{} ({})", preset.label, preset.when);
                                menu = menu.item(PopupMenuItem::new(label).on_click(command(
                                    "thread.snooze",
                                    json!({"snoozedUntil": preset.until}),
                                    window,
                                )));
                            }
                            menu
                        },
                    )
                };
            }
            menu = menu.separator().item(
                PopupMenuItem::new("Rename thread")
                    .icon(icon("icons/pencil.svg"))
                    .on_click(window.listener_for(&view, |this, _, window, cx| this.start_rename(window, cx))),
            );
            if regenerate {
                let label = if regenerating { "Regenerating…" } else { "Regenerate title" };
                menu = menu.item(
                    PopupMenuItem::new(label)
                        .icon(icon("icons/refresh-cw.svg"))
                        .disabled(regenerating)
                        .on_click(command("thread.metadata.update", json!({"regenerateTitle": true}), window)),
                );
            }
            menu = menu.item(PopupMenuItem::new("Mark unread").icon(icon("icons/mail-open.svg")).on_click(command(
                "thread.mark-unread",
                json!({}),
                window,
            )));
            if auto_settle {
                let enabled = thread.auto_settle_disabled_at.is_none();
                let command = command.clone();
                menu = menu.submenu_with_icon(
                    Some(icon("icons/timer.svg").into()),
                    "Auto-settle behavior",
                    window,
                    cx,
                    move |menu, window, _| {
                        menu.item(PopupMenuItem::new("Enabled").checked(enabled).on_click(command(
                            "thread.auto-settle.set",
                            json!({"enabled": true}),
                            window,
                        )))
                        .item(
                            PopupMenuItem::new("Disabled").checked(!enabled).on_click(command(
                                "thread.auto-settle.set",
                                json!({"enabled": false}),
                                window,
                            )),
                        )
                    },
                );
            }
            let (path, branch, link, id) = (workspace_path.clone(), branch.clone(), link.clone(), thread.id.clone());
            menu = menu.separator().submenu_with_icon(
                Some(icon("icons/copy.svg").into()),
                "Copy",
                window,
                cx,
                move |mut menu, _, _| {
                    if let Some(path) = &path {
                        menu = menu.item(
                            PopupMenuItem::new("Path").icon(icon("icons/folder.svg")).on_click(copy(path.clone())),
                        );
                    }
                    if let Some(branch) = &branch {
                        menu = menu.item(
                            PopupMenuItem::new("Branch")
                                .icon(icon("icons/git-branch.svg"))
                                .on_click(copy(branch.clone())),
                        );
                    }
                    if let Some(link) = &link {
                        menu = menu.item(
                            PopupMenuItem::new("Thread")
                                .icon(icon("icons/message-circle.svg"))
                                .on_click(copy(link.clone())),
                        );
                    }
                    menu.item(PopupMenuItem::new("Thread ID").icon(icon("icons/hash.svg")).on_click(copy(id.clone())))
                },
            );
            menu.separator()
                .item(
                    PopupMenuItem::new("Archive thread")
                        .icon(icon("icons/archive.svg"))
                        .disabled(running)
                        .on_click(window.listener_for(&view, |this, _, window, cx| this.archive_thread(window, cx))),
                )
                .item(
                    PopupMenuItem::new("Delete")
                        .icon(icon("icons/trash.svg"))
                        .on_click(window.listener_for(&view, |this, _, window, cx| this.delete_thread(window, cx))),
                )
                .min_w(px(220.))
        });
        menu.focus_handle(cx).focus(window, cx);
        let subscription = cx.subscribe_in(&menu, window, |this, _, _: &DismissEvent, _, cx| {
            this.title.menu = None;
            cx.notify();
        });
        self.title.menu = Some((menu, subscription));
        cx.notify();
    }

    /// The open title menu, under the title's left edge.
    pub(super) fn render_title_menu(&self) -> Option<AnyElement> {
        let (menu, _) = self.title.menu.as_ref()?;
        let bounds = self.title.bounds.get();
        let corner = point(bounds.left(), bounds.bottom() + px(4.));
        Some(deferred(anchored().position(corner).child(menu.clone())).with_priority(2).into_any_element())
    }

    pub(super) fn start_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread) = self.menu_thread() else { return };
        let (thread_id, original) = (thread.id.clone(), thread.title.clone());
        let input = cx.new(|cx| InputState::new(window, cx).default_value(original.clone()));
        input.update(cx, |input, cx| {
            input.focus(window, cx);
            input.select_all(window, cx);
        });
        let subscription = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| match event {
            InputEvent::PressEnter { .. } | InputEvent::Blur => this.commit_rename(window, cx),
            _ => {}
        });
        self.title.rename = Some(Rename { thread_id, original, input, _subscription: subscription });
        cx.notify();
    }

    pub(super) fn cancel_rename(&mut self, cx: &mut Context<Self>) {
        self.title.rename = None;
        cx.notify();
    }

    /// `resolveRenameCommit`: an empty title is refused, an unchanged one is a no-op.
    fn commit_rename(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let Some(rename) = self.title.rename.take() else { return };
        let title = rename.input.read(cx).value().trim().to_owned();
        if title.is_empty() {
            self.error = Some("Thread title cannot be empty".into());
        } else if title != rename.original {
            let command = json!({
                "type": "thread.metadata.update",
                "commandId": new_id(),
                "threadId": rename.thread_id,
                "title": title,
            });
            self.dispatch(command, cx);
        }
        cx.notify();
    }

    /// `thread.archive`, confirmed first when the setting asks; the view moves to a new draft.
    fn archive_thread(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread) = self.menu_thread() else { return };
        let (id, project_id, title) = (thread.id.clone(), thread.project_id.clone(), thread.title.clone());
        let archive = move |this: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>| {
            this.dispatch(json!({"type": "thread.archive", "commandId": new_id(), "threadId": id}), cx);
            this.start_draft(project_id.clone(), window, cx);
        };
        if !self.server_info.confirm_thread_archive {
            archive(self, window, cx);
            return;
        }
        self.confirm(format!("Archive thread \"{title}\"?"), None, "Archive", archive, window, cx);
    }

    /// `thread.delete`, confirmed unless the user turned that off.
    fn delete_thread(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread) = self.menu_thread() else { return };
        let (id, title) = (thread.id.clone(), thread.title.clone());
        let delete = move |this: &mut Workspace, _: &mut Window, cx: &mut Context<Workspace>| {
            this.dispatch(json!({"type": "thread.delete", "commandId": new_id(), "threadId": id}), cx);
        };
        if !self.server_info.confirm_thread_delete {
            delete(self, window, cx);
            return;
        }
        let detail = "This permanently clears conversation history for this thread.";
        self.confirm(format!("Delete thread \"{title}\"?"), Some(detail), "Delete", delete, window, cx);
    }

    fn confirm(
        &mut self,
        title: String,
        detail: Option<&'static str>,
        ok: &'static str,
        then: impl Fn(&mut Workspace, &mut Window, &mut Context<Workspace>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let view = cx.entity();
        let then = Rc::new(then);
        let title = SharedString::from(title);
        window.open_alert_dialog(cx, move |alert, _, _| {
            let (view, then) = (view.clone(), then.clone());
            let alert = alert.title(title.clone());
            let alert = match detail {
                Some(detail) => alert.description(detail),
                None => alert,
            };
            alert
                .button_props(gpui_kit::component::dialog::DialogButtonProps::default().ok_text(ok))
                .show_cancel(true)
                .on_ok(move |_, window, cx| {
                    let then = then.clone();
                    view.update(cx, |this, cx| then(this, window, cx));
                    true
                })
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{FixedOffset, TimeZone as _};

    fn at(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<FixedOffset> {
        FixedOffset::east_opt(0).unwrap().with_ymd_and_hms(y, m, d, h, min, 0).unwrap()
    }

    #[test]
    fn default_presets_follow_the_web() {
        // Wednesday 2026-11-04 10:00.
        let presets = snooze_presets(at(2026, 11, 4, 10, 0), None);
        let labels: Vec<String> = presets.iter().map(|p| format!("{} ({})", p.label, p.when)).collect();
        assert_eq!(
            labels,
            [
                "In 1 hour (11:00 AM)",
                "In 3 hours (1:00 PM)",
                "This evening (6:00 PM)",
                "Tomorrow (9:00 AM)",
                "Next week (Mon 9:00 AM)"
            ]
        );
        assert_eq!(presets[3].until, "2026-11-05T09:00:00.000Z");
    }

    #[test]
    fn passed_today_options_drop_and_same_minute_rows_merge() {
        // Sunday 20:00: "today 18:00" has passed; Monday reads as Tomorrow.
        let presets = snooze_presets(at(2026, 11, 8, 20, 0), None);
        let labels: Vec<&str> = presets.iter().map(|p| p.label.as_str()).collect();
        assert_eq!(labels, ["In 1 hour", "In 3 hours", "Tomorrow"]);
        let custom = json!([{"kind": "after", "minutes": 60}, {"kind": "at", "day": "today", "time": "11:00"}]);
        assert_eq!(snooze_presets(at(2026, 11, 4, 10, 0), Some(&custom)).len(), 1);
    }

    #[test]
    fn hosted_links_escape_everything_but_ids_safe_characters() {
        assert_eq!(hosted_thread_link("env-1", "thread-1"), "https://app.t3.codes/threads/env-1/thread-1");
        assert_eq!(
            hosted_thread_link("e", "thread:provider:codex.x"),
            "https://app.t3.codes/threads/e/thread%3Aprovider%3Acodex%2Ex"
        );
    }
}
