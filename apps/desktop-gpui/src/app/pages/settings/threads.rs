//! Settings › Threads (`ThreadsSettingsPanel` in `apps/web/src/components/settings/SettingsPanels.tsx`,
//! `ProjectDefaultsSettings category="general"`, `SnoozePresetsSettings.tsx`,
//! `ThreadTitleInstructionsSettings.tsx`).
//!
//! Scoped in the web (`serverScoped`, written to every selected environment or project): every row
//! here. The web hides Snooze at project scope; Model, Permissions and Workspace reword their
//! descriptions there. This port edits the connected environment's settings only.

use std::collections::HashMap;

use chrono::Local;
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::NumberInput;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{Disableable as _, Icon, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, px,
};
use serde_json::{Value, json};

use super::super::code::pending::{
    Control, Row, SERVER_SCOPES, SettingRowSpec, advanced, generates_text, muted, page, patch, reset_button,
    resolve_pasted_path, resolve_text_generation, section, select, switch, trigger,
};
use crate::app::Workspace;
use crate::model::ModelSelection;
use crate::theme::tokens;

#[path = "snooze.rs"]
mod snooze;

use snooze::{KindMemory, Preset, Unit};

const STREAMING: &[(&str, &str)] = &[("turn", "Wait for the full response"), ("paragraph", "Show finished paragraphs")];

const ROWS: &[SettingRowSpec] = &[
    SettingRowSpec {
        id: "ghq-clone-layout",
        label: "Clone into host folders",
        description: "Clones go to github.com/owner/repo under the folder above, like ghq.",
        path: "ghqCloneLayout",
        control: Control::Switch,
        default: Some("false"),
        scopes: SERVER_SCOPES,
    },
    SettingRowSpec {
        id: "auto-resume-limited-threads",
        label: "Resume after usage limits",
        description: "Automatically resume threads when their usage limits reset. You can cancel auto-resume in each thread.",
        path: "autoResumeLimitedThreads",
        control: Control::Switch,
        default: None,
        scopes: SERVER_SCOPES,
    },
    SettingRowSpec {
        id: "snooze-limited-threads",
        label: "Snooze until limits reset",
        description: "Snooze threads until their usage limits reset. Enable auto-resume to continue the agent after the reset.",
        path: "snoozeLimitedThreads",
        control: Control::Switch,
        default: None,
        scopes: SERVER_SCOPES,
    },
    SettingRowSpec {
        id: "auto-settle-merged-threads",
        label: "Auto-settle merged threads",
        description: "Settle a thread when its pull request merges. Closed pull requests still settle automatically.",
        path: "sidebarAutoSettleOnMerge",
        control: Control::Switch,
        default: Some("true"),
        scopes: SERVER_SCOPES,
    },
    SettingRowSpec {
        id: "keep-threads-with-pr",
        label: "Keep threads with a PR",
        description: "Inactivity does not settle threads with a pull request. Merge and close still do.",
        path: "sidebarAutoSettleKeepPullRequests",
        control: Control::Switch,
        default: None,
        scopes: SERVER_SCOPES,
    },
    SettingRowSpec {
        id: "thread-title-instructions",
        label: "Thread title instructions",
        description: "Appended to the prompt for new and regenerated thread titles.",
        path: "threadTitleInstructions",
        control: Control::Textarea("Keep titles to a few words. Lead with the main subject."),
        default: Some("\"\""),
        scopes: SERVER_SCOPES,
    },
];

fn spec(id: &str) -> &'static SettingRowSpec {
    ROWS.iter().find(|row| row.id == id).expect("settings row")
}

/// `runtimeModeConfig.ts`, in menu order.
const RUNTIME_MODES: [(&str, &str, &str, Lucide); 4] = [
    ("approval-required", "Supervised", "Ask before commands and file changes.", Lucide::Lock),
    ("auto-accept-edits", "Auto-accept edits", "Auto-approve edits, ask before other actions.", Lucide::PenLine),
    ("auto", "Auto", "Supported providers approve routine actions; others still ask.", Lucide::Sparkles),
    ("full-access", "Full access", "Allow commands and edits without prompts.", Lucide::ShieldOff),
];
const DEFAULT_RUNTIME_MODE: &str = "full-access";
const AUTO_SETTLE_DEFAULT_DAYS: i64 = 3;

