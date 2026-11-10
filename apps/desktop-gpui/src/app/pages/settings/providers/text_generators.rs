//! "Text generators" on the Providers page (`TextGeneratorsSettings.tsx`, `textGenerators.ts`):
//! model APIs that only write text, for thread titles, commit messages and transcript cleanup.
//! A generator's id shares the namespace of provider instances, so a model selection names one
//! the same way, and the writing pickers list them beside the coding agents.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::dialog::DialogEntrance;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{Disableable as _, Icon, Selectable as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Anchor, AnyElement, App, AppContext as _, Context, ElementId, Entity, FontWeight, InteractiveElement as _,
    IntoElement, ParentElement as _, Pixels, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, WeakEntity, Window, div, px, svg,
};
use reqwest::Url;
use serde_json::{Value, json};

use super::accounts::{failure_message, settings_row};
use super::add::{black, footer, panel, white, wizard_steps};
use super::instances::initials;
use crate::app::Workspace;
use crate::app::model_picker::{OnPick, PickHost, generator_owns};
use crate::app::pages::settings::connections::logic::SETTINGS_WRITE;
use crate::app::pages::settings::model_select::generates_text;
use crate::app::pages::settings::widgets::{Switch, section_with_action};
use crate::app::request_cards::{
    INVALID_REFERENCE, REFERENCE_PLACEHOLDER, SecretSource, secret_reference, secret_sources,
};
use crate::model::{ModelSelection, Provider};
use crate::rpc::RpcError;
use crate::theme::{Tokens, tokens};

const LIST_MODELS: &str = "server.listTextGeneratorModels";

/// `TextGeneratorPreset`: a service the add dialog offers. Custom has no URL; the user enters it.
struct Preset {
    id: &'static str,
    label: &'static str,
    kind: &'static str,
    base_url: &'static str,
}

/// `TEXT_GENERATOR_PRESETS`.
const PRESETS: [Preset; 4] = [
    Preset { id: "openai", label: "OpenAI", kind: "openaiCompatible", base_url: "https://api.openai.com/v1" },
    Preset {
        id: "openrouter",
        label: "OpenRouter",
        kind: "openaiCompatible",
        base_url: "https://openrouter.ai/api/v1",
    },
    Preset { id: "anthropic", label: "Anthropic", kind: "anthropic", base_url: "https://api.anthropic.com/v1" },
    Preset { id: "custom", label: "Custom (OpenAI-compatible)", kind: "openaiCompatible", base_url: "" },
];
const CUSTOM: usize = 3;

/// Text tasks a generator can write, in the order the settings pages show them (`TASKS`).
const TASKS: [(&str, &str); 3] = [
    ("textGenerationModelSelection", "Thread titles and other generated text"),
    ("sourceControlWriterModelSelection", "Commit messages, pull requests, and branch names"),
    ("speechPostProcessingModelSelection", "Voice transcript cleanup"),
];

/// The server drops text generators under a provider allow-list.
pub(in crate::app::pages::settings) fn text_generators_allowed(policy: &Value) -> bool {
    policy.pointer("/limits/providers/allowedDrivers").is_none()
}

/// `TextGeneratorBaseUrl`: an http(s) URL without credentials, query or fragment.
fn valid_base_url(url: &str) -> bool {
    if url.is_empty() || url.len() > 2048 || url.trim() != url {
        return false;
    }
    Url::parse(url).is_ok_and(|url| {
        matches!(url.scheme(), "http" | "https")
            && url.username().is_empty()
            && url.password().is_none_or(str::is_empty)
            && url.query().is_none_or(str::is_empty)
            && url.fragment().is_none_or(str::is_empty)
    })
}

/// `new URL(url).host`: the host and any non-default port.
fn url_host(url: &str) -> Option<String> {
    let url = Url::parse(url).ok()?;
    let host = url.host_str()?;
    Some(url.port().map_or_else(|| host.to_owned(), |port| format!("{host}:{port}")))
}

/// `textGeneratorDisplayName`: the label, else the preset it matches, else the URL's host.
fn display_name(config: &Value) -> String {
    if let Some(label) = config["label"].as_str().filter(|label| !label.is_empty()) {
        return label.to_owned();
    }
    let (kind, base_url) =
        (config["kind"].as_str().unwrap_or_default(), config["baseUrl"].as_str().unwrap_or_default());
    if let Some(preset) = PRESETS.iter().find(|preset| preset.base_url == base_url && preset.kind == kind) {
        return preset.label.to_owned();
    }
    url_host(base_url).unwrap_or_else(|| base_url.to_owned())
}

/// `textGeneratorBrand`: the service a generator reaches, which picks its icon. It stands in for
/// the driver in picker entries only.
fn brand(kind: &str, base_url: &str) -> &'static str {
    if kind == "anthropic" {
        return "anthropic";
    }
    let url = Url::parse(base_url).ok();
    let host = url.as_ref().and_then(Url::host_str).unwrap_or_default();
    if host == "openrouter.ai" || host.ends_with(".openrouter.ai") {
        "openrouter"
    } else if host == "api.openai.com" {
        "openai"
    } else if url.as_ref().and_then(Url::port) == Some(11434) || host.contains("ollama") {
        "ollama"
    } else {
        "openaiCompatible"
    }
}

fn config_brand(config: &Value) -> &'static str {
    brand(config["kind"].as_str().unwrap_or_default(), config["baseUrl"].as_str().unwrap_or_default())
}

/// `nextTextGeneratorId`: selections name a generator by the id a provider instance would use,
/// so a new one must not match a configured instance, a driver's default instance, or another
/// generator.
fn next_id(base: &str, settings: &Value, providers: &[Provider]) -> String {
    let keys = |key: &str| settings[key].as_object().into_iter().flat_map(|map| map.keys().map(String::as_str));
    let taken: HashSet<&str> = keys("providerInstances")
        .chain(keys("textGenerators"))
        .chain(providers.iter().map(|provider| provider.instance_id.as_str()))
        .chain(super::drivers::DRIVERS.iter().map(|driver| driver.kind))
        .collect();
    let mut id = base.to_owned();
    let mut suffix = 2;
    while taken.contains(id.as_str()) {
        id = format!("{base}-{suffix}");
        suffix += 1;
    }
    id
}

