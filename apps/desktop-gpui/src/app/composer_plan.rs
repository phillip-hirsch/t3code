//! Plan mode: `resolveComposerInteractionMode`, the Build/Plan toggle of
//! `ComposerFooterModeControls`, `/plan` and `/default`, and the plan follow-up
//! (`ComposerPrimaryActions`: Refine, Implement, Implement in a new thread, as
//! `useProposedPlanSend.ts` sends them).

use std::collections::HashMap;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{Icon, Sizable as _, h_flex};
use gpui_kit::{
    AnyElement, Context, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, Window, div, prelude::FluentBuilder as _, px,
};
use serde_json::{Value, json};

use super::composer::control_button;
use super::{Target, Workspace, new_id};
use crate::model::Provider;
use crate::theme::Tokens;

const PLAN_IMPLEMENTATION_PROMPT_PREFIX: &str = "PLEASE IMPLEMENT THIS PLAN:\n";

/// The composer's per-target choices that live only in this client (`composerDraftStore`).
#[derive(Default)]
pub(super) struct ComposerUi {
    /// Build or Plan as picked per target; a thread takes it on its next send.
    interaction: HashMap<Target, &'static str>,
    /// The fallback notice the user dismissed, as `instance:resetsAt`.
    pub(super) dismissed_fallback: Option<String>,
    /// "Implement in a new thread" is creating its thread.
    implementing: bool,
    /// Why the last send was refused for length; it follows edits until it clears.
    pub(super) length_error: Option<String>,
}

/// `resolveComposerInteractionMode`: whether the toggle shows, and the mode sends use.
pub(super) fn resolve_interaction_mode(
    plan_mode_enabled: bool,
    provider: Option<&Provider>,
    requested: Option<&str>,
) -> (bool, &'static str) {
    let enabled = plan_mode_enabled && provider.is_some_and(|p| p.show_interaction_mode_toggle != Some(false));
    let mode = if enabled && requested == Some("plan") { "plan" } else { "default" };
    (enabled, mode)
}

/// `parseStandaloneComposerSlashCommand`: a prompt that is only `/plan` or `/default`.
pub(super) fn standalone_slash_command(text: &str) -> Option<&'static str> {
    let command = text.trim().strip_prefix('/')?.trim_end().to_lowercase();
    match command.as_str() {
        "plan" => Some("plan"),
        "default" => Some("default"),
        _ => None,
    }
}

/// `resolvePlanFollowUpSubmission`: typed text refines the plan, an empty prompt implements it.
pub(super) fn plan_follow_up(draft: &str, plan_markdown: &str) -> (String, &'static str) {
    match draft.trim() {
        "" => (format!("{PLAN_IMPLEMENTATION_PROMPT_PREFIX}{}", plan_markdown.trim()), "default"),
        text => (text.to_owned(), "plan"),
    }
}

/// `buildPlanImplementationThreadTitle` + `truncate`: the plan's first heading, 50 characters at most.
fn implementation_thread_title(plan_markdown: &str) -> String {
    let heading = plan_markdown.lines().find_map(|line| {
        let indent = line.len() - line.trim_start_matches(' ').len();
        let rest = line[indent..].trim_start_matches('#');
        let hashes = line.len() - indent - rest.len();
        (indent <= 3 && (1..=6).contains(&hashes) && rest.starts_with([' ', '\t']))
            .then(|| rest.trim())
            .filter(|title| !title.is_empty())
    });
    let title = heading.map_or_else(|| "Implement plan".to_owned(), |title| format!("Implement {title}"));
    match title.char_indices().nth(50) {
        Some((end, _)) => format!("{}...", &title[..end]),
        None => title,
    }
}

impl Workspace {
    fn requested_interaction_mode(&self) -> Option<&str> {
        let target = self.target.as_ref()?;
        if let Some(mode) = self.composer_ui.interaction.get(target) {
            return Some(mode);
        }
        match target {
            Target::Thread { .. } => self.open_projection()?.thread.interaction_mode.as_deref(),
            Target::Draft { .. } => None,
        }
    }

    /// Whether the Build/Plan toggle shows, and the interaction mode the next send uses.
    pub(super) fn interaction_mode(&self) -> (bool, &'static str) {
        let provider = self.composer_provider();
        resolve_interaction_mode(
            self.client_settings.values.bool("planModeEnabled"),
            provider,
            self.requested_interaction_mode(),
        )
    }

    /// `handleInteractionModeChange`.
    pub(super) fn set_interaction_mode(&mut self, mode: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        let (enabled, current) = self.interaction_mode();
        let Some(target) = self.target.clone() else { return };
        if (mode == "plan" && !enabled) || mode == current {
            return;
        }
        self.composer_ui.interaction.insert(target, mode);
        self.focus_composer(window, cx);
        cx.notify();
    }

