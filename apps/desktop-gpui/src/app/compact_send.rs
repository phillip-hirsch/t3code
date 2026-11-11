//! Compact-before-send (`resumeCompactionTokens` in `ChatView.tsx`, `planComposerTurn`, the
//! Compact / Keep toggle of `ComposerPrimaryActions.tsx`): a Claude thread left idle past its
//! prompt cache compacts before the next message, unless the user keeps its full history.

use std::collections::HashSet;
use std::time::Duration;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, Sizable as _, v_flex};
use gpui_kit::{
    AnyElement, Context, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Task, div, prelude::FluentBuilder as _, px,
};

use super::{Target, Workspace};
use crate::model::RuntimeRequest;
use crate::theme::Tokens;

/// `COMPACTION_SUGGESTION_MIN_TOKENS`.
const MIN_TOKENS: u64 = 200_000;
/// Claude's 1h cache plus slack, for usage that reports no TTL.
const FALLBACK_TTL_MS: u64 = 70 * 60_000;
/// `CLAUDE_RESUME_COMPACTION_NEVER_ANSWER`.
const NEVER_ANSWER: &str = "Don't ask again";

#[derive(Default)]
pub(super) struct CompactState {
    /// Threads whose next send keeps the whole history.
    full_history: HashSet<Target>,
    /// Environment and provider instance pairs answered "Don't ask again" in Claude's own resume
    /// question; kept for the session, where the web keeps them in `localStorage`.
    dismissed: HashSet<(SharedString, String)>,
    /// Repaints when the open thread's offer comes due, as the web's minute clock does.
    wake: Option<(i64, Task<()>)>,
}

/// `COMPACT_COMMAND_PATTERN`: a typed `/compact`, with or without instructions, is the compaction.
pub(super) fn is_compact_command(text: &str) -> bool {
    text.get(..8).is_some_and(|head| head.eq_ignore_ascii_case("/compact"))
        && text[8..].chars().next().is_none_or(char::is_whitespace)
}

/// `isClaudeResumeCompactionQuestion`: "This session is 1h 5m old and uses 230,000 tokens. Compact
/// it before continuing?"
fn is_resume_question(question: &str) -> bool {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let minutes = |s: &str| s.strip_suffix('m').is_some_and(digits);
    let Some((age, tokens)) = question
        .strip_prefix("This session is ")
        .and_then(|rest| rest.strip_suffix(" tokens. Compact it before continuing?"))
        .and_then(|rest| rest.split_once(" old and uses "))
    else {
        return false;
    };
    let age_ok = match age.split_once("h ") {
        Some((hours, rest)) => digits(hours) && minutes(rest),
        None => minutes(age),
    };
    let mut groups = tokens.split(',');
    let lead_ok = groups.next().is_some_and(|lead| digits(lead) && lead.len() <= 3);
    age_ok && lead_ok && groups.all(|group| group.len() == 3 && digits(group))
}

/// `hasDismissedResumeCompaction`: Claude's resume question was answered "Don't ask again".
fn dismissed_natively(requests: &[RuntimeRequest]) -> bool {
    requests.iter().filter(|r| r.kind == "user_input" && r.status == "resolved").any(|request| {
        request.answers.as_object().is_some_and(|answers| {
            answers
                .iter()
                .any(|(question, answer)| is_resume_question(question) && answer.as_str() == Some(NEVER_ANSWER))
        })
    })
}

/// `shouldOfferResumeCompaction` as a time: when a Claude context this large has outlived its
/// prompt cache, or `None` when it never qualifies.
fn offered_at(driver: &str, used: u64, updated_at: Option<&str>, ttl_ms: Option<u64>) -> Option<i64> {
    if driver != "claudeAgent" || used < MIN_TOKENS {
        return None;
    }
    let updated = crate::timestamp::parse_ms(updated_at?)?;
    Some(updated.saturating_add(i64::try_from(ttl_ms.unwrap_or(FALLBACK_TTL_MS)).unwrap_or(i64::MAX)))
}

/// `resolveIdleCompaction`: the instance's own mode, else the environment's.
fn idle_compaction_mode<'a>(settings: &'a crate::settings::Settings, instance_id: &str) -> &'a str {
    let own = settings.get("providerInstances")[instance_id]["config"]["idleCompaction"].as_str();
    match own.filter(|mode| *mode != "default") {
        Some(mode) => mode,
        None => {
            super::pages::settings::idle_compaction(settings.get("idleCompaction"), settings.get("compactIdleThreads"))
        }
    }
}

/// A conversation worth compacting: a user message other than a bare `/compact`.
fn has_compactable_conversation(projection: &crate::model::ThreadProjection) -> bool {
    projection.visible_turn_items.iter().map(|row| &row.item).any(|item| {
        item.item_type == "user_message"
            && (item.str("text").is_some_and(|text| !text.trim().eq_ignore_ascii_case("/compact"))
                || item.raw.get("attachments").and_then(|a| a.as_array()).is_some_and(|a| !a.is_empty()))
    })
}

