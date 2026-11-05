//! Which environment or project a settings page applies to, where each value comes from,
//! and the write a change becomes. Ports `settingsScope.ts`, `scopedSettings.ts`,
//! `SettingInheritance.tsx` and `resolveProjectSettings` (`packages/shared/src/projectSettings.ts`)
//! for this client's one environment. Projects are not grouped across checkouts here, as in
//! the sidebar, so the web's checkout axis never applies and a project has one member.

use std::collections::HashMap;

use serde_json::{Map, Value, json};

use super::{Settings, contract};

/// The scope picked in the scope sentence (`SettingsScopeSearch`, minus `checkout`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScopeSearch {
    pub project: Option<String>,
    pub machine: Option<String>,
}

impl ScopeSearch {
    /// `selectEnvironmentAxis`: `None` is "All environments".
    pub fn with_machine(&self, machine: Option<String>) -> Self {
        Self { project: self.project.clone(), machine }
    }

    /// `selectProjectAxis`: `None` is "All projects".
    pub fn with_project(&self, project: Option<String>) -> Self {
        Self { project, machine: self.machine.clone() }
    }
}

/// The connected environment.
pub struct Environment<'a> {
    pub id: &'a str,
    pub label: &'a str,
    /// `None` while disconnected.
    pub settings: Option<&'a Settings>,
}

/// A project as the scope layer needs it.
#[derive(Clone, Debug)]
pub struct Project {
    pub id: String,
    pub name: String,
    /// The project's legacy `defaultModelSelection`, used until settings fold it.
    pub legacy_model_selection: Option<Value>,
    /// The project's legacy `defaultThreadEnvMode`, used until settings fold it.
    pub legacy_thread_env_mode: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScopeKind {
    All,
    Environment,
    Project { project_id: String },
    Unavailable { message: &'static str },
}

/// `ResolvedSettingsScope`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scope {
    pub kind: ScopeKind,
    pub label: String,
}

impl Scope {
    pub fn project_id(&self) -> Option<&str> {
        match &self.kind {
            ScopeKind::Project { project_id } => Some(project_id),
            _ => None,
        }
    }
}

/// `resolveSettingsScope`. A stale selection reads as unavailable, never as "all".
pub fn resolve(search: &ScopeSearch, projects: &[Project], environment: &Environment) -> Scope {
    let unavailable =
        |message| Scope { kind: ScopeKind::Unavailable { message }, label: "Unavailable selection".into() };
    if search.machine.as_deref().is_some_and(|machine| machine != environment.id) {
        return unavailable("This environment is no longer available.");
    }
    if let Some(project_id) = &search.project {
        let Some(project) = projects.iter().find(|p| &p.id == project_id) else {
            return unavailable("This project is no longer available.");
        };
        let machine = if search.machine.is_some() { environment.label } else { "All checkouts" };
        return Scope {
            kind: ScopeKind::Project { project_id: project.id.clone() },
            label: format!("{} / {machine}", project.name),
        };
    }
    if search.machine.is_some() {
        return Scope { kind: ScopeKind::Environment, label: environment.label.to_owned() };
    }
    Scope { kind: ScopeKind::All, label: "All environments".into() }
}

/// Where a project-scoped value comes from (`ProjectSettingSource`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Environment,
    Project,
    /// The repository's `t3.json`. This client does not read project files yet, so no
    /// value resolves here; the web shows the same before the file loads.
    ProjectFile,
}

/// The settings one write target sees (`ScopedSettingsTarget`).
#[derive(Clone, Debug)]
pub struct Target {
    pub project_id: Option<String>,
    pub settings: Settings,
    /// Project-scoped keys only; other keys always come from the environment.
    pub sources: HashMap<String, Source>,
}

impl Target {
    pub fn source(&self, key: &str) -> Source {
        self.sources.get(key).copied().unwrap_or(Source::Environment)
    }
}

/// `resolveScopedSettingsTargets`: nothing while disconnected or unavailable.
pub fn targets(scope: &Scope, projects: &[Project], environment: &Environment) -> Vec<Target> {
    let Some(settings) = environment.settings else { return Vec::new() };
    match &scope.kind {
        ScopeKind::Unavailable { .. } => Vec::new(),
        ScopeKind::Project { project_id } => projects
            .iter()
            .filter(|p| &p.id == project_id)
            .map(|project| resolve_project_settings(settings, Some(project)))
            .collect(),
        ScopeKind::All | ScopeKind::Environment => vec![resolve_project_settings(settings, None)],
    }
}