/// What the server's error says, else the transport's.
fn message(error: &RpcError) -> String {
    failure_message(error, &error.to_string())
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Model {
    id: String,
    name: String,
}

fn models_of(reply: &Value) -> Vec<Model> {
    reply["models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|model| {
            let id = model["id"].as_str()?.to_owned();
            let name = model["name"].as_str().map_or_else(|| id.clone(), str::to_owned);
            Some(Model { id, name })
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum ModelList {
    Loading,
    Ready(Vec<Model>),
    Failed(String),
}

/// One generator's list, for the connection settings it was asked with: editing them asks again.
struct Listed {
    kind: String,
    base_url: String,
    api_key: Value,
    request: u64,
    list: ModelList,
    /// Never asked: the session lacks `settings:write`, and the list holds that refusal.
    refused: bool,
}

impl Listed {
    fn asked_with(&self, config: &Value) -> bool {
        config["kind"] == self.kind.as_str()
            && config["baseUrl"] == self.base_url.as_str()
            && config["apiKey"] == self.api_key
    }

    /// Still answers for `config` while the grant is as it was: a refusal is asked again once
    /// granted, and a list is refused once the grant is gone.
    fn current(&self, config: &Value, refused: bool) -> bool {
        self.asked_with(config) && self.refused == refused
    }
}

fn enabled_generators(generators: &Value) -> Vec<(&String, &Value)> {
    generators.as_object().into_iter().flatten().filter(|(_, config)| config["enabled"] != false).collect()
}

/// The pickers' stand-ins for one environment's generators, rebuilt only when their inputs change.
struct Snapshot {
    revision: u64,
    generators: Value,
    selections: Vec<(String, String)>,
    providers: Rc<[Provider]>,
}

/// Each environment's text generator model lists (`textGeneratorModelsAtom`), shared by every
/// picker and dialog that shows them.
#[derive(Default)]
pub(in crate::app::pages::settings) struct TextGeneratorCatalog {
    lists: HashMap<(SharedString, String), Listed>,
    /// Bumped whenever a list changes, so snapshots rebuild.
    revision: u64,
    requests: u64,
    snapshots: HashMap<SharedString, Snapshot>,
}

impl TextGeneratorCatalog {
    fn list(&self, environment: &SharedString, id: &str) -> Option<&ModelList> {
        self.lists.get(&(environment.clone(), id.to_owned())).map(|listed| &listed.list)
    }

    /// `rereadTextGeneratorModels`: a saved key changes behind its redaction marker, so the
    /// list is asked for again on next use.
    fn forget(&mut self, environment: &SharedString, id: &str) {
        if self.lists.remove(&(environment.clone(), id.to_owned())).is_some() {
            self.revision += 1;
        }
    }

    /// The pickers' providers for `environment`'s `generators`, from its lists as they stand.
    fn targets(
        &mut self,
        environment: &SharedString,
        generators: &Value,
        selections: &[Option<&ModelSelection>],
    ) -> Rc<[Provider]> {
        let enabled = enabled_generators(generators);
        let selections: Vec<(String, String)> =
            selections.iter().flatten().map(|s| (s.instance_id.clone(), s.model.clone())).collect();
        if let Some(cached) = self.snapshots.get(environment)
            && cached.revision == self.revision
            && cached.generators == *generators
            && cached.selections == selections
        {
            return cached.providers.clone();
        }
        let providers: Rc<[Provider]> = enabled
            .iter()
            .filter_map(|(id, config)| {
                let selected: Vec<&str> = selections
                    .iter()
                    .filter(|(instance, _)| instance == *id)
                    .map(|(_, model)| model.as_str())
                    .collect();
                snapshot(id, config, self.list(environment, id), &selected)
            })
            .collect();
        let revision = self.revision;
        let snapshot = Snapshot { revision, generators: generators.clone(), selections, providers: providers.clone() };
        self.snapshots.insert(environment.clone(), snapshot);
        providers
    }

    /// Starts `id`'s list for `config` as `list`, returning the request a reply must match.
    fn record(&mut self, environment: &SharedString, id: &str, config: &Value, list: ModelList, refused: bool) -> u64 {
        self.requests += 1;
        let listed = Listed {
            kind: config["kind"].as_str().unwrap_or_default().to_owned(),
            base_url: config["baseUrl"].as_str().unwrap_or_default().to_owned(),
            api_key: config["apiKey"].clone(),
            request: self.requests,
            list,
            refused,
        };
        self.lists.insert((environment.clone(), id.to_owned()), listed);
        self.revision += 1;
        self.requests
    }

    /// Records a reply unless the list was asked for again, or forgotten, since.
    fn settle(&mut self, environment: SharedString, id: String, request: u64, list: ModelList) -> bool {
        match self.lists.get_mut(&(environment, id)) {
            Some(listed) if listed.request == request => {
                listed.list = list;
                self.revision += 1;
                true
            }
            _ => false,
        }
    }
}

/// `textGeneratorSnapshot`: a provider record the pickers list for a generator. Its models are
/// the listed ones, then any selected model the list lacks, so a selection keeps its model while
/// the list loads or after it fails.
fn snapshot(id: &str, config: &Value, list: Option<&ModelList>, selected: &[&str]) -> Option<Provider> {
    let listed: &[Model] = match list {
        Some(ModelList::Ready(models)) => models,
        _ => &[],
    };
    let mut models: Vec<Value> = listed.iter().map(|m| json!({"slug": m.id, "name": m.name})).collect();
    let mut missing: Vec<&str> = Vec::new();
    for slug in selected {
        if !listed.iter().any(|m| m.id == *slug) && !missing.contains(slug) {
            missing.push(slug);
        }
    }
    models.extend(missing.iter().map(|slug| json!({"slug": slug, "name": slug})));
    let failure = match list {
        Some(ModelList::Failed(message)) => Some(message.as_str()),
        _ => None,
    };
    let mut record = json!({
        "instanceId": id,
        "driver": config_brand(config),
        "displayName": display_name(config),
        "supportsTextGeneration": true,
        "enabled": config["enabled"] != false,
        "installed": true,
        "status": if failure.is_some() { "error" } else { "ready" },
        "auth": {"status": "unknown"},
        "models": models,
    });
    if let Some(failure) = failure {
        record["message"] = json!(failure);
    }
    serde_json::from_value(record).ok()
}

/// `useTextGenerationTargets`' providers: `providers` that generate text, less any a generator
/// in `configured` (the environment's `textGenerators`) owns by id, then the enabled `generators`.
pub(in crate::app::pages::settings) fn writing_providers<'a>(
    providers: &'a [Provider],
    configured: &Value,
    generators: &'a [Provider],
) -> Vec<&'a Provider> {
    providers
        .iter()
        .filter(|p| generates_text(p) && !generator_owns(p, configured, generators))
        .chain(generators)
        .collect()
}

fn selection_of(value: &Value) -> Option<ModelSelection> {
    serde::Deserialize::deserialize(value).ok()
}

impl Workspace {
    /// The enabled text generators on `environment` as picker providers, asking for any model
    /// list not yet loaded. `selections` keep their models while the lists load.
    pub(in crate::app::pages::settings) fn text_generation_targets(
        &mut self,
        environment: Option<&SharedString>,
        selections: &[Option<&ModelSelection>],
        cx: &mut Context<Self>,
    ) -> Rc<[Provider]> {
        let Some(environment) = environment else { return Rc::default() };
        let Some(info) = self.env_info(environment) else { return Rc::default() };
        let generators = info.settings.get("textGenerators").clone();
        for (id, config) in enabled_generators(&generators) {
            self.ensure_text_generator_models(environment, id, config, cx);
        }
        self.settings_ui.text_generators.targets(environment, &generators, selections)
    }

    /// Asks for a generator's models unless they are loaded for its current connection settings.
    /// Listing spends the stored key, so it needs `settings:write` on that environment.
    fn ensure_text_generator_models(
        &mut self,
        environment: &SharedString,
        id: &str,
        config: &Value,
        cx: &mut Context<Self>,
    ) {
        // Only a checked session refuses; a pending or failed check waits for its retry.
        let session = self.settings_ui.grants.get(environment.as_ref()).and_then(|(_, load)| load.session());
        let denied = session.is_some_and(|session| !session.grants(SETTINGS_WRITE));
        let key = (environment.clone(), id.to_owned());
        if self.settings_ui.text_generators.lists.get(&key).is_some_and(|listed| listed.current(config, denied)) {
            return;
        }
        let Some(client) = self.env_client(environment) else { return };
        if let Some(refusal) = self.list_models_refusal(environment) {
            if denied {
                self.settings_ui.text_generators.record(environment, id, config, ModelList::Failed(refusal), true);
            }
            return;
        }
        let request = self.settings_ui.text_generators.record(environment, id, config, ModelList::Loading, false);
        let (environment, id) = (environment.clone(), id.to_owned());
        cx.spawn(async move |this, cx| {
            let result = client.request(LIST_MODELS, json!({ "id": id })).await;
            let list = match result {
                Ok(reply) => ModelList::Ready(models_of(&reply)),
                Err(error) => ModelList::Failed(message(&error)),
            };
            let _ = this.update(cx, |this, cx| {
                if this.settings_ui.text_generators.settle(environment, id, request, list) {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// `server.listTextGeneratorModels` spends the stored key, so it needs `settings:write` on
    /// `environment`: `None` when this connection has it, else the web's refusal.
    fn list_models_refusal(&self, environment: &str) -> Option<String> {
        (!self.environment_grants(environment, SETTINGS_WRITE))
            .then(|| format!("This connection requires {SETTINGS_WRITE}."))
    }

    fn reread_text_generator_models(&mut self, environment: &SharedString, id: &str, cx: &mut Context<Self>) {
        self.settings_ui.text_generators.forget(environment, id);
        cx.notify();
    }

    /// Providers › Text generators: each generator with its switch and actions, and Add.
    pub(super) fn render_text_generators(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let t = *tokens(cx);
        let environment = self.settings_environment()?;
        let info = self.env_info(&environment)?;
        if !text_generators_allowed(&info.managed_policy) {
            return None;
        }
        let read_only = self.providers_read_only();
        let generators = info.settings.get("textGenerators").as_object().cloned().unwrap_or_default();
        let mut rows: Vec<AnyElement> = Vec::new();
        if generators.is_empty() {
            rows.push(
                settings_row(
                    &t,
                    "No text generators yet",
                    Some(div().child("Add a model API to write thread titles, commit messages, and other short text without starting a coding agent.").into_any_element()),
                    None,
                )
                .into_any_element(),
            );
        }
        for (id, config) in generators {
            let name = display_name(&config);
            let enabled = config["enabled"] != false;
            let base_url = config["baseUrl"].as_str().unwrap_or_default();
            let address =
                base_url.strip_prefix("https://").or_else(|| base_url.strip_prefix("http://")).unwrap_or(base_url);
            let title = h_flex()
                .gap(px(10.))
                .child(generator_icon(config_brand(&config), &name, px(20.), &t))
                .child(name.clone());
            // Indented past the icon so it lines up with the name.
            let description = div()
                .pl(px(30.))
                .truncate()
                .child(format!("{}{address}", if enabled { "" } else { "Off · " }))
                .into_any_element();
            let control =
                (!read_only).then(|| self.render_generator_controls(&environment, &id, &config, &name, &t, cx));
            rows.push(settings_row(&t, title, Some(description), control).into_any_element());
        }
        let add = (!read_only).then(|| {
            Button::new("text-generators-add")
                .outline()
                .xsmall()
                .icon(Lucide::Plus)
                .label("Add generator")
                .on_click(cx.listener(|this, _, window, cx| this.open_text_generator_wizard(None, window, cx)))
                .into_any_element()
        });
        Some(section_with_action("text-generators", "Text generators", add, rows, &t))
    }

    fn render_generator_controls(
        &self,
        environment: &SharedString,
        id: &str,
        config: &Value,
        name: &str,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let view = cx.entity();
        let enabled = config["enabled"] != false;
        let (menu_id, menu_config) = (id.to_owned(), config.clone());
        let more = Button::new(ElementId::Name(format!("text-generator-more-{id}").into()))
            .ghost()
            .small()
            .size(px(28.))
            .text_color(t.muted_foreground)
            .icon(Icon::new(Lucide::EllipsisVertical).with_size(px(14.)))
            .tooltip(format!("More actions for {name}"))
            .dropdown_menu_with_anchor(Anchor::TopRight, {
                let error = t.error;
                move |menu, window, _| {
                    let (edit_id, edit_config) = (menu_id.clone(), menu_config.clone());
                    let (show_id, show_config) = (menu_id.clone(), menu_config.clone());
                    let (remove_id, remove_name) = (menu_id.clone(), display_name(&menu_config));
                    menu.item(PopupMenuItem::new("Edit").on_click(window.listener_for(
                        &view,
                        move |this, _, window, cx| {
                            this.open_text_generator_wizard(Some((edit_id.clone(), edit_config.clone())), window, cx);
                        },
                    )))
                    .item(PopupMenuItem::new("Show models").disabled(!enabled).on_click(window.listener_for(
                        &view,
                        move |this, _, window, cx| {
                            this.open_text_generator_models(show_id.clone(), show_config.clone(), window, cx);
                        },
                    )))
                    .item(
                        PopupMenuItem::element(move |_, _| div().text_color(error).child("Remove")).on_click(
                            window.listener_for(&view, move |this, _, window, cx| {
                                this.confirm_remove_text_generator(remove_id.clone(), remove_name.clone(), window, cx);
                            }),
                        ),
                    )
                }
            });
        let (switch_environment, switch_id, switch_config) = (environment.clone(), id.to_owned(), config.clone());
        let toggle = Switch::new(ElementId::Name(format!("text-generator-enabled-{id}").into()))
            .checked(enabled)
            .accessibility_label(format!("Enable {name}"))
            .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                if this.providers_read_only_on(Some(&switch_environment)) {
                    return;
                }
                let mut next = switch_config.clone();
                next["enabled"] = json!(*checked);
                let patch = json!({ "textGenerators": { switch_id.clone(): next } });
                this.update_settings_on(Some(switch_environment.clone()), patch, cx, |_, _, _| {});
            }));
        h_flex().gap(px(8.)).child(more).child(toggle).into_any_element()
    }

    fn confirm_remove_text_generator(&mut self, id: String, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(environment) = self.settings_environment() else { return };
        let label = self
            .env_info(&environment)
            .and_then(|info| info.environment_label.clone())
            .unwrap_or_else(|| "this environment".into());
        let workspace = cx.weak_entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let (workspace, environment, id) = (workspace.clone(), environment.clone(), id.clone());
            alert
                .entrance(DialogEntrance::None)
                .title(format!("Remove {name}?"))
                .description(format!(
                    "This deletes the API key stored on {label}. Anything set to use this generator falls back to the text generation model."
                ))
                .show_cancel(true)
                .ok_text("Remove")
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, _, cx| {
                    let patch = json!({ "textGenerators": { id.clone(): null } });
                    let environment = environment.clone();
                    let _ = workspace.update(cx, |this, cx| {
                        if !this.providers_read_only_on(Some(&environment)) {
                            this.update_settings_on(Some(environment), patch, cx, |_, _, _| {});
                        }
                    });
                    true
                })
        });
    }

    /// Opens the add wizard, or the edit wizard for `editing` (its id and saved config).
    fn open_text_generator_wizard(
        &mut self,
        editing: Option<(String, Value)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(environment) = self.settings_environment() else { return };
        if self.providers_read_only_on(Some(&environment)) {
            return;
        }
        let workspace = cx.weak_entity();
        // The wizard reads the workspace, which this call is still updating.
        window.defer(cx, move |window, cx| {
            let wizard = cx.new(|cx| GeneratorWizard::new(workspace, environment, editing, window, cx));
            window.open_dialog(cx, move |dialog, _, _| {
                dialog.entrance(DialogEntrance::None).p_0().w(px(576.)).centered().child(wizard.clone())
            });
        });
    }

    fn open_text_generator_models(&mut self, id: String, config: Value, window: &mut Window, cx: &mut Context<Self>) {
        let Some(environment) = self.settings_environment() else { return };
        let workspace = cx.weak_entity();
        let dialog = cx.new(|cx| ModelsDialog::new(workspace, environment, id, config, window, cx));
        window.open_dialog(cx, move |modal, _, cx| {
            dialog.update(cx, |this, cx| this.render_dialog(modal.entrance(DialogEntrance::None), cx))
        });
    }
}

/// `ProviderInstanceIcon` for a generator's brand; a custom endpoint has none, so it shows its
/// name's initials, marked with the protocol it speaks.
fn generator_icon(brand: &str, name: &str, size: Pixels, t: &Tokens) -> AnyElement {
    if let Some((path, tint)) = crate::assets::provider_icon(brand) {
        return svg().path(path).size(size).flex_none().text_color(tint.unwrap_or(t.foreground)).into_any_element();
    }
    let protocol = div()
        .absolute()
        .right(px(-5.))
        .bottom(px(-5.))
        .size(px(12.))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .border_1()
        .border_color(t.border)
        .bg(t.card)
        .child(svg().path("icons/app/openai.svg").size(px(8.)).text_color(t.foreground));
    div()
        .relative()
        .size(size)
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.))
        .border_1()
        .border_color(t.border)
        .bg(t.card)
        .text_size(px(9.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(t.foreground)
        .child(initials(name))
        .child(protocol)
        .into_any_element()
}

// MARK: Add and edit

const STEPS: &[&str] = &["Connect", "Use it"];

/// `SecretSourceInput` for the API key: a typed value, or a reference a secret provider reads.
#[derive(Debug, Default, PartialEq)]
struct KeySource {
    /// The committed `{provider, reference}`; `None` while the key is a typed value.
    secret: Option<Value>,
    /// A provider picked here; it applies once a reference for it is committed.
    pending: Option<String>,
    /// Reference text the provider can't read, kept in the field beside its message.
    rejected: Option<String>,
}

impl KeySource {
    fn of(api_key: Option<&Value>) -> Self {
        Self { secret: api_key.filter(|key| key.is_object()).cloned(), ..Self::default() }
    }

    /// The provider the key is read from; `None` is Value.
    fn provider(&self) -> Option<&str> {
        self.pending.as_deref().or_else(|| self.secret.as_ref()?["provider"].as_str())
    }

    /// The reference field's text.
    fn reference(&self) -> &str {
        let committed = self.secret.as_ref().filter(|secret| secret["provider"].as_str() == self.provider());
        self.rejected.as_deref().or_else(|| committed?["reference"].as_str()).unwrap_or_default()
    }

    /// The picked source and committed field can be saved (`onValidityChange`).
    fn valid(&self) -> bool {
        self.pending.is_none() && self.rejected.is_none()
    }

    fn pick(&mut self, source: Option<&str>) {
        self.rejected = None;
        match source {
            None => {
                self.pending = None;
                self.secret = None;
            }
            Some(source) => {
                let committed = self.secret.as_ref().is_some_and(|secret| secret["provider"] == source);
                self.pending = (!committed).then(|| source.to_owned());
            }
        }
    }

    /// Commits the reference field: blank clears a committed reference, and only a provider
    /// this build reads checks the syntax.
    fn commit(&mut self, text: &str, sources: &[SecretSource]) {
        let Some(provider) = self.provider().map(str::to_owned) else { return };
        let reference = text.trim();
        if reference.is_empty() {
            self.rejected = None;
            if self.pending.is_none() {
                self.secret = None;
            }
            return;
        }
        let known = sources.iter().any(|source| source.id == provider.as_str());
        let decoded = if known {
            secret_reference(&provider, reference)
        } else {
            Some(json!({ "provider": provider, "reference": reference }))
        };
        match decoded {
            Some(secret) => *self = Self { secret: Some(secret), ..Self::default() },
            None => self.rejected = Some(text.to_owned()),
        }
    }
}

/// `TextGeneratorWizard`: Connect checks the settings by listing the service's models and saves
/// the generator; Use it then shows each text task's model picker, so any of them can switch to
/// it. A typed key goes once to the environment's secret storage; settings keep a redaction marker.
struct GeneratorWizard {
    workspace: WeakEntity<Workspace>,
    environment: SharedString,
    /// The generator being edited: its id and saved config.
    existing: Option<(String, Value)>,
    step: usize,
    preset: usize,
    base_url: Entity<InputState>,
    key: Entity<InputState>,
    key_source: KeySource,
    /// The key's secret reference while it is read from a secret provider.
    reference: Entity<InputState>,
    label: Entity<InputState>,
    checking: bool,
    check_error: Option<String>,
    model_count: usize,
    /// Fixed once saved, so editing the form again after Back updates the same generator.
    saved_id: Option<String>,
    /// Each task's last test: the `instance:model` it tried and its error, if any.
    tests: HashMap<&'static str, (String, Option<String>)>,
    testing: HashSet<&'static str>,
    _subscriptions: Vec<Subscription>,
}

impl GeneratorWizard {
    fn new(
        workspace: WeakEntity<Workspace>,
        environment: SharedString,
        existing: Option<(String, Value)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let config = existing.as_ref().map(|(_, config)| config);
        let saved_url = config.and_then(|c| c["baseUrl"].as_str()).unwrap_or(PRESETS[0].base_url).to_owned();
        let saved_label = config.and_then(|c| c["label"].as_str()).unwrap_or_default().to_owned();
        let configured = config.is_some_and(|c| c["apiKey"].as_str().is_some_and(|key| !key.is_empty()));
        let base_url =
            cx.new(|cx| InputState::new(window, cx).placeholder("http://localhost:11434/v1").default_value(saved_url));
        let key = cx.new(|cx| {
            InputState::new(window, cx).masked(true).placeholder(if configured {
                "Stored secret, enter a new value to replace"
            } else {
                ""
            })
        });
        let label_placeholder = config.map_or_else(|| PRESETS[0].label.to_owned(), display_name);
        let label = cx.new(|cx| InputState::new(window, cx).placeholder(label_placeholder).default_value(saved_label));
        let key_source = KeySource::of(config.map(|c| &c["apiKey"]));
        let reference_text = key_source.reference().to_owned();
        let reference =
            cx.new(|cx| InputState::new(window, cx).placeholder(REFERENCE_PLACEHOLDER).default_value(reference_text));
        let mut subscriptions: Vec<Subscription> = [&base_url, &key, &label]
            .into_iter()
            .map(|input| {
                cx.subscribe_in(input, window, |this, _, event: &InputEvent, window, cx| match event {
                    InputEvent::PressEnter { .. } => this.connect(window, cx),
                    InputEvent::Change => {
                        this.check_error = None;
                        cx.notify();
                    }
                    _ => {}
                })
            })
            .collect();
        // A `DraftInput`: the reference applies on blur or Enter.
        subscriptions.push(cx.subscribe_in(
            &reference,
            window,
            |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { .. } => this.connect(window, cx),
                InputEvent::Blur => this.commit_reference(window, cx),
                InputEvent::Change => {
                    this.check_error = None;
                    cx.notify();
                }
                InputEvent::Focus => {}
            },
        ));
        if existing.is_none() {
            key.update(cx, |key, cx| key.focus(window, cx));
        }
        let saved_id = existing.as_ref().map(|(id, _)| id.clone());
        Self {
            workspace,
            environment,
            existing,
            step: 0,
            preset: 0,
            base_url,
            key,
            key_source,
            reference,
            label,
            checking: false,
            check_error: None,
            model_count: 0,
            saved_id,
            tests: HashMap::new(),
            testing: HashSet::new(),
            _subscriptions: subscriptions,
        }
    }

    fn existing_config(&self) -> Option<&Value> {
        self.existing.as_ref().map(|(_, config)| config)
    }

    /// Presets know their URL; only a custom endpoint, or one already saved as such, asks for it.
    fn shows_base_url(&self) -> bool {
        match self.existing_config() {
            None => self.preset == CUSTOM,
            Some(config) => !matches!(config_brand(config), "openai" | "anthropic" | "openrouter"),
        }
    }

    fn key_required(&self) -> bool {
        self.existing.is_none() && self.preset != CUSTOM
    }

    /// `decodeTextGenerator` over the form; `None` while the URL is not one the server accepts.
    /// A blank key keeps the saved one: its redaction marker, or its secret reference.
    fn config(&self, cx: &App) -> Option<Value> {
        let read = |input: &Entity<InputState>| input.read(cx).value().trim().to_owned();
        let existing = self.existing_config();
        let base_url = read(&self.base_url);
        if !valid_base_url(&base_url) {
            return None;
        }
        let typed = read(&self.key);
        // A saved literal key arrives redacted; sending the marker back keeps it.
        let stored = existing.and_then(|c| c["apiKey"].as_str()).unwrap_or_default();
        let api_key = match &self.key_source.secret {
            Some(secret) => secret.clone(),
            None => json!(if typed.is_empty() { stored } else { typed.as_str() }),
        };
        let kind = existing.and_then(|c| c["kind"].as_str()).unwrap_or(PRESETS[self.preset].kind);
        let mut config = json!({
            "kind": kind,
            "baseUrl": base_url,
            "apiKey": api_key,
            "enabled": existing.is_none_or(|c| c["enabled"] != false),
        });
        let label = read(&self.label);
        if !label.is_empty() {
            config["label"] = json!(label);
        }
        Some(config)
    }

    fn can_connect(&self, cx: &App) -> bool {
        self.key_source.valid() && self.config(cx).is_some_and(|config| !self.key_required() || config["apiKey"] != "")
    }

    /// `useSecretSources`: the secret providers the key can be read from; none unless the build
    /// has the secrets feature and the environment reads references.
    fn key_sources(&self, cx: &App) -> Vec<SecretSource> {
        if !crate::app::pages::settings::secrets_enabled() {
            return Vec::new();
        }
        let Some(workspace) = self.workspace.upgrade() else { return Vec::new() };
        let Some(info) = workspace.read(cx).env_info(&self.environment) else { return Vec::new() };
        secret_sources(info.config["secretReferences"] == true, info.settings.get("secretProviders"))
    }

    fn pick_key_source(&mut self, source: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        if self.checking {
            return;
        }
        self.key_source.pick(source.as_deref());
        self.check_error = None;
        let text = self.key_source.reference().to_owned();
        self.reference.update(cx, |input, cx| input.set_value(text, window, cx));
        let focus = if self.key_source.provider().is_some() { &self.reference } else { &self.key };
        focus.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    fn commit_reference(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.key_source.provider().is_none() {
            return;
        }
        let text = self.reference.read(cx).value().to_string();
        let sources = self.key_sources(cx);
        self.key_source.commit(&text, &sources);
        if self.key_source.secret.is_some() && self.key_source.rejected.is_none() {
            // The committed reference replaces a typed key, as the web's single draft does.
            self.key.update(cx, |input, cx| input.set_value("", window, cx));
        }
        cx.notify();
    }

    fn select_preset(&mut self, preset: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.checking || self.preset == preset {
            return;
        }
        self.preset = preset;
        self.check_error = None;
        let url = PRESETS[preset].base_url;
        self.base_url.update(cx, |input, cx| input.set_value(url, window, cx));
        let placeholder = if preset == CUSTOM { "e.g. My local model" } else { PRESETS[preset].label };
        self.label.update(cx, |input, cx| input.set_placeholder(placeholder, window, cx));
        let focus = if preset == CUSTOM { &self.base_url } else { &self.key };
        focus.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    fn navigate(&mut self, step: usize, _: &mut Window, cx: &mut Context<Self>) {
        if step <= self.step && !self.checking {
            self.step = step;
            cx.notify();
        }
    }

    /// Lists the models the form reaches; on success saves the generator and moves to Use it.
    fn connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.step != 0 || self.checking {
            return;
        }
        // Pressing Connect blurs the reference field first on the web, which commits it.
        if self.key_source.reference() != self.reference.read(cx).value().as_ref() {
            self.commit_reference(window, cx);
        }
        if !self.can_connect(cx) {
            return;
        }
        let Some(config) = self.config(cx) else { return };
        let Some(workspace) = self.workspace.upgrade() else { return };
        let environment = self.environment.clone();
        let (client, id, refusal) = {
            let workspace = workspace.read(cx);
            if workspace.providers_read_only_on(Some(&environment)) {
                return;
            }
            let Some(info) = workspace.env_info(&environment) else { return };
            let id = self
                .saved_id
                .clone()
                .unwrap_or_else(|| next_id(PRESETS[self.preset].id, &info.settings, &info.providers));
            (workspace.env_client(&environment), id, workspace.list_models_refusal(&environment))
        };
        let Some(client) = client else { return };
        if let Some(refusal) = refusal {
            self.check_error = Some(refusal);
            cx.notify();
            return;
        }
        self.checking = true;
        self.check_error = None;
        cx.notify();
        let payload = json!({ "id": id, "config": config });
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request(LIST_MODELS, payload).await;
            let _ = this.update_in(cx, |this, _, cx| {
                this.checking = false;
                match result {
                    Err(error) => this.check_error = Some(message(&error)),
                    Ok(reply) => {
                        let patch = json!({ "textGenerators": { id.clone(): config } });
                        workspace.update(cx, |workspace, cx| {
                            if workspace.providers_read_only_on(Some(&environment)) {
                                return;
                            }
                            let (saved_environment, saved_id) = (environment.clone(), id.clone());
                            workspace.update_settings_on(Some(environment.clone()), patch, cx, move |w, ok, cx| {
                                if ok {
                                    w.reread_text_generator_models(&saved_environment, &saved_id, cx);
                                }
                            });
                        });
                        this.saved_id = Some(id);
                        this.model_count = models_of(&reply).len();
                        this.step = 1;
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// `testTextGeneratorModel`: has the selected generator model answer a tiny prompt.
    fn test(&mut self, task: &'static str, selection: ModelSelection, window: &mut Window, cx: &mut Context<Self>) {
        if !self.testing.insert(task) {
            return;
        }
        let Some(workspace) = self.workspace.upgrade() else {
            self.testing.remove(task);
            return;
        };
        let Some(client) = workspace.read(cx).env_client(&self.environment) else {
            self.testing.remove(task);
            return;
        };
        let key = format!("{}:{}", selection.instance_id, selection.model);
        if let Some(refusal) = workspace.read(cx).list_models_refusal(&self.environment) {
            self.testing.remove(task);
            self.tests.insert(task, (key, Some(refusal)));
            cx.notify();
            return;
        }
        cx.notify();
        let payload = json!({ "id": selection.instance_id, "test": true, "model": selection.model });
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request(LIST_MODELS, payload).await;
            let _ = this.update(cx, |this, cx| {
                this.testing.remove(task);
                this.tests.insert(task, (key, result.err().map(|error| message(&error))));
                cx.notify();
            });
        })
        .detach();
    }

    fn render_connect(&self, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let field = |label: &'static str, input: &Entity<InputState>| {
            v_flex()
                .gap(px(6.))
                .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(label))
                .child(Input::new(input).small().disabled(self.checking))
        };
        let services = self.existing.is_none().then(|| {
            let cards = PRESETS.iter().enumerate().map(|(index, preset)| {
                let selected = index == self.preset;
                let icon = if index == CUSTOM {
                    Icon::new(Lucide::Server).with_size(px(20.)).into_any_element()
                } else {
                    generator_icon(brand(preset.kind, preset.base_url), preset.label, px(20.), t)
                };
                Button::new(SharedString::from(format!("text-generator-service-{}", preset.id)))
                    .ghost()
                    .flex_1()
                    .h(px(72.))
                    .rounded(px(8.))
                    .disabled(self.checking)
                    .selected(selected)
                    .map(|card| {
                        if selected {
                            card.border_1().border_color(t.primary).bg(t.primary.opacity(0.05)).text_color(t.foreground)
                        } else {
                            card.border_1()
                                .border_color(if t.dark { white(0.1) } else { black(0.1) })
                                .bg(t.background)
                                .text_color(t.muted_foreground)
                        }
                    })
                    .child(
                        v_flex().items_center().gap(px(8.)).child(icon).child(
                            div()
                                .text_sm()
                                .when(selected, |el| el.font_weight(FontWeight::MEDIUM))
                                .child(if index == CUSTOM { "Custom" } else { preset.label }),
                        ),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| this.select_preset(index, window, cx)))
            });
            v_flex()
                .gap(px(6.))
                .child(div().text_sm().font_weight(FontWeight::MEDIUM).child("Service"))
                .child(h_flex().gap(px(8.)).children(cards))
        });
        v_flex()
            .gap(px(16.))
            .children(services)
            .when(self.shows_base_url(), |el| el.child(field("Base URL", &self.base_url)))
            .child(self.render_key_field(t, cx))
            .child(field("Name (optional)", &self.label))
            .children(self.check_error.clone().map(|error| div().text_sm().text_color(t.error_foreground).child(error)))
            .into_any_element()
    }

    /// The API key: its source (`SecretSourceSelect`) when there is one to pick, then the typed
    /// value, or the reference with its message.
    fn render_key_field(&self, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let sources = self.key_sources(cx);
        let provider = self.key_source.provider().map(str::to_owned);
        let picker = (!sources.is_empty() || provider.is_some())
            .then(|| self.render_key_source(provider.as_deref(), &sources, t, cx));
        let value: AnyElement = match provider {
            None => Input::new(&self.key).small().disabled(self.checking).into_any_element(),
            Some(_) => v_flex()
                .gap(px(4.))
                .child(
                    div().font_family(t.fonts.mono).child(Input::new(&self.reference).small().disabled(self.checking)),
                )
                .children(
                    self.key_source
                        .rejected
                        .is_some()
                        .then(|| div().text_xs().text_color(t.error_foreground).child(INVALID_REFERENCE)),
                )
                .into_any_element(),
        };
        let label = if self.key_required() { "API key" } else { "API key (optional)" };
        v_flex()
            .gap(px(6.))
            .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(label))
            .child(h_flex().items_start().gap(px(6.)).children(picker).child(div().flex_1().min_w_0().child(value)))
            .into_any_element()
    }

    /// `SecretSourceSelect`: Value, each source, and a saved provider no longer among them.
    fn render_key_source(
        &self,
        provider: Option<&str>,
        sources: &[SecretSource],
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let known = provider.is_none_or(|id| sources.iter().any(|source| source.id == id));
        let one_password = || Icon::empty().path("icons/app/onepassword.svg");
        let icon = match provider {
            None => Icon::new(Lucide::Type),
            Some(_) if known => one_password(),
            Some(_) => Icon::new(Lucide::KeyRound),
        };
        let title: SharedString = match provider {
            None => "Value".into(),
            Some(id) => {
                sources.iter().find(|source| source.id == id).map_or_else(|| id.to_owned().into(), |s| s.label.clone())
            }
        };
        let view = cx.entity();
        let sources = sources.to_vec();
        let unknown = provider.filter(|_| !known).map(str::to_owned);
        Button::new("text-generator-key-source")
            .outline()
            .small()
            .disabled(self.checking)
            .icon(icon.size(px(14.)).text_color(t.muted_foreground))
            .tooltip(format!("API key source: {title}"))
            .dropdown_menu(move |menu, window, _| {
                let entry = |icon: Icon, text: SharedString, source: Option<String>| {
                    PopupMenuItem::element(move |_, _| {
                        h_flex().gap(px(8.)).child(icon.clone().size(px(14.))).child(text.clone())
                    })
                    .on_click(window.listener_for(
                        &view,
                        move |this: &mut Self, _, window, cx| {
                            this.pick_key_source(source.clone(), window, cx);
                        },
                    ))
                };
                let mut menu = menu.item(entry(Icon::new(Lucide::Type), "Value".into(), None));
                for source in &sources {
                    menu = menu.item(entry(one_password(), source.label.clone(), Some(source.id.to_string())));
                }
                if let Some(id) = &unknown {
                    menu = menu.item(entry(Icon::new(Lucide::KeyRound), id.clone().into(), Some(id.clone())));
                }
                menu
            })
            .into_any_element()
    }

    fn render_use_it(&mut self, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let name =
            self.config(cx).map_or_else(|| PRESETS[self.preset].label.to_owned(), |config| display_name(&config));
        let count = self.model_count;
        let intro = format!(
            "{name} is connected and lists {count} {}. Switch any of these to it, or keep what they use now.",
            if count == 1 { "model" } else { "models" }
        );
        let rows: Vec<AnyElement> =
            TASKS.iter().filter_map(|&(key, label)| self.render_task(key, label, t, cx)).collect();
        v_flex()
            .gap(px(16.))
            .child(div().text_sm().text_color(t.muted_foreground).child(intro))
            .child(v_flex().gap(px(16.)).children(rows))
            .into_any_element()
    }

    /// `TextGenerationTaskPicker`: one task's model, saved as it changes, picked from every
    /// text generation target.
    fn render_task(
        &mut self,
        key: &'static str,
        label: &'static str,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let workspace = self.workspace.upgrade()?;
        let environment = self.environment.clone();
        let (own, selection, testable) = {
            let settings = &workspace.read(cx).env_info(&environment)?.settings;
            let own = selection_of(settings.get(key));
            // Source control and voice follow the text generation model until they get their own.
            let selection = own.clone().or_else(|| selection_of(settings.get("textGenerationModelSelection")))?;
            let testable = settings.get("textGenerators").get(&selection.instance_id).is_some();
            (own, selection, testable)
        };
        let picker = workspace.update(cx, |w, cx| {
            let extra = w.text_generation_targets(Some(&environment), &[Some(&selection)], cx);
            let id = SharedString::from(format!("text-generator-task-{key}"));
            w.refresh_picker_extra(&id, &extra);
            let trigger =
                w.picker_trigger_among(id.clone(), Some(&selection), None, Some(&environment), Some(&extra), cx);
            let pick_environment = environment.clone();
            let on_pick: OnPick = Rc::new(
                move |w: &mut Workspace, picked: ModelSelection, _: &mut Window, cx: &mut Context<Workspace>| {
                    if !w.environment_grants(&pick_environment, SETTINGS_WRITE) {
                        return;
                    }
                    let picked = ModelSelection { options: None, ..picked };
                    let patch = json!({ key: picked.to_json() });
                    w.update_settings_on(Some(pick_environment.clone()), patch, cx, |_, _, _| {});
                },
            );
            let host = PickHost {
                environment: Some(environment.clone()),
                current: Some(selection.clone()),
                include: generates_text,
                extra: Some(extra),
                on_pick,
            };
            w.undocked_model_picker(id, trigger, host, cx)
        });
        let test_key = format!("{}:{}", selection.instance_id, selection.model);
        let shown = self.tests.get(key).filter(|(tried, _)| *tried == test_key).map(|(_, error)| error.clone());
        let status: Option<AnyElement> = match shown {
            Some(Some(error)) => Some(div().text_xs().text_color(t.error_foreground).child(error).into_any_element()),
            Some(None) => Some(
                div()
                    .text_xs()
                    .text_color(t.muted_foreground)
                    .child(format!("{} answered. It works.", selection.model))
                    .into_any_element(),
            ),
            None if own.is_none() => Some(
                div()
                    .text_xs()
                    .text_color(t.muted_foreground)
                    .child("Follows the text generation model")
                    .into_any_element(),
            ),
            None => None,
        };
        let testing = self.testing.contains(key);
        // Only a generator's model can be tried cheaply; agents start a whole session.
        let test = testable.then(|| {
            Button::new(SharedString::from(format!("text-generator-test-{key}")))
                .outline()
                .xsmall()
                .disabled(testing)
                .child(div().relative().child(div().when(testing, |el| el.opacity(0.)).child("Test")).when(
                    testing,
                    |el| {
                        el.child(
                            div()
                                .absolute()
                                .inset_0()
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(Spinner::new().xsmall()),
                        )
                    },
                ))
                .on_click(cx.listener(move |this, _, window, cx| this.test(key, selection.clone(), window, cx)))
        });
        Some(
            h_flex()
                .gap(px(8.))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .pr(px(8.))
                        .gap(px(2.))
                        .child(div().text_sm().child(label))
                        .children(status),
                )
                .child(picker)
                // A fixed slot, so pickers and Test buttons line up in columns.
                .child(h_flex().w(px(52.)).flex_none().justify_end().children(test))
                .into_any_element(),
        )
    }

    fn render_footer(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.step > 0 {
            return footer()
                .justify_between()
                .child(
                    Button::new("text-generator-back")
                        .ghost()
                        .small()
                        .label("Back")
                        .on_click(cx.listener(|this, _, window, cx| this.navigate(0, window, cx))),
                )
                .child(
                    Button::new("text-generator-done")
                        .primary()
                        .small()
                        .label("Done")
                        .on_click(|_, window, cx| window.close_dialog(cx)),
                )
                .into_any_element();
        }
        let label = if self.checking {
            "Connecting"
        } else if self.existing.is_some() {
            "Save and continue"
        } else {
            "Connect"
        };
        footer()
            .child(
                Button::new("text-generator-cancel")
                    .outline()
                    .small()
                    .label("Cancel")
                    .on_click(|_, window, cx| window.close_dialog(cx)),
            )
            .child(
                Button::new("text-generator-connect")
                    .primary()
                    .small()
                    .loading(self.checking)
                    .label(label)
                    .disabled(self.checking || !self.can_connect(cx))
                    .on_click(cx.listener(|this, _, window, cx| this.connect(window, cx))),
            )
            .into_any_element()
    }
}