/// The snooze editor's draft. Rows keep a key across edits and sorts so a row's own fields
/// stay with it; edits build on the draft so a second edit before the first echoes back
/// does not undo the first.
#[derive(Default)]
struct SnoozeEditor {
    rows: Vec<(u64, Preset)>,
    next_key: u64,
    /// `None` forces the next render to adopt the saved list, as after a failed write.
    saved_source: Option<Vec<Preset>>,
    written: Vec<Vec<Preset>>,
    memory: HashMap<u64, KindMemory>,
    units: HashMap<u64, Unit>,
}

impl SnoozeEditor {
    fn fresh_keys(&mut self, count: usize) -> Vec<u64> {
        let start = self.next_key;
        self.next_key += count as u64;
        (start..self.next_key).collect()
    }

    fn adopt(&mut self, saved: &[Preset]) {
        if self.saved_source.as_deref() == Some(saved) {
            return;
        }
        self.saved_source = Some(saved.to_vec());
        match snooze::reconcile(&self.written, saved) {
            Some(pending) => self.written = pending,
            None => {
                self.written.clear();
                let keys = self.fresh_keys(saved.len());
                self.rows = keys.into_iter().zip(saved.iter().cloned()).collect();
                snooze::sort_rows(&mut self.rows, &Local::now());
            }
        }
    }

    fn presets(&self) -> Vec<Preset> {
        self.rows.iter().map(|(_, preset)| preset.clone()).collect()
    }
}

