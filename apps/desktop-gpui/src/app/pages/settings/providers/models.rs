//! An instance's models (`ProviderModelsSection.tsx`, `CustomModelEditor.tsx`,
//! `customModelEditor.logic.ts`): which ones the picker shows, favorites and order, and the
//! custom models saved in the instance's `config.customModels`.

use std::collections::HashSet;

use gpui_kit::component::button::ButtonVariants as _;
use gpui_kit::component::input::InputEvent;
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Context, Entity, InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};
use serde_json::{Value, json};

use super::instances::InstanceRow;
use super::{ProvidersPage, ghost_icon_button, ghost_text_button, outline_button, select_button, switch, text_input};
use crate::app::Workspace;
use crate::theme::{self, Tokens};

/// `MAX_CUSTOM_MODEL_LENGTH`.
const MAX_CUSTOM_MODEL_LENGTH: usize = 256;
/// More models than this get a filter field.
const FILTER_THRESHOLD: usize = 8;

/// Models UI state for the selected instance; reset when another instance is selected.
#[derive(Default)]
pub(super) struct ModelsUi {
    instance: Option<String>,
    adding: bool,
    /// Focus the add field on the next render, once.
    focus_add: bool,
    error: Option<String>,
    /// The custom model whose editor is open, and its draft.
    editing: Option<CustomModelDraft>,
    editor_error: Option<String>,
    next_key: u64,
    pub revealed_email: bool,
}

impl ModelsUi {
    fn key(&mut self) -> u64 {
        self.next_key += 1;
        self.next_key
    }
}

/// A model as the list shows it: the server's models, then saved custom ones it does not report.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct DisplayModel {
    pub slug: String,
    pub name: String,
    pub is_custom: bool,
    pub capabilities: Option<Value>,
}

/// A saved custom model (`CustomModelDefinition`); `capabilities` holds `optionDescriptors`.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct CustomModel {
    pub slug: String,
    pub name: String,
    pub capabilities: Option<Value>,
}

/// `readCustomModelEntries`: strings or `{slug, name?, capabilities?}`, deduplicated by slug.
pub(super) fn read_custom_models(value: Option<&Value>) -> Vec<CustomModel> {
    let mut entries: Vec<CustomModel> = Vec::new();
    for raw in value.and_then(Value::as_array).into_iter().flatten() {
        let (slug, name, capabilities) = match raw {
            Value::String(slug) => (Some(slug.as_str()), None, None),
            Value::Object(record) => (
                record.get("slug").and_then(Value::as_str),
                record.get("name").and_then(Value::as_str),
                record.get("capabilities").filter(|c| c.get("optionDescriptors").is_some_and(Value::is_array)),
            ),
            _ => continue,
        };
        let Some(slug) = slug.map(str::trim).filter(|s| !s.is_empty()) else { continue };
        if entries.iter().any(|entry| entry.slug == slug) {
            continue;
        }
        let name = name.map(str::trim).filter(|s| !s.is_empty()).unwrap_or(slug);
        entries.push(CustomModel {
            slug: slug.to_owned(),
            name: name.to_owned(),
            capabilities: capabilities.map(|c| json!({ "optionDescriptors": c["optionDescriptors"] })),
        });
    }
    entries
}

/// `toCustomModelSetting`: the bare slug unless it has a name or options.
pub(super) fn custom_model_setting(entry: &CustomModel) -> Value {
    let descriptors =
        entry.capabilities.as_ref().and_then(|c| c["optionDescriptors"].as_array()).filter(|d| !d.is_empty());
    let name = (entry.name != entry.slug).then_some(&entry.name);
    if name.is_none() && descriptors.is_none() {
        return json!(entry.slug);
    }
    let mut setting = json!({ "slug": entry.slug });
    if let Some(name) = name {
        setting["name"] = json!(name);
    }
    if let Some(descriptors) = descriptors {
        setting["capabilities"] = json!({ "optionDescriptors": descriptors });
    }
    setting
}

/// `deriveProviderModelsForDisplay`.
pub(super) fn models_for_display(live: &[Value], custom: &[CustomModel]) -> Vec<DisplayModel> {
    let decode = |model: &Value| DisplayModel {
        slug: model["slug"].as_str().unwrap_or_default().to_owned(),
        name: model["name"].as_str().or(model["slug"].as_str()).unwrap_or_default().to_owned(),
        is_custom: model["isCustom"] == true,
        capabilities: model.get("capabilities").filter(|c| c.is_object()).cloned(),
    };
    let server: Vec<DisplayModel> = live.iter().map(decode).filter(|m| !m.is_custom).collect();
    let live_custom: Vec<DisplayModel> = live.iter().map(decode).filter(|m| m.is_custom).collect();
    let customs =
        custom.iter().filter(|entry| !server.iter().any(|m| m.slug == entry.slug)).map(|entry| DisplayModel {
            slug: entry.slug.clone(),
            name: entry.name.clone(),
            is_custom: true,
            capabilities: entry
                .capabilities
                .clone()
                .or_else(|| live_custom.iter().find(|m| m.slug == entry.slug).and_then(|m| m.capabilities.clone())),
        });
    server.iter().cloned().chain(customs).collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Group {
    Favorite,
    Visible,
    Hidden,
}

pub(super) struct Preferences<'a> {
    pub hidden: &'a HashSet<String>,
    pub favorites: &'a HashSet<String>,
    pub order: &'a [String],
}

impl Preferences<'_> {
    pub fn group(&self, model: &DisplayModel) -> Group {
        if self.favorites.contains(&model.slug) {
            Group::Favorite
        } else if !model.is_custom && self.hidden.contains(&model.slug) {
            Group::Hidden
        } else {
            Group::Visible
        }
    }
}

/// `groupModelsForDisplay`: favorites, then shown, then hidden; each by the saved order, then as
/// reported.
pub(super) fn group_models(models: &[DisplayModel], prefs: &Preferences) -> Vec<DisplayModel> {
    let rank = |slug: &str| prefs.order.iter().position(|s| s == slug).unwrap_or(usize::MAX);
    let mut ordered: Vec<(usize, &DisplayModel)> = models.iter().enumerate().collect();
    ordered.sort_by_key(|(index, model)| (prefs.group(model) as u8, rank(&model.slug), *index));
    ordered.into_iter().map(|(_, model)| model.clone()).collect()
}

/// `nextHiddenModelsForBulkToggle`: shows every built-in model if all are hidden, else hides them.
pub(super) fn bulk_toggle_hidden(models: &[DisplayModel], hidden: &[String]) -> Vec<String> {
    let built_in: Vec<&str> = models.iter().filter(|m| !m.is_custom).map(|m| m.slug.as_str()).collect();
    if built_in.iter().all(|slug| hidden.iter().any(|h| h == slug)) {
        return hidden.iter().filter(|slug| !built_in.contains(&slug.as_str())).cloned().collect();
    }
    let mut next = hidden.to_vec();
    for slug in built_in {
        if !next.iter().any(|h| h == slug) {
            next.push(slug.to_owned());
        }
    }
    next
}

fn descriptors(capabilities: Option<&Value>) -> &[Value] {
    capabilities.and_then(|c| c["optionDescriptors"].as_array()).map_or(&[], Vec::as_slice)
}

/// `isReasoningControl` (`packages/client-runtime/src/modelTraits.ts`).
fn is_reasoning_control(descriptor: &Value) -> bool {
    matches!(descriptor["id"].as_str(), Some("reasoningEffort" | "effort" | "reasoning" | "variant" | "thinking"))
        || descriptor["role"] == "reasoning"
}

fn has_reasoning(model: &DisplayModel) -> bool {
    descriptors(model.capabilities.as_ref()).iter().any(is_reasoning_control)
}

