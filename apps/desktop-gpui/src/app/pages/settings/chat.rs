//! Settings → Chat (`ChatSettingsPanel` in `apps/web/src/components/settings/SettingsPanels.tsx`).
//! Every row is a device-local client setting; none is scoped in the web.

use gpui_kit::{AnyElement, Context, Window};

use super::widgets::{SettingRow, settings_section};
use crate::app::Workspace;
use crate::theme;

const MOD: &str = if cfg!(target_os = "macos") { "⌘" } else { "Ctrl" };

const SEND_SHORTCUTS: &[(&str, &str)] = &[
    ("enter", "Enter"),
    (
        "mod-enter-multiline",
        if cfg!(target_os = "macos") {
            "⌘ + Enter for multiline prompts"
        } else {
            "Ctrl + Enter for multiline prompts"
        },
    ),
    ("mod-enter", if cfg!(target_os = "macos") { "⌘ + Enter always" } else { "Ctrl + Enter always" }),
];

const FOLLOW_UPS: &[(&str, &str)] = &[("queue", "Queue"), ("steer", "Steer")];

/// The follow-up row says how to do the opposite for one message under the current send shortcut.
fn follow_up_description(send_shortcut: &str) -> String {
    let opposite = match send_shortcut {
        "mod-enter-multiline" => format!(
            "Press {MOD} + Enter for single-line prompts or {MOD} + Shift + Enter for multiline prompts to do the opposite for one message."
        ),
        "mod-enter" => format!("Press {MOD} + Shift + Enter to do the opposite for one message."),
        _ => format!("Press {MOD} + Enter to do the opposite for one message."),
    };
    format!("Queue follow-ups while the agent runs or steer the current run. {opposite}")
}

impl Workspace {
    pub(super) fn render_chat_section(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = *theme::tokens(cx);
        let view = self.settings_view();
        let follow_up = follow_up_description(view.str("sendShortcut"));
        let mut row = |this: &mut Self, row: SettingRow| this.setting_row(&view, row, window, cx);
        let skills = row(
            self,
            SettingRow::new("skills-in-slash-menu")
                .description("Also include skills in the / command menu. Skills always appear when you type $.")
                .setting("showSkillsInSlashMenu")
                .switch(),
        );
        let rich_text = row(
            self,
            SettingRow::new("composer-rich-text")
                .description("Show formatted Markdown as you type.")
                .setting("composerRichTextEnabled")
                .switch(),
        );
        let send_shortcut = row(
            self,
            SettingRow::new("send-shortcut")
                .description("Choose when Enter sends a prompt or inserts a new line")
                .setting("sendShortcut")
                .select(SEND_SHORTCUTS, 0.),
        );
        let follow_ups = [row(
            self,
            SettingRow::new("follow-up-behavior")
                .description(follow_up)
                .setting("followUpBehavior")
                .select(FOLLOW_UPS, 0.),
        )];
        let composer = [rich_text, send_shortcut, self.advanced_fold("chat", [skills], cx)];
        let sections = [
            settings_section("composer", "Composer", composer, &t),
            settings_section("follow-ups", "Follow-ups", follow_ups, &t),
        ];
        self.settings_page("chat", &view, sections, cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follow_up_description_names_the_opposite_shortcut() {
        assert!(
            follow_up_description("enter")
                .ends_with(&format!("Press {MOD} + Enter to do the opposite for one message."))
        );
        assert!(
            follow_up_description("mod-enter")
                .ends_with(&format!("Press {MOD} + Shift + Enter to do the opposite for one message."))
        );
        assert!(
            follow_up_description("mod-enter-multiline")
                .contains(&format!("{MOD} + Shift + Enter for multiline prompts"))
        );
    }
}