impl Workspace {
    pub(super) fn render_threads_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = *tokens(cx);
        let connected = self.client().is_some();
        let new_projects =
            [self.add_project_row(window, cx), self.render_spec(spec("ghq-clone-layout"), window, cx).render(&t)];
        let usage = ["auto-resume-limited-threads", "snooze-limited-threads"]
            .map(|id| self.render_spec(spec(id), window, cx).render(&t));
        let mut sections = vec![
            section("New projects", None, new_projects, &t),
            section("New threads", None, self.settings_new_thread_rows(cx), &t),
            section("Snooze", None, [self.snooze_row(window, cx)], &t),
            section("Usage limits", None, usage, &t),
        ];
        if connected {
            sections.push(section("Auto-settle", None, self.auto_settle_rows(window, cx), &t));
        }
        sections.push(section("Text generation", None, self.text_generation_rows(window, cx), &t));
        page("settings-threads", sections)
    }

    fn add_project_row(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = *tokens(cx);
        let saved = self.setting("addProjectBaseDirectory").as_str().unwrap_or_default().to_owned();
        let field = self.text_field(
            "add-project-starts-in",
            &saved,
            "~/",
            false,
            Some(resolve_pasted_path),
            window,
            cx,
            |this, text, cx| this.pending_update_settings(json!({ "addProjectBaseDirectory": text }), cx),
        );
        let reset = (!saved.is_empty()).then(|| {
            reset_button(
                "add-project-starts-in",
                cx.listener(|this, _, _, cx| {
                    this.pending_update_settings(json!({ "addProjectBaseDirectory": "" }), cx)
                }),
            )
        });
        Row::new("add-project-starts-in", "Add project starts in")
            .description("Leave empty to use \"~/\" when the Add Project browser opens.")
            .reset(reset)
            .control(field.sized(px(288.)))
            .render(&t)
    }

    /// `ProjectDefaultsSettings category="general"`: Model, Fallback model, Permissions, Workspace.
    fn settings_new_thread_rows(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let t = *tokens(cx);
        let stored = self.setting("defaultModelSelection").clone();
        let automatic = stored.is_null();
        let selection = self.server_info.new_thread_model(None);
        let picked = |this: &mut Self, value: Value, cx: &mut Context<Self>| {
            this.pending_update_settings(json!({ "defaultModelSelection": value }), cx)
        };
        let model_control: AnyElement = match &selection {
            Some(selection) if self.server_info.provider(&selection.instance_id).is_some() => h_flex()
                .gap(px(6.))
                .child(self.model_picker(
                    "default-model",
                    selection,
                    None,
                    |_| true,
                    cx,
                    move |this, pick, cx| picked(this, pick.to_json(), cx),
                ))
                .children(self.traits_picker("default-model-traits", selection, cx, move |this, pick, cx| {
                    picked(this, pick.to_json(), cx)
                }))
                .into_any_element(),
            _ => muted("No providers available", &t).into_any_element(),
        };
        let model = Row::new("default-model", "Model")
            .description(if self.client().is_some() && automatic {
                "Default model for new threads, selected automatically. Projects can override it."
            } else {
                "Default model for new threads. Projects can override it."
            })
            .reset((!automatic).then(|| {
                reset_button("default-model", cx.listener(move |this, _, _, cx| picked(this, Value::Null, cx)))
            }))
            .control(model_control);

        let fallback_stored: Option<ModelSelection> =
            serde_json::from_value(self.setting("fallbackModelSelection").clone()).ok();
        let fallback_selection = fallback_stored.clone().or(selection);
        let set_fallback = |this: &mut Self, value: Value, cx: &mut Context<Self>| {
            this.pending_update_settings(json!({ "fallbackModelSelection": value }), cx)
        };
        let fallback_control: AnyElement = match &fallback_selection {
            Some(selection) => h_flex()
                .gap(px(6.))
                .child(self.model_picker(
                    "fallback-model",
                    selection,
                    fallback_stored.is_none().then_some("None"),
                    |_| true,
                    cx,
                    move |this, pick, cx| set_fallback(this, pick.to_json(), cx),
                ))
                .children(fallback_stored.as_ref().and_then(|stored| {
                    self.traits_picker("fallback-model-traits", stored, cx, move |this, pick, cx| {
                        set_fallback(this, pick.to_json(), cx)
                    })
                }))
                .into_any_element(),
            None => muted("No providers available", &t).into_any_element(),
        };
        let fallback = Row::new("fallback-model", "Fallback model")
            .description("Used for new threads when the default model's provider is out of usage.")
            .reset(fallback_stored.is_some().then(|| {
                reset_button("fallback-model", cx.listener(move |this, _, _, cx| set_fallback(this, Value::Null, cx)))
            }))
            .control(fallback_control);

        let workspace_mode = self.setting("defaultThreadEnvMode").as_str().map(str::to_owned);
        let effective = workspace_mode.clone().unwrap_or_else(|| "local".into());
        let workspace = Row::new("new-threads", "Workspace")
            .description("Where new threads start. Projects and their t3.json can override it.")
            .reset(workspace_mode.is_some().then(|| {
                reset_button(
                    "default-workspace",
                    cx.listener(|this, _, _, cx| {
                        this.pending_update_settings(json!({ "defaultThreadEnvMode": null }), cx)
                    }),
                )
            }))
            .control(select(
                "default-workspace",
                env_mode_label(&effective),
                ["local", "worktree"].map(|mode| (mode.into(), env_mode_label(mode).into())).to_vec(),
                Some(effective.into()),
                cx,
                |this, mode, cx| this.pending_update_settings(json!({ "defaultThreadEnvMode": mode.as_ref() }), cx),
            ));

        vec![model.render(&t), fallback.render(&t), self.permissions_row(cx).render(&t), workspace.render(&t)]
    }

    fn permissions_row(&mut self, cx: &mut Context<Self>) -> Row {
        let t = *tokens(cx);
        let current = self.setting("defaultRuntimeMode").as_str().unwrap_or(DEFAULT_RUNTIME_MODE).to_owned();
        let (_, label, _, icon) =
            RUNTIME_MODES.iter().find(|(id, ..)| *id == current).copied().unwrap_or(RUNTIME_MODES[3]);
        let view = cx.entity();
        let menu_current = current.clone();
        let trigger = trigger("default-permissions", &t)
            .child(Icon::new(icon).with_size(px(14.)).text_color(t.muted_foreground))
            .child(div().flex_1().child(label))
            .child(Icon::new(Lucide::ChevronDown).with_size(px(14.)).opacity(0.7))
            .dropdown_menu_with_anchor(gpui_kit::Anchor::TopRight, move |mut menu, window, _| {
                for (mode, label, description, icon) in RUNTIME_MODES {
                    menu = menu.item(
                        PopupMenuItem::element(move |_, cx| {
                            let t = tokens(cx);
                            v_flex()
                                .min_w(px(256.))
                                .gap(px(2.))
                                .child(
                                    h_flex()
                                        .gap(px(6.))
                                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                                        .child(Icon::new(icon).with_size(px(14.)).text_color(t.muted_foreground))
                                        .child(label),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .line_height(px(16.))
                                        .text_color(t.muted_foreground)
                                        .child(description),
                                )
                        })
                        .checked(mode == menu_current)
                        .on_click(window.listener_for(&view, move |this, _, _, cx| {
                            this.pending_update_settings(json!({ "defaultRuntimeMode": mode }), cx)
                        })),
                    );
                }
                menu
            });
        Row::new("default-permissions", "Permissions")
            .description("Default permissions for new threads. Projects can override them.")
            .reset((current != DEFAULT_RUNTIME_MODE).then(|| {
                reset_button(
                    "default-permissions",
                    cx.listener(|this, _, _, cx| {
                        this.pending_update_settings(json!({ "defaultRuntimeMode": DEFAULT_RUNTIME_MODE }), cx)
                    }),
                )
            }))
            .control(trigger)
    }

    fn auto_settle_rows(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let t = *tokens(cx);
        let days = self.setting("sidebarAutoSettleAfterDays").as_i64();
        let mut rows = vec![self.render_spec(spec("auto-settle-merged-threads"), window, cx).render(&t)];
        rows.push(
            Row::new("auto-settle-inactive-threads", "Auto-settle inactive threads")
                .description("Sidebar threads with no activity for this long settle automatically.")
                .reset((days != Some(AUTO_SETTLE_DEFAULT_DAYS)).then(|| {
                    reset_button(
                        "auto-settle",
                        cx.listener(|this, _, _, cx| {
                            this.pending_update_settings(
                                json!({ "sidebarAutoSettleAfterDays": AUTO_SETTLE_DEFAULT_DAYS }),
                                cx,
                            )
                        }),
                    )
                }))
                .control(switch("auto-settle-inactive-threads", days.is_some(), cx, |this, on, cx| {
                    let days = on.then_some(AUTO_SETTLE_DEFAULT_DAYS);
                    this.pending_update_settings(json!({ "sidebarAutoSettleAfterDays": days }), cx)
                }))
                .render(&t),
        );
        if let Some(days) = days {
            let field = self.number_field("auto-settle-days", days, 1, 90, window, cx, |this, days, cx| {
                this.pending_update_settings(json!({ "sidebarAutoSettleAfterDays": days }), cx)
            });
            rows.push(
                Row::new("days-before-auto-settle", "Days of inactivity before auto-settle")
                    .description("Any new activity un-settles a thread automatically.")
                    .control(div().w(px(96.)).child(NumberInput::new(&field).small()))
                    .render(&t),
            );
            if self.server_info.capabilities["threadAutoSettlementScope"] == true {
                rows.push(self.render_spec(spec("keep-threads-with-pr"), window, cx).render(&t));
            }
        }
        rows
    }

    fn text_generation_rows(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let t = *tokens(cx);
        let stored: Option<ModelSelection> =
            serde_json::from_value(self.setting("textGenerationModelSelection").clone()).ok();
        let selection = resolve_text_generation(stored.as_ref(), &self.server_info.providers);
        let dirty = stored.as_ref().map(ModelSelection::to_json) != Some(default_text_generation());
        let connected = self.client().is_some();
        let save = |this: &mut Self, pick: ModelSelection, cx: &mut Context<Self>| {
            let resolved = resolve_text_generation(Some(&pick), &this.server_info.providers).unwrap_or(pick);
            this.pending_update_settings(json!({ "textGenerationModelSelection": resolved.to_json() }), cx)
        };
        let control: AnyElement = match (&selection, connected) {
            (_, false) => muted("Connect an environment to choose its text generation model.", &t).into_any_element(),
            (None, true) => muted("No text generation providers available.", &t).into_any_element(),
            (Some(selection), true) => h_flex()
                .gap(px(6.))
                .child(self.model_picker("text-generation-model", selection, None, generates_text, cx, save))
                .children(self.traits_picker("text-generation-traits", selection, cx, save))
                .into_any_element(),
        };
        let model = Row::new("text-generation-model", "Text generation model")
            .description("Used for thread titles and other generated text on connected devices with this provider. Source control can override it.")
            .reset((connected && dirty).then(|| {
                reset_button("text-generation-model", cx.listener(|this, _, _, cx| {
                    this.pending_update_settings(json!({ "textGenerationModelSelection": default_text_generation() }), cx)
                }))
            }))
            .control(control)
            .render(&t);

        let streaming = self.setting("responseStreamingMode").as_str().unwrap_or("paragraph").to_owned();
        let streaming_row = Row::new("response-streaming", "Response streaming")
            .description(if streaming == "turn" {
                "Text appears once the agent finishes its turn."
            } else {
                "Each paragraph or code block appears as soon as it is complete."
            })
            .reset((streaming != "paragraph").then(|| {
                reset_button(
                    "response-streaming",
                    cx.listener(|this, _, _, cx| {
                        this.pending_update_settings(json!({ "responseStreamingMode": "paragraph" }), cx)
                    }),
                )
            }))
            .control(select(
                "response-streaming",
                STREAMING.iter().find(|(id, _)| *id == streaming).map_or("", |(_, label)| label),
                STREAMING.iter().map(|(id, label)| ((*id).into(), (*label).into())).collect(),
                Some(streaming.into()),
                cx,
                |this, mode, cx| this.pending_update_settings(json!({ "responseStreamingMode": mode.as_ref() }), cx),
            ));
        let open = window.use_keyed_state("settings-threads-advanced", cx, |_, _| false);
        let rows = if *open.read(cx) {
            vec![self.render_spec(spec("thread-title-instructions"), window, cx).render(&t), streaming_row.render(&t)]
        } else {
            Vec::new()
        };
        let fold = advanced("threads-advanced", *open.read(cx), rows, &t, move |_, _, cx| {
            open.update(cx, |open, cx| {
                *open = !*open;
                cx.notify();
            })
        });
        vec![model, fold]
    }

    fn snooze_row(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = *tokens(cx);
        let saved: Vec<Preset> =
            serde_json::from_value(self.setting("snoozePresets").clone()).unwrap_or_else(|_| snooze::defaults());
        let editor = window.use_keyed_state("settings-snooze-editor", cx, |_, _| SnoozeEditor::default());
        editor.update(cx, |editor, _| editor.adopt(&saved));
        let presets = editor.read(cx).presets();
        let now = Local::now();
        let previews = snooze::previews(&presets, &now);
        let keys: Vec<u64> = editor.read(cx).rows.iter().map(|(key, _)| *key).collect();
        let mut list = Vec::new();
        for ((key, preset), preview) in keys.iter().copied().zip(presets.iter()).zip(previews) {
            list.push(self.snooze_preset_row(&editor, key, preset, preview, window, cx));
        }
        let reset = (!snooze::same_presets(&presets, &snooze::defaults())).then(|| {
            let editor = editor.clone();
            reset_button(
                "snooze-options",
                cx.listener(move |this, _, _, cx| {
                    let mut rows: Vec<(u64, Preset)> =
                        editor.update(cx, |e, _| e.fresh_keys(5)).into_iter().zip(snooze::defaults()).collect();
                    snooze::sort_rows(&mut rows, &Local::now());
                    this.save_snooze(&editor, rows, cx);
                }),
            )
        });
        let full = presets.len() >= snooze::MAX_PRESETS;
        let add_editor = editor.clone();
        let settle_editor = editor.clone();
        let body = v_flex()
            .id("snooze-options-list")
            .pt(px(12.))
            .pb(px(8.))
            .gap(px(6.))
            // Rows sort by wake once the pointer leaves the list, so a row never moves under it.
            .on_hover(move |hovered, _, cx| {
                if !hovered {
                    settle_editor.update(cx, |editor, cx| {
                        snooze::sort_rows(&mut editor.rows, &Local::now());
                        cx.notify();
                    });
                }
            })
            .children(list)
            .child(div().text_xs().text_color(t.muted_foreground).child(snooze::menu_order(&presets, &now)))
            .child(
                h_flex()
                    .child(
                        Button::new("snooze-add")
                            .ghost()
                            .xsmall()
                            .text_color(t.muted_foreground)
                            .icon(Icon::new(IconName::Plus).with_size(px(14.)))
                            .label("Add option")
                            .disabled(full)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let mut rows = add_editor.read(cx).rows.clone();
                                let presets: Vec<Preset> = rows.iter().map(|(_, p)| p.clone()).collect();
                                let key = add_editor.update(cx, |e, _| e.fresh_keys(1))[0];
                                rows.push((key, snooze::next_candidate(&presets)));
                                this.save_snooze(&add_editor, rows, cx);
                            })),
                    )
                    .children(full.then(|| {
                        div()
                            .pl(px(8.))
                            .text_xs()
                            .text_color(t.muted_foreground)
                            .child(format!("Up to {} options", snooze::MAX_PRESETS))
                    })),
            );
        Row::new("snooze-options", "Snooze options")
            .description("Choices in every snooze menu, listed soonest first. Weekdays mean the next one after today.")
            .reset(reset)
            .body(body)
            .render(&t)
    }

    fn snooze_preset_row(
        &mut self,
        editor: &Entity<SnoozeEditor>,
        key: u64,
        preset: &Preset,
        preview: snooze::Preview,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = *tokens(cx);
        let when = match preset {
            Preset::After { .. } => "after",
            Preset::At { day, .. } => day.as_str(),
        };
        let when_label = snooze::WHEN.iter().find(|(id, _)| *id == when).map_or("", |(_, label)| label);
        let when_editor = editor.clone();
        let when_select = select(
            &format!("snooze-when-{key}"),
            when_label,
            snooze::WHEN.iter().map(|(id, label)| ((*id).into(), (*label).into())).collect(),
            Some(when.to_owned().into()),
            cx,
            move |this, picked, cx| {
                let Some(preset) = when_editor.read(cx).rows.iter().find(|(k, _)| *k == key).map(|(_, p)| p.clone())
                else {
                    return;
                };
                let memory = when_editor.read(cx).memory.get(&key).cloned().unwrap_or_default();
                let Some(next) = snooze::with_when(&preset, &picked, &memory) else { return };
                if std::mem::discriminant(&next) != std::mem::discriminant(&preset) {
                    let memory = match &preset {
                        Preset::After { minutes } => KindMemory { minutes: *minutes, ..memory },
                        Preset::At { time, .. } => KindMemory { time: time.clone(), ..memory },
                    };
                    when_editor.update(cx, |e, _| e.memory.insert(key, memory));
                }
                this.replace_snooze(&when_editor, key, next, cx);
            },
        );
        let fields: AnyElement = match preset {
            Preset::After { minutes } => self.snooze_duration_fields(editor, key, *minutes, window, cx),
            Preset::At { time, .. } => {
                let time_editor = editor.clone();
                let field = self.text_field(
                    &format!("snooze-time-{key}"),
                    time,
                    "09:00",
                    false,
                    None,
                    window,
                    cx,
                    move |this, text, cx| {
                        let Some(Preset::At { day, .. }) =
                            time_editor.read(cx).rows.iter().find(|(k, _)| *k == key).map(|(_, p)| p.clone())
                        else {
                            return;
                        };
                        if snooze::parse_time(&text).is_some() {
                            this.replace_snooze(&time_editor, key, Preset::At { day, time: text }, cx);
                        }
                    },
                );
                h_flex()
                    .gap(px(8.))
                    .child(div().text_xs().text_color(t.muted_foreground).child("at"))
                    .child(field.sized(px(128.)))
                    .into_any_element()
            }
        };
        let preview: AnyElement = match preview {
            snooze::Preview::Menu { label, when } => h_flex()
                .min_w_0()
                .child(div().truncate().text_color(t.foreground).child(label))
                .child(div().pl(px(8.)).child(when))
                .into_any_element(),
            snooze::Preview::Muted(text) => div().truncate().child(text).into_any_element(),
        };
        let remove_editor = editor.clone();
        h_flex()
            .id(SharedString::from(format!("snooze-option-{key}")))
            .flex_wrap()
            .gap(px(8.))
            .child(h_flex().gap(px(8.)).child(div().w(px(144.)).child(when_select)).child(fields))
            .child(
                h_flex().ml_auto().min_w_0().gap(px(8.)).text_xs().text_color(t.muted_foreground).child(preview).child(
                    Button::new(SharedString::from(format!("snooze-remove-{key}")))
                        .ghost()
                        .xsmall()
                        .icon(Icon::new(IconName::Close).with_size(px(14.)))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let rows: Vec<(u64, Preset)> =
                                remove_editor.read(cx).rows.iter().filter(|(k, _)| *k != key).cloned().collect();
                            this.save_snooze(&remove_editor, rows, cx);
                        })),
                ),
            )
            .into_any_element()
    }

    /// `DurationFields`: an amount stepper and a unit. The chosen unit stays put while
    /// stepping, so 59 → 60 minutes does not jump to hours.
    fn snooze_duration_fields(
        &mut self,
        editor: &Entity<SnoozeEditor>,
        key: u64,
        minutes: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let chosen = editor.read(cx).units.get(&key).copied().unwrap_or_else(|| snooze::split_minutes(minutes).1);
        let unit = if minutes % chosen.minutes() == 0 { chosen } else { snooze::split_minutes(minutes).1 };
        let amount = minutes / unit.minutes();
        let max = snooze::MAX_MINUTES / unit.minutes();
        let amount_editor = editor.clone();
        let field =
            self.number_field(&format!("snooze-amount-{key}"), amount, 1, max, window, cx, move |this, amount, cx| {
                this.replace_snooze(&amount_editor, key, Preset::After { minutes: amount * unit.minutes() }, cx);
            });
        let unit_editor = editor.clone();
        let unit_select = select(
            &format!("snooze-unit-{key}"),
            unit.label(amount),
            Unit::ALL.iter().map(|u| (u.id().into(), u.label(amount).into())).collect(),
            Some(unit.id().into()),
            cx,
            move |this, picked, cx| {
                let Some(next) = Unit::ALL.into_iter().find(|u| u.id() == picked.as_ref()) else { return };
                unit_editor.update(cx, |e, _| e.units.insert(key, next));
                let minutes = (amount * next.minutes()).min(snooze::MAX_MINUTES);
                this.replace_snooze(&unit_editor, key, Preset::After { minutes }, cx);
            },
        );
        h_flex()
            .gap(px(8.))
            .child(div().w(px(128.)).child(NumberInput::new(&field).small()))
            .child(div().w(px(144.)).child(unit_select))
            .into_any_element()
    }

    fn replace_snooze(&mut self, editor: &Entity<SnoozeEditor>, key: u64, next: Preset, cx: &mut Context<Self>) {
        let rows =
            editor.read(cx).rows.iter().map(|(k, p)| (*k, if *k == key { next.clone() } else { p.clone() })).collect();
        self.save_snooze(editor, rows, cx);
    }

    fn save_snooze(&mut self, editor: &Entity<SnoozeEditor>, rows: Vec<(u64, Preset)>, cx: &mut Context<Self>) {
        let presets: Vec<Preset> = rows.iter().map(|(_, p)| p.clone()).collect();
        if presets == editor.read(cx).presets() {
            editor.update(cx, |e, cx| {
                e.rows = rows;
                cx.notify();
            });
            return;
        }
        editor.update(cx, |e, cx| {
            e.rows = rows;
            e.written.push(presets.clone());
            cx.notify();
        });
        let editor = editor.clone();
        self.update_settings_then(patch("snoozePresets", json!(presets)), cx, move |_, ok, cx| {
            if !ok {
                editor.update(cx, |e, cx| {
                    e.written.clear();
                    e.saved_source = None;
                    cx.notify();
                });
            }
        });
    }
}

fn env_mode_label(mode: &str) -> &'static str {
    if mode == "worktree" { "New worktree" } else { "Current checkout" }
}

/// `DEFAULT_UNIFIED_SETTINGS.textGenerationModelSelection`.
pub(super) fn default_text_generation() -> Value {
    json!({"instanceId": "codex", "model": "gpt-6-luna", "options": [{"id": "reasoningEffort", "value": "low"}]})
}