/// `resolveProjectSettings` without the `t3.json` tier: a project's overrides over the
/// environment's settings. Overrides naming a provider instance the project disables are
/// skipped, like the web; the web's text generation fallback for that case is not ported.
pub fn resolve_project_settings(settings: &Settings, project: Option<&Project>) -> Target {
    let overrides = project_overrides(settings, project);
    let mut effective = settings.clone();
    let mut sources = HashMap::new();
    for (key, value) in overrides {
        sources.insert(key.clone(), Source::Project);
        effective.set(key, value);
    }
    Target { project_id: project.map(|p| p.id.clone()), settings: effective, sources }
}

/// One key of `resolve_project_settings`, without copying the whole document.
pub fn resolve_project_setting(settings: &Settings, project: Option<&Project>, key: &str) -> Value {
    project_overrides(settings, project).remove(key).unwrap_or_else(|| settings.get(key).clone())
}

/// The project-scoped values a project overrides.
fn project_overrides(settings: &Settings, project: Option<&Project>) -> Map<String, Value> {
    let project_id = project.map(|p| p.id.as_str());
    let stored = project_id.and_then(|id| settings.get("projectSettingsOverrides").get(id)).and_then(Value::as_object);
    let mut overrides = Map::new();
    if let Some(project) = project.filter(|_| !settings.bool("projectSettingsFolded")) {
        if let Some(model) = project.legacy_model_selection.as_ref().filter(|m| !m.is_null()) {
            overrides.insert("defaultModelSelection".into(), model.clone());
        }
        if let Some(mode) = &project.legacy_thread_env_mode {
            overrides.insert("defaultThreadEnvMode".into(), mode.as_str().into());
        }
    }
    if let Some(stored) = stored {
        overrides.extend(stored.iter().map(|(k, v)| (k.clone(), v.clone())));
    }
    let mut resolved = Map::new();
    for key in &contract().project_scoped_keys {
        let Some(value) = overrides.remove(key) else { continue };
        let names_disabled_instance = matches!(key.as_str(), "textGenerationModelSelection" | "defaultModelSelection")
            && value
                .get("instanceId")
                .and_then(Value::as_str)
                .is_some_and(|instance| !provider_instance_enabled(settings, project_id, instance));
        if !names_disabled_instance {
            resolved.insert(key.clone(), value);
        }
    }
    resolved
}

/// `resolveProjectProviderInstanceEnabled`, treating an unconfigured instance as enabled.
fn provider_instance_enabled(settings: &Settings, project_id: Option<&str>, instance: &str) -> bool {
    let project_override = project_id.and_then(|id| {
        settings
            .get("projectSettingsOverrides")
            .get(id)?
            .get("providerInstances")?
            .get(instance)?
            .get("enabled")?
            .as_bool()
    });
    project_override
        .or_else(|| settings.get("providerInstances").get(instance)?.get("enabled")?.as_bool())
        .unwrap_or(true)
}

/// `scopedSettingsAreMixed`: the targets disagree on any of `keys`.
pub fn are_mixed(targets: &[Target], keys: &[&str]) -> bool {
    let Some(first) = targets.first() else { return false };
    targets.iter().any(|target| keys.iter().any(|key| target.settings.get(key) != first.settings.get(key)))
}

/// Where a row's value comes from across targets (`ScopedSettingSource`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowSource {
    Environment,
    Project,
    ProjectFile,
    Mixed,
}

/// `scopedSettingsSource`.
pub fn row_source(targets: &[Target], keys: &[&str]) -> RowSource {
    let scoped: Vec<&str> = keys.iter().copied().filter(|k| contract().is_project_scoped(k)).collect();
    let mut sources = targets.iter().flat_map(|t| scoped.iter().map(|k| t.source(k)));
    let Some(first) = sources.next() else { return RowSource::Environment };
    if sources.any(|source| source != first) {
        return RowSource::Mixed;
    }
    match first {
        Source::Environment => RowSource::Environment,
        Source::Project => RowSource::Project,
        Source::ProjectFile => RowSource::ProjectFile,
    }
}

/// The marker beside a row's title (`SettingInheritanceState`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InheritanceState {
    Default,
    Environment,
    Inherited,
    Overridden,
    Mixed,
}