    pub(super) fn toggle_interaction_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (enabled, mode) = self.interaction_mode();
        if enabled {
            self.set_interaction_mode(if mode == "plan" { "default" } else { "plan" }, window, cx);
        }
    }

    /// `thread.interaction-mode.set` when `mode` differs from the thread's
    /// (`persistThreadSettingsForNextTurn`).
    pub(super) fn interaction_mode_command(&self, thread_id: &str, mode: &str) -> Option<Value> {
        let current = self.open_projection()?.thread.interaction_mode.as_deref().unwrap_or("default");
        (current != mode).then(|| {
            json!({"type": "thread.interaction-mode.set", "commandId": new_id(), "threadId": thread_id, "interactionMode": mode})
        })
    }

    /// Records the mode a send used, so the composer keeps showing it.
    pub(super) fn remember_interaction_mode(&mut self, target: Target, mode: &'static str) {
        self.composer_ui.interaction.insert(target, mode);
    }

    /// `shouldShowPlanFollowUpPrompt` and `findLatestProposedPlan`: the plan the
    /// next send follows up on, as (plan id, markdown).
    pub(super) fn plan_follow_up_target(&self) -> Option<(String, String)> {
        self.active_plan_follow_up().map(|(id, markdown)| (id.to_owned(), markdown.to_owned()))
    }

    /// Whether the next send follows up on a plan; for render, without copying the plan.
    pub(super) fn has_plan_follow_up(&self) -> bool {
        self.active_plan_follow_up().is_some()
    }

    fn active_plan_follow_up(&self) -> Option<(&str, &str)> {
        let projection = self.open_projection()?;
        let (enabled, mode) = self.interaction_mode();
        if !enabled
            || mode != "plan"
            || projection.is_running()
            || !projection.pending_user_inputs().is_empty()
            || self.has_attachments()
        {
            return None;
        }
        let (plan, markdown) = projection.latest_proposed_plan()?;
        (plan.status == "active").then_some((plan.id.as_str(), markdown))
    }

    /// `useImplementPlanInNewThread`: a fresh default-mode thread that implements
    /// the plan; it is deleted again when its first message fails.
    fn implement_plan_in_new_thread(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some((plan_id, markdown)), Some(client), Some(Target::Thread { id: source_id, .. })) =
            (self.plan_follow_up_target(), self.client(), self.target.clone())
        else {
            return;
        };
        let (Some(project), Some(model)) = (self.target_project(), self.composer_model()) else { return };
        if self.composer_ui.implementing || self.sending {
            return;
        }
        let projection = self.open_projection();
        let thread_id = new_id();
        let title = implementation_thread_title(&markdown);
        let runtime_mode = self.default_runtime_mode();
        let create = json!({
            "type": "thread.create",
            "commandId": new_id(),
            "createdBy": "user",
            "creationSource": "web",
            "threadId": thread_id,
            "projectId": project.id,
            "title": title,
            "modelSelection": model.to_json(),
            "runtimeMode": runtime_mode,
            "interactionMode": "default",
            "branch": projection.and_then(|p| p.thread.branch.clone()),
            "worktreePath": projection.and_then(|p| p.thread.worktree_path.clone()),
        });
        let message = json!({
            "type": "message.dispatch",
            "commandId": new_id(),
            "createdBy": "user",
            "creationSource": "web",
            "threadId": thread_id,
            "messageId": new_id(),
            "text": format!("{PLAN_IMPLEMENTATION_PROMPT_PREFIX}{}", markdown.trim()),
            "attachments": [],
            "titleSeed": title,
            "modelSelection": model.to_json(),
            "sourcePlanRef": {"threadId": source_id, "planId": plan_id},
            "deliveryIntent": "auto",
            "dispatchMode": {"type": "start_immediately"},
        });
        self.composer_ui.implementing = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let dispatch = |command: Value| client.request("orchestration.dispatchCommand", command);
            let mut result = dispatch(create).await.map(|_| ());
            if result.is_ok() {
                result = dispatch(message).await.map(|_| ());
                if result.is_err() {
                    let delete = json!({"type": "thread.delete", "commandId": new_id(), "threadId": thread_id});
                    if let Err(error) = dispatch(delete).await {
                        log::warn!("could not clean up the implementation thread: {error}");
                    }
                }
            }
            let _ = this.update_in(cx, |this, window, cx| {
                this.composer_ui.implementing = false;
                match result {
                    Ok(()) => this.select_thread(thread_id, window, cx),
                    Err(error) => this.error = Some(format!("Could not start implementation thread: {error}").into()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The Build/Plan toggle in the footer.
    pub(super) fn render_interaction_toggle(
        &self,
        mode: &'static str,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let plan = mode == "plan";
        let tooltip = if plan {
            "Plan mode. Click to return to normal build mode"
        } else {
            "Default mode. Click to enter plan mode"
        };
        control_button("interaction-mode", t)
            .child(
                Icon::new(if plan { Lucide::PencilRuler } else { Lucide::Bot })
                    .with_size(px(if plan { 16. } else { 17. }))
                    .when(!plan, |icon| icon.text_color(t.muted_foreground)),
            )
            .child(if plan { "Plan" } else { "Build" })
            .tooltip(tooltip)
            .on_click(cx.listener(|this, _, window, cx| this.toggle_interaction_mode(window, cx)))
            .into_any_element()
    }

    /// Refine while text is typed; otherwise Implement with "Implement in a new thread" beside it.
    pub(super) fn render_plan_follow_up_action(&self, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.sending || self.composer_ui.implementing;
        let enabled = !busy && self.client().is_some() && !self.target_rewinding();
        let pill = |id: &'static str| {
            h_flex()
                .id(id)
                .h(px(32.))
                .flex_none()
                .items_center()
                .justify_center()
                .gap(px(6.))
                .rounded_full()
                .bg(t.primary)
                .text_color(t.primary_foreground)
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .when(!enabled, |el| el.opacity(0.64))
                .when(enabled, |el| el.cursor_pointer().hover(|s| s.bg(t.primary_hover)))
        };
        let label = |idle: &'static str| if busy { "Sending..." } else { idle };
        if !self.composer_empty {
            return pill("plan-refine")
                .px(px(16.))
                .child(label("Refine"))
                .when(enabled, |el| {
                    el.on_click(
                        cx.listener(|this, _, window, cx| {
                            this.submit_with(super::send::Intent::Foreground, window, cx)
                        }),
                    )
                })
                .into_any_element();
        }
        let view = cx.entity();
        h_flex()
            .child(pill("plan-implement").px(px(16.)).rounded_r(px(0.)).child(label("Implement")).when(enabled, |el| {
                el.on_click(
                    cx.listener(|this, _, window, cx| this.submit_with(super::send::Intent::Foreground, window, cx)),
                )
            }))
            .child(
                div().child(
                    gpui_kit::component::button::Button::new("plan-implement-menu")
                        .h(px(32.))
                        .px(px(8.))
                        .rounded_l(px(0.))
                        .rounded_r_full()
                        .border_l_1()
                        .border_color(t.primary_foreground.opacity(0.2))
                        .bg(t.primary)
                        .text_color(t.primary_foreground)
                        .when(!enabled, |b| b.opacity(0.64))
                        .child(Icon::new(Lucide::ChevronDown).with_size(px(14.)))
                        .tooltip("Implementation actions")
                        .dropdown_menu_with_anchor(gpui_kit::Anchor::BottomRight, move |menu, window, _| {
                            menu.item(PopupMenuItem::new("Implement in a new thread").disabled(!enabled).on_click(
                                window.listener_for(&view, |this, _, window, cx| {
                                    this.implement_plan_in_new_thread(window, cx)
                                }),
                            ))
                        }),
                ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(toggle: Option<bool>) -> Provider {
        let mut value = json!({"instanceId": "p", "driver": "codex", "enabled": true});
        if let Some(toggle) = toggle {
            value["showInteractionModeToggle"] = toggle.into();
        }
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn plan_mode_needs_the_setting_and_a_provider_that_shows_the_toggle() {
        let shown = provider(None);
        assert_eq!(resolve_interaction_mode(true, Some(&shown), Some("plan")), (true, "plan"));
        assert_eq!(resolve_interaction_mode(true, Some(&shown), None), (true, "default"));
        assert_eq!(resolve_interaction_mode(false, Some(&shown), Some("plan")), (false, "default"));
        assert_eq!(resolve_interaction_mode(true, None, Some("plan")), (false, "default"));
        assert_eq!(resolve_interaction_mode(true, Some(&provider(Some(false))), Some("plan")), (false, "default"));
        assert_eq!(resolve_interaction_mode(true, Some(&provider(Some(true))), Some("plan")), (true, "plan"));
    }

    #[test]
    fn only_a_bare_plan_or_default_switches_modes() {
        assert_eq!(standalone_slash_command(" /plan "), Some("plan"));
        assert_eq!(standalone_slash_command("/DEFAULT"), Some("default"));
        assert_eq!(standalone_slash_command("/plan this"), None);
        assert_eq!(standalone_slash_command("plan"), None);
        assert_eq!(standalone_slash_command("/planning"), None);
    }

    #[test]
    fn an_empty_follow_up_implements_and_text_refines() {
        assert_eq!(
            plan_follow_up("  ", "# Plan\n- a\n"),
            ("PLEASE IMPLEMENT THIS PLAN:\n# Plan\n- a".into(), "default")
        );
        assert_eq!(plan_follow_up(" tweak it ", "# Plan"), ("tweak it".into(), "plan"));
    }

    #[test]
    fn implementation_threads_are_named_after_the_plan_heading() {
        assert_eq!(implementation_thread_title("intro\n## Add  login \nbody"), "Implement Add  login");
        assert_eq!(implementation_thread_title("no heading\n#tag"), "Implement plan");
        assert_eq!(implementation_thread_title("    # indented code"), "Implement plan");
        let long = format!("# {}", "x".repeat(60));
        assert_eq!(implementation_thread_title(&long), format!("Implement {}...", "x".repeat(40)));
    }
}
