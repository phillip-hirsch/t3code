//! Settings › Threads (`ThreadsSettingsPanel` in `apps/web/src/components/settings/SettingsPanels.tsx`,
//! `ProjectDefaultsSettings category="general"`, `SnoozePresetsSettings.tsx`,
//! `ThreadTitleInstructionsSettings.tsx`).
//!
//! Every row writes through the settings scope. The web hides Snooze at project scope.

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
use serde_json::json;

use super::super::fields::resolve_pasted_path;
use super::super::model_select::{generates_text, muted, resolve_text_generation, trigger};
use super::super::widgets::{SettingRow, SettingsView, settings_section, switch};
use crate::app::Workspace;
use crate::model::ModelSelection;
use crate::theme::tokens;

#[path = "snooze.rs"]
mod snooze;

use snooze::{KindMemory, Preset, Unit};

const STREAMING: &[(&str, &str)] = &[("turn", "Wait for the full response"), ("paragraph", "Show finished paragraphs")];

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
    pub(super) fn render_threads_settings(
        &mut self,
        view: &SettingsView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = *tokens(cx);
        let connected = self.settings_client().is_some();
        let base = self.add_project_field(view, window, cx);
        let new_threads = self.new_threads_section(view, true, window, cx);
        let snooze = (!view.scope.is_project()).then(|| self.snooze_row(view, window, cx));
        let auto_settle = connected.then(|| self.auto_settle_rows(view, window, cx));
        let text_generation = self.text_generation_rows(view, window, cx);
        let mut row = |this: &mut Self, row: SettingRow| this.setting_row(view, row, window, cx);
        let new_projects = [
            row(
                self,
                SettingRow::new("add-project-starts-in")
                    .description("Leave empty to use \"~/\" when the Add Project browser opens.")
                    .setting("addProjectBaseDirectory")
                    .control(base),
            ),
            row(
                self,
                SettingRow::new("ghq-clone-layout")
                    .description("Clones go to github.com/owner/repo under the folder above, like ghq.")
                    .setting("ghqCloneLayout")
                    .switch(),
            ),
        ];
        let usage = [
            row(
                self,
                SettingRow::new("auto-resume-limited-threads")
                    .description("Automatically resume threads when their usage limits reset. You can cancel auto-resume in each thread.")
                    .setting("autoResumeLimitedThreads")
                    .switch(),
            ),
            row(
                self,
                SettingRow::new("snooze-limited-threads")
                    .description("Snooze threads until their usage limits reset. Enable auto-resume to continue the agent after the reset.")
                    .setting("snoozeLimitedThreads")
                    .switch(),
            ),
        ];
        let mut sections = vec![settings_section("new-projects", "New projects", new_projects, &t), new_threads];
        sections.extend(snooze.map(|snooze| settings_section("snooze", "Snooze", [snooze], &t)));
        sections.push(settings_section("usage-limits", "Usage limits", usage, &t));
        sections.extend(auto_settle.map(|rows| settings_section("auto-settle", "Auto-settle", rows, &t)));
        sections.push(settings_section("text-generation", "Text generation", text_generation, &t));
        self.settings_page("threads", view, sections, cx)
    }

    /// The folder box; a pasted full path replaces what is there (`resolvePastedPath`).
    fn add_project_field(&mut self, view: &SettingsView, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let saved = view.str("addProjectBaseDirectory").to_owned();
        self.text_field(
            "add-project-starts-in",
            &saved,
            "~/",
            false,
            Some(resolve_pasted_path),
            window,
            cx,
            |this, text, window, cx| this.update_settings(json!({ "addProjectBaseDirectory": text }), window, cx),
        )
        .sized(px(288.))
        .into_any_element()
    }

    /// `ProjectDefaultsSettings`: the "New threads" section of Threads (`category="general"`, with
    /// Permissions) and of Projects (`category="project"`). Every row writes through the scope;
    /// the fallback model is the environment's alone.
    pub(in crate::app) fn new_threads_section(
        &mut self,
        view: &SettingsView,
        with_permissions: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = *tokens(cx);
        let project_scope = view.scope.is_project();
        let mixed = view.mixed(&["defaultModelSelection"]);
        let automatic =
            !view.targets.is_empty() && view.targets.iter().all(|t| t.settings.get("defaultModelSelection").is_null());
        let pick = |key: &'static str| {
            move |this: &mut Self, pick: ModelSelection, window: &mut Window, cx: &mut Context<Self>| {
                this.update_settings(json!({ key: pick.to_json() }), window, cx);
            }
        };
        let stored: Option<ModelSelection> = serde::Deserialize::deserialize(view.value("defaultModelSelection")).ok();
        let selection = stored
            .filter(|s| {
                self.settings_info().provider(&s.instance_id).is_some_and(crate::model::Provider::is_selectable)
            })
            .or_else(|| self.settings_info().automatic_model());
        let model: AnyElement = match &selection {
            Some(selection) => h_flex()
                .gap(px(6.))
                .child(self.model_picker(
                    "default-model",
                    Some(selection),
                    mixed.then_some(if automatic { "Automatic (mixed)" } else { "Mixed" }),
                    |_| true,
                    cx,
                    pick("defaultModelSelection"),
                ))
                .children(if mixed {
                    None
                } else {
                    self.traits_picker("default-model-traits", selection, cx, pick("defaultModelSelection"))
                })
                .into_any_element(),
            None => muted(if mixed { "Mixed" } else { "No providers available" }, &t).into_any_element(),
        };
        let model_status = mixed.then(|| {
            let differ = if automatic { "Automatic defaults differ." } else { "Defaults differ." };
            format!("{differ} Select one environment or choose a shared model to edit effort.")
        });
        let mut rows = vec![
            self.setting_row(
                view,
                SettingRow::titled("default-model", "Model")
                    .description(if project_scope {
                        "Model for new threads in this project."
                    } else if automatic {
                        "Default model for new threads, selected automatically. Projects can override it."
                    } else {
                        "Default model for new threads. Projects can override it."
                    })
                    .status(model_status)
                    .setting("defaultModelSelection")
                    .control(model),
                window,
                cx,
            ),
        ];
        if !project_scope {
            let fallback = self.fallback_model_control(view, selection.as_ref(), cx);
            rows.push(
                self.setting_row(
                    view,
                    SettingRow::new("fallback-model")
                        .description("Used for new threads when the default model's provider is out of usage.")
                        .setting("fallbackModelSelection")
                        .control(fallback),
                    window,
                    cx,
                ),
            );
        }
        if with_permissions {
            let permissions = self.permissions_control(view, cx);
            rows.push(
                self.setting_row(
                    view,
                    SettingRow::new("default-permissions")
                        .description(if project_scope {
                            "Permissions for new threads in this project."
                        } else {
                            "Default permissions for new threads. Projects can override them."
                        })
                        .setting("defaultRuntimeMode")
                        .control(permissions),
                    window,
                    cx,
                ),
            );
        }
        let mixed_mode = view.mixed(&["defaultThreadEnvMode"]);
        let mode = view.str("defaultThreadEnvMode");
        let mode = if mode.is_empty() { "local" } else { mode };
        let workspace = self.value_select(
            "default-workspace",
            "defaultThreadEnvMode",
            &if mixed_mode { serde_json::Value::Null } else { json!(mode) },
            ["local", "worktree"].map(|mode| (json!(mode), env_mode_label(mode).into())).to_vec(),
            if mixed_mode { "Mixed" } else { env_mode_label(mode) },
            0.,
            cx,
        );
        rows.push(
            self.setting_row(
                view,
                SettingRow::titled("new-threads", "Workspace")
                    .description(if project_scope {
                        "Where new threads in this project start."
                    } else {
                        "Where new threads start. Projects and their t3.json can override it."
                    })
                    .setting("defaultThreadEnvMode")
                    .control(workspace),
                window,
                cx,
            ),
        );
        settings_section("new-threads", "New threads", rows, &t)
    }

    /// The fallback picker: the stored model, else the default shown as "None".
    fn fallback_model_control(
        &mut self,
        view: &SettingsView,
        default: Option<&ModelSelection>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = *tokens(cx);
        let mixed = view.mixed(&["fallbackModelSelection"]);
        let pick = |this: &mut Self, pick: ModelSelection, window: &mut Window, cx: &mut Context<Self>| {
            this.update_settings(json!({ "fallbackModelSelection": pick.to_json() }), window, cx);
        };
        let stored: Option<ModelSelection> = serde::Deserialize::deserialize(view.value("fallbackModelSelection")).ok();
        let Some(shown) = stored.as_ref().or(default) else {
            return muted("No providers available", &t).into_any_element();
        };
        let label = if mixed { Some("Mixed") } else { stored.is_none().then_some("None") };
        h_flex()
            .gap(px(6.))
            .child(self.model_picker("fallback-model", Some(shown), label, |_| true, cx, pick))
            .children(
                stored
                    .as_ref()
                    .filter(|_| !mixed)
                    .and_then(|stored| self.traits_picker("fallback-model-traits", stored, cx, pick)),
            )
            .into_any_element()
    }

    fn permissions_control(&mut self, view: &SettingsView, cx: &mut Context<Self>) -> AnyElement {
        let t = *tokens(cx);
        let mixed = view.mixed(&["defaultRuntimeMode"]);
        let current = view.str("defaultRuntimeMode");
        let current = if mixed {
            String::new()
        } else if current.is_empty() {
            DEFAULT_RUNTIME_MODE.to_owned()
        } else {
            current.to_owned()
        };
        let (_, label, _, icon) =
            RUNTIME_MODES.iter().find(|(id, ..)| *id == current).copied().unwrap_or(RUNTIME_MODES[3]);
        let view = cx.entity();
        trigger("default-permissions", &t)
            .children((!mixed).then(|| Icon::new(icon).with_size(px(14.)).text_color(t.muted_foreground)))
            .child(div().flex_1().child(if mixed { "Mixed" } else { label }))
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
                        .checked(mode == current)
                        .on_click(window.listener_for(&view, move |this, _, window, cx| {
                            this.update_settings(json!({ "defaultRuntimeMode": mode }), window, cx);
                        })),
                    );
                }
                menu
            })
            .into_any_element()
    }

    fn auto_settle_rows(
        &mut self,
        view: &SettingsView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let t = *tokens(cx);
        let days = view.value("sidebarAutoSettleAfterDays").as_i64();
        let toggle = switch(&"auto-settle-inactive-threads".into(), days.is_some(), false, &t).on_click(cx.listener(
            move |this, _, window, cx| {
                let days = days.is_none().then_some(AUTO_SETTLE_DEFAULT_DAYS);
                this.update_settings(json!({ "sidebarAutoSettleAfterDays": days }), window, cx);
            },
        ));
        let field = days.map(|days| {
            let field =
                self.number_field("auto-settle-days", days, (1, 90, 1), window, cx, |this, days, window, cx| {
                    this.update_settings(json!({ "sidebarAutoSettleAfterDays": days }), window, cx);
                });
            div().w(px(96.)).child(NumberInput::new(&field).small())
        });
        let keep = self.settings_info().capability("threadAutoSettlementScope");
        let mut row = |this: &mut Self, row: SettingRow| this.setting_row(view, row, window, cx);
        let mut rows = vec![
            row(
                self,
                SettingRow::new("auto-settle-merged-threads")
                    .description("Settle a thread when its pull request merges. Closed pull requests still settle automatically.")
                    .setting("sidebarAutoSettleOnMerge")
                    .switch(),
            ),
            row(
                self,
                SettingRow::new("auto-settle-inactive-threads")
                    .description("Sidebar threads with no activity for this long settle automatically.")
                    .setting("sidebarAutoSettleAfterDays")
                    .control(toggle),
            ),
        ];
        if let Some(field) = field {
            rows.push(row(
                self,
                SettingRow::new("days-before-auto-settle")
                    .description("Any new activity un-settles a thread automatically.")
                    .control(field),
            ));
            if keep {
                rows.push(row(
                    self,
                    SettingRow::titled("keep-threads-with-pr", "Keep threads with a PR")
                        .description(
                            "Inactivity does not settle threads with a pull request. Merge and close still do.",
                        )
                        .setting("sidebarAutoSettleKeepPullRequests")
                        .switch(),
                ));
            }
        }
        rows
    }

    fn text_generation_rows(
        &mut self,
        view: &SettingsView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let t = *tokens(cx);
        let stored: Option<ModelSelection> =
            serde::Deserialize::deserialize(view.value("textGenerationModelSelection")).ok();
        let selection = resolve_text_generation(stored.as_ref(), &self.settings_info().providers);
        let connected = self.settings_client().is_some();
        let save = |this: &mut Self, pick: ModelSelection, window: &mut Window, cx: &mut Context<Self>| {
            let resolved = resolve_text_generation(Some(&pick), &this.settings_info().providers).unwrap_or(pick);
            this.update_settings(json!({ "textGenerationModelSelection": resolved.to_json() }), window, cx);
        };
        let control: AnyElement = match (&selection, connected) {
            (_, false) => muted("Connect an environment to choose its text generation model.", &t).into_any_element(),
            (None, true) => muted("No text generation providers available.", &t).into_any_element(),
            (Some(selection), true) => h_flex()
                .gap(px(6.))
                .child(self.model_picker("text-generation-model", Some(selection), None, generates_text, cx, save))
                .children(self.traits_picker("text-generation-traits", selection, cx, save))
                .into_any_element(),
        };
        let streaming = view.str("responseStreamingMode").to_owned();
        let mut row = |this: &mut Self, row: SettingRow| this.setting_row(view, row, window, cx);
        let model = row(
            self,
            SettingRow::new("text-generation-model")
                .description("Used for thread titles and other generated text on connected devices with this provider. Source control can override it.")
                .setting("textGenerationModelSelection")
                .control(control),
        );
        let instructions = row(
            self,
            SettingRow::new("thread-title-instructions")
                .description("Appended to the prompt for new and regenerated thread titles.")
                .setting("threadTitleInstructions")
                .textarea("Keep titles to a few words. Lead with the main subject."),
        );
        let streaming = row(
            self,
            SettingRow::new("response-streaming")
                .description(if streaming == "turn" {
                    "Text appears once the agent finishes its turn."
                } else {
                    "Each paragraph or code block appears as soon as it is complete."
                })
                .setting("responseStreamingMode")
                .select(STREAMING, 224.),
        );
        vec![model, self.advanced_fold("threads", [instructions, streaming], cx)]
    }

    fn snooze_row(&mut self, view: &SettingsView, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = *tokens(cx);
        let saved = snooze::saved_presets(view.value("snoozePresets"));
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
        let full = presets.len() + snooze::unreadable_count(view.value("snoozePresets")) >= snooze::MAX_PRESETS;
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
                            .on_click(cx.listener(move |this, _, window, cx| {
                                let mut rows = add_editor.read(cx).rows.clone();
                                let presets: Vec<Preset> = rows.iter().map(|(_, p)| p.clone()).collect();
                                let key = add_editor.update(cx, |e, _| e.fresh_keys(1))[0];
                                rows.push((key, snooze::next_candidate(&presets)));
                                this.save_snooze(&add_editor, rows, window, cx);
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
        let row = SettingRow::new("snooze-options")
            .description("Choices in every snooze menu, listed soonest first. Weekdays mean the next one after today.")
            .setting("snoozePresets")
            .body(body);
        self.setting_row(view, row, window, cx)
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
        let when_options =
            snooze::WHEN.iter().map(|(id, label)| (SharedString::from(*id), (*label).into(), *id == when)).collect();
        let when_select = self.menu_select(
            format!("snooze-when-{key}"),
            when_options,
            when_label,
            0.,
            cx,
            move |this, picked: SharedString, window, cx| {
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
                this.replace_snooze(&when_editor, key, next, window, cx);
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
                    move |this, text, window, cx| {
                        let Some(Preset::At { day, .. }) =
                            time_editor.read(cx).rows.iter().find(|(k, _)| *k == key).map(|(_, p)| p.clone())
                        else {
                            return;
                        };
                        if snooze::parse_time(&text).is_some() {
                            this.replace_snooze(&time_editor, key, Preset::At { day, time: text }, window, cx);
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
                        .on_click(cx.listener(move |this, _, window, cx| {
                            let rows: Vec<(u64, Preset)> =
                                remove_editor.read(cx).rows.iter().filter(|(k, _)| *k != key).cloned().collect();
                            this.save_snooze(&remove_editor, rows, window, cx);
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
        let field = self.number_field(
            &format!("snooze-amount-{key}"),
            amount,
            (1, max, 1),
            window,
            cx,
            move |this, amount, window, cx| {
                this.replace_snooze(
                    &amount_editor,
                    key,
                    Preset::After { minutes: amount * unit.minutes() },
                    window,
                    cx,
                );
            },
        );
        let unit_editor = editor.clone();
        let unit_options = Unit::ALL.iter().map(|u| (*u, u.label(amount).into(), *u == unit)).collect();
        let unit_select = self.menu_select(
            format!("snooze-unit-{key}"),
            unit_options,
            unit.label(amount),
            0.,
            cx,
            move |this, next: Unit, window, cx| {
                unit_editor.update(cx, |e, _| e.units.insert(key, next));
                let minutes = (amount * next.minutes()).min(snooze::MAX_MINUTES);
                this.replace_snooze(&unit_editor, key, Preset::After { minutes }, window, cx);
            },
        );
        h_flex()
            .gap(px(8.))
            .child(div().w(px(128.)).child(NumberInput::new(&field).small()))
            .child(div().w(px(144.)).child(unit_select))
            .into_any_element()
    }

    fn replace_snooze(
        &mut self,
        editor: &Entity<SnoozeEditor>,
        key: u64,
        next: Preset,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let rows =
            editor.read(cx).rows.iter().map(|(k, p)| (*k, if *k == key { next.clone() } else { p.clone() })).collect();
        self.save_snooze(editor, rows, window, cx);
    }

    fn save_snooze(
        &mut self,
        editor: &Entity<SnoozeEditor>,
        rows: Vec<(u64, Preset)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
        let list = snooze::write_value(self.settings_view().value("snoozePresets"), &presets);
        self.update_settings_then(json!({ "snoozePresets": list }), window, cx, move |_, ok, cx| {
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