/// The inheritance state and tooltip of a server-scoped row, as `SettingsRow` derives them.
pub fn row_inheritance(
    scope: &Scope,
    targets: &[Target],
    environment: &Environment,
    keys: &[&str],
) -> (InheritanceState, String) {
    let project_scope = scope.project_id().is_some();
    let has_scoped = keys.iter().any(|k| contract().is_project_scoped(k));
    let source = project_scope.then(|| row_source(targets, keys));
    let customized = environment
        .settings
        .is_some_and(|env| !targets.is_empty() && keys.iter().any(|k| env.get(k) != contract().default_value(k)));
    if are_mixed(targets, keys) {
        (InheritanceState::Mixed, "Mixed across selected environments".into())
    } else if source == Some(RowSource::Project) {
        (InheritanceState::Overridden, "Overridden for this project".into())
    } else if source == Some(RowSource::ProjectFile) {
        (InheritanceState::Inherited, "Inherited from the repository's t3.json".into())
    } else if source == Some(RowSource::Environment) && has_scoped {
        (InheritanceState::Inherited, format!("Inherited from {}", environment.label))
    } else if customized {
        (InheritanceState::Environment, "Set on the environment".into())
    } else {
        (InheritanceState::Default, "Built-in default".into())
    }
}

/// One row of the "where this value comes from" popover.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layer {
    pub label: String,
    pub value: String,
    pub effective: bool,
    pub set: bool,
}

/// `settingInheritanceLayers`: project, environment, `t3.json`, then the built-in default.
pub fn inheritance_layers(target: &Target, environment_settings: &Settings, key: &str) -> Vec<Layer> {
    let contract = contract();
    let scoped = contract.is_project_scoped(key);
    let source = if scoped { target.source(key) } else { Source::Environment };
    let environment_value = environment_settings.get(key);
    let environment_set = environment_value != contract.default_value(key);
    let file_backed = contract.file_backed_built_ins.get(key);
    let mut layers = Vec::new();
    if target.project_id.is_some() && scoped {
        let set = source == Source::Project;
        let value = if set { format_value(key, target.settings.get(key)) } else { "Inherits".into() };
        layers.push(Layer { label: "Project".into(), value, effective: set, set });
    }
    layers.push(Layer {
        label: "Environment".into(),
        value: if environment_set { format_value(key, environment_value) } else { "Inherits".into() },
        effective: source == Source::Environment && environment_set,
        set: environment_set,
    });
    if file_backed.is_some() && target.project_id.is_some() {
        let set = source == Source::ProjectFile;
        let value = if set { format_value(key, target.settings.get(key)) } else { "Inherits".into() };
        layers.push(Layer { label: "t3.json".into(), value, effective: set, set });
    }
    layers.push(Layer {
        label: "Default".into(),
        value: format_value(key, file_backed.unwrap_or_else(|| contract.default_value(key))),
        effective: source == Source::Environment && !environment_set,
        set: true,
    });
    layers
}

/// `formatValue` in `SettingInheritance.tsx`.
pub fn format_value(key: &str, value: &Value) -> String {
    match value {
        Value::Null => match key {
            "pullRequestMergeMethod" => "Last selected",
            "sidebarAutoSettleAfterDays" => "Never",
            "defaultModelSelection" => "Automatic",
            "sourceControlWriterModelSelection" => "Text generation model",
            "reviewGuideModelSelection" => "Source control writer model",
            "defaultThreadEnvMode" | "worktreeSubmodules" => "Inherit",
            _ => "Not set",
        }
        .into(),
        Value::Bool(on) => if *on { "On" } else { "Off" }.into(),
        Value::Number(n) if key == "sidebarAutoSettleAfterDays" => {
            format!("{n} {}", if n.as_f64() == Some(1.) { "day" } else { "days" })
        }
        Value::Number(n) => n.to_string(),
        Value::String(s) => match (key, s.as_str()) {
            ("defaultThreadEnvMode", "worktree") => "New worktree".into(),
            ("defaultThreadEnvMode", "local") => "Current checkout".into(),
            ("worktreeSubmodules", "recursive") => "Recursive".into(),
            ("worktreeSubmodules", "top-level") => "Top level only".into(),
            ("worktreeSubmodules", "none") => "Skip".into(),
            ("pullRequestMergeMethod", "merge") => "Merge".into(),
            ("pullRequestMergeMethod", "squash") => "Squash and merge".into(),
            ("pullRequestMergeMethod", "rebase") => "Rebase and merge".into(),
            (_, "") => "Empty".into(),
            _ => s.clone(),
        },
        Value::Array(items) => format!("{} {}", items.len(), if items.len() == 1 { "item" } else { "items" }),
        Value::Object(object) => {
            let text = |field: &str| object.get(field).and_then(Value::as_str);
            if let (Some(profile), "backgroundActivity") = (text("profile"), key) {
                return match profile {
                    "balanced" => "Balanced",
                    "performance" => "Performance",
                    "battery-saver" => "Battery saver",
                    "custom" => "Advanced",
                    other => other,
                }
                .into();
            }
            if let Some(model) = text("model") {
                return model.into();
            }
            match (key, text("mode")) {
                ("worktreeCleanup", Some(mode)) => if mode == "off" { "Off" } else { "Custom" }.into(),
                ("sourceControlWritingStyle", Some(mode)) => match mode {
                    "repo_conventions" => "Repository conventions",
                    "conventional_commits" => "Conventional Commits",
                    "custom" => "Custom instructions",
                    other => other,
                }
                .into(),
                (_, Some(mode)) => mode.into(),
                _ => "Custom".into(),
            }
        }
    }
}

