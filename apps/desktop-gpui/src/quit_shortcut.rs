//! The Ctrl+Q (⌘Q) quit gate set by `confirmQuit`, ported from the desktop app's
//! `QuitHold.ts` plus the hint `QuitHoldOverlay.tsx` shows.

use std::time::{Duration, Instant};

use serde_json::Value;

pub const HOLD: Duration = Duration::from_millis(1200);
pub const DOUBLE_PRESS: Duration = Duration::from_millis(500);
// Once held, quitting waits for Q's release or a quiet period, so trailing
// auto-repeats cannot reach the next app.
const RELEASE_GRACE: Duration = Duration::from_millis(600);
// A slow repeat rate can exceed the grace; two cadences keep behind the next repeat.
const REPEAT_CADENCE_MULTIPLIER: u32 = 2;
const HINT_LINGER: Duration = Duration::from_millis(1200);

/// The `confirmQuit` client setting; older documents stored a boolean.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Direct,
    Hold,
    DoublePress,
}

impl Mode {
    pub fn parse(value: &Value) -> Self {
        match value {
            Value::String(mode) if mode == "direct" => Self::Direct,
            Value::String(mode) if mode == "double-click" => Self::DoublePress,
            Value::Bool(false) => Self::Direct,
            _ => Self::Hold,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hint {
    Hold,
    PressAgain,
}

impl Hint {
    pub fn message(self, shortcut: &str) -> String {
        match self {
            Self::Hold => format!("Hold {shortcut} or press twice to quit"),
            Self::PressAgain => format!("Press {shortcut} again to quit"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Not the quit shortcut; the key goes on to the app.
    Pass,
    Handled,
    Quit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Watchdog {
    Release,
    Quit,
}

pub struct QuitShortcut {
    /// The mode of the press being shown.
    mode: Option<Mode>,
    holding: bool,
    notified: bool,
    armed: bool,
    quit_on_release: bool,
    held_since: Instant,
    last_press: Option<Instant>,
    last_repeat: Option<Instant>,
    cadence: Duration,
    watchdog: Option<(Instant, Watchdog)>,
    hint: Option<Hint>,
    hint_hide_at: Option<Instant>,
}

impl QuitShortcut {
    pub fn new(now: Instant) -> Self {
        Self {
            mode: None,
            holding: false,
            notified: false,
            armed: false,
            quit_on_release: false,
            held_since: now,
            last_press: None,
            last_repeat: None,
            cadence: Duration::ZERO,
            watchdog: None,
            hint: None,
            hint_hide_at: None,
        }
    }

    pub fn hint(&self) -> Option<Hint> {
        self.hint
    }

    /// When [`Self::tick`] has work next.
    pub fn deadline(&self) -> Option<Instant> {
        [self.watchdog.map(|(at, _)| at), self.hint_hide_at].into_iter().flatten().min()
    }

    /// A key down. `shortcut` is Ctrl+Q (⌘Q on macOS) with no other modifier.
    pub fn key_down(&mut self, shortcut: bool, is_q: bool, repeat: bool, mode: Mode, now: Instant) -> Outcome {
        if shortcut && repeat {
            self.cadence = now - self.last_repeat.unwrap_or(self.held_since);
            self.last_repeat = Some(now);
        }
        if self.quit_on_release {
            if is_q {
                self.quit_after_quiet_period(now);
            }
            return Outcome::Handled;
        }
        if !shortcut {
            // A fresh key also ends a hold whose Q release never arrived.
            if !repeat {
                self.last_press = None;
                self.release(false, now);
            }
            return Outcome::Pass;
        }
        if repeat {
            if self.mode == Some(Mode::Hold) && self.armed && now - self.held_since >= HOLD {
                self.armed = false;
                self.quit_on_release = true;
                self.quit_after_quiet_period(now);
            }
            return Outcome::Handled;
        }

        let previous = self.last_press.replace(now);
        if self.holding || self.notified {
            self.release(false, now);
        }
        if previous.is_some_and(|at| now - at <= DOUBLE_PRESS) {
            return self.quit_now(now);
        }
        self.holding = true;
        self.held_since = now;
        match mode {
            Mode::Direct => return self.quit_now(now),
            Mode::DoublePress => {
                self.watchdog = Some((now + DOUBLE_PRESS, Watchdog::Release));
                self.show(mode, Hint::PressAgain);
            }
            Mode::Hold => {
                self.armed = true;
                self.watchdog = Some((now + HOLD + RELEASE_GRACE, Watchdog::Release));
                self.show(mode, Hint::Hold);
            }
        }
        Outcome::Handled
    }

    pub fn q_up(&mut self, now: Instant) -> Outcome {
        let quit = self.quit_on_release;
        self.release(true, now);
        if quit { self.quit_now(now) } else { Outcome::Pass }
    }

    /// Ctrl (⌘) went up.
    pub fn modifier_up(&mut self, now: Instant) {
        if self.quit_on_release {
            self.quit_after_quiet_period(now);
        } else {
            self.release(true, now);
        }
    }

    /// Runs what is due at `now`.
    pub fn tick(&mut self, now: Instant) -> Outcome {
        if let Some((at, watchdog)) = self.watchdog.filter(|(at, _)| *at <= now) {
            self.watchdog = None;
            match watchdog {
                Watchdog::Release => self.release(false, at),
                Watchdog::Quit => return self.quit_now(now),
            }
        }
        if self.hint_hide_at.is_some_and(|at| at <= now) {
            self.hint = None;
            self.hint_hide_at = None;
        }
        Outcome::Handled
    }

    fn show(&mut self, mode: Mode, hint: Hint) {
        self.mode = Some(mode);
        self.notified = true;
        self.hint = Some(hint);
        self.hint_hide_at = None;
    }

    fn release(&mut self, keep_double_press_hint: bool, now: Instant) {
        if !self.holding && !self.notified {
            return;
        }
        let keep = keep_double_press_hint && self.mode == Some(Mode::DoublePress) && self.notified;
        self.holding = false;
        self.armed = false;
        self.quit_on_release = false;
        self.last_repeat = None;
        self.cadence = Duration::ZERO;
        if keep {
            return;
        }
        self.mode = None;
        self.watchdog = None;
        if std::mem::take(&mut self.notified) {
            // A press-again hint goes at once; a hold hint lingers.
            match self.hint {
                Some(Hint::PressAgain) => self.hint = None,
                _ => self.hint_hide_at = Some(now + HINT_LINGER),
            }
        }
    }

    fn quit_now(&mut self, now: Instant) -> Outcome {
        self.release(false, now);
        self.last_press = None;
        Outcome::Quit
    }

    fn quit_after_quiet_period(&mut self, now: Instant) {
        let quiet = RELEASE_GRACE.max(self.cadence * REPEAT_CADENCE_MULTIPLIER);
        self.watchdog = Some((now + quiet, Watchdog::Quit));
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn ms(start: Instant, at: u64) -> Instant {
        start + Duration::from_millis(at)
    }

    /// Ctrl+Q down.
    fn press(gate: &mut QuitShortcut, mode: Mode, at: Instant) -> Outcome {
        gate.key_down(true, true, false, mode, at)
    }

    #[test]
    fn direct_quits_on_the_first_press() {
        let t = Instant::now();
        let mut gate = QuitShortcut::new(t);
        assert_eq!(press(&mut gate, Mode::Direct, t), Outcome::Quit);
    }

    #[test]
    fn double_press_quits_only_on_a_second_press_in_time() {
        let t = Instant::now();
        let mut gate = QuitShortcut::new(t);
        assert_eq!(press(&mut gate, Mode::DoublePress, t), Outcome::Handled);
        // Releasing keeps the "press again" hint until its window closes.
        assert_eq!(gate.q_up(ms(t, 80)), Outcome::Pass);
        gate.modifier_up(ms(t, 90));
        assert_eq!(gate.hint(), Some(Hint::PressAgain));
        assert_eq!(gate.deadline(), Some(ms(t, 500)));
        gate.tick(ms(t, 500));
        assert_eq!(gate.hint(), None);
        // Too late for the first, so this press starts over.
        assert_eq!(press(&mut gate, Mode::DoublePress, ms(t, 1100)), Outcome::Handled);
        gate.q_up(ms(t, 1150));
        assert_eq!(press(&mut gate, Mode::DoublePress, ms(t, 1400)), Outcome::Quit);
    }

    #[test]
    fn hold_quits_after_the_hold_once_repeats_go_quiet() {
        let t = Instant::now();
        let mut gate = QuitShortcut::new(t);
        assert_eq!(press(&mut gate, Mode::Hold, t), Outcome::Handled);
        assert_eq!(gate.hint(), Some(Hint::Hold));
        let mut at = 500;
        while at <= 1250 {
            assert_eq!(gate.key_down(true, true, true, Mode::Hold, ms(t, at)), Outcome::Handled);
            at += 50;
        }
        // Held: the next repeat or a quiet grace decides, not the hold watchdog.
        assert_eq!(gate.deadline(), Some(ms(t, 1250 + 600)));
        assert_eq!(gate.tick(ms(t, 1849)), Outcome::Handled);
        assert_eq!(gate.tick(ms(t, 1850)), Outcome::Quit);
    }

    #[test]
    fn a_held_shortcut_quits_when_q_is_released() {
        let t = Instant::now();
        let mut gate = QuitShortcut::new(t);
        press(&mut gate, Mode::Hold, t);
        gate.key_down(true, true, true, Mode::Hold, ms(t, 1200));
        // Other keys are swallowed until the quit.
        assert_eq!(gate.key_down(false, false, false, Mode::Hold, ms(t, 1210)), Outcome::Handled);
        assert_eq!(gate.q_up(ms(t, 1220)), Outcome::Quit);
    }

    #[test]
    fn letting_go_early_cancels_and_the_hint_lingers() {
        let t = Instant::now();
        let mut gate = QuitShortcut::new(t);
        press(&mut gate, Mode::Hold, t);
        gate.key_down(true, true, true, Mode::Hold, ms(t, 600));
        assert_eq!(gate.q_up(ms(t, 700)), Outcome::Pass);
        assert_eq!(gate.hint(), Some(Hint::Hold));
        assert_eq!(gate.deadline(), Some(ms(t, 1900)));
        gate.tick(ms(t, 1900));
        assert_eq!(gate.hint(), None);
        // Repeats after the release no longer count toward a hold.
        assert_eq!(gate.key_down(true, true, true, Mode::Hold, ms(t, 2000)), Outcome::Handled);
        assert_eq!(gate.deadline(), None);
    }

    #[test]
    fn hold_mode_also_quits_on_two_quick_presses() {
        let t = Instant::now();
        let mut gate = QuitShortcut::new(t);
        press(&mut gate, Mode::Hold, t);
        gate.q_up(ms(t, 100));
        assert_eq!(press(&mut gate, Mode::Hold, ms(t, 300)), Outcome::Quit);
    }

    #[test]
    fn another_key_between_presses_breaks_the_double_press() {
        let t = Instant::now();
        let mut gate = QuitShortcut::new(t);
        press(&mut gate, Mode::Hold, t);
        assert_eq!(gate.key_down(false, false, false, Mode::Hold, ms(t, 100)), Outcome::Pass);
        assert_eq!(press(&mut gate, Mode::Hold, ms(t, 200)), Outcome::Handled);
    }

    #[test]
    fn the_hold_watchdog_releases_a_hold_without_repeats() {
        let t = Instant::now();
        let mut gate = QuitShortcut::new(t);
        press(&mut gate, Mode::Hold, t);
        assert_eq!(gate.deadline(), Some(ms(t, 1800)));
        assert_eq!(gate.tick(ms(t, 1800)), Outcome::Handled);
        assert_eq!(gate.hint(), Some(Hint::Hold));
        gate.tick(ms(t, 3000));
        assert_eq!(gate.hint(), None);
    }

    #[test]
    fn modes_parse_the_setting_and_its_legacy_boolean() {
        assert_eq!(Mode::parse(&json!("direct")), Mode::Direct);
        assert_eq!(Mode::parse(&json!("double-click")), Mode::DoublePress);
        assert_eq!(Mode::parse(&json!("hold")), Mode::Hold);
        assert_eq!(Mode::parse(&json!(true)), Mode::Hold);
        assert_eq!(Mode::parse(&json!(false)), Mode::Direct);
    }
}
