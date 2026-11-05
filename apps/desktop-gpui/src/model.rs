//! Wire shapes and reducers for the orchestration read model.
//!
//! Mirrors `packages/client-runtime/src/state/shellReducer.ts` and
//! `orchestrationV2Projection.ts`. Enum-like fields stay strings so new server
//! values never fail a whole frame; only fields the UI reads are typed.

use std::rc::Rc;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

/// Decodes an array element by element, skipping elements that fail, like the
/// TS `ForwardCompatibleArray`, so one new-shaped record never drops a frame.
fn lossy<'de, D: serde::Deserializer<'de>, T: DeserializeOwned>(deserializer: D) -> Result<Vec<T>, D::Error> {
    let values = Vec::<Value>::deserialize(deserializer)?;
    Ok(values
        .into_iter()
        .filter_map(|value| {
            T::deserialize(value).map_err(|error| log::warn!("skipping undecodable element: {error}")).ok()
        })
        .collect())
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ModelSelection {
    #[serde(alias = "provider")]
    pub instance_id: String,
    pub model: String,
    #[serde(default)]
    pub options: Option<Vec<Value>>,
}

impl ModelSelection {
    pub fn to_json(&self) -> Value {
        let mut value = serde_json::json!({"instanceId": self.instance_id, "model": self.model});
        if let Some(options) = &self.options {
            value["options"] = Value::Array(options.clone());
        }
        value
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderModel {
    pub slug: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub is_custom: bool,
    #[serde(default)]
    pub is_default: Option<bool>,
    #[serde(default)]
    pub short_name: Option<String>,
    #[serde(default)]
    pub sub_provider: Option<String>,
    #[serde(default)]
    pub capabilities: Option<ModelCapabilities>,
    /// `"new"` shows a NEW badge in the model picker.
    #[serde(default)]
    pub badge: Option<String>,
    #[serde(default)]
    pub is_legacy: bool,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCapabilities {
    #[serde(default)]
    pub option_descriptors: Vec<OptionDescriptor>,
}

/// A model option such as reasoning effort (`ProviderOptionDescriptor`).
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OptionDescriptor {
    pub id: String,
    pub label: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub options: Vec<OptionChoice>,
    #[serde(default)]
    pub current_value: Option<Value>,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub prompt_injected_values: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OptionChoice {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub is_default: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Provider {
    pub instance_id: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub driver: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub availability: Option<String>,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub models: Vec<ProviderModel>,
    #[serde(default)]
    pub usage_limits: Option<UsageLimits>,
    #[serde(default)]
    pub auth: ProviderAuth,
    #[serde(default)]
    pub setup: Option<ProviderSetup>,
    #[serde(default)]
    pub installed: bool,
    #[serde(default)]
    pub runtime_paths: Option<RuntimePaths>,
    #[serde(default)]
    pub supports_text_generation: Option<bool>,
    #[serde(default)]
    pub accent_color: Option<String>,
    /// `versionAdvisory`, `compatibilityAdvisory` and `updateState` (`packages/contracts/src/server.ts`).
    #[serde(default)]
    pub version_advisory: Option<Value>,
    #[serde(default)]
    pub compatibility_advisory: Option<Value>,
    #[serde(default)]
    pub update_state: Option<Value>,
    #[serde(default)]
    pub native_sessions: Option<NativeSessions>,
    #[serde(default)]
    pub configurable_providers: bool,
    #[serde(default)]
    pub supports_conversation_rollback: Option<bool>,
    #[serde(default, deserialize_with = "lossy")]
    pub skills: Vec<ProviderSkill>,
    #[serde(default, deserialize_with = "lossy")]
    pub slash_commands: Vec<SlashCommand>,
    #[serde(default, deserialize_with = "lossy")]
    pub workspace_snapshots: Vec<WorkspaceSnapshot>,
    /// Empty means every runtime mode.
    #[serde(default)]
    pub supported_runtime_modes: Vec<String>,
    #[serde(default)]
    pub message: Option<String>,
}

/// `ServerProviderAuth` (`packages/contracts/src/server.ts`).
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderAuth {
    #[serde(default)]
    pub status: String,
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub can_logout: Option<bool>,
    #[serde(default)]
    pub subscription_sharing: Option<bool>,
    #[serde(default)]
    pub profile_id: Option<String>,
    /// The plan, e.g. "Claude Max".
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub workspace_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSetup {
    pub can_authenticate: bool,
    pub can_install: bool,
    #[serde(default)]
    pub documentation_url: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimePaths {
    pub home_path: String,
    #[serde(default)]
    pub shadow_home_path: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeSessions {
    #[serde(default)]
    pub can_list: bool,
    #[serde(default)]
    pub can_load: bool,
    #[serde(default)]
    pub can_resume: bool,
    #[serde(default)]
    pub can_delete: bool,
}

/// `ServerProviderSkill` (`packages/contracts/src/server.ts`).
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSkill {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub short_description: Option<String>,
    #[serde(default)]
    pub user_invocable: Option<bool>,
}

/// `ServerProviderSlashCommand`.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SlashCommand {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub input: Option<Value>,
}

/// `ServerProviderWorkspaceSnapshot`: skills and commands as seen from one checkout.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceSnapshot {
    pub cwd: String,
    #[serde(default, deserialize_with = "lossy")]
    pub slash_commands: Vec<SlashCommand>,
    #[serde(default, deserialize_with = "lossy")]
    pub skills: Vec<ProviderSkill>,
}

/// `ServerProviderUsageLimits` (`packages/contracts/src/providerUsageLimits.ts`).
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageLimits {
    #[serde(default)]
    pub windows: Vec<UsageWindow>,
    #[serde(default)]
    pub unavailable: Option<Value>,
    #[serde(default)]
    pub checked_at: Option<String>,
    #[serde(default)]
    pub credential_fingerprint: Option<String>,
    #[serde(default)]
    pub external_usage: Option<ExternalUsage>,
}

/// A host that shows usage on its own site instead of reporting windows.
#[derive(Clone, Debug, Deserialize)]
pub struct ExternalUsage {
    pub label: String,
    pub url: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageWindow {
    pub id: String,
    pub label: String,
    pub used_percent: f64,
    /// `session`, `weekly`, `monthly` or `other`.
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub resets_at: Option<String>,
    #[serde(default)]
    pub window_duration_mins: Option<f64>,
}

impl UsageWindow {
    /// `remainingPercent` in `packages/shared/src/usageLimits.ts`.
    pub fn remaining_percent(&self) -> u32 {
        (100. - self.used_percent.clamp(0., 100.)).round() as u32
    }
}

impl Provider {
    pub fn is_selectable(&self) -> bool {
        self.enabled && self.availability.as_deref() != Some("unavailable")
    }

    /// `getDefaultProviderInstanceModel` in `apps/web/src/providerInstances.ts`.
    pub fn default_model(&self) -> Option<&str> {
        self.models
            .iter()
            .find(|m| m.is_default == Some(true) && !m.is_custom)
            .or_else(|| self.models.iter().find(|m| !m.is_custom))
            .or_else(|| self.models.first())
            .map(|m| m.slug.as_str())
    }
}

/// What the client needs from `subscribeServerConfig`.
#[derive(Default)]
pub struct ServerInfo {
    pub providers: Vec<Provider>,
    pub default_model_selection: Option<ModelSelection>,
    /// The environment's name, e.g. the machine's host name.
    pub environment_label: Option<String>,
    platform_machine: Option<String>,
    pub new_projects_root: Option<String>,
    /// The full `ServerSettings` over the contract's defaults; see `crate::settings`.
    pub settings: crate::settings::Settings,
    pub environment_id: Option<String>,
    /// The raw `config` of the last snapshot; read `settings` and `capabilities` instead of its copies.
    pub config: Value,
    /// The raw `ServerProvider` records, for settings pages that edit them.
    pub providers_json: Vec<Value>,
    pub platform_os: Option<String>,
    /// `environment.capabilities` from the config snapshot.
    pub capabilities: Value,
    /// `config.keybindings`, the server's resolved rules (defaults not merged in).
    pub keybindings: Rc<Vec<crate::keybindings::Rule>>,
    pub keybindings_config_path: Option<String>,
    /// `config.availableEditors`, editor ids that can open a path on the server's host.
    pub available_editors: Vec<String>,
    /// The "No project" workspace; a project rooted there is the scratch project.
    pub scratch_workspace_root: Option<String>,
    /// `ManagedPolicySummary`, or null when no administrator policy applies.
    pub managed_policy: Value,
    /// `settings.providerInstances`, keyed by instance id.
    pub provider_instances: Value,
    /// Model picker favorites as `(instanceId, model)`.
    pub favorites: Vec<(String, String)>,
}

impl ServerInfo {
    pub fn apply(&mut self, event: &Value) -> Result<(), serde_json::Error> {
        match event.get("type").and_then(Value::as_str).unwrap_or_default() {
            "snapshot" => {
                let config = &event["config"];
                self.config = config.clone();
                self.providers = lossy(&config["providers"])?;
                self.providers_json = config["providers"].as_array().cloned().unwrap_or_default();
                self.apply_settings(&config["settings"])?;
                let text = |pointer| config.pointer(pointer).and_then(Value::as_str).map(str::to_owned);
                self.environment_label = text("/environment/label");
                self.environment_id = text("/environment/environmentId");
                self.platform_machine = text("/environment/platform/machine");
                self.new_projects_root = text("/newProjectsRoot");
                self.platform_os = text("/environment/platform/os");
                self.capabilities = config.pointer("/environment/capabilities").cloned().unwrap_or_default();
                self.keybindings = Rc::new(crate::keybindings::decode_rules(&config["keybindings"]));
                self.keybindings_config_path = text("/keybindingsConfigPath");
                self.available_editors = config["availableEditors"]
                    .as_array()
                    .map(|ids| ids.iter().filter_map(|id| id.as_str().map(str::to_owned)).collect())
                    .unwrap_or_default();
                self.scratch_workspace_root = text("/scratchWorkspaceRoot");
                self.managed_policy = config.get("managedPolicy").cloned().unwrap_or_default();
            }
            "keybindingsUpdated" => {
                let keybindings = &event["payload"]["keybindings"];
                self.keybindings = Rc::new(crate::keybindings::decode_rules(keybindings));
                if let Some(config) = self.config.as_object_mut() {
                    config.insert("keybindings".into(), keybindings.clone());
                }
            }
            "providerStatuses" => {
                self.providers = lossy(&event["payload"]["providers"])?;
                self.providers_json = event["payload"]["providers"].as_array().cloned().unwrap_or_default();
            }
            "settingsUpdated" => self.apply_settings(&event["payload"]["settings"])?,
            _ => {}
        }
        Ok(())
    }

    /// Takes a full `ServerSettings`, from a snapshot, `settingsUpdated` or `server.updateSettings`.
    pub fn apply_settings(&mut self, settings: &Value) -> Result<(), serde_json::Error> {
        self.settings = crate::settings::Settings::layered(&crate::settings::contract().server_defaults, settings);
        self.default_model_selection =
            Option::deserialize(settings.get("defaultModelSelection").unwrap_or(&Value::Null))?;
        self.provider_instances = settings.get("providerInstances").cloned().unwrap_or_default();
        self.favorites = settings
            .get("favorites")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|f| Some((f.get("provider")?.as_str()?.to_owned(), f.get("model")?.as_str()?.to_owned())))
            .collect();
        Ok(())
    }

    pub fn capability(&self, name: &str) -> bool {
        self.capabilities.get(name).and_then(Value::as_bool) == Some(true)
    }

    /// `capabilities.fileAttachments.maxUploadBytes`; `None` refuses files.
    pub fn file_upload_limit(&self) -> Option<u64> {
        self.capabilities.pointer("/fileAttachments/maxUploadBytes").and_then(Value::as_u64)
    }

    /// `ResolvedKeybindingsConfig` as sent, for project actions' shortcut labels and edits.
    pub fn keybindings_json(&self) -> &Value {
        &self.config["keybindings"]
    }

    /// `resolveEnvironmentMachineKind` in `packages/contracts/src/server.ts`.
    pub fn machine_kind(&self) -> &str {
        self.settings.get("environmentIcon").as_str().or(self.platform_machine.as_deref()).unwrap_or("server")
    }

    /// Every model of every usable provider, labelled for a picker.
    pub fn model_choices(&self) -> Vec<(gpui_kit::SharedString, ModelSelection)> {
        self.providers
            .iter()
            .filter(|p| p.is_selectable())
            .flat_map(|provider| {
                let provider_name = provider.display_name.as_deref().unwrap_or(&provider.instance_id);
                provider.models.iter().map(move |model| {
                    let label = format!("{provider_name} · {}", model.name.as_deref().unwrap_or(&model.slug));
                    let selection = ModelSelection {
                        instance_id: provider.instance_id.clone(),
                        model: model.slug.clone(),
                        options: None,
                    };
                    (label.into(), selection)
                })
            })
            .collect()
    }

    pub fn model(&self, selection: &ModelSelection) -> Option<&ProviderModel> {
        self.provider(&selection.instance_id)?.models.iter().find(|m| m.slug == selection.model)
    }

    pub fn provider(&self, instance_id: &str) -> Option<&Provider> {
        self.providers.iter().find(|p| p.instance_id == instance_id)
    }

    /// A project-scoped setting as the project's threads see it (`resolveProjectSettings`).
    pub fn project_setting(&self, project: Option<&ProjectShell>, key: &str) -> Value {
        let project = project.map(ProjectShell::scope_project);
        crate::settings::scope::resolve_project_setting(&self.settings, project.as_ref(), key)
    }

    /// Model for a new thread: the project's or the user's default when its
    /// provider is usable, else the first ready provider's default model.
    /// Mirrors `resolveDefaultProviderModelSelection`.
    pub fn new_thread_model(&self, project: Option<&ProjectShell>) -> Option<ModelSelection> {
        let preferred = ModelSelection::deserialize(self.project_setting(project, "defaultModelSelection")).ok();
        if let Some(selection) = preferred {
            if self.provider(&selection.instance_id).is_some_and(Provider::is_selectable) {
                return Some(selection);
            }
        }
        let provider = self
            .providers
            .iter()
            .find(|p| p.is_selectable() && p.status == "ready")
            .or_else(|| self.providers.iter().find(|p| p.is_selectable() && p.status != "error"))?;
        Some(ModelSelection {
            instance_id: provider.instance_id.clone(),
            model: provider.default_model()?.to_owned(),
            options: None,
        })
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepositoryIdentity {
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub canonical_key: Option<String>,
    /// Work tree root, or the git dir for a bare repository.
    #[serde(default)]
    pub root_path: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub origin: Option<RepositoryOrigin>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepositoryOrigin {
    pub canonical_key: String,
    #[serde(default)]
    pub display_name: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectShell {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    pub workspace_root: String,
    #[serde(default)]
    pub repository_identity: Option<RepositoryIdentity>,
    #[serde(default)]
    pub default_model_selection: Option<ModelSelection>,
    /// Legacy per-project env mode, used until settings fold it into `projectSettingsOverrides`.
    #[serde(default)]
    pub default_thread_env_mode: Option<String>,
    #[serde(default)]
    pub archived_at: Option<String>,
    #[serde(default)]
    pub pinned_at: Option<String>,
    /// Forward-compatible union keyed by `kind` (`packages/contracts/src/project.ts`).
    #[serde(default)]
    pub project_icon: Option<Value>,
    /// The repo favicon the server found, relative to the workspace root.
    #[serde(default)]
    pub favicon_path: Option<String>,
    /// Legacy project-record scripts; settings overrides win (`resolveProjectScripts`).
    #[serde(default, deserialize_with = "lossy")]
    pub scripts: Vec<ProjectScript>,
}

impl ProjectShell {
    pub fn scope_project(&self) -> crate::settings::scope::Project {
        crate::settings::scope::Project {
            id: self.id.clone(),
            name: self.title.clone(),
            legacy_model_selection: self.default_model_selection.as_ref().map(ModelSelection::to_json),
            legacy_thread_env_mode: self.default_thread_env_mode.clone(),
        }
    }
}

/// `ProjectScript` (`packages/contracts/src/project.ts`).
#[derive(Clone, Debug, Deserialize, serde::Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProjectScript {
    pub id: String,
    pub name: String,
    pub command: String,
    pub icon: String,
    pub run_on_worktree_create: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_on_settle: Option<bool>,
    #[serde(default, rename = "async", skip_serializing_if = "Option::is_none")]
    pub run_async: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_open_preview: Option<bool>,
}

/// The icons a project script may use (`ProjectScriptIcon`).
pub const PROJECT_SCRIPT_ICONS: [&str; 6] = ["play", "test", "lint", "configure", "build", "debug"];

/// What a checkout's `t3.json` declares (`useT3ProjectFileState` in `apps/web/src/hooks`).
#[derive(Clone, Debug, PartialEq)]
pub enum T3ProjectFile {
    Loading,
    Missing,
    /// Present but fails to decode, so everything it declares is ignored.
    Invalid,
    Valid(Vec<T3ProjectFileScript>),
}

/// `T3ProjectFileScript` (`packages/contracts/src/t3ProjectFile.ts`).
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct T3ProjectFileScript {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub run_on_worktree_create: Option<bool>,
    #[serde(default)]
    pub run_on_settle: Option<bool>,
    #[serde(default, rename = "async")]
    pub run_async: Option<bool>,
    #[serde(default)]
    pub preview_url: Option<String>,
    #[serde(default)]
    pub auto_open_preview: Option<bool>,
}

impl T3ProjectFile {
    /// `parseT3ProjectFile`: one bad script invalidates the whole file, like the schema decode.
    pub fn parse(contents: &str) -> Self {
        let Ok(Value::Object(file)) = serde_json::from_str::<Value>(contents) else { return Self::Invalid };
        let Some(scripts) = file.get("scripts") else { return Self::Valid(Vec::new()) };
        let Ok(scripts) = Vec::<T3ProjectFileScript>::deserialize(scripts) else { return Self::Invalid };
        let valid = scripts.iter().all(|script| {
            !script.name.trim().is_empty()
                && !script.command.trim().is_empty()
                && script.icon.as_deref().is_none_or(|icon| PROJECT_SCRIPT_ICONS.contains(&icon))
                && script.preview_url.as_deref().is_none_or(|url| !url.trim().is_empty())
        });
        if valid { Self::Valid(scripts) } else { Self::Invalid }
    }

    pub fn scripts(&self) -> &[T3ProjectFileScript] {
        match self {
            Self::Valid(scripts) => scripts,
            _ => &[],
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Lineage {
    #[serde(default)]
    pub parent_thread_id: Option<String>,
    #[serde(default)]
    pub relationship_to_parent: Option<String>,
    #[serde(default)]
    pub side_chat: Option<bool>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingRequestSummary {
    pub kind: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingBackgroundTask {
    pub kind: String,
    #[serde(default)]
    pub awaited: Option<bool>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadGoal {
    pub status: String,
}

/// Keeps "absent" (`None`) apart from an explicit `null` (`Some(None)`).
fn present<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(deserializer: D) -> Result<Option<T>, D::Error> {
    T::deserialize(deserializer).map(Some)
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadShell {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub status: String,
    /// Absent from older servers, which fall back to `status`; `null` means no activity run.
    #[serde(default, deserialize_with = "present")]
    pub activity_run_status: Option<Option<String>>,
    #[serde(default)]
    pub creation_source: Option<String>,
    #[serde(default)]
    pub blocked_on_thread_wait: Option<bool>,
    #[serde(default)]
    pub pending_runtime_request: Option<PendingRequestSummary>,
    #[serde(default)]
    pub lineage: Lineage,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub last_error_class: Option<String>,
    #[serde(default)]
    pub has_actionable_proposed_plan: bool,
    #[serde(default)]
    pub pinned_at: Option<String>,
    #[serde(default)]
    pub archived_at: Option<String>,
    #[serde(default)]
    pub deleted_at: Option<String>,
    pub updated_at: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub provider_instance_id: Option<String>,
    #[serde(default)]
    pub model_selection: Option<ModelSelection>,
    #[serde(default)]
    pub interaction_mode: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub worktree_path: Option<String>,
    #[serde(default)]
    pub active_provider_thread_id: Option<String>,
    #[serde(default)]
    pub latest_run_id: Option<String>,
    #[serde(default)]
    pub active_run_id: Option<String>,
    #[serde(default)]
    pub latest_run_requested_at: Option<String>,
    #[serde(default)]
    pub latest_run_started_at: Option<String>,
    #[serde(default, deserialize_with = "present")]
    pub latest_run_completed_at: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    pub activity_run_started_at: Option<Option<String>>,
    #[serde(default)]
    pub latest_user_message_at: Option<String>,
    #[serde(default, deserialize_with = "present")]
    pub latest_user_authored_message_at: Option<Option<String>>,
    #[serde(default)]
    pub pending_guest_message_count: u32,
    #[serde(default, deserialize_with = "lossy")]
    pub pending_background_tasks: Vec<PendingBackgroundTask>,
    #[serde(default)]
    pub goal: Option<ThreadGoal>,
    #[serde(default)]
    pub title_regeneration: Option<Value>,
    #[serde(default)]
    pub settled_override: Option<String>,
    #[serde(default)]
    pub settled_at: Option<String>,
    #[serde(default)]
    pub unsettled_at: Option<String>,
    #[serde(default)]
    pub snoozed_until: Option<String>,
    #[serde(default)]
    pub snoozed_at: Option<String>,
    #[serde(default)]
    pub auto_settle_disabled_at: Option<String>,
    #[serde(default)]
    pub pin_order_key: Option<String>,
    #[serde(default)]
    pub active_order_key: Option<String>,
    /// Absent on servers without visit tracking; `Some(None)` when never visited.
    #[serde(default, deserialize_with = "present")]
    pub last_visited_at: Option<Option<String>>,
}

impl ThreadShell {
    pub fn last_visited(&self) -> Option<&str> {
        self.last_visited_at.as_ref()?.as_deref()
    }

    /// The server keeps a read watermark for this thread.
    pub fn tracks_visits(&self) -> bool {
        self.last_visited_at.is_some()
    }
}

impl ThreadShell {
    /// Subagents and side chats live under their parent, not in the main list.
    pub fn is_listed(&self) -> bool {
        self.lineage.relationship_to_parent.as_deref() != Some("subagent") && self.lineage.side_chat != Some(true)
    }
}

pub fn is_active_run_status(status: &str) -> bool {
    matches!(status, "preparing" | "queued" | "starting" | "running" | "waiting")
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ShellSnapshot {
    snapshot_sequence: u64,
    #[serde(default, deserialize_with = "lossy")]
    projects: Vec<ProjectShell>,
    #[serde(default, deserialize_with = "lossy")]
    threads: Vec<ThreadShell>,
}

#[derive(Default)]
pub struct ShellState {
    pub projects: Vec<ProjectShell>,
    pub threads: Vec<ThreadShell>,
    pub sequence: u64,
    pub live: bool,
    /// Counts applied elements, so views can cache what they derive from projects and threads.
    pub revision: u64,
    /// A full snapshot failed to decode, so deltas wait for the next one.
    needs_snapshot: bool,
}

impl ShellState {
    /// Applies one `orchestration.subscribeShell` element.
    pub fn apply(&mut self, item: &Value) -> Result<(), serde_json::Error> {
        let kind = item.get("kind").and_then(Value::as_str).unwrap_or_default();
        let sequence = item.get("sequence").and_then(Value::as_u64);
        if let Some(sequence) = sequence {
            if sequence <= self.sequence || self.needs_snapshot {
                return Ok(());
            }
        }
        self.revision += 1;
        match kind {
            "synchronized" => self.live = true,
            "snapshot" => {
                let roots = item.get("resolvedRepositoryIdentityRoots").and_then(Value::as_array);
                let snapshot = match ShellSnapshot::deserialize(&item["snapshot"]) {
                    Ok(snapshot) => snapshot,
                    // An enrichment patch only refines names; the list stays usable without it.
                    Err(error) if roots.is_some() => {
                        log::warn!("skipping undecodable shell enrichment: {error}");
                        return Ok(());
                    }
                    Err(error) => {
                        // Resume from scratch, so a resubscribe fetches a fresh snapshot.
                        self.sequence = 0;
                        self.needs_snapshot = true;
                        return Err(error);
                    }
                };
                match roots {
                    // Enrichment patch: only repository identity changes, for these roots.
                    Some(roots) => {
                        for root in roots.iter().filter_map(Value::as_str) {
                            let identity = snapshot
                                .projects
                                .iter()
                                .find(|p| p.workspace_root == root)
                                .and_then(|p| p.repository_identity.clone());
                            for project in self.projects.iter_mut().filter(|p| p.workspace_root == root) {
                                project.repository_identity = identity.clone();
                            }
                        }
                    }
                    None => {
                        let mut projects = snapshot.projects;
                        for project in &mut projects {
                            if project.repository_identity.is_none() {
                                project.repository_identity = self
                                    .projects
                                    .iter()
                                    .find(|old| old.workspace_root == project.workspace_root)
                                    .and_then(|old| old.repository_identity.clone());
                            }
                        }
                        self.projects = projects;
                        self.threads = snapshot.threads;
                        self.sequence = snapshot.snapshot_sequence;
                        self.needs_snapshot = false;
                    }
                }
            }
            // An undecodable delta is skipped but still advances the cursor, like the
            // thread reducer: replaying it after a resubscribe would fail the same way.
            "project.updated" => match ProjectShell::deserialize(&item["project"]) {
                Err(error) => log::warn!("skipping undecodable project update: {error}"),
                Ok(mut project) => match self.projects.iter_mut().find(|p| p.id == project.id) {
                    Some(existing) => {
                        if project.repository_identity.is_none() {
                            project.repository_identity = existing.repository_identity.take();
                        }
                        *existing = project;
                    }
                    None => self.projects.push(project),
                },
            },
            "project.removed" => {
                let id = item["projectId"].as_str().unwrap_or_default();
                self.projects.retain(|p| p.id != id);
            }
            "thread.updated" => match ThreadShell::deserialize(&item["thread"]) {
                Err(error) => log::warn!("skipping undecodable thread update: {error}"),
                Ok(thread) => {
                    let active = item.get("location").and_then(Value::as_str) == Some("active");
                    match (self.threads.iter().position(|t| t.id == thread.id), active) {
                        (Some(index), true) => self.threads[index] = thread,
                        (None, true) => self.threads.push(thread),
                        (Some(index), false) => {
                            self.threads.remove(index);
                        }
                        (None, false) => {}
                    }
                }
            },
            "thread.removed" => {
                let id = item["threadId"].as_str().unwrap_or_default();
                self.threads.retain(|t| t.id != id);
            }
            _ => {}
        }
        if let Some(sequence) = sequence {
            self.sequence = sequence;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppThread {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub model_selection: Option<ModelSelection>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub runtime_mode: Option<String>,
    #[serde(default)]
    pub worktree_path: Option<String>,
    #[serde(default)]
    pub active_provider_thread_id: Option<String>,
    /// Why the last `checkpoint.rollback` failed, keyed by its command id.
    #[serde(default)]
    pub rollback_failure: Option<RollbackFailure>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RollbackFailure {
    pub request_id: String,
    pub message: String,
}

/// The token counts behind the context meter: `ThreadTokenUsageSnapshot` on a
/// provider thread, `OrchestrationV2ProviderTurnTokenUsage` on a provider turn.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsage {
    pub used_tokens: u64,
    #[serde(default)]
    pub max_tokens: Option<u64>,
    #[serde(default)]
    pub auto_compact_threshold: Option<u64>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderThread {
    pub id: String,
    #[serde(default)]
    pub context_usage: Option<TokenUsage>,
    #[serde(default)]
    pub provider_session_id: Option<String>,
    #[serde(default)]
    pub provider_instance_id: Option<String>,
    #[serde(default)]
    pub app_thread_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSession {
    pub id: String,
    pub status: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderTurn {
    pub id: String,
    #[serde(default)]
    pub token_usage: Option<TokenUsage>,
    #[serde(default)]
    pub provider_thread_id: Option<String>,
    #[serde(default)]
    pub run_attempt_id: Option<String>,
    #[serde(default)]
    pub ordinal: i64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Run {
    pub id: String,
    pub status: String,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub completed_at: Option<String>,
    #[serde(default)]
    pub ordinal: i64,
    #[serde(default)]
    pub active_attempt_id: Option<String>,
    #[serde(default)]
    pub provider_thread_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunAttempt {
    pub id: String,
    pub run_id: String,
    #[serde(default)]
    pub root_node_id: Option<String>,
    pub status: String,
    #[serde(default)]
    pub provider_turn_id: Option<String>,
    #[serde(default)]
    pub completed_at: Option<String>,
}

/// A captured checkpoint, the target of `checkpoint.rollback`.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Checkpoint {
    pub id: String,
    pub scope_id: String,
    #[serde(default)]
    pub run_id: Option<String>,
    #[serde(default)]
    pub ordinal_within_scope: i64,
    #[serde(default)]
    pub app_run_ordinal: Option<i64>,
    pub status: String,
    #[serde(default)]
    pub captured_at: String,
}

/// An execution node; items reach their run attempt through `parentNodeId`.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionNode {
    pub id: String,
    #[serde(default)]
    pub parent_node_id: Option<String>,
    #[serde(default)]
    pub root_node_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeRequest {
    pub id: String,
    pub kind: String,
    pub status: String,
    #[serde(default)]
    pub response_capability: Value,
}

impl RuntimeRequest {
    pub fn is_live(&self) -> bool {
        self.capability() == Some("live")
    }

    pub fn capability(&self) -> Option<&str> {
        self.response_capability.get("type").and_then(Value::as_str)
    }

    pub fn supports_feedback(&self) -> bool {
        self.is_live() && self.response_capability.get("supportsFeedback").and_then(Value::as_bool) == Some(true)
    }
}

/// A turn item. `raw` holds the whole record for type-specific fields.
#[derive(Clone, Debug)]
pub struct TurnItem {
    pub id: String,
    pub item_type: String,
    pub thread_id: String,
    pub run_id: Option<String>,
    pub node_id: Option<String>,
    pub ordinal: i64,
    pub status: String,
    /// The stream sequence that last wrote this item; 0 when unknown.
    pub revision: u64,
    pub raw: Value,
}

impl<'de> Deserialize<'de> for TurnItem {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = Value::deserialize(deserializer)?;
        let text = |key: &str| raw.get(key).and_then(Value::as_str).map(str::to_owned);
        let id = text("id").ok_or_else(|| serde::de::Error::missing_field("id"))?;
        Ok(Self {
            id,
            item_type: text("type").unwrap_or_default(),
            thread_id: text("threadId").unwrap_or_default(),
            run_id: text("runId"),
            node_id: text("nodeId"),
            ordinal: raw.get("ordinal").and_then(Value::as_i64).unwrap_or_default(),
            status: text("status").unwrap_or_default(),
            revision: 0,
            raw,
        })
    }
}

impl TurnItem {
    pub fn str(&self, key: &str) -> Option<&str> {
        self.raw.get(key).and_then(Value::as_str)
    }

    pub fn bool(&self, key: &str) -> bool {
        self.raw.get(key).and_then(Value::as_bool) == Some(true)
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VisibleRow {
    pub visibility: String,
    pub source_thread_id: String,
    pub source_item_id: String,
    /// Shared with `turn_items`: a streamed update is stored once.
    pub item: Rc<TurnItem>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadProjection {
    pub thread: AppThread,
    #[serde(default, deserialize_with = "lossy")]
    pub runs: Vec<Run>,
    #[serde(default, deserialize_with = "lossy")]
    pub attempts: Vec<RunAttempt>,
    #[serde(default, deserialize_with = "lossy")]
    pub runtime_requests: Vec<RuntimeRequest>,
    #[serde(default, deserialize_with = "lossy")]
    pub turn_items: Vec<Rc<TurnItem>>,
    /// The timeline, in render order.
    #[serde(default, deserialize_with = "lossy")]
    pub visible_turn_items: Vec<VisibleRow>,
    #[serde(default, deserialize_with = "lossy")]
    pub provider_threads: Vec<ProviderThread>,
    #[serde(default, deserialize_with = "lossy")]
    pub provider_turns: Vec<ProviderTurn>,
    #[serde(default, deserialize_with = "lossy")]
    pub checkpoints: Vec<Checkpoint>,
    #[serde(default, deserialize_with = "lossy")]
    pub nodes: Vec<ExecutionNode>,
    #[serde(default, deserialize_with = "lossy")]
    pub provider_sessions: Vec<ProviderSession>,
}

impl ThreadProjection {
    /// The newest turn report, else the active provider thread's snapshot, as
    /// `latestProviderTurnTokenUsage` + `deriveLatestContextWindowSnapshot`
    /// in `packages/client-runtime/src/state/contextWindow.ts`.
    pub fn context_usage(&self) -> Option<&TokenUsage> {
        let mut latest: Option<&TokenUsage> = None;
        for usage in self.provider_turns.iter().filter_map(|turn| turn.token_usage.as_ref()) {
            // Ties go to the later entry, since live upserts append.
            if latest.is_none_or(|current| usage.updated_at >= current.updated_at) {
                latest = Some(usage);
            }
        }
        latest.or_else(|| {
            let active = self.thread.active_provider_thread_id.as_ref()?;
            self.provider_threads.iter().find(|thread| &thread.id == active)?.context_usage.as_ref()
        })
    }

    /// `resolveThreadProviderSession` in `packages/client-runtime/src/state/threadWorkflows.ts`.
    pub fn provider_session(&self) -> Option<&ProviderSession> {
        let active = self.runs.iter().rev().find(|run| is_active_run_status(&run.status));
        let provider_thread_id =
            active.and_then(|run| run.provider_thread_id.as_ref()).or(self.thread.active_provider_thread_id.as_ref());
        let attached = provider_thread_id
            .and_then(|id| self.provider_threads.iter().find(|thread| &thread.id == id))
            .or_else(|| {
                self.provider_threads.iter().find(|thread| {
                    thread.app_thread_id.as_ref() == Some(&self.thread.id) && thread.provider_session_id.is_some()
                })
            });
        match attached.and_then(|thread| thread.provider_session_id.as_ref()) {
            Some(id) => self.provider_sessions.iter().find(|session| &session.id == id),
            None => self.provider_sessions.iter().rev().find(|s| !matches!(s.status.as_str(), "stopped" | "error")),
        }
    }

    pub fn is_running(&self) -> bool {
        self.runs.iter().any(|run| is_active_run_status(&run.status))
    }

    /// The run `run.interrupt` should target: the last unfinished one.
    pub fn interruptible_run(&self) -> Option<&Run> {
        self.runs
            .iter()
            .rev()
            .find(|run| matches!(run.status.as_str(), "preparing" | "starting" | "running" | "waiting"))
    }

    /// Pending approvals joined to their `approval_request` item, oldest first.
    pub fn pending_approvals(&self) -> Vec<(&RuntimeRequest, Option<&TurnItem>)> {
        self.pending_requests(
            |kind| !matches!(kind, "user_input" | "auth_refresh" | "dynamic_tool_call"),
            "approval_request",
        )
    }

    /// Pending questions joined to their `user_input_request` item; requests
    /// whose item has not arrived yet are skipped, as `derivePendingThreadRequests` does.
    pub fn pending_user_inputs(&self) -> Vec<(&RuntimeRequest, &TurnItem)> {
        self.pending_requests(|kind| kind == "user_input", "user_input_request")
            .into_iter()
            .filter_map(|(request, item)| Some((request, item?)))
            .collect()
    }

    fn pending_requests(
        &self,
        kind: impl Fn(&str) -> bool,
        item_type: &str,
    ) -> Vec<(&RuntimeRequest, Option<&TurnItem>)> {
        self.runtime_requests
            .iter()
            .filter(|request| request.status == "pending" && kind(&request.kind))
            .map(|request| {
                let item = self
                    .turn_items
                    .iter()
                    .rev()
                    .find(|item| item.item_type == item_type && item.str("requestId") == Some(request.id.as_str()));
                (request, item.map(Rc::as_ref))
            })
            .collect()
    }

    fn apply_event(&mut self, event: &Value, sequence: u64) -> Result<(), serde_json::Error> {
        let event_type = event.get("type").and_then(Value::as_str).unwrap_or_default();
        let payload = &event["payload"];
        match event_type {
            "run.created" | "run.updated" => {
                upsert(&mut self.runs, Run::deserialize(payload)?, |r| &r.id);
                self.refilter_visible();
            }
            "run-attempt.created" | "run-attempt.updated" => {
                upsert(&mut self.attempts, RunAttempt::deserialize(payload)?, |a| &a.id);
                self.refilter_visible();
            }
            "runtime-request.updated" => {
                upsert(&mut self.runtime_requests, RuntimeRequest::deserialize(payload)?, |r| &r.id)
            }
            "turn-item.updated" => {
                let mut item = TurnItem::deserialize(payload)?;
                item.revision = sequence;
                self.apply_turn_item(item);
            }
            "provider-thread.updated" => {
                upsert(&mut self.provider_threads, ProviderThread::deserialize(payload)?, |t| &t.id)
            }
            "provider-turn.updated" => {
                let mut turn = ProviderTurn::deserialize(payload)?;
                if turn.token_usage.is_none() {
                    let previous = self.provider_turns.iter().find(|t| t.id == turn.id);
                    turn.token_usage = previous.and_then(|t| t.token_usage.clone());
                }
                upsert(&mut self.provider_turns, turn, |t| &t.id);
            }
            "checkpoint.captured" => upsert(&mut self.checkpoints, Checkpoint::deserialize(payload)?, |c| &c.id),
            "node.updated" => upsert(&mut self.nodes, ExecutionNode::deserialize(payload)?, |n| &n.id),
            "provider-session.attached" | "provider-session.updated" => {
                upsert(&mut self.provider_sessions, ProviderSession::deserialize(payload)?, |s| &s.id)
            }
            "provider-session.detached" => {
                let id = payload["providerSessionId"].as_str().unwrap_or_default();
                if let Some(session) = self.provider_sessions.iter_mut().find(|s| s.id == id) {
                    session.status = "stopped".into();
                }
            }
            "thread.pull-request-link-updated" => {}
            kind if kind.starts_with("thread.") => self.thread = AppThread::deserialize(payload)?,
            _ => {}
        }
        Ok(())
    }

    fn apply_turn_item(&mut self, item: TurnItem) {
        let item = Rc::new(item);
        let was_interrupt_request = self
            .turn_items
            .iter()
            .any(|existing| existing.id == item.id && existing.item_type == "run_interrupt_request");
        let is_interrupt_request = item.item_type == "run_interrupt_request";
        upsert(&mut self.turn_items, item.clone(), |i| &i.id);
        // Only interrupt requests change other rows' visibility.
        if is_interrupt_request || was_interrupt_request {
            self.refilter_visible();
        }
        if self.is_visible(&item) {
            self.upsert_visible(item);
        } else {
            self.visible_turn_items.retain(|row| row.source_item_id != item.id);
        }
    }

    /// `isOrchestrationV2TurnItemVisible` in `packages/shared/src/orchestrationV2Timeline.ts`.
    fn is_visible(&self, item: &TurnItem) -> bool {
        let run_status = item
            .run_id
            .as_ref()
            .and_then(|id| self.runs.iter().find(|run| &run.id == id))
            .map(|run| run.status.as_str());
        if run_status == Some("rolled_back") {
            return false;
        }
        if run_status == Some("cancelled")
            && item.item_type == "user_message"
            && item.str("inputIntent") == Some("queued_turn")
        {
            return false;
        }
        if item.item_type == "run_interrupt_result" {
            if let (Some(run_id), Some(node_id)) = (&item.run_id, &item.node_id) {
                let superseded = self.attempts.iter().any(|attempt| {
                    &attempt.run_id == run_id
                        && attempt.root_node_id.as_ref() == Some(node_id)
                        && attempt.status == "superseded"
                });
                let has_request = self.turn_items.iter().any(|candidate| {
                    candidate.item_type == "run_interrupt_request" && candidate.run_id.as_ref() == Some(run_id)
                });
                if superseded && !has_request {
                    return false;
                }
            }
        }
        true
    }

    fn refilter_visible(&mut self) {
        let hidden: Vec<String> = self
            .visible_turn_items
            .iter()
            .filter(|row| row.visibility == "local" && !self.is_visible(&row.item))
            .map(|row| row.source_item_id.clone())
            .collect();
        if !hidden.is_empty() {
            self.visible_turn_items.retain(|row| !hidden.contains(&row.source_item_id));
        }
    }

    /// `upsertVisibleTurnItem`: replace in place when the ordinal is unchanged,
    /// else insert before the first later local row.
    fn upsert_visible(&mut self, item: Rc<TurnItem>) {
        let rows = &mut self.visible_turn_items;
        let existing = rows.iter().rposition(|row| row.source_item_id == item.id);
        if let Some(index) = existing {
            let row = &rows[index];
            if row.visibility == "local" && row.source_thread_id == item.thread_id && row.item.ordinal == item.ordinal {
                rows[index].item = item;
                return;
            }
            rows.remove(index);
        }
        let insertion = rows
            .iter()
            .position(|row| {
                row.visibility == "local"
                    && (row.item.ordinal > item.ordinal || (row.item.ordinal == item.ordinal && row.item.id > item.id))
            })
            .unwrap_or(rows.len());
        let row = VisibleRow {
            visibility: "local".into(),
            source_thread_id: item.thread_id.clone(),
            source_item_id: item.id.clone(),
            item,
        };
        rows.insert(insertion, row);
    }
}

fn upsert<T>(items: &mut Vec<T>, item: T, id: impl Fn(&T) -> &String) {
    match items.iter().position(|existing| id(existing) == id(&item)) {
        Some(index) => items[index] = item,
        None => items.push(item),
    }
}

/// One open thread's projection and resume cursor.
#[derive(Default)]
pub struct ThreadState {
    pub projection: Option<ThreadProjection>,
    pub sequence: u64,
    pub live: bool,
    /// Set by `thread.deleted`; the view should close.
    pub deleted: bool,
}

impl ThreadState {
    /// Applies one `orchestration.subscribeThread` element.
    pub fn apply(&mut self, item: &Value) -> Result<(), serde_json::Error> {
        match item.get("kind").and_then(Value::as_str).unwrap_or_default() {
            "synchronized" => self.live = true,
            "snapshot" => {
                let sequence = item["snapshotSequence"].as_u64().unwrap_or_default();
                match ThreadProjection::deserialize(&item["projection"]) {
                    Ok(mut projection) => {
                        // Freshly decoded, so every item is still unique and can be stamped.
                        for item in projection.turn_items.iter_mut() {
                            if let Some(item) = Rc::get_mut(item) {
                                item.revision = sequence;
                            }
                        }
                        // Rows share the table's item, so later updates replace one allocation.
                        let by_id: std::collections::HashMap<&str, &Rc<TurnItem>> =
                            projection.turn_items.iter().map(|item| (item.id.as_str(), item)).collect();
                        let shared: Vec<Option<Rc<TurnItem>>> = projection
                            .visible_turn_items
                            .iter()
                            .map(|row| by_id.get(row.item.id.as_str()).map(|item| Rc::clone(item)))
                            .collect();
                        for (row, shared) in projection.visible_turn_items.iter_mut().zip(shared) {
                            match shared {
                                Some(item) if item.thread_id == row.item.thread_id => row.item = item,
                                _ => {
                                    if let Some(item) = Rc::get_mut(&mut row.item) {
                                        item.revision = sequence;
                                    }
                                }
                            }
                        }
                        self.projection = Some(projection);
                        self.sequence = sequence;
                    }
                    Err(error) => {
                        // Resume from scratch, so a resubscribe fetches a fresh snapshot.
                        self.projection = None;
                        self.sequence = 0;
                        return Err(error);
                    }
                }
            }
            "event" => {
                let sequence = item["sequence"].as_u64().unwrap_or_default();
                // Without a projection the cursor stays put, so a resubscribe refetches the snapshot.
                let Some(projection) = self.projection.as_mut().filter(|_| !self.deleted) else { return Ok(()) };
                if sequence <= self.sequence {
                    return Ok(());
                }
                self.sequence = sequence;
                let event = &item["event"];
                if event.get("type").and_then(Value::as_str) == Some("thread.deleted") {
                    self.deleted = true;
                } else if let Err(error) = projection.apply_event(event, sequence) {
                    // An event we cannot decode still advances the cursor, like `unknown-event`.
                    log::warn!("skipping undecodable thread event {sequence}: {error}");
                }
            }
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn thread(id: &str, updated_at: &str) -> Value {
        json!({"id": id, "projectId": "p", "title": id, "status": "idle", "updatedAt": updated_at, "lineage": {"rootThreadId": id}})
    }

    fn project(root: &str, identity: Option<&str>) -> Value {
        json!({"id": root, "title": root, "workspaceRoot": root, "repositoryIdentity": identity.map(|name| json!({"displayName": name, "canonicalKey": name})), "scripts": []})
    }

    #[test]
    fn shell_enrichment_snapshot_only_patches_identity() {
        let mut shell = ShellState::default();
        shell
            .apply(&json!({"kind":"snapshot","snapshot":{"snapshotSequence":5,"projects":[project("a", None), project("b", None)],"threads":[thread("t", "1")]}}))
            .unwrap();
        shell
            .apply(&json!({"kind":"snapshot","resolvedRepositoryIdentityRoots":["a"],"snapshot":{"snapshotSequence":5,"projects":[project("a", Some("repo"))],"threads":[]}}))
            .unwrap();
        assert_eq!(shell.threads.len(), 1);
        assert_eq!(shell.projects.len(), 2);
        assert_eq!(shell.projects[0].repository_identity.as_ref().unwrap().display_name.as_deref(), Some("repo"));
    }

    #[test]
    fn an_undecodable_shell_delta_is_skipped_but_advances_the_cursor() {
        let mut shell = ShellState::default();
        shell.apply(&json!({"kind":"snapshot","snapshot":{"snapshotSequence":5,"projects":[],"threads":[thread("t", "1")]}})).unwrap();
        // Not an error: only a failed full snapshot asks for a resubscribe.
        shell.apply(&json!({"kind":"thread.updated","sequence":6,"location":"active","thread":{"id": 1}})).unwrap();
        assert_eq!(shell.sequence, 6);
        assert_eq!(shell.threads.len(), 1);
    }

    #[test]
    fn shell_deltas_respect_sequence_and_location() {
        let mut shell = ShellState::default();
        shell.apply(&json!({"kind":"snapshot","snapshot":{"snapshotSequence":5,"projects":[],"threads":[thread("t", "1")]}})).unwrap();
        shell
            .apply(&json!({"kind":"thread.updated","sequence":4,"location":"active","thread":thread("old", "1")}))
            .unwrap();
        assert_eq!(shell.threads.len(), 1, "stale delta is skipped");
        shell
            .apply(&json!({"kind":"thread.updated","sequence":6,"location":"active","thread":thread("u", "2")}))
            .unwrap();
        shell
            .apply(&json!({"kind":"thread.updated","sequence":7,"location":"archive","thread":thread("t", "3")}))
            .unwrap();
        let ids: Vec<_> = shell.threads.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, ["u"]);
        assert_eq!(shell.sequence, 7);
    }

    fn item(id: &str, item_type: &str, run_id: &str, ordinal: i64) -> Value {
        json!({"id": id, "type": item_type, "threadId": "t", "runId": run_id, "nodeId": null, "ordinal": ordinal, "status": "completed", "updatedAt": "x"})
    }

    fn projection(items: &[Value]) -> Value {
        let rows: Vec<_> = items
            .iter()
            .enumerate()
            .map(|(i, item)| json!({"position": i, "visibility": "local", "sourceThreadId": "t", "sourceItemId": item["id"], "item": item}))
            .collect();
        json!({"thread": {"id": "t", "projectId": "p", "title": "t"}, "runs": [{"id": "r1", "status": "running", "ordinal": 1}], "turnItems": items, "visibleTurnItems": rows})
    }

    fn event(sequence: u64, event_type: &str, payload: Value) -> Value {
        json!({"kind": "event", "sequence": sequence, "event": {"type": event_type, "threadId": "t", "occurredAt": "x", "payload": payload}})
    }

    fn timeline(state: &ThreadState) -> Vec<String> {
        state.projection.as_ref().unwrap().visible_turn_items.iter().map(|row| row.source_item_id.clone()).collect()
    }

    #[test]
    fn an_update_is_stored_once_for_the_table_and_the_timeline() {
        let mut state = ThreadState::default();
        state
            .apply(&json!({"kind":"snapshot","snapshotSequence":1,"projection": projection(&[item("a", "user_message", "r1", 1)])}))
            .unwrap();
        let shared = |state: &ThreadState, id: &str| {
            let p = state.projection.as_ref().unwrap();
            let row = p.visible_turn_items.iter().find(|row| row.source_item_id == id).unwrap();
            let entry = p.turn_items.iter().find(|item| item.id == id).unwrap();
            Rc::ptr_eq(&row.item, entry)
        };
        assert!(shared(&state, "a"), "a snapshot's rows share the table's items");
        state.apply(&event(2, "turn-item.updated", item("b", "assistant_message", "r1", 2))).unwrap();
        assert!(shared(&state, "b"), "an update shares one allocation");
    }

    #[test]
    fn turn_items_insert_in_ordinal_order_and_replace_in_place() {
        let mut state = ThreadState::default();
        state
            .apply(&json!({"kind":"snapshot","snapshotSequence":1,"projection": projection(&[item("a", "user_message", "r1", 1), item("c", "assistant_message", "r1", 3)])}))
            .unwrap();
        state.apply(&event(2, "turn-item.updated", item("b", "reasoning", "r1", 2))).unwrap();
        assert_eq!(timeline(&state), ["a", "b", "c"]);
        let mut updated = item("c", "assistant_message", "r1", 3);
        updated["text"] = json!("done");
        state.apply(&event(3, "turn-item.updated", updated)).unwrap();
        assert_eq!(timeline(&state), ["a", "b", "c"]);
        assert_eq!(state.projection.as_ref().unwrap().visible_turn_items[2].item.str("text"), Some("done"));
        state.apply(&event(3, "turn-item.updated", item("z", "reasoning", "r1", 9))).unwrap();
        assert_eq!(timeline(&state).len(), 3, "replayed sequence is ignored");
    }

    #[test]
    fn thread_snapshots_skip_bad_elements_and_reset_the_cursor_on_failure() {
        let mut state = ThreadState::default();
        let mut value = projection(&[item("a", "user_message", "r1", 1)]);
        value["turnItems"].as_array_mut().unwrap().push(json!({"type": "future"}));
        value["visibleTurnItems"].as_array_mut().unwrap().push(json!({"position": 9}));
        value["runs"].as_array_mut().unwrap().push(json!({"id": "r2"}));
        state.apply(&json!({"kind": "snapshot", "snapshotSequence": 4, "projection": value})).unwrap();
        let projection = state.projection.as_ref().unwrap();
        assert_eq!((projection.turn_items.len(), projection.runs.len()), (1, 1));
        assert_eq!(timeline(&state), ["a"]);
        assert_eq!(projection.turn_items[0].revision, 4);

        assert!(state.apply(&json!({"kind": "snapshot", "snapshotSequence": 6, "projection": {"runs": []}})).is_err());
        state.apply(&event(7, "turn-item.updated", item("b", "reasoning", "r1", 2))).unwrap();
        assert!(state.projection.is_none());
        assert_eq!(state.sequence, 0, "a resubscribe refetches the snapshot");
    }

    #[test]
    fn shell_snapshots_skip_bad_threads_and_reset_the_cursor_on_failure() {
        let mut shell = ShellState::default();
        let snapshot = |sequence: Value| json!({"kind": "snapshot", "snapshot": {"snapshotSequence": sequence, "projects": [project("a", None), {"id": "x"}], "threads": [thread("t", "1"), {"id": "bad"}]}});
        shell.apply(&snapshot(json!(5))).unwrap();
        assert_eq!((shell.projects.len(), shell.threads.len()), (1, 1));

        assert!(shell.apply(&snapshot(json!(null))).is_err());
        shell
            .apply(&json!({"kind":"thread.updated","sequence":9,"location":"active","thread":thread("u", "2")}))
            .unwrap();
        assert_eq!((shell.sequence, shell.threads.len()), (0, 1), "deltas wait for a fresh snapshot");
        shell.apply(&snapshot(json!(10))).unwrap();
        shell
            .apply(&json!({"kind":"thread.updated","sequence":11,"location":"active","thread":thread("u", "2")}))
            .unwrap();
        assert_eq!((shell.sequence, shell.threads.len()), (11, 2));
    }

    #[test]
    fn deleting_the_thread_ends_the_stream() {
        let mut state = ThreadState::default();
        state.apply(&json!({"kind":"snapshot","snapshotSequence":1,"projection": projection(&[])})).unwrap();
        state.apply(&event(2, "thread.deleted", json!({"threadId": "t", "deletedAt": "x"}))).unwrap();
        state.apply(&event(3, "turn-item.updated", item("a", "user_message", "r1", 1))).unwrap();
        assert!(state.deleted);
        assert!(timeline(&state).is_empty());
        assert_eq!(state.projection.as_ref().unwrap().thread.title, "t", "the payload is not decoded as a thread");
    }

    #[test]
    fn rolled_back_runs_leave_the_timeline() {
        let mut state = ThreadState::default();
        state.apply(&json!({"kind":"snapshot","snapshotSequence":1,"projection": projection(&[item("a", "user_message", "r1", 1)])})).unwrap();
        state.apply(&event(2, "run.updated", json!({"id": "r1", "status": "rolled_back", "ordinal": 1}))).unwrap();
        assert!(timeline(&state).is_empty());
    }

    #[test]
    fn new_thread_model_falls_back_to_a_ready_provider() {
        let mut info = ServerInfo::default();
        info.apply(&json!({"version": 1, "type": "snapshot", "config": {
            "providers": [
                {"instanceId": "off", "driver": "codex", "enabled": false, "status": "ready", "models": [{"slug": "a"}]},
                {"driver": "future", "enabled": true, "status": "ready"},
                {"instanceId": "claude", "driver": "claudeAgent", "enabled": true, "status": "ready",
                 "models": [{"slug": "custom", "isCustom": true}, {"slug": "haiku"}, {"slug": "opus", "isDefault": true}]}
            ],
            "settings": {
                "defaultModelSelection": {"instanceId": "off", "model": "a"},
                "projectSettingsOverrides": {"p": {"defaultModelSelection": {"instanceId": "claude", "model": "haiku"}}}
            },
            "environment": {"capabilities": {"threadStop": true}}
        }}))
        .unwrap();
        assert!(info.capability("threadStop"));
        assert_eq!(info.providers.len(), 2, "an undecodable provider is skipped");
        let selection = info.new_thread_model(None).unwrap();
        assert_eq!((selection.instance_id.as_str(), selection.model.as_str()), ("claude", "opus"));
        let project: ProjectShell =
            serde_json::from_value(json!({"id": "p", "title": "P", "workspaceRoot": "/p"})).unwrap();
        assert_eq!(info.new_thread_model(Some(&project)).unwrap().model, "haiku", "the project's override wins");
    }

    #[test]
    fn pending_approvals_join_their_request_item() {
        let mut approval = item("i", "approval_request", "r1", 2);
        approval["requestId"] = json!("q");
        let mut value = projection(&[approval]);
        value["runtimeRequests"] = json!([
            {"id": "q", "kind": "command", "status": "pending", "responseCapability": {"type": "live", "providerSessionId": "s"}},
            {"id": "u", "kind": "user_input", "status": "pending", "responseCapability": {"type": "live", "providerSessionId": "s"}},
            {"id": "done", "kind": "command", "status": "resolved", "responseCapability": {"type": "live", "providerSessionId": "s"}}
        ]);
        let projection = ThreadProjection::deserialize(&value).unwrap();
        let approvals = projection.pending_approvals();
        assert_eq!(approvals.len(), 1);
        assert_eq!(approvals[0].1.map(|item| item.id.as_str()), Some("i"));
    }
}
