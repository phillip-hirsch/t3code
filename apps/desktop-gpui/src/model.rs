//! Wire shapes and reducers for the orchestration read model.
//!
//! Mirrors `packages/client-runtime/src/state/shellReducer.ts` and
//! `orchestrationV2Projection.ts`. Enum-like fields stay strings so new server
//! values never fail a whole frame; only fields the UI reads are typed.

use serde::Deserialize;
use serde_json::Value;

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
}

/// `ServerProviderUsageLimits` (`packages/contracts/src/providerUsageLimits.ts`).
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageLimits {
    #[serde(default)]
    pub windows: Vec<UsageWindow>,
    #[serde(default)]
    pub unavailable: Option<Value>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageWindow {
    pub id: String,
    pub label: String,
    pub used_percent: f64,
}

impl UsageWindow {
    /// `remainingPercent` in `packages/shared/src/usageLimits.ts`.
    pub fn remaining_percent(&self) -> u32 {
        (100. - self.used_percent.clamp(0., 100.)).round() as u32
    }
}

impl Provider {
    fn is_selectable(&self) -> bool {
        self.enabled && self.availability.as_deref() != Some("unavailable")
    }

    /// `getDefaultProviderInstanceModel` in `apps/web/src/providerInstances.ts`.
    fn default_model(&self) -> Option<&str> {
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
    pub thread_stop: bool,
    /// The environment's name, e.g. the machine's host name.
    pub environment_label: Option<String>,
    platform_machine: Option<String>,
    environment_icon: Option<String>,
}

impl ServerInfo {
    pub fn apply(&mut self, event: &Value) -> Result<(), serde_json::Error> {
        match event.get("type").and_then(Value::as_str).unwrap_or_default() {
            "snapshot" => {
                let config = &event["config"];
                self.providers = Vec::deserialize(&config["providers"])?;
                self.apply_settings(&config["settings"])?;
                self.thread_stop =
                    config.pointer("/environment/capabilities/threadStop").and_then(Value::as_bool) == Some(true);
                let text = |pointer| config.pointer(pointer).and_then(Value::as_str).map(str::to_owned);
                self.environment_label = text("/environment/label");
                self.platform_machine = text("/environment/platform/machine");
            }
            "providerStatuses" => self.providers = Vec::deserialize(&event["payload"]["providers"])?,
            "settingsUpdated" => self.apply_settings(&event["payload"]["settings"])?,
            _ => {}
        }
        Ok(())
    }

    fn apply_settings(&mut self, settings: &Value) -> Result<(), serde_json::Error> {
        self.default_model_selection =
            Option::deserialize(settings.get("defaultModelSelection").unwrap_or(&Value::Null))?;
        self.environment_icon = settings.get("environmentIcon").and_then(Value::as_str).map(str::to_owned);
        Ok(())
    }

    /// `resolveEnvironmentMachineKind` in `packages/contracts/src/server.ts`.
    pub fn machine_kind(&self) -> &str {
        self.environment_icon.as_deref().or(self.platform_machine.as_deref()).unwrap_or("server")
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

    pub fn model_name(&self, selection: &ModelSelection) -> Option<&str> {
        let provider = self.provider(&selection.instance_id)?;
        provider.models.iter().find(|m| m.slug == selection.model)?.name.as_deref()
    }

    pub fn provider(&self, instance_id: &str) -> Option<&Provider> {
        self.providers.iter().find(|p| p.instance_id == instance_id)
    }

    /// Model for a new thread: the project's or the user's default when its
    /// provider is usable, else the first ready provider's default model.
    /// Mirrors `resolveDefaultProviderModelSelection`.
    pub fn new_thread_model(&self, project: Option<&ProjectShell>) -> Option<ModelSelection> {
        let preferred =
            project.and_then(|p| p.default_model_selection.as_ref()).or(self.default_model_selection.as_ref());
        if let Some(selection) = preferred {
            if self.provider(&selection.instance_id).is_some_and(Provider::is_selectable) {
                return Some(selection.clone());
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
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectShell {
    pub id: String,
    pub title: String,
    pub workspace_root: String,
    #[serde(default)]
    pub repository_identity: Option<RepositoryIdentity>,
    #[serde(default)]
    pub default_model_selection: Option<ModelSelection>,
    #[serde(default)]
    pub pinned_at: Option<String>,
    #[serde(default)]
    pub archived_at: Option<String>,
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
pub struct ThreadShell {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub status: String,
    #[serde(default)]
    pub activity_run_status: Option<String>,
    #[serde(default)]
    pub blocked_on_thread_wait: Option<bool>,
    #[serde(default)]
    pub pending_runtime_request: Option<PendingRequestSummary>,
    #[serde(default)]
    pub lineage: Lineage,
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
}

/// Sidebar pill, in the web's precedence order (`apps/web/src/components/Sidebar.logic.ts`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThreadPill {
    Approval,
    Input,
    Waiting,
    Working,
    Failed,
    Limited,
    PlanReady,
    Idle,
}

impl ThreadShell {
    /// Subagents and side chats live under their parent, not in the main list.
    pub fn is_listed(&self) -> bool {
        self.lineage.relationship_to_parent.as_deref() != Some("subagent") && self.lineage.side_chat != Some(true)
    }

    pub fn pill(&self) -> ThreadPill {
        if let Some(request) = &self.pending_runtime_request {
            match request.kind.as_str() {
                "user_input" => return ThreadPill::Input,
                "auth_refresh" => {}
                _ => return ThreadPill::Approval,
            }
        }
        let status = self.activity_run_status.as_deref().unwrap_or(&self.status);
        if is_active_run_status(status) {
            return if self.blocked_on_thread_wait == Some(true) { ThreadPill::Waiting } else { ThreadPill::Working };
        }
        if self.status == "failed" {
            return if self.last_error_class.as_deref() == Some("usage_limit") {
                ThreadPill::Limited
            } else {
                ThreadPill::Failed
            };
        }
        if self.has_actionable_proposed_plan {
            return ThreadPill::PlanReady;
        }
        ThreadPill::Idle
    }
}

pub fn is_active_run_status(status: &str) -> bool {
    matches!(status, "preparing" | "queued" | "starting" | "running" | "waiting")
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ShellSnapshot {
    snapshot_sequence: u64,
    #[serde(default)]
    projects: Vec<ProjectShell>,
    #[serde(default)]
    threads: Vec<ThreadShell>,
}

#[derive(Default)]
pub struct ShellState {
    pub projects: Vec<ProjectShell>,
    pub threads: Vec<ThreadShell>,
    pub sequence: u64,
    pub live: bool,
}

impl ShellState {
    /// Applies one `orchestration.subscribeShell` element.
    pub fn apply(&mut self, item: &Value) -> Result<(), serde_json::Error> {
        let kind = item.get("kind").and_then(Value::as_str).unwrap_or_default();
        let sequence = item.get("sequence").and_then(Value::as_u64);
        if let Some(sequence) = sequence {
            if sequence <= self.sequence {
                return Ok(());
            }
        }
        match kind {
            "synchronized" => self.live = true,
            "snapshot" => {
                let snapshot = ShellSnapshot::deserialize(&item["snapshot"])?;
                match item.get("resolvedRepositoryIdentityRoots").and_then(Value::as_array) {
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
                    }
                }
            }
            "project.updated" => {
                let mut project = ProjectShell::deserialize(&item["project"])?;
                match self.projects.iter_mut().find(|p| p.id == project.id) {
                    Some(existing) => {
                        if project.repository_identity.is_none() {
                            project.repository_identity = existing.repository_identity.take();
                        }
                        *existing = project;
                    }
                    None => self.projects.push(project),
                }
            }
            "project.removed" => {
                let id = item["projectId"].as_str().unwrap_or_default();
                self.projects.retain(|p| p.id != id);
            }
            "thread.updated" => {
                let thread = ThreadShell::deserialize(&item["thread"])?;
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

    /// Listed threads for a project, newest activity first.
    pub fn project_threads(&self, project_id: &str) -> Vec<&ThreadShell> {
        let mut threads: Vec<_> = self
            .threads
            .iter()
            .filter(|t| {
                t.project_id == project_id && t.is_listed() && t.archived_at.is_none() && t.deleted_at.is_none()
            })
            .collect();
        threads.sort_by(|a, b| {
            b.pinned_at.is_some().cmp(&a.pinned_at.is_some()).then_with(|| b.updated_at.cmp(&a.updated_at))
        });
        threads
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
    pub worktree_path: Option<String>,
    #[serde(default)]
    pub active_provider_thread_id: Option<String>,
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
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderTurn {
    pub id: String,
    #[serde(default)]
    pub token_usage: Option<TokenUsage>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Run {
    pub id: String,
    pub status: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunAttempt {
    pub id: String,
    pub run_id: String,
    #[serde(default)]
    pub root_node_id: Option<String>,
    pub status: String,
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
        self.response_capability.get("type").and_then(Value::as_str) == Some("live")
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
    pub item: TurnItem,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadProjection {
    pub thread: AppThread,
    #[serde(default)]
    pub runs: Vec<Run>,
    #[serde(default)]
    pub attempts: Vec<RunAttempt>,
    #[serde(default)]
    pub runtime_requests: Vec<RuntimeRequest>,
    #[serde(default)]
    pub turn_items: Vec<TurnItem>,
    /// The timeline, in render order.
    #[serde(default)]
    pub visible_turn_items: Vec<VisibleRow>,
    #[serde(default)]
    pub provider_threads: Vec<ProviderThread>,
    #[serde(default)]
    pub provider_turns: Vec<ProviderTurn>,
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
                (request, item)
            })
            .collect()
    }

    fn apply_event(&mut self, event: &Value) -> Result<(), serde_json::Error> {
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
            "turn-item.updated" => self.apply_turn_item(TurnItem::deserialize(payload)?),
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
            "thread.pull-request-link-updated" => {}
            kind if kind.starts_with("thread.") => self.thread = AppThread::deserialize(payload)?,
            _ => {}
        }
        Ok(())
    }

    fn apply_turn_item(&mut self, item: TurnItem) {
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
    fn upsert_visible(&mut self, item: TurnItem) {
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
}

impl ThreadState {
    /// Applies one `orchestration.subscribeThread` element.
    pub fn apply(&mut self, item: &Value) -> Result<(), serde_json::Error> {
        match item.get("kind").and_then(Value::as_str).unwrap_or_default() {
            "synchronized" => self.live = true,
            "snapshot" => {
                self.projection = Some(ThreadProjection::deserialize(&item["projection"])?);
                self.sequence = item["snapshotSequence"].as_u64().unwrap_or_default();
            }
            "event" => {
                let sequence = item["sequence"].as_u64().unwrap_or_default();
                if sequence <= self.sequence {
                    return Ok(());
                }
                self.sequence = sequence;
                if let Some(projection) = &mut self.projection {
                    // An event we cannot decode still advances the cursor, like `unknown-event`.
                    if let Err(error) = projection.apply_event(&item["event"]) {
                        log::warn!("skipping undecodable thread event {sequence}: {error}");
                    }
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
                {"instanceId": "claude", "driver": "claudeAgent", "enabled": true, "status": "ready",
                 "models": [{"slug": "custom", "isCustom": true}, {"slug": "haiku"}, {"slug": "opus", "isDefault": true}]}
            ],
            "settings": {"defaultModelSelection": {"instanceId": "off", "model": "a"}},
            "environment": {"capabilities": {"threadStop": true}}
        }}))
        .unwrap();
        assert!(info.thread_stop);
        let selection = info.new_thread_model(None).unwrap();
        assert_eq!((selection.instance_id.as_str(), selection.model.as_str()), ("claude", "opus"));
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
