//! `/clear`: the next message in a thread starts a fresh provider session, while earlier
//! messages stay visible (`freshSessionArms.ts`, `ComposerFreshSession.tsx`).

use std::collections::HashMap;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{Icon, Sizable as _};
use gpui_kit::{AnyElement, Context, IntoElement as _, SharedString, Styled as _, px};

use super::banners::{Variant, composer_banner};
use super::{Target, Workspace};
use crate::theme::Tokens;

/// Threads whose next message starts a fresh session. Each arm has its own number, so a send
/// clears only the arm it read, never one made while it was in flight.
#[derive(Default)]
pub(super) struct FreshSessions {
    arms: HashMap<Target, u64>,
    next: u64,
}

/// `activeMessageCount > 0`: the messages on screen, a fork's inherited ones included.
fn has_messages(projection: &crate::model::ThreadProjection) -> bool {
    projection
        .visible_turn_items
        .iter()
        .any(|row| matches!(row.item.item_type.as_str(), "user_message" | "assistant_message"))
}

/// `isClearComposerCommand`: `/clear` as the whole message arms a fresh session instead of sending.
pub(super) fn is_clear_command(text: &str) -> bool {
    text.trim().strip_prefix('/').is_some_and(|rest| rest.eq_ignore_ascii_case("clear"))
}

impl Workspace {
    /// `freshSessionAvailable`: an open thread with messages, on a server that honors `freshContext`.
    pub(super) fn fresh_session_available(&self) -> bool {
        matches!(self.target, Some(Target::Thread { .. }))
            && self.server_info().capability("freshContext")
            && self.open_projection().is_some_and(has_messages)
    }

    /// While a question or approval owns the composer, its text answers that, not the agent.
    pub(super) fn fresh_session_command_available(&self) -> bool {
        self.fresh_session_available()
            && self
                .open_projection()
                .is_some_and(|p| p.pending_approvals().is_empty() && p.pending_user_inputs().is_empty())
    }

    pub(super) fn arm_fresh_session(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.target.clone() else { return };
        let sessions = &mut self.fresh_sessions;
        sessions.next += 1;
        sessions.arms.insert(target, sessions.next);
        cx.notify();
    }

    /// With `arm`, clears only that arm.
    pub(super) fn disarm_fresh_session(&mut self, target: &Target, arm: Option<u64>) {
        if arm.is_none_or(|arm| self.fresh_sessions.arms.get(target) == Some(&arm)) {
            self.fresh_sessions.arms.remove(target);
        }
    }

    /// The open thread's arm, while `/clear` can be used here.
    pub(super) fn fresh_session_arm(&self) -> Option<u64> {
        let target = self.target.as_ref()?;
        self.fresh_sessions.arms.get(target).copied().filter(|_| self.fresh_session_available())
    }

    /// `freshSessionBannerItem`; an approval's feedback goes to that approval, so it waits.
    pub(super) fn render_fresh_session_banner(&self, t: &Tokens, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.fresh_session_arm()?;
        if self.open_projection().is_some_and(|p| !p.pending_approvals().is_empty()) {
            return None;
        }
        let dismiss = Button::new("fresh-session-dismiss")
            .ghost()
            .xsmall()
            .icon(Icon::new(IconName::X).size(px(14.)))
            .tooltip("Keep the current session")
            .on_click(cx.listener(|this, _, _, cx| {
                if let Some(target) = this.target.clone() {
                    this.disarm_fresh_session(&target, None);
                }
                cx.notify();
            }));
        Some(composer_banner(
            Variant::Info,
            Icon::new(IconName::Eraser),
            SharedString::from("Your next message starts a fresh session"),
            Some("The agent won't see earlier messages in this thread.".into()),
            Some(dismiss.into_any_element()),
            t,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_a_bare_clear_arms_a_fresh_session() {
        for text in ["/clear", "  /CLEAR  ", "/clear\n"] {
            assert!(is_clear_command(text), "{text:?}");
        }
        for text in ["/clear now", "clear", "/clearer", "ask /clear", "/"] {
            assert!(!is_clear_command(text), "{text:?}");
        }
    }

    #[test]
    fn a_fork_counts_the_messages_it_inherited() {
        let inherited = json!({"id": "m", "type": "assistant_message", "threadId": "parent", "ordinal": 1});
        let fork = |rows: Vec<serde_json::Value>| -> crate::model::ThreadProjection {
            serde_json::from_value(
                json!({"thread": {"id": "t", "title": "T"}, "turnItems": [], "visibleTurnItems": rows}),
            )
            .unwrap()
        };
        let row =
            json!({"visibility": "inherited", "sourceThreadId": "parent", "sourceItemId": "m", "item": inherited});
        assert!(has_messages(&fork(vec![row])));
        assert!(!has_messages(&fork(vec![])));
    }
}