impl Workspace {
    /// `compactThreadUnavailable`: the open thread cannot compact now, before a send.
    pub(super) fn compact_unavailable(&self, projection: &crate::model::ThreadProjection) -> bool {
        let compactable =
            self.composer_provider().is_some_and(|p| p.slash_commands.iter().any(|c| c.name == "compact"));
        !compactable
            || self.client().is_none()
            || projection.is_running()
            || self.target_sending()
            || self.target_rewinding()
            || !projection.pending_approvals().is_empty()
            || !projection.pending_user_inputs().is_empty()
            || !has_compactable_conversation(projection)
    }

    /// The open thread's used tokens and when compacting them is offered, before the clock.
    fn resume_compaction(&self) -> Option<(u64, i64)> {
        if !matches!(self.target, Some(Target::Thread { .. })) {
            return None;
        }
        let projection = self.open_projection()?;
        let provider = self.composer_provider()?;
        let environment = self.active_environment()?;
        if idle_compaction_mode(&self.server_info().settings, &provider.instance_id) == "off"
            || self.composer_ui.compact.dismissed.contains(&(environment.clone(), provider.instance_id.clone()))
            || dismissed_natively(&projection.runtime_requests)
            || self.compact_unavailable(projection)
            || projection.has_held_queued_runs()
        {
            return None;
        }
        let (usage, live) = projection.context_window()?;
        // Only a live turn's usage knows the cache's lifetime.
        let ttl = usage.prompt_cache_ttl_ms.filter(|_| live);
        Some((usage.used_tokens, offered_at(&provider.driver, usage.used_tokens, usage.updated_at.as_deref(), ttl)?))
    }

    /// `resumeCompactionTokens`: what a send into the open thread compacts first, unless kept.
    pub(super) fn resume_compaction_tokens(&self) -> Option<u64> {
        let now = crate::timestamp::now_ms();
        self.resume_compaction().filter(|(_, at)| *at <= now).map(|(tokens, _)| tokens)
    }

    /// `compactBeforeSendTokens` as the footer shows it: not over Resume or a queued message's edit.
    fn compact_offer(&self) -> Option<u64> {
        let resume_shown = self.can_resume() && self.composer_empty && !self.has_attachments();
        if self.question.is_some() || self.editing_queued() || resume_shown {
            return None;
        }
        self.resume_compaction_tokens()
    }

    pub(super) fn keeps_full_history(&self) -> bool {
        self.target.as_ref().is_some_and(|target| self.composer_ui.compact.full_history.contains(target))
    }

    /// `compactBeforeSend` of `planComposerTurn`: this send compacts the thread first.
    pub(super) fn compacts_before_send(&self, text: &str, fresh_context: bool) -> bool {
        !fresh_context
            && !self.keeps_full_history()
            && !is_compact_command(text)
            && self.resume_compaction_tokens().is_some()
    }

    /// The submit button's state: the tokens a plain send summarizes first.
    pub(super) fn compact_send_tokens(&self) -> Option<u64> {
        self.compact_offer().filter(|_| !self.keeps_full_history())
    }

    /// A turn sent from `target` started: its next send offers compaction again.
    pub(super) fn forget_full_history(&mut self, target: &Target) {
        self.composer_ui.compact.full_history.remove(target);
    }

    fn toggle_keep_full_history(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.target.clone() else { return };
        let history = &mut self.composer_ui.compact.full_history;
        if !history.remove(&target) {
            history.insert(target);
        }
        cx.notify();
    }

    /// After the open thread changed: remembers a "Don't ask again" for its provider, and wakes
    /// the composer when its offer comes due.
    pub(super) fn sync_resume_compaction(&mut self, cx: &mut Context<Self>) {
        let dismissed = self.open_projection().is_some_and(|p| dismissed_natively(&p.runtime_requests));
        if let (true, Some(provider), Some(environment)) =
            (dismissed, self.composer_provider(), self.active_environment())
        {
            let key = (environment.clone(), provider.instance_id.clone());
            self.composer_ui.compact.dismissed.insert(key);
        }
        let now = crate::timestamp::now_ms();
        let due = self.resume_compaction().map(|(_, at)| at).filter(|at| *at > now);
        if self.composer_ui.compact.wake.as_ref().map(|(at, _)| *at) == due {
            return;
        }
        self.composer_ui.compact.wake = due.map(|at| {
            let delay = Duration::from_millis(u64::try_from(at - now).unwrap_or_default());
            let task = cx.spawn(async move |this, cx| {
                cx.background_executor().timer(delay).await;
                let _ = this.update(cx, |this, cx| {
                    this.composer_ui.compact.wake = None;
                    cx.notify();
                });
            });
            (at, task)
        });
    }