/// `listProjectOverrides`: projects that override any of `keys`.
pub fn overriding_projects(settings: &Settings, keys: &[&str]) -> Vec<String> {
    let scoped: Vec<&str> = keys.iter().copied().filter(|k| contract().is_project_scoped(k)).collect();
    let Some(overrides) = settings.get("projectSettingsOverrides").as_object() else { return Vec::new() };
    overrides
        .iter()
        .filter(|(_, entry)| scoped.iter().any(|key| entry.get(key).is_some()))
        .map(|(id, _)| id.clone())
        .collect()
}

/// What a change writes: the device's part, and the environment's (`planScopedSettingsPatch`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Plan {
    pub client_patch: Map<String, Value>,
    pub server_patch: Option<Map<String, Value>>,
    /// Why nothing could be written, shown as "Setting not saved".
    pub unavailable: Option<String>,
}

/// Splits a unified patch into the device's keys and a write to the environment or to the
/// project's overrides.
pub fn plan_patch(scope: &Scope, projects: &[Project], environment: &Environment, patch: &Map<String, Value>) -> Plan {
    let contract = contract();
    let client_patch: Map<String, Value> =
        patch.iter().filter(|(k, _)| contract.is_client_key(k)).map(|(k, v)| (k.clone(), v.clone())).collect();
    let server_patch: Map<String, Value> =
        patch.iter().filter(|(k, _)| contract.is_server_key(k)).map(|(k, v)| (k.clone(), v.clone())).collect();
    let project_id = scope.project_id();
    let unscopable = project_id.is_some() && server_patch.keys().any(|k| !contract.is_project_scoped(k));
    let write = match (environment.settings, server_patch.is_empty()) {
        (_, true) | (None, _) => None,
        (Some(settings), false) => match &scope.kind {
            ScopeKind::Project { project_id } if !unscopable => projects
                .iter()
                .find(|p| &p.id == project_id)
                .map(|project| override_patch(settings, project, &server_patch)),
            ScopeKind::All | ScopeKind::Environment => Some(for_destination(settings, server_patch)),
            _ => None,
        },
    };
    let unavailable = (client_patch.is_empty() && write.is_none() && !patch.is_empty()).then(|| match &scope.kind {
        ScopeKind::Unavailable { message } => (*message).to_owned(),
        _ if unscopable => "This setting is environment-wide and cannot be overridden by a project.".into(),
        ScopeKind::Project { .. } => {
            "Connect the selected checkouts, or update their environments, to save a project override.".into()
        }
        ScopeKind::Environment => format!("Connect {} to save this setting.", environment.label),
        ScopeKind::All => "Connect an environment to save this setting.".into(),
    });
    Plan { client_patch, server_patch: write, unavailable }
}

/// `planScopedSettingsClear`: removes the project's overrides of `keys`.
pub fn plan_clear(scope: &Scope, environment: &Environment, keys: &[&str]) -> Plan {
    let write = match (scope.project_id(), environment.settings) {
        (Some(project_id), Some(settings)) => Some(clear_overrides_patch(settings, &[project_id], keys)),
        _ => None,
    };
    let unavailable = write
        .is_none()
        .then(|| "Connect the selected checkouts, or update their environments, to reset this override.".into());
    Plan { client_patch: Map::new(), server_patch: write, unavailable }
}

/// `planProjectOverridesClear`: removes the overrides of `keys` in each of `project_ids`.
pub fn plan_clear_projects(environment: &Environment, project_ids: &[&str], keys: &[&str]) -> Plan {
    let write = environment.settings.map(|settings| clear_overrides_patch(settings, project_ids, keys));
    let unavailable = write.is_none().then(|| "Connect the environments to reset these overrides.".into());
    Plan { client_patch: Map::new(), server_patch: write, unavailable }
}

fn clear_overrides_patch(settings: &Settings, project_ids: &[&str], keys: &[&str]) -> Map<String, Value> {
    let entries: Map<String, Value> = project_ids
        .iter()
        .map(|id| {
            // `clearProjectSettingsOverrides`: null once nothing is left.
            let next =
                settings.get("projectSettingsOverrides").get(id).and_then(Value::as_object).and_then(|current| {
                    let mut next = current.clone();
                    for key in keys {
                        next.remove(*key);
                    }
                    (!next.is_empty()).then_some(Value::Object(next))
                });
            ((*id).to_owned(), next.unwrap_or(Value::Null))
        })
        .collect();
    single("projectSettingsOverrides", Value::Object(entries))
}