impl Render for GeneratorWizard {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *tokens(cx);
        let title = self
            .existing_config()
            .map_or_else(|| "Add a text generator".to_owned(), |c| format!("Edit {}", display_name(c)));
        let step = self.step;
        let steps = wizard_steps("text-generator-step", STEPS, step, |index| index <= step, &t, cx, Self::navigate);
        let header = v_flex()
            .gap(px(8.))
            .p(px(24.))
            .pb(px(12.))
            .child(div().text_xl().line_height(px(27.5)).font_weight(FontWeight::SEMIBOLD).child(title))
            .child(div().text_sm().text_color(t.muted_foreground).child(
                "Writes thread titles, commit messages, and other short text without starting a coding agent. Your API key is stored on this environment.",
            ))
            .child(steps);
        let body = if self.step == 0 { self.render_connect(&t, cx) } else { self.render_use_it(&t, cx) };
        let max_h = window.viewport_size().height - px(64.);
        v_flex()
            .id("text-generator-wizard")
            .max_h(max_h)
            .overflow_y_scroll()
            .child(header)
            .child(panel(&t).child(body))
            .child(self.render_footer(cx))
    }
}

// MARK: Show models

/// `TextGeneratorModelsDialog`: a saved generator's models, searchable past eight, with a
/// refresh that asks the API again.
struct ModelsDialog {
    workspace: WeakEntity<Workspace>,
    environment: SharedString,
    id: String,
    config: Value,
    query: Entity<InputState>,
    refreshing: bool,
    _subscription: Subscription,
}

