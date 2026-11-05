//! Settings → Chat (`ChatSettingsPanel` in `apps/web/src/components/settings/SettingsPanels.tsx`).
//! Every row is a device-local client setting; none is scoped in the web.

use gpui_kit::{AnyElement, Context, IntoElement as _, SharedString};

use super::placeholder::{Choice, Control, Literal, MOD_LABEL, SettingRowSpec, page, section};
use crate::app::Workspace;
use crate::theme;

static RICH_TEXT: SettingRowSpec = SettingRowSpec {
    id: "composer-rich-text",
    label: "Rich text composer",
    description: "Show formatted Markdown as you type.",
    path: "composerRichTextEnabled",
    control: Control::Switch,
    scopes: &[],
};

static SEND_SHORTCUT: SettingRowSpec = SettingRowSpec {
    id: "send-shortcut",
    label: "Send shortcut",
    description: "Choose when Enter sends a prompt or inserts a new line",
    path: "sendShortcut",
    control: Control::Select(&[
        Choice { value: Literal::Str("enter"), label: "Enter", short: None },
        Choice {
            value: Literal::Str("mod-enter-multiline"),
            label: "{mod} + Enter for multiline prompts",
            short: None,
        },
        Choice { value: Literal::Str("mod-enter"), label: "{mod} + Enter always", short: None },
    ]),
    scopes: &[],
};

static SKILLS_IN_SLASH_MENU: SettingRowSpec = SettingRowSpec {
    id: "skills-in-slash-menu",
    label: "Show skills in slash menu",
    description: "Also include skills in the / command menu. Skills always appear when you type $.",
    path: "showSkillsInSlashMenu",
    control: Control::Switch,
    scopes: &[],
};

static FOLLOW_UP_BEHAVIOR: SettingRowSpec = SettingRowSpec {
    id: "follow-up-behavior",
    label: "Follow-up behavior",
    // Replaced by `follow_up_description`, which depends on the send shortcut.
    description: "Queue follow-ups while the agent runs or steer the current run.",
    path: "followUpBehavior",
    control: Control::Select(&[
        Choice { value: Literal::Str("queue"), label: "Queue", short: None },
        Choice { value: Literal::Str("steer"), label: "Steer", short: None },
    ]),
    scopes: &[],
};

/// The follow-up row says how to do the opposite for one message under the current send shortcut.
fn follow_up_description(send_shortcut: &str) -> String {
    let m = MOD_LABEL;
    let opposite = match send_shortcut {
        "mod-enter-multiline" => format!(
            "Press {m} + Enter for single-line prompts or {m} + Shift + Enter for multiline prompts to do the opposite for one message."
        ),
        "mod-enter" => format!("Press {m} + Shift + Enter to do the opposite for one message."),
        _ => format!("Press {m} + Enter to do the opposite for one message."),
    };
    format!("{} {opposite}", FOLLOW_UP_BEHAVIOR.description)
}

impl Workspace {
    pub(super) fn render_chat_section(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let t = *theme::tokens(cx);
        let row = |this: &Self, spec: &'static SettingRowSpec, cx: &mut Context<Self>| {
            this.render_client_row(spec, SharedString::new_static(spec.description), false, cx)
        };
        let advanced = vec![row(self, &SKILLS_IN_SLASH_MENU, cx)];
        let composer = vec![
            row(self, &RICH_TEXT, cx),
            row(self, &SEND_SHORTCUT, cx),
            self.render_advanced_fold("chat", advanced, cx),
        ];
        let send_shortcut = self.client_settings.values.get("sendShortcut").clone();
        let follow_up = self.render_client_row(
            &FOLLOW_UP_BEHAVIOR,
            follow_up_description(send_shortcut.as_str().unwrap_or("enter")).into(),
            false,
            cx,
        );
        page(
            "settings-chat",
            [
                section(Some("Composer"), None, composer, &t).into_any_element(),
                section(Some("Follow-ups"), None, [follow_up], &t).into_any_element(),
            ],
        )
        .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follow_up_description_names_the_opposite_shortcut() {
        let m = MOD_LABEL;
        assert!(
            follow_up_description("enter").ends_with(&format!("Press {m} + Enter to do the opposite for one message."))
        );
        assert!(
            follow_up_description("mod-enter")
                .ends_with(&format!("Press {m} + Shift + Enter to do the opposite for one message."))
        );
        assert!(
            follow_up_description("mod-enter-multiline")
                .contains(&format!("{m} + Shift + Enter for multiline prompts"))
        );
    }
}