/// The project's override entry with `patch` folded in. Entries store whole values, so a
/// partial object patch is completed from the project's effective value.
fn override_patch(settings: &Settings, project: &Project, patch: &Map<String, Value>) -> Map<String, Value> {
    let contract = contract();
    let effective = resolve_project_settings(settings, Some(project)).settings;
    let mut next = settings
        .get("projectSettingsOverrides")
        .get(&project.id)
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    for (key, value) in patch {
        let base = effective.get(key);
        if key == "worktreeCleanup" && value.get("mode").and_then(Value::as_str) == Some("custom") {
            let mut rules = worktree_cleanup_rules(settings, Some(project));
            merge_into(&mut rules, value.get("rules"));
            next.insert(key.clone(), json!({"mode": "custom", "rules": rules}));
        } else if value.is_null() && !contract.nullable_override_keys.contains(key) {
            next.remove(key);
        } else if let (Some(value), Some(base)) = (value.as_object(), base.as_object()) {
            let model_selection = matches!(
                key.as_str(),
                "defaultModelSelection"
                    | "sourceControlWriterModelSelection"
                    | "reviewGuideModelSelection"
                    | "textGenerationModelSelection"
            ) && (value.contains_key("instanceId") || value.contains_key("model"));
            // A new model must not inherit the previous model's options.
            let mut merged = if model_selection {
                ["instanceId", "model"]
                    .into_iter()
                    .filter_map(|field| Some((field.to_owned(), base.get(field)?.clone())))
                    .collect()
            } else {
                base.clone()
            };
            merged.extend(value.iter().map(|(k, v)| (k.clone(), v.clone())));
            next.insert(key.clone(), Value::Object(merged));
        } else {
            next.insert(key.clone(), value.clone());
        }
    }
    single("projectSettingsOverrides", Value::Object(Map::from_iter([(project.id.clone(), Value::Object(next))])))
}

/// Environment writes, in the form the destination reads: the idle compaction mode by the
/// server's version, and storage cleanup's worktree rules mirrored into a custom
/// `worktreeCleanup` while one is set (`forDestination` and its caller).
fn for_destination(previous: &Settings, mut patch: Map<String, Value>) -> Map<String, Value> {
    let worktree_rules = patch
        .get("storageCleanup")
        .and_then(Value::as_object)
        .map(|cleanup| {
            cleanup
                .iter()
                .filter(|(k, _)| k.starts_with("worktree"))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect::<Map<_, _>>()
        })
        .filter(|rules| !rules.is_empty());
    if let Some(rules) = worktree_rules.filter(|_| !previous.get("worktreeCleanup").is_null()) {
        let mut merged = worktree_cleanup_rules(previous, None);
        merge_into(&mut merged, Some(&Value::Object(rules)));
        patch.insert("worktreeCleanup".into(), json!({"mode": "custom", "rules": merged}));
    }
    if let Some(mode) = patch.remove("idleCompaction") {
        patch.remove("compactIdleThreads");
        let has_modes = previous.as_map().contains_key("idleCompaction");
        if has_modes {
            patch.insert("idleCompaction".into(), mode);
        } else if previous.get("compactIdleThreads").is_string() {
            patch.insert("compactIdleThreads".into(), mode);
        } else {
            patch.insert("compactIdleThreads".into(), (mode == "beforeCacheExpires").into());
        }
    }
    patch
}

/// `resolveWorktreeCleanup`.
/// `resolveWorktreeCleanup`: the worktree rules a project's cleanup mode resolves to.
pub fn worktree_cleanup_rules(settings: &Settings, project: Option<&Project>) -> Map<String, Value> {
    const RULES: [&str; 6] = [
        "worktreeKeepWhen",
        "worktreeAfterDays",
        "worktreeSettledAfterDays",
        "worktreeOnMerge",
        "worktreeOnDelete",
        "worktreeUnchanged",
    ];
    let policy = resolve_project_settings(settings, project).settings.get("worktreeCleanup").clone();
    let storage = settings.get("storageCleanup");
    match policy.get("mode").and_then(Value::as_str) {
        Some("custom") => policy.get("rules").and_then(Value::as_object).cloned().unwrap_or_default(),
        Some("off") => {
            let mut rules: Map<String, Value> = RULES[1..].iter().map(|k| ((*k).to_owned(), Value::Null)).collect();
            rules.insert("worktreeKeepWhen".into(), storage["worktreeKeepWhen"].clone());
            for flag in ["worktreeOnMerge", "worktreeOnDelete", "worktreeUnchanged"] {
                rules.insert(flag.into(), false.into());
            }
            rules
        }
        _ => RULES.iter().map(|k| ((*k).to_owned(), storage.get(*k).cloned().unwrap_or(Value::Null))).collect(),
    }
}

