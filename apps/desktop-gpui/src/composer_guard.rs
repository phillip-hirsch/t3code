//! `composerTypingGuard.ts`: approvals and questions that arrive while the user is typing wait
//! until typing has rested, the composer loses focus, or a message goes out, so a keystroke never
//! lands in a card that just appeared.

use std::collections::HashSet;
use std::time::{Duration, Instant};

/// `COMPOSER_TYPING_IDLE_MS`.
pub const TYPING_IDLE: Duration = Duration::from_millis(1500);

#[derive(Default)]
pub struct TypingGuard {
    seen: HashSet<String>,
    held: HashSet<String>,
    focused: bool,
    last_typed: Option<Instant>,
}

impl TypingGuard {
    /// The pending request ids now; ones first seen mid-typing are held. `true` when the held set
    /// changed.
    pub fn requests<'a>(&mut self, ids: impl IntoIterator<Item = &'a str>, now: Instant) -> bool {
        let ids: Vec<&str> = ids.into_iter().collect();
        let before = self.held.len();
        self.held.retain(|id| ids.contains(&id.as_str()));
        let mut changed = self.held.len() != before;
        let typing = self.focused && self.last_typed.is_some_and(|at| now.saturating_duration_since(at) < TYPING_IDLE);
        for id in ids {
            if self.seen.insert(id.to_owned()) && typing {
                changed |= self.held.insert(id.to_owned());
            }
        }
        changed
    }

    pub fn typed(&mut self, now: Instant) {
        self.last_typed = Some(now);
    }

    pub fn focus(&mut self) {
        self.focused = true;
    }

    /// Blur, send or idle: everything held shows. `true` when something was held.
    pub fn release(&mut self, blur: bool) -> bool {
        let held = !self.held.is_empty();
        self.held.clear();
        self.last_typed = None;
        if blur {
            self.focused = false;
        }
        held
    }

    pub fn is_held(&self, id: &str) -> bool {
        self.held.contains(id)
    }

    /// When the held requests show unless typing goes on.
    pub fn idle_at(&self) -> Option<Instant> {
        self.last_typed.filter(|_| !self.held.is_empty()).map(|at| at + TYPING_IDLE)
    }

    /// Another target: only focus carries over.
    pub fn reset(&mut self) {
        *self = Self { focused: self.focused, ..Self::default() };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_arriving_mid_typing_wait_until_typing_rests() {
        let start = Instant::now();
        let mut guard = TypingGuard::default();
        assert!(!guard.requests(["old"], start), "requests already there show");
        guard.focus();
        guard.typed(start);
        assert!(guard.requests(["old", "new"], start + Duration::from_millis(500)));
        assert!(guard.is_held("new") && !guard.is_held("old"));
        assert_eq!(guard.idle_at(), Some(start + TYPING_IDLE));
        guard.typed(start + Duration::from_secs(1));
        assert_eq!(guard.idle_at(), Some(start + Duration::from_secs(1) + TYPING_IDLE), "typing pushes it back");
        assert!(guard.release(false));
        assert!(!guard.is_held("new") && guard.idle_at().is_none());
        assert!(!guard.requests(["old", "new"], start + Duration::from_secs(1)), "a seen request is never held again");
    }

    #[test]
    fn only_focused_recent_typing_holds_and_resolved_requests_drop_out() {
        let start = Instant::now();
        let mut guard = TypingGuard::default();
        guard.typed(start);
        assert!(!guard.requests(["unfocused"], start), "typing elsewhere holds nothing");
        guard.focus();
        guard.typed(start);
        assert!(!guard.requests(["late"], start + TYPING_IDLE), "typing rested");
        guard.typed(start);
        assert!(guard.requests(["a"], start));
        assert!(guard.requests(Vec::<&str>::new(), start), "a resolved request is no longer held");
        assert!(guard.idle_at().is_none());
        guard.requests(["b"], start);
        assert!(guard.release(true));
        guard.typed(start);
        assert!(!guard.requests(["c"], start), "blurred");
        guard.focus();
        guard.reset();
        guard.typed(start);
        assert!(guard.requests(["c"], start), "a new target keeps focus and forgets what it saw");
    }
}
