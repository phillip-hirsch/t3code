//! The server's keybindings, merged with the defaults, as GPUI key bindings for the
//! commands this client implements. Edits in Settings → Keybindings land here through
//! `keybindingsUpdated`, so the app honors them without a restart.

use std::collections::HashSet;

use gpui_kit::{Action, App, KeyBinding};

use crate::keybindings::{self, Rule, Shortcut};

gpui_kit::actions!(workspace, [GoBack, GoForward, OpenUsage]);

/// Commands with a handler in this client, and the action each dispatches.
const COMMANDS: &[(&str, fn() -> Box<dyn Action>)] = &[
    ("sidebar.toggle", || Box::new(super::ToggleSidebar)),
    ("threadPanel.toggle", || Box::new(super::ToggleThreadDetails)),
    ("navigation.back", || Box::new(GoBack)),
    ("navigation.forward", || Box::new(GoForward)),
    ("usage.open", || Box::new(OpenUsage)),
    ("commandPalette.toggle", || Box::new(super::TogglePalette)),
    ("chat.new", || Box::new(super::NewThread)),
    ("chat.newLocal", || Box::new(super::NewThreadInProject)),
    ("preview.toggle", || Box::new(super::TogglePreview)),
    ("rightPanel.toggle", || Box::new(super::ToggleRightPanel)),
    ("chat.find", || Box::new(super::FindInThread)),
];

/// The when-clause context (`ShortcutMatchContext` in `apps/web/src/keybindings.ts`).
/// This is the desktop app, and it has none of the web's terminal, preview or picker
/// surfaces yet, so their conditions read false.
fn when_context(name: &str) -> bool {
    name == "isDesktop"
}

/// GPUI's keystroke syntax for a shortcut: `mod` is Cmd on macOS and Ctrl elsewhere.
pub(super) fn gpui_keystroke(shortcut: &Shortcut, mac: bool) -> String {
    let mut parts = Vec::new();
    if shortcut.ctrl_key || (shortcut.mod_key && !mac) {
        parts.push("ctrl");
    }
    if shortcut.alt_key {
        parts.push("alt");
    }
    if shortcut.shift_key {
        parts.push("shift");
    }
    if shortcut.meta_key || (shortcut.mod_key && mac) {
        parts.push("cmd");
    }
    parts.push(match shortcut.key.as_str() {
        " " => "space",
        "arrowup" => "up",
        "arrowdown" => "down",
        "arrowleft" => "left",
        "arrowright" => "right",
        key => key,
    });
    parts.join("-")
}

/// Rules GPUI should bind, as (keystroke, command), in keymap order: like the web,
/// the last matching rule wins, and GPUI gives later bindings precedence.
fn bindable(rules: &[Rule], mac: bool) -> Vec<(String, &str)> {
    rules
        .iter()
        .filter(|rule| COMMANDS.iter().any(|(command, _)| *command == rule.command))
        .filter(|rule| rule.when_ast.as_ref().is_none_or(|when| keybindings::evaluate_when(when, &when_context)))
        .map(|rule| (gpui_keystroke(&rule.shortcut, mac), rule.command.as_str()))
        .collect()
}

/// Replaces this client's command bindings, leaving gpui-component's own in place.
pub fn install(server_rules: &[Rule], cx: &mut App) {
    let ours: HashSet<&str> = COMMANDS.iter().map(|(_, action)| action().name()).collect();
    let kept: Vec<KeyBinding> = cx
        .key_bindings()
        .borrow()
        .bindings()
        .filter(|binding| !ours.contains(binding.action().name()))
        .cloned()
        .collect();
    let merged = keybindings::merge_with_defaults(server_rules);
    let bound = bindable(&merged, cfg!(target_os = "macos"));
    log::info!("keymap: {bound:?}");
    let added: Vec<KeyBinding> = bound
        .into_iter()
        .filter_map(|(keystroke, command)| {
            let (_, action) = COMMANDS.iter().find(|(name, _)| *name == command)?;
            KeyBinding::load(&keystroke, action(), None, false, None, cx)
                .inspect_err(|error| log::warn!("keybinding {keystroke} for {command}: {error}"))
                .ok()
        })
        .collect();
    cx.clear_key_bindings();
    cx.bind_keys(kept.into_iter().chain(added));
}

impl super::Workspace {
    /// Reinstalls the keymap when the server's keybindings changed.
    pub(super) fn sync_keymap(&mut self, cx: &mut gpui_kit::Context<Self>) {
        if *self.installed_keybindings != *self.server_info.keybindings {
            self.installed_keybindings = self.server_info.keybindings.clone();
            install(&self.installed_keybindings, cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keybindings::{compile, defaults, parse_shortcut};

    #[test]
    fn shortcuts_map_to_gpui_keystrokes() {
        let keystroke = |key: &str, mac| gpui_keystroke(&parse_shortcut(key).unwrap(), mac);
        assert_eq!(keystroke("mod+shift+b", false), "ctrl-shift-b");
        assert_eq!(keystroke("mod+shift+b", true), "shift-cmd-b");
        assert_eq!(keystroke("ctrl+alt+arrowup", true), "ctrl-alt-up");
        assert_eq!(keystroke("mod+-", false), "ctrl--");
        assert_eq!(keystroke("space", false), "space");
    }

    #[test]
    fn only_implemented_commands_whose_conditions_hold_are_bound() {
        let bound = bindable(defaults(), false);
        assert!(bound.contains(&("ctrl-b".into(), "sidebar.toggle")));
        assert!(bound.contains(&("ctrl-shift-b".into(), "threadPanel.toggle")), "!terminalFocus holds");
        assert!(bound.contains(&("ctrl-[".into(), "navigation.back")));
        assert!(bound.contains(&("ctrl-k".into(), "commandPalette.toggle")));
        let gated = [compile("mod+y", "sidebar.toggle", Some("terminalFocus")).unwrap()];
        assert!(bindable(&gated, false).is_empty(), "terminalFocus never holds here");
        let custom = keybindings::merge_with_defaults(&[compile("mod+alt+y", "sidebar.toggle", None).unwrap()]);
        let sidebar: Vec<_> = bindable(&custom, false).into_iter().filter(|(_, c)| *c == "sidebar.toggle").collect();
        assert_eq!(sidebar, [("ctrl-alt-y".to_owned(), "sidebar.toggle")], "the server's rule replaces the default");
    }
}
