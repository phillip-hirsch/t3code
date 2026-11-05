//! Webhook tasks (`ScheduledTasksSettings.tsx`: `WebhookEndpointField`, `WebhookFiltersField`,
//! `WebhookDeliveriesDialog`, the webhook fields of the editor). Seams for the webhook port;
//! the list and editor in `mod.rs` / `editor.rs` already call each of them:
//!
//! - [`WebhookFields`] owns the editor's webhook-only state: the `DraftState` fields
//!   `signature*`, `maxDeliveryAgeMinutes`, `replayKey`, `coalesceKey`, `coalesceWindowSeconds`,
//!   `filters` and `savedFilters`, and the inputs that edit them. The editor builds it once per
//!   opening with [`WebhookFields::new`], asks it for [`WebhookFields::errors`]
//!   (`scheduleDraftErrors`' webhook branch) and, on save, for [`WebhookFields::schedule`]
//!   (`scheduleFromDraft`'s webhook branch plus `submit`'s filter and signing-secret checks).
//! - [`render_webhook_fields`] draws the editor body while "On webhook" is picked: endpoint URL
//!   with Copy/Rotate (`scheduledTasks.rotateWebhookToken`) down to the signature grid.
//! - [`render_webhook_task_extras`] adds a webhook task's row-menu entries: "Deliveries", in place
//!   of "Run now", opening the deliveries dialog (`scheduledTasks.listWebhookDeliveries`,
//!   `scheduledTasks.getWebhookDelivery`, both with `acceptAnyOutcome: true`).

use gpui_kit::component::menu::PopupMenu;
use gpui_kit::{AnyElement, App, Context, Entity, IntoElement, Render, Window, div};
use serde_json::Value;

use super::logic::{DraftError, ScheduledTask};
use crate::rpc::RpcClient;

/// `DEFAULT_WEBHOOK_PROMPT`: the prompt a blank task gets when switched to "On webhook".
pub(super) const DEFAULT_WEBHOOK_PROMPT: &str = "Handle this webhook:\n{{body}}";

/// The editor's webhook state; see the module docs.
pub(super) struct WebhookFields {}

impl WebhookFields {
    /// `taskToDraft`'s webhook fields from the task being edited, or the defaults for a new task.
    pub(super) fn new(_task: Option<&ScheduledTask>, _window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self {}
    }

    /// Why the webhook fields cannot save (`scheduleDraftErrors`' webhook branch).
    pub(super) fn errors(&self) -> Vec<DraftError> {
        Vec::new()
    }

    /// The `ScheduledTaskUpsertSchedule` to save. `live` is the stored task while editing,
    /// for its `webhook.hasSecret`.
    pub(super) fn schedule(&self, _live: Option<&ScheduledTask>) -> Result<Value, DraftError> {
        Err(DraftError::new("Webhook tasks are not available yet", "Pick a time or an interval."))
    }
}

impl Render for WebhookFields {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

/// The editor body while "On webhook" is picked. `live` is the stored task (it carries the
/// endpoint once saved); `client` reaches `scheduledTasks.rotateWebhookToken`.
pub(super) fn render_webhook_fields(
    fields: &Entity<WebhookFields>,
    _live: Option<&ScheduledTask>,
    _client: &RpcClient,
    _window: &mut Window,
    _cx: &mut App,
) -> AnyElement {
    fields.clone().into_any_element()
}

/// Adds `task`'s webhook-only row-menu entries; called only for webhook tasks, after "Edit".
pub(super) fn render_webhook_task_extras(
    menu: PopupMenu,
    _task: &ScheduledTask,
    _client: &RpcClient,
    _window: &mut Window,
    _cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    menu
}