    /// The Compact / Keep toggle beside the send button.
    pub(super) fn render_compact_toggle(&self, narrow: bool, t: &Tokens, cx: &mut Context<Self>) -> Option<AnyElement> {
        let tokens = super::details::format_context_tokens(self.compact_offer()?);
        let keep = self.keeps_full_history();
        let (summary, hint) = if keep {
            (format!("Next send keeps all {tokens} tokens."), "Click to compact first")
        } else {
            (format!("Next send compacts {tokens} tokens first."), "Click to keep full history")
        };
        let toggle = div()
            .id("compact-before-send")
            .h(px(28.))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(6.))
            .px(px(8.))
            .rounded(px(8.))
            .font_weight(FontWeight::MEDIUM)
            // `aria-pressed` while compacting.
            .map(|el| {
                if keep {
                    el.text_color(t.foreground.opacity(0.8))
                } else {
                    el.bg(t.accent.opacity(0.6)).text_color(t.foreground)
                }
            })
            .cursor_pointer()
            .hover(|s| s.bg(t.accent).text_color(t.foreground))
            .child(Icon::new(Lucide::Minimize2).with_size(px(16.)).text_color(t.muted_foreground))
            .when(!narrow, |el| el.child(if keep { "Keep" } else { "Compact" }))
            .child(div().text_color(t.muted_foreground).child(tokens))
            .tooltip(move |window, cx| {
                let (summary, hint) = (summary.clone(), hint);
                Tooltip::element(move |_, _| v_flex().child(summary.clone()).child(hint)).build(window, cx)
            })
            .on_click(cx.listener(|this, _, _, cx| this.toggle_keep_full_history(cx)));
        let toggle = super::composer::action_control(toggle, &self.composer_action_focus[super::composer::COMPACT], t);
        Some(t.breakpoint.control_text(toggle).into_any_element())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_typed_compact_is_already_the_compaction() {
        assert!(is_compact_command("/compact"));
        assert!(is_compact_command("/COMPACT keep the plan"));
        assert!(!is_compact_command("/compacting"));
        assert!(!is_compact_command("please /compact"));
        assert!(!is_compact_command("/comp"));
        assert!(!is_compact_command("/compacté"));
    }

    #[test]
    fn claude_context_is_offered_once_its_cache_expires() {
        let updated = "2026-11-09T10:00:00.000Z";
        let at = crate::timestamp::parse_ms(updated).unwrap();
        assert_eq!(offered_at("claudeAgent", 230_000, Some(updated), None), Some(at + 70 * 60_000));
        assert_eq!(offered_at("claudeAgent", 230_000, Some(updated), Some(300_000)), Some(at + 300_000));
        assert_eq!(offered_at("claudeAgent", 199_999, Some(updated), None), None, "too small to bother");
        assert_eq!(offered_at("codex", 230_000, Some(updated), None), None, "only Claude resumes stale caches");
        assert_eq!(offered_at("claudeAgent", 230_000, None, None), None);
    }

    #[test]
    fn dont_ask_again_in_claudes_own_question_withholds_the_offer() {
        let request = |status: &str, answers: serde_json::Value| -> RuntimeRequest {
            serde_json::from_value(json!({"id": "r", "kind": "user_input", "status": status, "answers": answers}))
                .unwrap()
        };
        let question = "This session is 1h 5m old and uses 230,000 tokens. Compact it before continuing?";
        assert!(dismissed_natively(&[request("resolved", json!({question: NEVER_ANSWER}))]));
        assert!(!dismissed_natively(&[request("resolved", json!({question: "Compact and continue"}))]));
        assert!(!dismissed_natively(&[request("pending", json!({question: NEVER_ANSWER}))]));
        assert!(!dismissed_natively(&[request("resolved", json!({"Something else?": NEVER_ANSWER}))]));
        assert!(is_resume_question("This session is 45m old and uses 1,200,000 tokens. Compact it before continuing?"));
        assert!(!is_resume_question("This session is 45m old and uses 1,20,000 tokens. Compact it before continuing?"));
        assert!(!is_resume_question("This session is 1h old and uses 230,000 tokens. Compact it before continuing?"));
    }

    #[test]
    fn an_instance_mode_overrides_the_environment_default() {
        let settings = |value: serde_json::Value| crate::settings::Settings::layered(&serde_json::Map::new(), &value);
        let own = settings(
            json!({"idleCompaction": "onReturn", "providerInstances": {"c": {"config": {"idleCompaction": "off"}}}}),
        );
        assert_eq!(idle_compaction_mode(&own, "c"), "off");
        let default = settings(
            json!({"idleCompaction": "off", "providerInstances": {"c": {"config": {"idleCompaction": "default"}}}}),
        );
        assert_eq!(idle_compaction_mode(&default, "c"), "off");
        assert_eq!(idle_compaction_mode(&settings(json!({"compactIdleThreads": true})), "c"), "beforeCacheExpires");
    }
}