/// `describeModelCapabilities`: "Fast mode", and "No reasoning" only where other models reason.
pub(super) fn capability_labels(model: &DisplayModel, flag_missing_reasoning: bool) -> Vec<&'static str> {
    let descriptors = descriptors(model.capabilities.as_ref());
    let mut labels = Vec::new();
    let fast = descriptors.iter().any(|d| {
        d["id"] == "fastMode"
            || (d["id"] == "serviceTier"
                && d["type"] == "select"
                && d["options"].as_array().is_some_and(|o| o.iter().any(|o| o["id"] == "fast" || o["label"] == "Fast")))
    });
    if fast {
        labels.push("Fast mode");
    }
    if flag_missing_reasoning
        && !model.is_custom
        && model.capabilities.is_some()
        && model.slug != "default"
        && model.slug != "auto"
        && !has_reasoning(model)
    {
        labels.push("No reasoning");
    }
    labels
}

/// `handleAdd`'s checks: the slug to add, or the error to show.
pub(super) fn validate_new_slug(
    input: &str,
    models: &[DisplayModel],
    custom: &[CustomModel],
) -> Result<String, String> {
    let slug = input.trim();
    if slug.is_empty() {
        return Err("Enter a model slug.".into());
    }
    if models.iter().any(|m| !m.is_custom && m.slug == slug) {
        return Err("That model is already built in.".into());
    }
    if slug.chars().count() > MAX_CUSTOM_MODEL_LENGTH {
        return Err(format!("Model slugs must be {MAX_CUSTOM_MODEL_LENGTH} characters or less."));
    }
    if custom.iter().any(|entry| entry.slug == slug) {
        return Err("That custom model is already saved.".into());
    }
    Ok(slug.to_owned())
}

/// `modelListChanges`: favorites to flip so `current` becomes `next`.
fn favorite_changes(current: &[String], next: &[String]) -> Vec<(String, bool)> {
    let mut changes: Vec<(String, bool)> =
        current.iter().filter(|m| !next.contains(m)).map(|m| (m.clone(), false)).collect();
    changes.extend(next.iter().filter(|m| !current.contains(m)).map(|m| (m.clone(), true)));
    changes
}

fn placeholder_for(driver: &str) -> &'static str {
    match driver {
        "codex" => "gpt-6.7-codex-ultra-preview",
        "claudeAgent" => "claude-sonnet-5",
        "cursor" => "claude-sonnet-4-6",
        "opencode" => "openai/gpt-5",
        "pi" => "anthropic/claude-sonnet-5",
        _ => "model-slug",
    }
}

// The custom model editor (`customModelEditor.logic.ts`).