impl ModelsDialog {
    fn new(
        workspace: WeakEntity<Workspace>,
        environment: SharedString,
        id: String,
        config: Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search models"));
        let subscription = cx.subscribe_in(&query, window, |_, _, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        Self { workspace, environment, id, config, query, refreshing: false, _subscription: subscription }
    }

    /// `refreshTextGeneratorModels`: skips the server's cache, then has open pickers read again.
    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.refreshing {
            return;
        }
        let Some(workspace) = self.workspace.upgrade() else { return };
        let Some(client) = workspace.read(cx).env_client(&self.environment) else { return };
        let (environment, id, name) = (self.environment.clone(), self.id.clone(), display_name(&self.config));
        if let Some(refusal) = workspace.read(cx).list_models_refusal(&environment) {
            let title = SharedString::from(format!("Couldn't load {name} models"));
            window.push_notification(Notification::error(refusal).title(title), cx);
            return;
        }
        self.refreshing = true;
        cx.notify();
        let payload = json!({ "id": id, "refresh": true });
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request(LIST_MODELS, payload).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.refreshing = false;
                workspace.update(cx, |w, cx| w.reread_text_generator_models(&environment, &id, cx));
                if let Err(error) = result {
                    let title = SharedString::from(format!("Couldn't load {name} models"));
                    window.push_notification(Notification::error(message(&error)).title(title), cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn render_dialog(
        &mut self,
        modal: gpui_kit::component::dialog::Dialog,
        cx: &mut Context<Self>,
    ) -> gpui_kit::component::dialog::Dialog {
        let t = *tokens(cx);
        let name = display_name(&self.config);
        let list = self.workspace.upgrade().and_then(|workspace| {
            workspace.update(cx, |w, cx| {
                w.ensure_text_generator_models(&self.environment, &self.id, &self.config, cx);
                match w.list_models_refusal(&self.environment) {
                    Some(refusal) => Some(ModelList::Failed(refusal)),
                    None => w.settings_ui.text_generators.list(&self.environment, &self.id).cloned(),
                }
            })
        });
        let description = match &list {
            Some(ModelList::Ready(models)) => format!(
                "{} {} available. Pick one in a text generation model picker.",
                models.len(),
                if models.len() == 1 { "model" } else { "models" }
            ),
            Some(ModelList::Failed(_)) => "The model list couldn't load.".to_owned(),
            _ => "Loading the model list.".to_owned(),
        };
        let body: AnyElement = match &list {
            Some(ModelList::Failed(error)) => {
                div().text_sm().text_color(t.error_foreground).child(error.clone()).into_any_element()
            }
            Some(ModelList::Ready(models)) => self.render_list(models, &t, cx),
            _ => Spinner::new().small().color(t.muted_foreground).into_any_element(),
        };
        modal
            .w(px(448.))
            .title(format!("{name} models"))
            .child(
                v_flex()
                    .gap(px(16.))
                    .child(div().text_sm().text_color(t.muted_foreground).child(description))
                    .child(body),
            )
            .footer(
                h_flex()
                    .w_full()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("text-generator-models-refresh")
                            .outline()
                            .small()
                            .icon(Lucide::RefreshCw)
                            .label("Refresh")
                            .loading(self.refreshing)
                            .disabled(self.refreshing)
                            .on_click(cx.listener(|this, _, window, cx| this.refresh(window, cx))),
                    )
                    .child(
                        Button::new("text-generator-models-done")
                            .primary()
                            .small()
                            .label("Done")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    ),
            )
    }

    fn render_list(&self, models: &[Model], t: &Tokens, cx: &App) -> AnyElement {
        let query = self.query.read(cx).value().trim().to_lowercase();
        let shown: Vec<&Model> = models
            .iter()
            .filter(|m| {
                query.is_empty() || m.id.to_lowercase().contains(&query) || m.name.to_lowercase().contains(&query)
            })
            .collect();
        let rows: Vec<AnyElement> = shown
            .iter()
            .enumerate()
            .map(|(index, model)| {
                v_flex()
                    .px(px(12.))
                    .py(px(8.))
                    .when(index > 0, |el| el.border_t_1().border_color(t.border.opacity(0.6)))
                    .child(div().text_sm().truncate().child(model.name.clone()))
                    .when(model.name != model.id, |el| {
                        el.child(
                            div()
                                .text_xs()
                                .font_family(t.fonts.mono)
                                .text_color(t.muted_foreground)
                                .truncate()
                                .child(model.id.clone()),
                        )
                    })
                    .into_any_element()
            })
            .collect();
        let list = div()
            .id("text-generator-models")
            .max_h(px(320.))
            .overflow_y_scroll()
            .rounded(px(8.))
            .border_1()
            .border_color(t.border)
            .map(|el| {
                if rows.is_empty() {
                    el.child(
                        div().px(px(12.)).py(px(8.)).text_sm().text_color(t.muted_foreground).child("No models match."),
                    )
                } else {
                    el.children(rows)
                }
            });
        v_flex()
            .gap(px(8.))
            .when(models.len() > 8, |el| el.child(Input::new(&self.query).small()))
            .child(list)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_api_key_picks_a_secret_source_and_commits_references_like_the_web() {
        let work = SecretSource { id: "work".into(), label: "Work".into() };
        let sources = [work];
        let saved = json!({"provider": "work", "reference": "op://Vault/Item/key"});
        let mut key = KeySource::of(Some(&saved));
        assert_eq!((key.provider(), key.reference(), key.valid()), (Some("work"), "op://Vault/Item/key", true));

        key.pick(Some("home"));
        assert_eq!((key.provider(), key.reference(), key.valid()), (Some("home"), "", false), "waits for a reference");
        key.commit("", &sources);
        assert!(!key.valid(), "a blank reference for a new source is not a key");
        key.commit("op://Vault/New/key", &sources);
        assert_eq!(key.secret, Some(json!({"provider": "home", "reference": "op://Vault/New/key"})));
        assert!(key.valid(), "a source no longer configured takes any reference");

        key.pick(Some("work"));
        key.commit("not a reference", &sources);
        assert_eq!((key.reference(), key.valid()), ("not a reference", false), "1Password checks its syntax");
        key.commit(" op://Vault/Item/key ", &sources);
        assert_eq!(key.secret, Some(saved.clone()));
        assert!(key.valid());

        key.commit("", &sources);
        assert_eq!((key.secret.as_ref(), key.provider()), (None, None), "clearing it returns to Value");
        assert_eq!(KeySource::of(Some(&json!("********"))), KeySource::default(), "a literal key is a Value");
        let mut key = KeySource::of(Some(&saved));
        key.pick(None);
        assert_eq!((key.secret.as_ref(), key.valid()), (None, true));
    }

    #[test]
    fn base_urls_follow_the_contract() {
        for url in ["https://api.openai.com/v1", "http://localhost:11434/v1", "http://192.168.1.4:1234"] {
            assert!(valid_base_url(url), "{url}");
        }
        for url in [
            "",
            "ftp://host/v1",
            "https://user:pw@host/v1",
            "https://host/v1?x=1",
            "https://host/v1#a",
            " https://h",
            "nope",
        ] {
            assert!(!valid_base_url(url), "{url}");
        }
    }

    #[test]
    fn names_and_brands_follow_the_service() {
        let config = |kind: &str, base_url: &str| json!({"kind": kind, "baseUrl": base_url});
        assert_eq!(display_name(&config("openaiCompatible", "https://openrouter.ai/api/v1")), "OpenRouter");
        assert_eq!(display_name(&config("openaiCompatible", "http://localhost:11434/v1")), "localhost:11434");
        assert_eq!(display_name(&json!({"label": "Mine", "kind": "anthropic", "baseUrl": "x"})), "Mine");
        assert_eq!(brand("anthropic", "https://proxy.example/v1"), "anthropic");
        assert_eq!(brand("openaiCompatible", "https://eu.openrouter.ai/api/v1"), "openrouter");
        assert_eq!(brand("openaiCompatible", "https://api.openai.com/v1"), "openai");
        assert_eq!(brand("openaiCompatible", "http://box:11434/v1"), "ollama");
        assert_eq!(brand("openaiCompatible", "http://lmstudio.local:1234/v1"), "openaiCompatible");
    }

    #[test]
    fn new_ids_skip_instances_drivers_and_generators() {
        let settings = json!({
            "providerInstances": {"openai": {"driver": "codex"}},
            "textGenerators": {"openai-2": {}},
        });
        let providers: Vec<Provider> = serde_json::from_value(json!([{"instanceId": "openai-3"}])).unwrap();
        assert_eq!(next_id("openai", &settings, &providers), "openai-4");
        assert_eq!(next_id("codex", &json!({}), &[]), "codex-2", "a driver's default instance id is taken");
        assert_eq!(next_id("anthropic", &json!({}), &[]), "anthropic");
    }

    #[test]
    fn snapshots_keep_selected_models_and_report_failures() {
        let config = json!({"kind": "openaiCompatible", "baseUrl": "https://openrouter.ai/api/v1", "enabled": true});
        let ready = ModelList::Ready(vec![Model { id: "a".into(), name: "A".into() }]);
        let provider = snapshot("or", &config, Some(&ready), &["b", "a", "b"]).unwrap();
        let slugs: Vec<&str> = provider.models.iter().map(|m| m.slug.as_str()).collect();
        assert_eq!(slugs, ["a", "b"]);
        assert_eq!((provider.driver.as_str(), provider.status.as_str()), ("openrouter", "ready"));
        assert_eq!(provider.supports_text_generation, Some(true));

        let failed = snapshot("or", &config, Some(&ModelList::Failed("bad key".into())), &[]).unwrap();
        assert_eq!((failed.status.as_str(), failed.message.as_deref()), ("error", Some("bad key")));
        assert!(snapshot("or", &config, Some(&ModelList::Loading), &[]).unwrap().models.is_empty());
    }

    #[test]
    fn generators_replace_providers_with_their_id() {
        let providers: Vec<Provider> = serde_json::from_value(json!([
            {"instanceId": "codex", "driver": "codex", "enabled": true},
            {"instanceId": "openrouter", "driver": "opencode", "enabled": true},
            {"instanceId": "claude", "driver": "claudeAgent", "enabled": true},
            {"instanceId": "replay", "driver": "acpRegistry", "supportsTextGeneration": false},
        ]))
        .unwrap();
        let config = json!({"kind": "openaiCompatible", "baseUrl": "https://openrouter.ai/api/v1"});
        // A disabled generator lists no models but still owns its id, like the server's routing.
        let configured = json!({"openrouter": config, "claude": {"kind": "anthropic", "enabled": false}});
        let generators = [snapshot("openrouter", &config, None, &[]).unwrap()];
        let ids: Vec<(&str, &str)> = writing_providers(&providers, &configured, &generators)
            .iter()
            .map(|p| (p.instance_id.as_str(), p.driver.as_str()))
            .collect();
        assert_eq!(ids, [("codex", "codex"), ("openrouter", "openrouter")]);
    }

    #[test]
    fn a_stale_or_forgotten_list_reply_is_dropped() {
        let mut catalog = TextGeneratorCatalog::default();
        let environment = SharedString::from("env");
        let listed = |request| Listed {
            kind: "openaiCompatible".into(),
            base_url: "https://x".into(),
            api_key: json!(""),
            request,
            list: ModelList::Loading,
            refused: false,
        };
        catalog.lists.insert((environment.clone(), "g".into()), listed(2));
        assert!(!catalog.settle(environment.clone(), "g".into(), 1, ModelList::Failed("old".into())));
        assert!(catalog.settle(environment.clone(), "g".into(), 2, ModelList::Ready(Vec::new())));
        assert_eq!(catalog.list(&environment, "g"), Some(&ModelList::Ready(Vec::new())));
        catalog.forget(&environment, "g");
        assert!(!catalog.settle(environment.clone(), "g".into(), 2, ModelList::Ready(Vec::new())));
        assert_eq!(catalog.list(&environment, "g"), None);
        assert!(listed(0).asked_with(&json!({"kind": "openaiCompatible", "baseUrl": "https://x", "apiKey": ""})));
        assert!(!listed(0).asked_with(&json!({"kind": "openaiCompatible", "baseUrl": "https://y", "apiKey": ""})));
    }

    #[test]
    fn a_pick_resolves_against_models_listed_after_the_picker_opened() {
        use crate::app::pages::settings::model_select::resolve_text_generation;
        let mut catalog = TextGeneratorCatalog::default();
        let environment = SharedString::from("env");
        let generators = json!({"g": {"kind": "openaiCompatible", "baseUrl": "https://x", "apiKey": ""}});
        let request = catalog.record(&environment, "g", &generators["g"], ModelList::Loading, false);
        let stored = ModelSelection { instance_id: "g".into(), model: "a".into(), options: None };
        let pick = ModelSelection { model: "b".into(), ..stored.clone() };
        let resolve =
            |targets: &[Provider]| resolve_text_generation(Some(&pick), &writing_providers(&[], &generators, targets));
        let opened = catalog.targets(&environment, &generators, &[Some(&stored)]);
        assert_eq!(resolve(&opened).unwrap().model, "a", "the catalog from opening lacks b");
        let models = ["a", "b"].map(|id| Model { id: id.into(), name: id.into() }).to_vec();
        assert!(catalog.settle(environment.clone(), "g".into(), request, ModelList::Ready(models)));
        let current = catalog.targets(&environment, &generators, &[Some(&stored)]);
        assert_eq!(resolve(&current), Some(pick.clone()), "the pick keeps a model listed since");
    }

    #[test]
    fn a_refused_list_shows_why_until_the_grant_changes() {
        let mut catalog = TextGeneratorCatalog::default();
        let environment = SharedString::from("env");
        let config = json!({"kind": "openaiCompatible", "baseUrl": "https://x", "apiKey": ""});
        let generators = json!({ "g": config.clone() });
        let refusal = ModelList::Failed("This connection requires settings:write.".into());
        catalog.record(&environment, "g", &config, refusal, true);
        let stored = ModelSelection { instance_id: "g".into(), model: "a".into(), options: None };
        let shown = catalog.targets(&environment, &generators, &[Some(&stored)]);
        assert_eq!(
            (shown[0].status.as_str(), shown[0].message.as_deref()),
            ("error", Some("This connection requires settings:write.")),
            "the picker explains the refusal instead of showing the model as ready"
        );
        let key = (environment.clone(), "g".to_owned());
        let current = |catalog: &TextGeneratorCatalog, refused| catalog.lists[&key].current(&config, refused);
        assert!(current(&catalog, true), "still refused, so not asked");
        assert!(!current(&catalog, false), "the grant arriving asks again");
        let request = catalog.record(&environment, "g", &config, ModelList::Loading, false);
        assert!(catalog.settle(environment.clone(), "g".into(), request, ModelList::Ready(Vec::new())));
        assert!(!current(&catalog, true), "losing the grant refuses again");
    }
}