fn merge_into(target: &mut Map<String, Value>, source: Option<&Value>) {
    if let Some(source) = source.and_then(Value::as_object) {
        target.extend(source.iter().map(|(k, v)| (k.clone(), v.clone())));
    }
}

fn single(key: &str, value: Value) -> Map<String, Value> {
    Map::from_iter([(key.to_owned(), value)])
}

/// Keys of `patch` an older server silently dropped (`droppedSettingKeys`): the saved
/// value still equals the previous one although the patch changed it.
pub fn dropped_keys(patch: &Map<String, Value>, previous: &Settings, saved: &Settings) -> Vec<String> {
    patch
        .iter()
        .filter(|(key, value)| was_dropped(value, previous.get(key), saved.get(key)))
        .map(|(key, _)| key.clone())
        .collect()
}

fn was_dropped(value: &Value, before: &Value, saved: &Value) -> bool {
    match value {
        Value::Null => !before.is_null() && saved == before,
        Value::Object(fields) if !before.is_array() && !saved.is_array() => fields.iter().any(|(key, next)| {
            was_dropped(next, before.get(key).unwrap_or(&Value::Null), saved.get(key).unwrap_or(&Value::Null))
        }),
        _ => {
            let unchanged = match (value, before) {
                (Value::String(value), Value::String(before)) => value.trim() == before,
                _ => value == before,
            };
            !unchanged && saved == before
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(patch: Value) -> Settings {
        let mut settings = Settings::server_defaults();
        settings.apply(patch.as_object().unwrap());
        settings
    }

    fn project(id: &str) -> Project {
        Project { id: id.into(), name: "Repo".into(), legacy_model_selection: None, legacy_thread_env_mode: None }
    }

    fn env(settings: &Settings) -> Environment<'_> {
        Environment { id: "env-1", label: "Studio", settings: Some(settings) }
    }

    fn search(project: Option<&str>, machine: Option<&str>) -> ScopeSearch {
        ScopeSearch { project: project.map(Into::into), machine: machine.map(Into::into) }
    }

    #[test]
    fn scope_resolves_each_axis_and_stale_ids_never_broaden() {
        let s = Settings::server_defaults();
        let (e, projects) = (env(&s), [project("p1")]);
        assert_eq!(resolve(&search(None, None), &projects, &e).kind, ScopeKind::All);
        let environment = resolve(&search(None, Some("env-1")), &projects, &e);
        assert_eq!((environment.kind, environment.label.as_str()), (ScopeKind::Environment, "Studio"));
        assert_eq!(resolve(&search(Some("p1"), None), &projects, &e).label, "Repo / All checkouts");
        assert_eq!(resolve(&search(Some("p1"), Some("env-1")), &projects, &e).label, "Repo / Studio");
        assert!(matches!(resolve(&search(Some("gone"), None), &projects, &e).kind, ScopeKind::Unavailable { .. }));
        assert!(matches!(resolve(&search(None, Some("other")), &projects, &e).kind, ScopeKind::Unavailable { .. }));
        let picked = search(Some("p1"), Some("env-1"));
        assert_eq!(picked.with_project(None), search(None, Some("env-1")));
        assert_eq!(picked.with_machine(None), search(Some("p1"), None));
    }

    #[test]
    fn project_overrides_win_and_record_their_source() {
        let s = settings(json!({
            "defaultThreadEnvMode": "local",
            "projectSettingsOverrides": {"p1": {"defaultThreadEnvMode": "worktree"}},
        }));
        let target = resolve_project_settings(&s, Some(&project("p1")));
        assert_eq!(target.settings.get("defaultThreadEnvMode"), "worktree");
        assert_eq!(target.source("defaultThreadEnvMode"), Source::Project);
        assert_eq!(target.source("defaultRuntimeMode"), Source::Environment);
        let other = resolve_project_settings(&s, Some(&project("p2")));
        assert_eq!(other.settings.get("defaultThreadEnvMode"), "local");
        assert_eq!(resolve_project_setting(&s, Some(&project("p1")), "defaultThreadEnvMode"), "worktree");
        assert_eq!(resolve_project_setting(&s, None, "defaultThreadEnvMode"), "local");
    }

    #[test]
    fn legacy_project_fields_apply_until_folded() {
        let legacy = Project { legacy_thread_env_mode: Some("worktree".into()), ..project("p1") };
        let unfolded = resolve_project_settings(&settings(json!({"projectSettingsFolded": false})), Some(&legacy));
        assert_eq!(unfolded.source("defaultThreadEnvMode"), Source::Project);
        let folded = resolve_project_settings(&settings(json!({"projectSettingsFolded": true})), Some(&legacy));
        assert_eq!(folded.source("defaultThreadEnvMode"), Source::Environment);
    }

    #[test]
    fn overrides_naming_a_disabled_provider_instance_are_skipped() {
        let s = settings(json!({
            "projectSettingsFolded": true,
            "providerInstances": {"codex_2": {"enabled": false}},
            "projectSettingsOverrides": {"p1": {"defaultModelSelection": {"instanceId": "codex_2", "model": "m"}}},
        }));
        let target = resolve_project_settings(&s, Some(&project("p1")));
        assert_eq!(target.source("defaultModelSelection"), Source::Environment);
    }

    #[test]
    fn mixed_and_source_compare_every_target() {
        let a = resolve_project_settings(&settings(json!({"defaultRuntimeMode": "a"})), None);
        let mut b = a.clone();
        b.settings.set("defaultRuntimeMode", "b".into());
        b.sources.insert("defaultRuntimeMode".into(), Source::Project);
        assert!(are_mixed(&[a.clone(), b.clone()], &["defaultRuntimeMode"]));
        assert!(!are_mixed(&[a.clone(), a.clone()], &["defaultRuntimeMode"]));
        assert_eq!(row_source(&[a.clone(), b.clone()], &["defaultRuntimeMode"]), RowSource::Mixed);
        assert_eq!(row_source(&[b], &["defaultRuntimeMode"]), RowSource::Project);
        // Keys a project cannot override always come from the environment.
        assert_eq!(row_source(&[a], &["enableDeviceSupport"]), RowSource::Environment);
    }

    #[test]
    fn row_inheritance_explains_the_winning_tier() {
        let s = settings(json!({"projectSettingsOverrides": {"p1": {"defaultRuntimeMode": "approval-required"}}}));
        let e = env(&s);
        let projects = [project("p1"), project("p2")];
        let state = |search: ScopeSearch, keys: &[&str]| {
            let scope = resolve(&search, &projects, &e);
            row_inheritance(&scope, &targets(&scope, &projects, &e), &e, keys).0
        };
        assert_eq!(state(search(Some("p1"), None), &["defaultRuntimeMode"]), InheritanceState::Overridden);
        let (inherited, summary) = {
            let scope = resolve(&search(Some("p2"), None), &projects, &e);
            row_inheritance(&scope, &targets(&scope, &projects, &e), &e, &["defaultRuntimeMode"])
        };
        assert_eq!((inherited, summary.as_str()), (InheritanceState::Inherited, "Inherited from Studio"));
        assert_eq!(state(search(None, None), &["defaultRuntimeMode"]), InheritanceState::Default);
        let customized = settings(
            json!({"enableDeviceSupport": !contract().default_value("enableDeviceSupport").as_bool().unwrap()}),
        );
        let e = env(&customized);
        let scope = resolve(&search(None, None), &projects, &e);
        let t = targets(&scope, &projects, &e);
        assert_eq!(row_inheritance(&scope, &t, &e, &["enableDeviceSupport"]).0, InheritanceState::Environment);
    }

    #[test]
    fn inheritance_layers_follow_resolution_order() {
        let s = settings(json!({
            "defaultThreadEnvMode": "worktree",
            "projectSettingsOverrides": {"p1": {"defaultThreadEnvMode": "local"}},
        }));
        let overridden = resolve_project_settings(&s, Some(&project("p1")));
        let layers = inheritance_layers(&overridden, &s, "defaultThreadEnvMode");
        let summary: Vec<_> = layers.iter().map(|l| (l.label.as_str(), l.value.as_str(), l.effective)).collect();
        assert_eq!(
            summary,
            [
                ("Project", "Current checkout", true),
                ("Environment", "New worktree", false),
                ("t3.json", "Inherits", false),
                ("Default", "Current checkout", false),
            ]
        );
        let environment = resolve_project_settings(&Settings::server_defaults(), None);
        let layers = inheritance_layers(&environment, &Settings::server_defaults(), "defaultRuntimeMode");
        assert_eq!(
            layers.iter().map(|l| (l.label.as_str(), l.effective)).collect::<Vec<_>>(),
            [("Environment", false), ("Default", true)]
        );
    }

    #[test]
    fn patches_split_by_owner_and_scope() {
        let s = Settings::server_defaults();
        let e = env(&s);
        let projects = [project("p1")];
        let patch = json!({"confirmThreadDelete": false, "defaultRuntimeMode": "full-access"});
        let all = plan_patch(&resolve(&search(None, None), &projects, &e), &projects, &e, patch.as_object().unwrap());
        assert_eq!(all.client_patch, *json!({"confirmThreadDelete": false}).as_object().unwrap());
        assert_eq!(all.server_patch, json!({"defaultRuntimeMode": "full-access"}).as_object().cloned());

        let scope = resolve(&search(Some("p1"), None), &projects, &e);
        let project =
            plan_patch(&scope, &projects, &e, json!({"defaultRuntimeMode": "full-access"}).as_object().unwrap());
        assert_eq!(
            project.server_patch,
            json!({"projectSettingsOverrides": {"p1": {"defaultRuntimeMode": "full-access"}}}).as_object().cloned()
        );
        let wide = plan_patch(&scope, &projects, &e, json!({"enableDeviceSupport": true}).as_object().unwrap());
        assert_eq!(wide.server_patch, None);
        assert_eq!(
            wide.unavailable.as_deref(),
            Some("This setting is environment-wide and cannot be overridden by a project.")
        );
        let offline = Environment { settings: None, ..env(&s) };
        let disconnected =
            plan_patch(&scope, &projects, &offline, json!({"defaultRuntimeMode": "x"}).as_object().unwrap());
        assert!(disconnected.unavailable.is_some());
    }

    #[test]
    fn project_overrides_complete_partial_objects_and_clear_to_null() {
        let s = settings(json!({
            "defaultModelSelection": {"instanceId": "codex", "model": "gpt", "options": [{"id": "effort"}]},
            "projectSettingsOverrides": {"p1": {"defaultRuntimeMode": "full-access"}},
        }));
        let e = env(&s);
        let projects = [project("p1")];
        let scope = resolve(&search(Some("p1"), None), &projects, &e);
        let plan =
            plan_patch(&scope, &projects, &e, json!({"defaultModelSelection": {"model": "o3"}}).as_object().unwrap());
        assert_eq!(
            plan.server_patch.unwrap()["projectSettingsOverrides"]["p1"],
            json!({"defaultRuntimeMode": "full-access", "defaultModelSelection": {"instanceId": "codex", "model": "o3"}})
        );
        let clear = plan_clear(&scope, &e, &["defaultRuntimeMode"]);
        assert_eq!(clear.server_patch, json!({"projectSettingsOverrides": {"p1": null}}).as_object().cloned());
        assert_eq!(overriding_projects(&s, &["defaultRuntimeMode"]), ["p1"]);
        assert!(overriding_projects(&s, &["enableDeviceSupport"]).is_empty());

        // A project's first custom worktree rule starts from the rules it inherited.
        let s = settings(json!({"storageCleanup": {"worktreeOnDelete": true, "worktreeAfterDays": 14}}));
        let e = env(&s);
        let scope = resolve(&search(Some("p1"), None), &projects, &e);
        let rule = json!({"worktreeCleanup": {"mode": "custom", "rules": {"worktreeOnMerge": true}}});
        let plan = plan_patch(&scope, &projects, &e, rule.as_object().unwrap());
        let rules = &plan.server_patch.unwrap()["projectSettingsOverrides"]["p1"]["worktreeCleanup"]["rules"];
        assert_eq!((&rules["worktreeOnDelete"], &rules["worktreeAfterDays"]), (&json!(true), &json!(14)));
        assert_eq!(rules["worktreeOnMerge"], true);
    }

    #[test]
    fn idle_compaction_is_written_in_the_form_the_server_reads() {
        let patch = json!({"idleCompaction": "beforeCacheExpires"});
        let legacy =
            for_destination(&settings(json!({"compactIdleThreads": false})), patch.as_object().unwrap().clone());
        assert_eq!(legacy, *json!({"compactIdleThreads": true}).as_object().unwrap());
        let modern =
            for_destination(&settings(json!({"idleCompaction": "onReturn"})), patch.as_object().unwrap().clone());
        assert_eq!(modern, *patch.as_object().unwrap());
    }

    #[test]
    fn dropped_keys_find_writes_an_older_server_ignored() {
        let before = settings(json!({"defaultRuntimeMode": "a"}));
        let patch = json!({"defaultRuntimeMode": "b"});
        assert_eq!(dropped_keys(patch.as_object().unwrap(), &before, &before), ["defaultRuntimeMode"]);
        let saved = settings(json!({"defaultRuntimeMode": "b"}));
        assert!(dropped_keys(patch.as_object().unwrap(), &before, &saved).is_empty());
    }
}
