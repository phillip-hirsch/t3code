//! The quit shortcut (`confirmQuit`) and its hint, `QuitHoldOverlay.tsx`.

use std::time::Instant;

use gpui_kit::component::h_flex;
use gpui_kit::{
    App, BoxShadow, Context, FontWeight, IntoElement, KeyDownEvent, KeyUpEvent, Modifiers, ModifiersChangedEvent,
    ParentElement as _, Styled as _, Task, div, hsla, point, px, relative,
};

use super::Workspace;
use crate::quit_shortcut::{Mode, Outcome, QuitShortcut};

const SHORTCUT: &str = if cfg!(target_os = "macos") { "⌘Q" } else { "Ctrl+Q" };

pub(super) struct QuitState {
    gate: QuitShortcut,
    armed_for: Option<Instant>,
    _timer: Option<Task<()>>,
}

impl Default for QuitState {
    fn default() -> Self {
        Self { gate: QuitShortcut::new(Instant::now()), armed_for: None, _timer: None }
    }
}

/// Ctrl on Windows and Linux, ⌘ on macOS: the desktop app's quit modifier.
fn quit_modifier(modifiers: Modifiers) -> bool {
    if cfg!(target_os = "macos") { modifiers.platform } else { modifiers.control }
}

impl Workspace {
    /// True when the key belonged to the shortcut and must not reach the app.
    pub(super) fn quit_key_down(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        let keystroke = &event.keystroke;
        let m = keystroke.modifiers;
        let is_q = keystroke.key.eq_ignore_ascii_case("q");
        let others = m.alt || m.shift || m.function || if cfg!(target_os = "macos") { m.control } else { m.platform };
        let shortcut = is_q && quit_modifier(m) && !others;
        let mode = Mode::parse(self.client_settings.values.get("confirmQuit"));
        let outcome = self.quit.gate.key_down(shortcut, is_q, event.is_held, mode, Instant::now());
        self.apply_quit(outcome, cx)
    }

    pub(super) fn quit_key_up(&mut self, event: &KeyUpEvent, cx: &mut Context<Self>) {
        if event.keystroke.key.eq_ignore_ascii_case("q") {
            let outcome = self.quit.gate.q_up(Instant::now());
            self.apply_quit(outcome, cx);
        }
    }

    pub(super) fn quit_modifiers_changed(&mut self, event: &ModifiersChangedEvent, cx: &mut Context<Self>) {
        if !quit_modifier(event.modifiers) {
            self.quit.gate.modifier_up(Instant::now());
            self.apply_quit(Outcome::Handled, cx);
        }
    }

    fn apply_quit(&mut self, outcome: Outcome, cx: &mut Context<Self>) -> bool {
        if outcome == Outcome::Quit {
            cx.quit();
            return true;
        }
        let deadline = self.quit.gate.deadline();
        if deadline != self.quit.armed_for {
            self.quit.armed_for = deadline;
            self.quit._timer = deadline.map(|at| {
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(at.saturating_duration_since(Instant::now())).await;
                    let _ = this.update(cx, |this, cx| {
                        this.quit.armed_for = None;
                        let outcome = this.quit.gate.tick(Instant::now());
                        this.apply_quit(outcome, cx);
                        cx.notify();
                    });
                })
            });
            // Every hint change moves the deadline, so this repaint covers them.
            cx.notify();
        }
        outcome != Outcome::Pass
    }

    pub(super) fn render_quit_hint(&self, cx: &App) -> Option<impl IntoElement + use<>> {
        let hint = self.quit.gate.hint()?;
        let crate::theme::Tokens { foreground, background, .. } = *crate::theme::tokens(cx);
        Some(
            h_flex().absolute().left_0().right_0().top(relative(0.22)).justify_center().child(
                div()
                    .rounded_full()
                    .bg(foreground.opacity(0.95))
                    .px(px(32.))
                    .py(px(16.))
                    .text_size(px(24.))
                    .line_height(px(32.))
                    .font_weight(FontWeight::BOLD)
                    .text_color(background)
                    // `shadow-xl`.
                    .shadow(vec![
                        BoxShadow {
                            color: hsla(0., 0., 0., 0.1),
                            offset: point(px(0.), px(20.)),
                            blur_radius: px(25.),
                            spread_radius: px(-5.),
                            inset: false,
                        },
                        BoxShadow {
                            color: hsla(0., 0., 0., 0.1),
                            offset: point(px(0.), px(8.)),
                            blur_radius: px(10.),
                            spread_radius: px(-6.),
                            inset: false,
                        },
                    ])
                    .child(hint.message(SHORTCUT)),
            ),
        )
    }
}