#[derive(Clone, Debug, PartialEq)]
pub(super) struct CustomModelDraft {
    pub slug: String,
    pub name: String,
    pub descriptors: Vec<EditorDescriptor>,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct EditorDescriptor {
    pub key: u64,
    pub boolean: bool,
    pub id: String,
    pub label: String,
    pub choices: Vec<EditorChoice>,
    pub current_boolean: Option<bool>,
    pub description: Option<Value>,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct EditorChoice {
    pub key: u64,
    pub id: String,
    pub label: String,
    pub is_default: bool,
    pub description: Option<Value>,
}

pub(super) struct Preset {
    id: &'static str,
    label: &'static str,
    boolean: bool,
    choices: &'static [(&'static str, &'static str, bool)],
}

const EFFORT: &[(&str, &str, bool)] =
    &[("low", "Low", false), ("medium", "Medium", true), ("high", "High", false), ("xhigh", "Extra High", false)];

const fn select(
    id: &'static str,
    label: &'static str,
    choices: &'static [(&'static str, &'static str, bool)],
) -> Preset {
    Preset { id, label, boolean: false, choices }
}

const fn toggle(id: &'static str, label: &'static str) -> Preset {
    Preset { id, label, boolean: true, choices: &[] }
}

const CODEX_PRESETS: &[Preset] = &[
    select("reasoningEffort", "Reasoning", EFFORT),
    select("serviceTier", "Speed", &[("default", "Standard", true), ("fast", "Fast", false)]),
];
const CLAUDE_PRESETS: &[Preset] = &[
    select(
        "effort",
        "Reasoning",
        &[
            ("low", "Low", false),
            ("medium", "Medium", false),
            ("high", "High", true),
            ("xhigh", "Extra High", false),
            ("max", "Max", false),
        ],
    ),
    toggle("fastMode", "Fast Mode"),
    toggle("thinking", "Thinking"),
];
const CURSOR_PRESETS: &[Preset] =
    &[select("reasoning", "Reasoning", EFFORT), toggle("fastMode", "Fast Mode"), toggle("thinking", "Thinking")];
const GROK_PRESETS: &[Preset] = &[select("reasoningEffort", "Reasoning", EFFORT)];
const PI_PRESETS: &[Preset] = &[select(
    "thinking",
    "Thinking",
    &[
        ("off", "Off", false),
        ("minimal", "Minimal", false),
        ("low", "Low", false),
        ("medium", "Medium", true),
        ("high", "High", false),
        ("xhigh", "Extra High", false),
        ("max", "Max", false),
    ],
)];
const OPENCODE_PRESETS: &[Preset] = &[
    select("variant", "Reasoning", EFFORT),
    select("agent", "Agent", &[("build", "Build", true), ("plan", "Plan", false)]),
];

/// `DESCRIPTOR_PRESETS_BY_KIND`.
fn presets(driver: &str) -> &'static [Preset] {
    match driver {
        "codex" => CODEX_PRESETS,
        "claudeAgent" => CLAUDE_PRESETS,
        "cursor" => CURSOR_PRESETS,
        "grok" => GROK_PRESETS,
        "pi" => PI_PRESETS,
        "opencode" => OPENCODE_PRESETS,
        _ => &[],
    }
}

fn descriptor_from_preset(preset: &Preset, key: &mut impl FnMut() -> u64) -> EditorDescriptor {
    EditorDescriptor {
        key: key(),
        boolean: preset.boolean,
        id: preset.id.into(),
        label: preset.label.into(),
        choices: preset
            .choices
            .iter()
            .map(|(id, label, is_default)| EditorChoice {
                key: key(),
                id: (*id).into(),
                label: (*label).into(),
                is_default: *is_default,
                description: None,
            })
            .collect(),
        current_boolean: None,
        description: None,
    }
}

/// `descriptorToEditor`: prompt-injected choices are left out, and the current value is the default.
fn descriptor_to_editor(descriptor: &Value, key: &mut impl FnMut() -> u64) -> EditorDescriptor {
    let boolean = descriptor["type"] == "boolean";
    let injected: Vec<&str> =
        descriptor["promptInjectedValues"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    let options: Vec<&Value> = if boolean {
        vec![]
    } else {
        descriptor["options"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|o| !injected.contains(&o["id"].as_str().unwrap_or_default()))
            .collect()
    };
    let default_id = options
        .iter()
        .find(|o| o["id"] == descriptor["currentValue"])
        .or_else(|| options.iter().find(|o| o["isDefault"] == true))
        .and_then(|o| o["id"].as_str());
    EditorDescriptor {
        key: key(),
        boolean,
        id: descriptor["id"].as_str().unwrap_or_default().into(),
        label: descriptor["label"].as_str().unwrap_or_default().into(),
        choices: options
            .iter()
            .map(|o| EditorChoice {
                key: key(),
                id: o["id"].as_str().unwrap_or_default().into(),
                label: o["label"].as_str().unwrap_or_default().into(),
                is_default: o["id"].as_str() == default_id && default_id.is_some(),
                description: o.get("description").cloned(),
            })
            .collect(),
        current_boolean: if boolean { descriptor["currentValue"].as_bool() } else { None },
        description: descriptor.get("description").cloned(),
    }
}

/// `draftFromDefinition`.
pub(super) fn draft_from_definition(entry: &CustomModel, key: &mut impl FnMut() -> u64) -> CustomModelDraft {
    CustomModelDraft {
        slug: entry.slug.clone(),
        name: if entry.name == entry.slug { String::new() } else { entry.name.clone() },
        descriptors: descriptors(entry.capabilities.as_ref()).iter().map(|d| descriptor_to_editor(d, key)).collect(),
    }
}

/// `descriptorsFromCapabilities`: Claude's context choices need runtime mappings custom models lack.
pub(super) fn descriptors_from_capabilities(
    capabilities: Option<&Value>,
    driver: &str,
    key: &mut impl FnMut() -> u64,
) -> Vec<EditorDescriptor> {
    descriptors(capabilities)
        .iter()
        .filter(|d| driver != "claudeAgent" || d["id"] != "contextWindow")
        .map(|d| descriptor_to_editor(d, key))
        .collect()
}

/// `validateDraft`.
pub(super) fn validate_draft(draft: &CustomModelDraft) -> Option<String> {
    let mut seen_ids: Vec<&str> = Vec::new();
    for (index, descriptor) in draft.descriptors.iter().enumerate() {
        let position = format!("Option {}", index + 1);
        let id = descriptor.id.trim();
        if id.is_empty() {
            return Some(format!("{position} needs an id."));
        }
        if seen_ids.contains(&id) {
            return Some(format!("{position}: id \"{id}\" is used twice."));
        }
        seen_ids.push(id);
        if descriptor.label.trim().is_empty() {
            return Some(format!("{position} needs a label."));
        }
        if descriptor.boolean {
            continue;
        }
        if descriptor.choices.is_empty() {
            return Some(format!("{position} needs at least one choice."));
        }
        let mut seen_choices: Vec<&str> = Vec::new();
        for choice in &descriptor.choices {
            let choice_id = choice.id.trim();
            if choice_id.is_empty() {
                return Some(format!("{position} has a choice without a value."));
            }
            if seen_choices.contains(&choice_id) {
                return Some(format!("{position}: choice \"{choice_id}\" is used twice."));
            }
            seen_choices.push(choice_id);
        }
    }
    None
}

/// `definitionFromDraft`.
pub(super) fn definition_from_draft(draft: &CustomModelDraft) -> CustomModel {
    let descriptors: Vec<Value> = draft
        .descriptors
        .iter()
        .map(|descriptor| {
            let mut out = json!({"id": descriptor.id.trim(), "label": descriptor.label.trim()});
            if let Some(description) = &descriptor.description {
                out["description"] = description.clone();
            }
            if descriptor.boolean {
                out["type"] = json!("boolean");
                if let Some(current) = descriptor.current_boolean {
                    out["currentValue"] = json!(current);
                }
                return out;
            }
            let options: Vec<Value> = descriptor
                .choices
                .iter()
                .map(|choice| {
                    let id = choice.id.trim();
                    let label = choice.label.trim();
                    let mut option = json!({"id": id, "label": if label.is_empty() { id } else { label }});
                    if let Some(description) = &choice.description {
                        option["description"] = description.clone();
                    }
                    if choice.is_default {
                        option["isDefault"] = json!(true);
                    }
                    option
                })
                .collect();
            out["type"] = json!("select");
            if let Some(current) = descriptor.choices.iter().find(|c| c.is_default) {
                out["currentValue"] = json!(current.id.trim());
            }
            out["options"] = Value::Array(options);
            out
        })
        .collect();
    let name = draft.name.trim();
    CustomModel {
        slug: draft.slug.clone(),
        name: if name.is_empty() { draft.slug.clone() } else { name.to_owned() },
        capabilities: (!descriptors.is_empty()).then(|| json!({ "optionDescriptors": descriptors })),
    }
}

/// What the models section reads for one instance.
struct ModelsView {
    driver: String,
    models: Vec<DisplayModel>,
    custom: Vec<CustomModel>,
    hidden: Vec<String>,
    favorites: Vec<String>,
    order: Vec<String>,
}

fn models_view(settings: &Value, row: &InstanceRow, live: Option<&Value>, instance: &Value) -> ModelsView {
    let custom =
        if row.driver == "antigravity" { vec![] } else { read_custom_models(instance.pointer("/config/customModels")) };
    let live_models = live.and_then(|p| p["models"].as_array()).map_or(&[][..], Vec::as_slice);
    let strings = |value: Option<&Value>| -> Vec<String> {
        value.and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect()
    };
    let prefs = settings.get("providerModelPreferences").and_then(|p| p.get(&row.id));
    ModelsView {
        driver: row.driver.clone(),
        models: models_for_display(live_models, &custom),
        custom,
        hidden: strings(prefs.and_then(|p| p.get("hiddenModels"))),
        order: strings(prefs.and_then(|p| p.get("modelOrder"))),
        favorites: settings["favorites"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|f| f["provider"] == row.id.as_str())
            .filter_map(|f| f["model"].as_str().map(str::to_owned))
            .collect(),
    }
}

/// The current instance's models, read again at the time of an action.
fn current_models_view(this: &Workspace, instance_id: &str) -> Option<(InstanceRow, ModelsView)> {
    let row = super::instances::instance_rows(&this.server_info.settings, &this.server_info.providers_json)
        .into_iter()
        .find(|r| r.id == instance_id)?;
    let live = super::instances::live_provider(&this.server_info.providers_json, instance_id);
    let view = models_view(&this.server_info.settings, &row, live, &row.instance());
    Some((row, view))
}

impl Workspace {
    fn save_model_preferences(&mut self, patch: Value, window: &mut Window, cx: &mut Context<Self>) {
        self.save_settings(json!({ "patch": patch }), "Model preferences not saved", window, cx);
    }

    /// Saves `config.customModels`, keeping the rest of the config (`nextConfigBlobWithValue`).
    fn save_custom_models(
        &mut self,
        row: &InstanceRow,
        custom: &[CustomModel],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let instance = row.instance();
        let mut config = instance.get("config").and_then(Value::as_object).cloned().unwrap_or_default();
        config.insert("customModels".into(), Value::Array(custom.iter().map(custom_model_setting).collect()));
        let mut next = instance.as_object().cloned().unwrap_or_default();
        next.insert("config".into(), Value::Object(config));
        self.persist_provider_instance(&row.id, Value::Object(next), window, cx);
    }

    fn add_custom_model(
        &mut self,
        page: &Entity<ProvidersPage>,
        instance_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((row, view)) = current_models_view(self, instance_id) else { return };
        if row.driver == "antigravity" {
            return;
        }
        let key = format!("models-add:{instance_id}");
        let input = page.read(cx).input_value(&key, cx).unwrap_or_default();
        match validate_new_slug(&input, &view.models, &view.custom) {
            Err(error) => page.update(cx, |page, cx| {
                page.models.error = Some(error);
                cx.notify();
            }),
            Ok(slug) => {
                let mut custom = view.custom.clone();
                custom.push(CustomModel { slug: slug.clone(), name: slug, capabilities: None });
                self.save_custom_models(&row, &custom, window, cx);
                let filter = format!("models-filter:{instance_id}");
                page.update(cx, |page, cx| {
                    page.forget_inputs(&key);
                    if let Some(input) = page.inputs.get(filter.as_str()) {
                        input.state.update(cx, |input, cx| input.set_value("", window, cx));
                    }
                    page.models.adding = false;
                    page.models.error = None;
                    cx.notify();
                });
            }
        }
    }

    fn cancel_add_custom_model(page: &Entity<ProvidersPage>, instance_id: &str, cx: &mut App) {
        page.update(cx, |page, cx| {
            page.forget_inputs(&format!("models-add:{instance_id}"));
            page.models.adding = false;
            page.models.error = None;
            cx.notify();
        });
    }

    pub(super) fn render_models_section(
        &mut self,
        page: &Entity<ProvidersPage>,
        row: &InstanceRow,
        live: Option<&Value>,
        instance: &Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = *theme::tokens(cx);
        if page.read(cx).models.instance.as_deref() != Some(row.id.as_str()) {
            let id = row.id.clone();
            page.update(cx, |page, _| {
                let revealed_email = page.models.revealed_email;
                page.models = ModelsUi { instance: Some(id), revealed_email, ..Default::default() };
            });
        }
        let view = models_view(&self.server_info.settings, row, live, instance);
        let (hidden, favorites): (HashSet<String>, HashSet<String>) =
            (view.hidden.iter().cloned().collect(), view.favorites.iter().cloned().collect());
        let prefs = Preferences { hidden: &hidden, favorites: &favorites, order: &view.order };
        let display = group_models(&view.models, &prefs);
        let favorite_count = display.iter().filter(|m| prefs.group(m) == Group::Favorite).count();
        let hidden_count = display.iter().filter(|m| prefs.group(m) == Group::Hidden).count();
        let flag_missing_reasoning = view.models.iter().any(has_reasoning);
        let built_in: Vec<String> = view.models.iter().filter(|m| !m.is_custom).map(|m| m.slug.clone()).collect();
        let all_hidden = !built_in.is_empty() && built_in.iter().all(|s| hidden.contains(s));
        let antigravity = view.driver == "antigravity";

        let show_filter = view.models.len() > FILTER_THRESHOLD;
        let filter_input = show_filter.then(|| {
            self.page_input(
                page,
                format!("models-filter:{}", row.id).into(),
                "",
                "Filter models",
                false,
                window,
                cx,
                Box::new(|_, _, event, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        cx.notify();
                    }
                }),
            )
        });
        let filter = filter_input.as_ref().map(|i| i.read(cx).value().trim().to_lowercase()).unwrap_or_default();
        let filtering = !filter.is_empty();
        let visible: Vec<&DisplayModel> = display
            .iter()
            .filter(|m| {
                !filtering || m.name.to_lowercase().contains(&filter) || m.slug.to_lowercase().contains(&filter)
            })
            .collect();

        let ui = &page.read(cx).models;
        let (adding, error, editing) = (ui.adding, ui.error.clone(), ui.editing.as_ref().map(|d| d.slug.clone()));

        // Toolbar: filter, bulk toggle and counts, add.
        let mut counts = format!("{} model{}", view.models.len(), if view.models.len() == 1 { "" } else { "s" });
        if favorite_count > 0 {
            counts += &format!(" · {favorite_count} favorite{}", if favorite_count == 1 { "" } else { "s" });
        }
        if hidden_count > 0 {
            counts += &format!(" · {hidden_count} hidden");
        }
        let bulk_id = row.id.clone();
        let add_page = page.clone();
        let toolbar = h_flex()
            .flex_wrap()
            .justify_between()
            .gap_2()
            .when_some(filter_input.clone(), |el, input| el.child(text_input(&input, Some(224.))))
            .child(
                h_flex()
                    .gap_2()
                    .when(!built_in.is_empty(), |el| {
                        el.child(
                            ghost_text_button("models-bulk", if all_hidden { "Enable all" } else { "Disable all" }, &t).on_click(
                                cx.listener(move |this, _, window, cx| {
                                    let Some((_, view)) = current_models_view(this, &bulk_id) else { return };
                                    let next = bulk_toggle_hidden(&view.models, &view.hidden);
                                    let changes: Vec<Value> = view
                                        .models
                                        .iter()
                                        .filter(|m| !m.is_custom)
                                        .map(|m| json!({"provider": bulk_id, "model": m.slug, "hidden": next.contains(&m.slug)}))
                                        .collect();
                                    this.save_model_preferences(json!({ "setModelsHidden": changes }), window, cx);
                                }),
                            ),
                        )
                    })
                    .child(div().text_xs().text_color(t.muted_foreground).child(counts)),
            )
            .when(!antigravity && !adding, |el| {
                el.child(
                    ghost_text_button("models-add-open", "Add custom model", &t)
                        .ml_auto()
                        .mr(px(-8.))
                        .icon(Icon::new(gpui_kit::assets::IconName::Plus).size(px(12.)))
                        .on_click(cx.listener(move |_, _, _, cx| {
                            add_page.update(cx, |page, cx| {
                                page.models.adding = true;
                                page.models.focus_add = true;
                                cx.notify();
                            })
                        })),
                )
            });

        // The list, with group labels where groups start.
        let mut list: Vec<AnyElement> = Vec::new();
        if visible.is_empty() {
            list.push(
                div()
                    .px_2()
                    .py_2()
                    .text_xs()
                    .text_color(t.muted_foreground)
                    .child(if filtering { "No models match." } else { "No models reported for this provider yet." })
                    .into_any_element(),
            );
        }
        for (index, model) in visible.iter().enumerate() {
            let group = prefs.group(model);
            let starts = index == 0 || prefs.group(visible[index - 1]) != group;
            let label = match group {
                Group::Favorite if favorite_count > 0 => Some("Favorites"),
                Group::Visible if favorite_count > 0 => Some("All"),
                Group::Hidden => Some("Hidden from picker"),
                _ => None,
            };
            if let (true, Some(label)) = (starts, label) {
                list.push(
                    div()
                        .px_2()
                        .pb(px(6.))
                        .pt(px(if index == 0 { 4. } else { 20. }))
                        .text_size(px(11.))
                        .text_color(t.muted_foreground)
                        .child(label)
                        .into_any_element(),
                );
            }
            let position = display.iter().position(|m| m.slug == model.slug).unwrap_or(0);
            let can_up = !filtering && position > 0 && prefs.group(&display[position - 1]) == group;
            let can_down = !filtering && display.get(position + 1).is_some_and(|next| prefs.group(next) == group);
            list.push(self.render_model_row(
                page,
                row,
                model,
                group,
                can_up,
                can_down,
                filtering,
                flag_missing_reasoning,
                &t,
                cx,
            ));
            if model.is_custom && editing.as_deref() == Some(model.slug.as_str()) {
                list.push(self.render_custom_model_editor(page, row, &view, window, cx));
            }
        }

        // Adding a custom model.
        let add_form = (!antigravity && adding).then(|| {
            let instance_id = row.id.clone();
            let add_page = page.clone();
            let input = self.page_input(
                page,
                format!("models-add:{}", row.id).into(),
                "",
                placeholder_for(&row.driver),
                false,
                window,
                cx,
                Box::new(move |this, _, event, window, cx| match event {
                    InputEvent::PressEnter { .. } => this.add_custom_model(&add_page, &instance_id, window, cx),
                    InputEvent::Change => add_page.update(cx, |page, cx| {
                        if page.models.error.take().is_some() {
                            cx.notify();
                        }
                    }),
                    _ => {}
                }),
            );
            if page.update(cx, |page, _| std::mem::take(&mut page.models.focus_add)) {
                input.update(cx, |input, cx| input.focus(window, cx));
            }
            let (add_page, cancel_page, escape_page) = (page.clone(), page.clone(), page.clone());
            let (add_id, cancel_id, escape_id) = (row.id.clone(), row.id.clone(), row.id.clone());
            h_flex()
                .mt_3()
                .gap_2()
                .on_key_down(move |event: &KeyDownEvent, _, cx| {
                    if event.keystroke.key == "escape" {
                        Workspace::cancel_add_custom_model(&escape_page, &escape_id, cx);
                    }
                })
                .child(text_input(&input, None).flex_1())
                .child(outline_button("models-add-confirm", "Add").on_click(
                    cx.listener(move |this, _, window, cx| this.add_custom_model(&add_page, &add_id, window, cx)),
                ))
                .child(
                    super::Button::new("models-add-cancel")
                        .ghost()
                        .small()
                        .h(px(28.))
                        .label("Cancel")
                        .on_click(move |_, _, cx| Workspace::cancel_add_custom_model(&cancel_page, &cancel_id, cx)),
                )
        });

        v_flex()
            .child(toolbar)
            .child(div().id("models-list").mt_2().mx(px(-8.)).max_h(px(256.)).overflow_y_scroll().children(list))
            .children(add_form)
            .when_some(error.filter(|_| !antigravity), |el, error| {
                el.child(div().mt_2().text_xs().text_color(t.error).child(error))
            })
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_model_row(
        &mut self,
        page: &Entity<ProvidersPage>,
        row: &InstanceRow,
        model: &DisplayModel,
        group: Group,
        can_up: bool,
        can_down: bool,
        filtering: bool,
        flag_missing_reasoning: bool,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let hidden = group == Group::Hidden;
        let favorite = group == Group::Favorite;
        let id = |name: &str| SharedString::from(format!("model-{name}-{}-{}", row.id, model.slug));
        let labels = capability_labels(model, flag_missing_reasoning);
        let (instance_id, slug) = (row.id.clone(), model.slug.clone());

        let star = {
            let (instance_id, slug) = (instance_id.clone(), slug.clone());
            ghost_icon_button(id("favorite"), gpui_kit::assets::IconName::Star, t)
                .size(px(20.))
                .when(favorite, |b| b.text_color(t.warning))
                .tooltip(if favorite { "Remove from favorites" } else { "Add to favorites" })
                .on_click(cx.listener(move |this, _, window, cx| {
                    let Some((_, view)) = current_models_view(this, &instance_id) else { return };
                    let next = !view.favorites.contains(&slug);
                    let patch =
                        json!({"setModelFavorites": [{"provider": instance_id, "model": slug, "favorite": next}]});
                    this.save_model_preferences(patch, window, cx);
                }))
        };
        let mover = |direction: i32| {
            let (instance_id, slug) = (instance_id.clone(), slug.clone());
            cx.listener(move |this: &mut Workspace, _, window, cx| {
                let Some((_, view)) = current_models_view(this, &instance_id) else { return };
                let (hidden, favorites): (HashSet<String>, HashSet<String>) =
                    (view.hidden.iter().cloned().collect(), view.favorites.iter().cloned().collect());
                let prefs = Preferences { hidden: &hidden, favorites: &favorites, order: &view.order };
                let display = group_models(&view.models, &prefs);
                let Some(index) = display.iter().position(|m| m.slug == slug) else { return };
                let Some(other) = index.checked_add_signed(direction as isize).filter(|i| *i < display.len()) else {
                    return;
                };
                if prefs.group(&display[index]) != prefs.group(&display[other]) {
                    return;
                }
                let mut order: Vec<String> = display.iter().map(|m| m.slug.clone()).collect();
                order.swap(index, other);
                let patch = json!({"setProviderModelOrder": {"provider": instance_id, "modelOrder": order}});
                this.save_model_preferences(patch, window, cx);
            })
        };
        let toggle = {
            let (instance_id, slug) = (instance_id.clone(), slug.clone());
            cx.listener(move |this, checked: &bool, window, cx| {
                let patch = json!({"setModelsHidden": [{"provider": instance_id, "model": slug, "hidden": !*checked}]});
                this.save_model_preferences(patch, window, cx);
            })
        };
        let picker_tooltip = if model.is_custom {
            "Custom models are always shown in the picker"
        } else if hidden {
            "Hidden from picker"
        } else {
            "Shown in picker"
        };
        let name_tooltip = (model.name.clone(), (model.name != model.slug).then(|| model.slug.clone()));

        h_flex()
            .id(id("row"))
            .h(px(28.))
            .px_2()
            .gap_2()
            .rounded_md()
            .hover(|s| s.bg(t.muted.opacity(0.3)))
            .when(hidden, |el| el.opacity(0.5))
            .child(star)
            .child(
                h_flex()
                    .id(id("name"))
                    .flex_1()
                    .min_w_0()
                    .gap_2()
                    .overflow_hidden()
                    .child(
                        div()
                            .flex_none()
                            .max_w_full()
                            .truncate()
                            .text_xs()
                            .text_color(if hidden { t.muted_foreground } else { t.foreground.opacity(0.9) })
                            .child(model.name.clone()),
                    )
                    .when(model.name != model.slug, |el| {
                        el.child(
                            div()
                                .truncate()
                                .font_family(t.fonts.mono)
                                .text_size(px(11.))
                                .text_color(t.muted_foreground.opacity(0.7))
                                .child(model.slug.clone()),
                        )
                    })
                    .when(model.is_custom, |el| {
                        el.child(
                            div()
                                .flex_none()
                                .text_size(px(11.))
                                .text_color(t.muted_foreground.opacity(0.7))
                                .child("custom"),
                        )
                    })
                    .tooltip(move |window, cx| {
                        let (name, slug) = name_tooltip.clone();
                        gpui_kit::component::tooltip::Tooltip::element(move |_, cx| {
                            v_flex().child(name.clone()).children(
                                slug.clone().map(|s| {
                                    div().font_family(theme::tokens(cx).fonts.mono).text_size(px(11.)).child(s)
                                }),
                            )
                        })
                        .build(window, cx)
                    }),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(px(11.))
                    .text_color(t.muted_foreground.opacity(0.7))
                    .child(labels.join(" · ")),
            )
            .child(
                h_flex()
                    .flex_none()
                    .min_w(px(88.))
                    .justify_end()
                    .gap(px(2.))
                    .when(!hidden && !filtering, |el| {
                        el.child(
                            ghost_icon_button(id("up"), gpui_kit::assets::IconName::ArrowUp, t)
                                .size(px(20.))
                                .disabled(!can_up)
                                .tooltip("Move up")
                                .on_click(mover(-1)),
                        )
                        .child(
                            ghost_icon_button(id("down"), gpui_kit::assets::IconName::ArrowDown, t)
                                .size(px(20.))
                                .disabled(!can_down)
                                .tooltip("Move down")
                                .on_click(mover(1)),
                        )
                    })
                    .when(model.is_custom, |el| {
                        let (edit_page, edit_id, edit_slug) = (page.clone(), instance_id.clone(), slug.clone());
                        let (remove_id, remove_slug) = (instance_id.clone(), slug.clone());
                        let remove_page = page.clone();
                        el.child(
                            ghost_icon_button(id("edit"), gpui_kit::assets::IconName::Pencil, t)
                                .size(px(20.))
                                .tooltip("Edit name and options")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.toggle_custom_model_editor(&edit_page, &edit_id, &edit_slug, cx)
                                })),
                        )
                        .child(
                            ghost_icon_button(id("remove"), gpui_kit::assets::IconName::X, t)
                                .size(px(20.))
                                .tooltip("Remove custom model")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.remove_custom_model(&remove_page, &remove_id, &remove_slug, window, cx)
                                })),
                        )
                    }),
            )
            .child(
                div()
                    .id(id("switch-wrap"))
                    .flex_none()
                    .tooltip(move |window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new(picker_tooltip).build(window, cx)
                    })
                    .child(
                        switch(id("switch"), !hidden)
                            .small()
                            .disabled(model.is_custom)
                            .accessibility_label(format!("Show {} in the model picker", model.name))
                            .on_change(toggle),
                    ),
            )
            .into_any_element()
    }

    /// `handleRemove`: the model, its place in the order and its favorite.
    fn remove_custom_model(
        &mut self,
        page: &Entity<ProvidersPage>,
        instance_id: &str,
        slug: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((row, view)) = current_models_view(self, instance_id) else { return };
        page.update(cx, |page, cx| {
            if page.models.editing.as_ref().is_some_and(|d| d.slug == slug) {
                page.models.editing = None;
            }
            page.models.error = None;
            cx.notify();
        });
        let custom: Vec<CustomModel> = view.custom.iter().filter(|e| e.slug != slug).cloned().collect();
        self.save_custom_models(&row, &custom, window, cx);
        let order: Vec<String> = view.order.iter().filter(|m| *m != slug).cloned().collect();
        self.save_model_preferences(
            json!({"setProviderModelOrder": {"provider": instance_id, "modelOrder": order}}),
            window,
            cx,
        );
        let favorites: Vec<String> = view.favorites.iter().filter(|m| *m != slug).cloned().collect();
        let changes: Vec<Value> = favorite_changes(&view.favorites, &favorites)
            .into_iter()
            .map(|(model, favorite)| json!({"provider": instance_id, "model": model, "favorite": favorite}))
            .collect();
        if !changes.is_empty() {
            self.save_model_preferences(json!({ "setModelFavorites": changes }), window, cx);
        }
    }

    fn toggle_custom_model_editor(
        &mut self,
        page: &Entity<ProvidersPage>,
        instance_id: &str,
        slug: &str,
        cx: &mut Context<Self>,
    ) {
        let Some((_, view)) = current_models_view(self, instance_id) else { return };
        let Some(entry) = view.custom.iter().find(|e| e.slug == slug) else { return };
        page.update(cx, |page, cx| {
            page.forget_inputs(&format!("cm:{instance_id}:"));
            page.models.editor_error = None;
            page.models.editing = if page.models.editing.as_ref().is_some_and(|d| d.slug == slug) {
                None
            } else {
                let ui = &mut page.models;
                Some(draft_from_definition(entry, &mut || ui.key()))
            };
            cx.notify();
        });
    }

    fn close_custom_model_editor(page: &Entity<ProvidersPage>, instance_id: &str, cx: &mut App) {
        page.update(cx, |page, cx| {
            page.forget_inputs(&format!("cm:{instance_id}:"));
            page.models.editing = None;
            page.models.editor_error = None;
            cx.notify();
        });
    }

    /// Copies what the editor's text fields hold into the draft.
    fn sync_editor_draft(page: &Entity<ProvidersPage>, instance_id: &str, cx: &mut App) {
        page.update(cx, |page, cx| {
            let prefix = format!("cm:{instance_id}:");
            let read = |page: &ProvidersPage, key: String| page.input_value(&format!("{prefix}{key}"), cx);
            let Some(mut draft) = page.models.editing.clone() else { return };
            if let Some(name) = read(page, "name".into()) {
                draft.name = name;
            }
            for descriptor in &mut draft.descriptors {
                let key = descriptor.key;
                if let Some(id) = read(page, format!("{key}:id")) {
                    descriptor.id = id;
                }
                if let Some(label) = read(page, format!("{key}:label")) {
                    descriptor.label = label;
                }
                for choice in &mut descriptor.choices {
                    if let Some(id) = read(page, format!("{key}:{}:id", choice.key)) {
                        choice.id = id;
                    }
                    if let Some(label) = read(page, format!("{key}:{}:label", choice.key)) {
                        choice.label = label;
                    }
                }
            }
            page.models.editing = Some(draft);
        });
    }

    fn edit_draft(
        page: &Entity<ProvidersPage>,
        instance_id: &str,
        cx: &mut App,
        edit: impl FnOnce(&mut CustomModelDraft, &mut dyn FnMut() -> u64),
    ) {
        Self::sync_editor_draft(page, instance_id, cx);
        page.update(cx, |page, cx| {
            let ui = &mut page.models;
            let Some(mut draft) = ui.editing.take() else { return };
            edit(&mut draft, &mut || ui.key());
            ui.editing = Some(draft);
            ui.editor_error = None;
            cx.notify();
        });
    }

    fn save_custom_model_editor(
        &mut self,
        page: &Entity<ProvidersPage>,
        instance_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        Self::sync_editor_draft(page, instance_id, cx);
        let Some(draft) = page.read(cx).models.editing.clone() else { return };
        if let Some(problem) = validate_draft(&draft) {
            page.update(cx, |page, cx| {
                page.models.editor_error = Some(problem);
                cx.notify();
            });
            return;
        }
        let Some((row, view)) = current_models_view(self, instance_id) else { return };
        let next = definition_from_draft(&draft);
        let custom: Vec<CustomModel> =
            view.custom.iter().map(|e| if e.slug == next.slug { next.clone() } else { e.clone() }).collect();
        self.save_custom_models(&row, &custom, window, cx);
        Self::close_custom_model_editor(page, instance_id, cx);
    }

    fn render_custom_model_editor(
        &mut self,
        page: &Entity<ProvidersPage>,
        row: &InstanceRow,
        view: &ModelsView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = *theme::tokens(cx);
        let Some(draft) = page.read(cx).models.editing.clone() else { return div().into_any_element() };
        let error = page.read(cx).models.editor_error.clone();
        let presets = presets(&row.driver);
        let instance_id = row.id.clone();
        let prefix = format!("cm:{}:", row.id);
        let noop: fn() -> super::InputHandler = || Box::new(|_, _, _, _, _| {});
        let input = |this: &mut Workspace,
                     key: String,
                     value: &str,
                     placeholder: &str,
                     window: &mut Window,
                     cx: &mut Context<Workspace>| {
            this.page_input(page, format!("{prefix}{key}").into(), value, placeholder, false, window, cx, noop())
        };

        let name = input(self, "name".into(), &draft.name, &draft.slug, window, cx);
        let start_from: Vec<&DisplayModel> =
            view.models.iter().filter(|m| !m.is_custom && !descriptors(m.capabilities.as_ref()).is_empty()).collect();

        let mut descriptor_rows: Vec<AnyElement> = Vec::new();
        for (index, descriptor) in draft.descriptors.iter().enumerate() {
            let key = descriptor.key;
            let is_preset = presets.iter().any(|p| p.id == descriptor.id);
            let id_input =
                (!is_preset).then(|| input(self, format!("{key}:id"), &descriptor.id, "optionId", window, cx));
            let label_input = input(self, format!("{key}:label"), &descriptor.label, "Label", window, cx);
            let mut header = h_flex().flex_wrap().gap_2().child(
                div()
                    .w(px(56.))
                    .flex_none()
                    .text_size(px(11.))
                    .text_color(t.muted_foreground)
                    .child(format!("Option {}", index + 1)),
            );
            if !presets.is_empty() {
                let options: Vec<(SharedString, SharedString)> = presets
                    .iter()
                    .map(|p| (p.id.into(), p.id.into()))
                    .chain([("__custom__".into(), "Custom…".into())])
                    .collect();
                let current = if is_preset { descriptor.id.clone() } else { "__custom__".into() };
                let (page, instance_id) = (page.clone(), instance_id.clone());
                header = header.child(select_button(
                    SharedString::from(format!("cm-preset-{key}")),
                    options,
                    &current,
                    160.,
                    &t,
                    move |value, _, cx| {
                        Workspace::edit_draft(&page, &instance_id, cx, |draft, next_key| {
                            let Some(slot) = draft.descriptors.iter_mut().find(|d| d.key == key) else { return };
                            *slot = match presets.iter().find(|p| p.id == value.as_ref()) {
                                Some(preset) => descriptor_from_preset(preset, &mut || next_key()),
                                None => EditorDescriptor { key: next_key(), id: String::new(), ..slot.clone() },
                            };
                        })
                    },
                ));
            }
            if let Some(id_input) = id_input {
                header = header.child(text_input(&id_input, Some(144.)));
            }
            let (type_page, type_id, remove_page, remove_id) =
                (page.clone(), instance_id.clone(), page.clone(), instance_id.clone());
            header = header
                .child(text_input(&label_input, None).flex_1().min_w(px(80.)))
                .child(select_button(
                    SharedString::from(format!("cm-type-{key}")),
                    super::options(&[("select", "Choices"), ("boolean", "Toggle")]),
                    if descriptor.boolean { "boolean" } else { "select" },
                    96.,
                    &t,
                    move |value, _, cx| {
                        Workspace::edit_draft(&type_page, &type_id, cx, |draft, _| {
                            if let Some(d) = draft.descriptors.iter_mut().find(|d| d.key == key) {
                                d.boolean = value.as_ref() == "boolean";
                            }
                        })
                    },
                ))
                .child(
                    ghost_icon_button(
                        SharedString::from(format!("cm-remove-{key}")),
                        gpui_kit::assets::IconName::X,
                        &t,
                    )
                    .size(px(20.))
                    .tooltip(format!("Remove option {}", index + 1))
                    .on_click(move |_, _, cx| {
                        Workspace::edit_draft(&remove_page, &remove_id, cx, |draft, _| {
                            draft.descriptors.retain(|d| d.key != key)
                        })
                    }),
                );
            let mut block = v_flex()
                .gap_2()
                .rounded_md()
                .border_1()
                .border_color(t.border.opacity(0.6))
                .bg(t.background.opacity(0.4))
                .p(px(10.))
                .child(header);
            if !descriptor.boolean {
                let mut choices = v_flex().gap(px(6.)).pl(px(64.));
                for choice in &descriptor.choices {
                    let ckey = choice.key;
                    let value = input(self, format!("{key}:{ckey}:id"), &choice.id, "value", window, cx);
                    let label = input(self, format!("{key}:{ckey}:label"), &choice.label, "Label", window, cx);
                    let (default_page, default_id, remove_page, remove_id) =
                        (page.clone(), instance_id.clone(), page.clone(), instance_id.clone());
                    choices = choices.child(
                        h_flex()
                            .gap_2()
                            .child(text_input(&value, Some(112.)))
                            .child(text_input(&label, None).flex_1())
                            .child(
                                h_flex()
                                    .flex_none()
                                    .gap(px(6.))
                                    .text_size(px(11.))
                                    .text_color(t.muted_foreground)
                                    .child(
                                        switch(
                                            SharedString::from(format!("cm-default-{key}-{ckey}")),
                                            choice.is_default,
                                        )
                                        .small()
                                        .accessibility_label("Default choice")
                                        .on_change(
                                            move |checked: &bool, _, cx| {
                                                let checked = *checked;
                                                Workspace::edit_draft(&default_page, &default_id, cx, |draft, _| {
                                                    if let Some(d) = draft.descriptors.iter_mut().find(|d| d.key == key)
                                                    {
                                                        for c in &mut d.choices {
                                                            if c.key == ckey {
                                                                c.is_default = checked;
                                                            } else if checked {
                                                                c.is_default = false;
                                                            }
                                                        }
                                                    }
                                                })
                                            },
                                        ),
                                    )
                                    .child("Default"),
                            )
                            .child(
                                ghost_icon_button(
                                    SharedString::from(format!("cm-choice-remove-{key}-{ckey}")),
                                    gpui_kit::assets::IconName::X,
                                    &t,
                                )
                                .size(px(20.))
                                .tooltip("Remove choice")
                                .on_click(move |_, _, cx| {
                                    Workspace::edit_draft(&remove_page, &remove_id, cx, |draft, _| {
                                        if let Some(d) = draft.descriptors.iter_mut().find(|d| d.key == key) {
                                            d.choices.retain(|c| c.key != ckey);
                                        }
                                    })
                                }),
                            ),
                    );
                }
                let (add_page, add_id) = (page.clone(), instance_id.clone());
                choices = choices.child(
                    ghost_text_button(SharedString::from(format!("cm-choice-add-{key}")), "Add choice", &t)
                        .ml(px(-8.))
                        .icon(Icon::new(gpui_kit::assets::IconName::Plus).size(px(12.)))
                        .on_click(move |_, _, cx| {
                            Workspace::edit_draft(&add_page, &add_id, cx, |draft, next_key| {
                                if let Some(d) = draft.descriptors.iter_mut().find(|d| d.key == key) {
                                    d.choices.push(EditorChoice {
                                        key: next_key(),
                                        id: String::new(),
                                        label: String::new(),
                                        is_default: false,
                                        description: None,
                                    });
                                }
                            })
                        }),
                );
                block = block.child(choices);
            }
            descriptor_rows.push(block.into_any_element());
        }

        let mut quick_add = h_flex().flex_wrap().gap_1();
        for preset in presets.iter().filter(|p| !draft.descriptors.iter().any(|d| d.id == p.id)) {
            let (add_page, add_id) = (page.clone(), instance_id.clone());
            quick_add = quick_add.child(
                ghost_text_button(SharedString::from(format!("cm-preset-add-{}", preset.id)), preset.label, &t)
                    .icon(Icon::new(gpui_kit::assets::IconName::Plus).size(px(12.)))
                    .on_click(move |_, _, cx| {
                        Workspace::edit_draft(&add_page, &add_id, cx, |draft, next_key| {
                            draft.descriptors.push(descriptor_from_preset(preset, &mut || next_key()))
                        })
                    }),
            );
        }
        let (add_page, add_id) = (page.clone(), instance_id.clone());
        quick_add = quick_add.child(
            ghost_text_button("cm-custom-option", "Custom option", &t)
                .icon(Icon::new(gpui_kit::assets::IconName::Plus).size(px(12.)))
                .on_click(move |_, _, cx| {
                    Workspace::edit_draft(&add_page, &add_id, cx, |draft, next_key| {
                        draft.descriptors.push(EditorDescriptor {
                            key: next_key(),
                            boolean: false,
                            id: String::new(),
                            label: String::new(),
                            choices: vec![],
                            current_boolean: None,
                            description: None,
                        })
                    })
                }),
        );

        let copy_from = (!start_from.is_empty()).then(|| {
            let options: Vec<(SharedString, SharedString)> =
                start_from.iter().map(|m| (m.slug.clone().into(), m.name.clone().into())).collect();
            let models: Vec<DisplayModel> = start_from.iter().map(|m| (*m).clone()).collect();
            let (copy_page, copy_id, driver) = (page.clone(), instance_id.clone(), row.driver.clone());
            select_button("cm-copy-from".into(), options, "Copy from…", 176., &t, move |slug, _, cx| {
                let Some(model) = models.iter().find(|m| m.slug == slug.as_ref()) else { return };
                Workspace::edit_draft(&copy_page, &copy_id, cx, |draft, next_key| {
                    draft.descriptors =
                        descriptors_from_capabilities(model.capabilities.as_ref(), &driver, &mut || next_key());
                })
            })
        });

        let (save_page, save_id, cancel_page, cancel_id, escape_page, escape_id) =
            (page.clone(), instance_id.clone(), page.clone(), instance_id.clone(), page.clone(), instance_id.clone());
        v_flex()
            .mx_2()
            .mt_1()
            .mb_2()
            .gap_3()
            .rounded_md()
            .border_1()
            .border_color(t.border)
            .bg(t.muted.opacity(0.2))
            .p_3()
            .on_key_down(move |event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    Workspace::close_custom_model_editor(&escape_page, &escape_id, cx);
                }
            })
            .child(
                v_flex()
                    .gap_1()
                    .child(div().text_xs().text_color(t.muted_foreground).child("Display name"))
                    .child(text_input(&name, Some(288.))),
            )
            .child(
                v_flex()
                    .gap_2()
                    .child(
                        h_flex()
                            .flex_wrap()
                            .justify_between()
                            .gap_2()
                            .child(
                                div().text_xs().text_color(t.muted_foreground).child("Options shown in the composer"),
                            )
                            .children(copy_from),
                    )
                    .when(draft.descriptors.is_empty(), |el| {
                        el.child(
                            div()
                                .text_xs()
                                .text_color(t.muted_foreground.opacity(0.7))
                                .child("No custom options. The composer uses the provider's default options."),
                        )
                    })
                    .children(descriptor_rows)
                    .child(quick_add),
            )
            .when_some(error, |el, error| el.child(div().text_xs().text_color(t.error).child(error)))
            .child(
                h_flex()
                    .gap_2()
                    .child(outline_button("cm-save", "Save").on_click(cx.listener(move |this, _, window, cx| {
                        this.save_custom_model_editor(&save_page, &save_id, window, cx)
                    })))
                    .child(
                        super::Button::new("cm-cancel").ghost().small().h(px(28.)).label("Cancel").on_click(
                            move |_, _, cx| Workspace::close_custom_model_editor(&cancel_page, &cancel_id, cx),
                        ),
                    ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(slug: &str, custom: bool) -> DisplayModel {
        DisplayModel { slug: slug.into(), name: slug.into(), is_custom: custom, capabilities: None }
    }

    #[test]
    fn custom_models_round_trip() {
        let saved = json!(["plain", {"slug": "named", "name": "Named"}, {"slug": "plain"}, {"slug": " "}, 4]);
        let entries = read_custom_models(Some(&saved));
        assert_eq!(entries.iter().map(|e| e.slug.as_str()).collect::<Vec<_>>(), ["plain", "named"]);
        assert_eq!(custom_model_setting(&entries[0]), json!("plain"));
        assert_eq!(custom_model_setting(&entries[1]), json!({"slug": "named", "name": "Named"}));
    }

    #[test]
    fn display_keeps_server_models_first_and_drops_shadowed_customs() {
        let live = vec![
            json!({"slug": "a", "name": "A", "isCustom": false}),
            json!({"slug": "c", "name": "c", "isCustom": true, "capabilities": {"optionDescriptors": []}}),
        ];
        let custom = read_custom_models(Some(&json!(["a", "c"])));
        let shown = models_for_display(&live, &custom);
        assert_eq!(
            shown.iter().map(|m| (m.slug.as_str(), m.is_custom)).collect::<Vec<_>>(),
            [("a", false), ("c", true)]
        );
        assert!(shown[1].capabilities.is_some());
    }

    #[test]
    fn grouping_and_order() {
        let models = vec![model("a", false), model("b", false), model("c", false), model("d", true)];
        let hidden: HashSet<String> = ["b".to_owned(), "d".to_owned()].into();
        let favorites: HashSet<String> = ["c".to_owned()].into();
        let order = vec!["d".to_owned(), "a".to_owned()];
        let prefs = Preferences { hidden: &hidden, favorites: &favorites, order: &order };
        let slugs: Vec<String> = group_models(&models, &prefs).into_iter().map(|m| m.slug).collect();
        // Custom models never hide.
        assert_eq!(slugs, ["c", "d", "a", "b"]);
    }

    #[test]
    fn bulk_toggle() {
        let models = vec![model("a", false), model("b", false), model("x", true)];
        assert_eq!(bulk_toggle_hidden(&models, &["a".into()]), ["a", "b"]);
        assert_eq!(bulk_toggle_hidden(&models, &["a".into(), "b".into(), "z".into()]), ["z"]);
    }

    #[test]
    fn capability_flags() {
        let reasoning = DisplayModel {
            capabilities: Some(
                json!({"optionDescriptors": [{"id": "reasoningEffort", "type": "select", "options": []}]}),
            ),
            ..model("r", false)
        };
        let fast = DisplayModel {
            capabilities: Some(
                json!({"optionDescriptors": [{"id": "serviceTier", "type": "select", "options": [{"id": "fast", "label": "Fast"}]}]}),
            ),
            ..model("f", false)
        };
        assert_eq!(capability_labels(&fast, true), ["Fast mode", "No reasoning"]);
        assert!(capability_labels(&reasoning, true).is_empty());
        assert!(
            capability_labels(
                &DisplayModel { capabilities: Some(json!({"optionDescriptors": []})), ..model("default", false) },
                true
            )
            .is_empty()
        );
    }

    #[test]
    fn new_slug_checks() {
        let models = vec![model("built", false)];
        let custom = read_custom_models(Some(&json!(["mine"])));
        assert_eq!(validate_new_slug("  ", &models, &custom), Err("Enter a model slug.".into()));
        assert_eq!(validate_new_slug("built", &models, &custom), Err("That model is already built in.".into()));
        assert_eq!(validate_new_slug("mine", &models, &custom), Err("That custom model is already saved.".into()));
        assert!(validate_new_slug(&"x".repeat(257), &models, &custom).is_err());
        assert_eq!(validate_new_slug(" new ", &models, &custom), Ok("new".into()));
    }

    #[test]
    fn editor_round_trip() {
        let mut n = 0;
        let mut key = || {
            n += 1;
            n
        };
        let entry = CustomModel {
            slug: "m".into(),
            name: "m".into(),
            capabilities: Some(json!({"optionDescriptors": [
                {"id": "effort", "label": "Reasoning", "type": "select", "currentValue": "high",
                 "promptInjectedValues": ["ultrathink"],
                 "options": [{"id": "low", "label": "Low"}, {"id": "high", "label": "High", "isDefault": true}, {"id": "ultrathink", "label": "Ultra"}]},
                {"id": "fastMode", "label": "Fast", "type": "boolean", "currentValue": true},
            ]})),
        };
        let draft = draft_from_definition(&entry, &mut key);
        assert_eq!(draft.name, "");
        assert_eq!(draft.descriptors[0].choices.len(), 2);
        assert!(draft.descriptors[0].choices[1].is_default);
        assert_eq!(validate_draft(&draft), None);
        let saved = definition_from_draft(&draft);
        assert_eq!(saved.name, "m");
        assert_eq!(
            saved.capabilities.unwrap()["optionDescriptors"],
            json!([
                {"id": "effort", "label": "Reasoning", "type": "select", "currentValue": "high",
                 "options": [{"id": "low", "label": "Low"}, {"id": "high", "label": "High", "isDefault": true}]},
                {"id": "fastMode", "label": "Fast", "type": "boolean", "currentValue": true},
            ])
        );
    }

    #[test]
    fn draft_validation_messages() {
        let mut n = 0;
        let mut key = || {
            n += 1;
            n
        };
        let mut draft = CustomModelDraft {
            slug: "m".into(),
            name: String::new(),
            descriptors: vec![descriptor_from_preset(&presets("codex")[0], &mut key)],
        };
        draft.descriptors.push(descriptor_from_preset(&presets("codex")[0], &mut key));
        assert_eq!(validate_draft(&draft).as_deref(), Some("Option 2: id \"reasoningEffort\" is used twice."));
        draft.descriptors[1].id = "other".into();
        draft.descriptors[1].choices.clear();
        assert_eq!(validate_draft(&draft).as_deref(), Some("Option 2 needs at least one choice."));
        draft.descriptors[1].boolean = true;
        draft.descriptors[1].label = " ".into();
        assert_eq!(validate_draft(&draft).as_deref(), Some("Option 2 needs a label."));
    }

    #[test]
    fn claude_context_window_is_not_copied() {
        let mut n = 0;
        let caps = json!({"optionDescriptors": [{"id": "contextWindow", "label": "Context", "type": "select", "options": []}, {"id": "effort", "label": "R", "type": "select", "options": []}]});
        let copied = descriptors_from_capabilities(Some(&caps), "claudeAgent", &mut || {
            n += 1;
            n
        });
        assert_eq!(copied.iter().map(|d| d.id.as_str()).collect::<Vec<_>>(), ["effort"]);
    }

    #[test]
    fn favorite_list_changes() {
        assert_eq!(
            favorite_changes(&["a".into(), "b".into()], &["b".into(), "c".into()]),
            [("a".into(), false), ("c".into(), true)]
        );
    }
}
