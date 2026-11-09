//! The composer's per-target client state (`composerDraftStore`).

/// The composer's per-target choices that live only in this client (`composerDraftStore`).
#[derive(Default)]
pub(super) struct ComposerUi {
    /// The fallback notice the user dismissed, as `instance:resetsAt`.
    pub(super) dismissed_fallback: Option<String>,
    /// Why the last send was refused for length; it follows edits until it clears.
    pub(super) length_error: Option<String>,
    /// The footer controls row's laid-out width, and how its mode block fit there last.
    pub(super) footer_host: Option<f32>,
    pub(super) mode_fit: super::composer_fit::ModeFit,
    /// The footer controls' natural widths and what they were measured from; renders reuse
    /// them until that changes.
    pub(super) footer_widths:
        std::cell::RefCell<Option<(super::composer::FooterText, super::composer_fit::ControlsWidths)>>,
    /// The chip under the pointer and its window bounds, for its details tooltip.
    pub(super) chip_hover: Option<(String, gpui_kit::Bounds<gpui_kit::Pixels>)>,
}

impl super::Workspace {
    /// `thread.interaction-mode.set` back to build mode for a thread another client left in
    /// plan mode (`persistThreadSettingsForNextTurn`); plan mode is retired.
    pub(super) fn interaction_mode_command(&self, thread_id: &str) -> Option<serde_json::Value> {
        let current = self.open_projection()?.thread.interaction_mode.as_deref().unwrap_or("default");
        (current != "default").then(|| {
            serde_json::json!({"type": "thread.interaction-mode.set", "commandId": super::new_id(), "threadId": thread_id, "interactionMode": "default"})
        })
    }
}
