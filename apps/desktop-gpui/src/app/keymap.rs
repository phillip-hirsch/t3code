//! Keyboard shortcuts as the web resolves them (`apps/web/src/keybindings.ts`,
//! `shortcuts/dispatcher.ts`): the server's rules merged with the defaults, the last
//! rule whose `when` holds and whose shortcut matches wins, and `when` reads the live
//! focus and panel state. A keystroke interceptor resolves every key before GPUI's own
//! bindings, the terminal or a text field see it, then [`super::commands`] runs it.
//! Edits in Settings → Keybindings land through `keybindingsUpdated` without a restart.

use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::component::Sizable as _;
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::button::Button;
use gpui_kit::component::notification::Notification;
use gpui_kit::{Context, Focusable as _, KeyContext, Keystroke, Window};
use serde_json::Value;

use crate::keybindings::{self, Rule, Shortcut};

/// Marks a field that records a shortcut; app shortcuts leave its keys alone (`data-keybinding-capture`).
pub(super) const CAPTURE_CONTEXT: &str = "KeybindingCapture";
/// The browser surface and its chrome (`[data-preview-panel-mode]`).
pub(super) const PREVIEW_CONTEXT: &str = "PreviewPanel";
const TERMINAL_CONTEXT: &str = "Terminal";
const INPUT_CONTEXT: &str = "Input";
/// An open combobox: the palette or a project picker (`[role="combobox"][aria-expanded="true"]`).
pub(super) const COMBOBOX_CONTEXT: &str = "Combobox";
pub(super) const MAC: bool = cfg!(target_os = "macos");

/// `ShortcutMatchContext`, read when a key arrives.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct ShortcutContext {
    pub terminal_focus: bool,
    pub terminal_open: bool,
    pub preview_focus: bool,
    pub preview_open: bool,
    pub right_panel_open: bool,
    pub editable_focus: bool,
    pub model_picker_open: bool,
    pub composer_focus: bool,
    pub draft_thread_route: bool,
    pub turn_running: bool,
    pub usage_page_open: bool,
    pub composer_requires_modifier: bool,
    /// Not a `when` key: focus sits in an open combobox.
    pub combobox_focus: bool,
    /// Not a `when` key: a menu, popover, dialog or the palette is up
    /// (`LAUNCHER_SHORTCUT_BLOCKING_LAYERS`).
    pub layer_open: bool,
}

impl ShortcutContext {
    fn holds(&self, name: &str) -> bool {
        match name {
            "terminalFocus" => self.terminal_focus,
            "terminalOpen" => self.terminal_open,
            "previewFocus" => self.preview_focus,
            "previewOpen" => self.preview_open,
            "rightPanelOpen" => self.right_panel_open,
            "editableFocus" => self.editable_focus,
            "modelPickerOpen" => self.model_picker_open,
            "composerFocus" => self.composer_focus,
            "draftThreadRoute" => self.draft_thread_route,
            "turnRunning" => self.turn_running,
            "usagePageOpen" => self.usage_page_open,
            "composerRequiresModifier" => self.composer_requires_modifier,
            "isDesktop" => true,
            _ => false,
        }
    }

    fn admits(&self, rule: &Rule) -> bool {
        rule.when_ast.as_ref().is_none_or(|when| keybindings::evaluate_when(when, &|name| self.holds(name)))
    }
}

/// Whether the focused element sits in `name`'s key context.
pub(super) fn focus_in(stack: &[KeyContext], name: &str) -> bool {
    stack.iter().any(|context| context.contains(name))
}

/// `shortcutForContext`: Mod+Enter's alternate send moves to Mod+Shift+Enter when Enter needs Mod to send.
fn shortcut_for_context<'a>(rule: &'a Rule, context: &ShortcutContext) -> std::borrow::Cow<'a, Shortcut> {
    let s = &rule.shortcut;
    if context.composer_requires_modifier
        && rule.command == "composer.sendAlternate"
        && s.key == "enter"
        && s.mod_key
        && !(s.meta_key || s.ctrl_key || s.alt_key || s.shift_key)
    {
        return std::borrow::Cow::Owned(Shortcut { shift_key: true, ..s.clone() });
    }
    std::borrow::Cow::Borrowed(s)
}

/// GPUI's key name in the web's `KeyboardEvent.key` spelling.
fn web_key(key: &str) -> &str {
    match key {
        "up" => "arrowup",
        "down" => "arrowdown",
        "left" => "arrowleft",
        "right" => "arrowright",
        "space" => " ",
        key => key,
    }
}

/// `US_SHIFTED_TO_UNSHIFTED_KEYS`: GPUI on Windows reports Shift+4 as `$` with Shift
/// consumed, where the web still sees `Digit4` with Shift held.
fn unshifted(key: &str) -> Option<&'static str> {
    const PAIRS: [(&str, &str); 21] = [
        ("!", "1"),
        ("@", "2"),
        ("#", "3"),
        ("$", "4"),
        ("%", "5"),
        ("^", "6"),
        ("&", "7"),
        ("*", "8"),
        ("(", "9"),
        (")", "0"),
        ("~", "`"),
        ("_", "-"),
        ("+", "="),
        ("{", "["),
        ("}", "]"),
        ("|", "\\"),
        (":", ";"),
        ("\"", "'"),
        ("<", ","),
        (">", "."),
        ("?", "/"),
    ];
    PAIRS.iter().find(|(shifted, _)| *shifted == key).map(|(_, plain)| *plain)
}

/// `matchesKeybindingShortcut`: GPUI's `key` is the unshifted key (the web's `code`),
/// `key_char` the typed character (the web's `key`); either may match.
pub(super) fn matches(keystroke: &Keystroke, shortcut: &Shortcut, mac: bool) -> bool {
    let m = &keystroke.modifiers;
    // AltGr (Ctrl+Alt off macOS) typing a character other than a letter or digit is text.
    if !mac
        && m.control
        && m.alt
        && keystroke.key_char.as_deref().is_some_and(|typed| {
            let mut chars = typed.chars();
            !matches!((chars.next(), chars.next()), (Some(c), None) if c.is_ascii_alphanumeric())
        })
    {
        return false;
    }
    let meta = shortcut.meta_key || (shortcut.mod_key && mac);
    let ctrl = shortcut.ctrl_key || (shortcut.mod_key && !mac);
    if m.platform != meta || m.control != ctrl || m.alt != shortcut.alt_key {
        return false;
    }
    let key = keystroke.key.to_lowercase();
    if m.shift == shortcut.shift_key
        && (web_key(&key) == shortcut.key
            || keystroke
                .key_char
                .as_deref()
                .is_some_and(|typed| typed.chars().count() == 1 && typed.to_lowercase() == shortcut.key))
    {
        return true;
    }
    !m.shift && shortcut.shift_key && unshifted(&key) == Some(shortcut.key.as_str())
}

/// `resolveShortcutCommand`.
pub(super) fn resolve<'a>(rules: &'a [Rule], keystroke: &Keystroke, context: &ShortcutContext) -> Option<&'a str> {
    resolve_on(rules, keystroke, context, MAC)
}

/// `resolveChatShortcutCommand`: the primary's rules decide every command but a project
/// script's, which comes from the rules of the environment the open thread lives on.
pub(super) fn resolve_chat<'a>(
    primary: &'a [Rule],
    active: &'a [Rule],
    keystroke: &Keystroke,
    context: &ShortcutContext,
) -> Option<&'a str> {
    resolve_chat_on(primary, active, keystroke, context, MAC)
}

fn resolve_chat_on<'a>(
    primary: &'a [Rule],
    active: &'a [Rule],
    keystroke: &Keystroke,
    context: &ShortcutContext,
    mac: bool,
) -> Option<&'a str> {
    match resolve_on(primary, keystroke, context, mac) {
        Some(command) if script_id(command).is_none() => Some(command),
        _ => resolve_on(active, keystroke, context, mac).filter(|command| script_id(command).is_some()),
    }
}

/// [`resolve`] as on macOS (`mod` is Cmd) or elsewhere (`mod` is Ctrl).
fn resolve_on<'a>(rules: &'a [Rule], keystroke: &Keystroke, context: &ShortcutContext, mac: bool) -> Option<&'a str> {
    rules
        .iter()
        .rev()
        .filter(|rule| context.admits(rule))
        .find(|rule| matches(keystroke, &shortcut_for_context(rule, context), mac))
        .map(|rule| rule.command.as_str())
        // `useThreadLifecycle`: an open project list pins its highlighted project with this key instead.
        .filter(|command| !(context.combobox_focus && *command == "thread.pin"))
}

/// `effectiveShortcutsForCommand`: the command's shortcuts no later rule claims, newest first.
fn effective_shortcuts<'a>(
    rules: &'a [Rule],
    command: &'a str,
    context: &'a ShortcutContext,
    mac: bool,
) -> impl Iterator<Item = Shortcut> + 'a {
    let normalize = move |s: &Shortcut| {
        (s.meta_key || (s.mod_key && mac), s.ctrl_key || (s.mod_key && !mac), s.shift_key, s.alt_key)
    };
    let mut claimed: Vec<std::borrow::Cow<'a, Shortcut>> = Vec::new();
    rules.iter().rev().filter(|rule| context.admits(rule)).filter_map(move |rule| {
        let shortcut = shortcut_for_context(rule, context);
        if claimed.iter().any(|seen| seen.key == shortcut.key && normalize(seen) == normalize(&shortcut)) {
            return None;
        }
        let effective = (rule.command == command).then(|| shortcut.clone().into_owned());
        claimed.push(shortcut);
        effective
    })
}

/// `effectiveShortcutsForCommand(…)[0]`.
fn effective_shortcut(rules: &[Rule], command: &str, context: &ShortcutContext, mac: bool) -> Option<Shortcut> {
    effective_shortcuts(rules, command, context, mac).next()
}

/// `PREVIEW_FORWARDED_COMMANDS`: the shell commands a focused browser page hands back to the app
/// (`ReopenClosedViewShortcut.tsx`). `view.reopenClosed` joins them once this client keeps a
/// closed-view history.
pub(super) const PAGE_FORWARDED_COMMANDS: [&str; 6] = [
    "workspace.focusNext",
    "workspace.focusPrevious",
    "sidebar.toggle",
    "rightPanel.toggle",
    "rightPanel.toggleMaximized",
    "preview.focusUrl",
];

/// `PreviewForwardedShortcut`, with `mod` resolved to the platform's modifier for the page.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub(super) struct PageChord {
    pub command: &'static str,
    pub key: String,
    pub meta: bool,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

/// The chords a focused browser page forwards, resolved as the web registers them: the preview
/// focused and open, the terminal as it is, nothing else focused.
pub(super) fn page_chords(rules: &[Rule], terminal_open: bool) -> Vec<PageChord> {
    page_chords_on(rules, terminal_open, MAC)
}

fn page_chords_on(rules: &[Rule], terminal_open: bool, mac: bool) -> Vec<PageChord> {
    let context = &ShortcutContext { preview_focus: true, preview_open: true, terminal_open, ..Default::default() };
    PAGE_FORWARDED_COMMANDS
        .into_iter()
        .flat_map(|command| {
            effective_shortcuts(rules, command, context, mac).map(move |s| PageChord {
                command,
                meta: s.meta_key || (s.mod_key && mac),
                ctrl: s.ctrl_key || (s.mod_key && !mac),
                shift: s.shift_key,
                alt: s.alt_key,
                key: s.key,
            })
        })
        .collect()
}

/// `formatShortcutKeyLabel`.
fn key_label(key: &str) -> String {
    match key {
        " " => "Space".into(),
        "escape" => "Esc".into(),
        "arrowup" => "Up".into(),
        "arrowdown" => "Down".into(),
        "arrowleft" => "Left".into(),
        "arrowright" => "Right".into(),
        key if key.chars().count() == 1 => key.to_uppercase(),
        key => {
            let mut chars = key.chars();
            chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
        }
    }
}

/// `formatShortcutLabel`.
pub(super) fn format_label(shortcut: &Shortcut, mac: bool) -> String {
    let meta = shortcut.meta_key || (shortcut.mod_key && mac);
    let ctrl = shortcut.ctrl_key || (shortcut.mod_key && !mac);
    let key = key_label(&shortcut.key);
    if mac {
        let flag = |on: bool, glyph: &'static str| if on { glyph } else { "" };
        return format!(
            "{}{}{}{}{key}",
            flag(ctrl, "\u{2303}"),
            flag(shortcut.alt_key, "\u{2325}"),
            flag(shortcut.shift_key, "\u{21e7}"),
            flag(meta, "\u{2318}")
        );
    }
    let mut parts: Vec<&str> = Vec::new();
    for (on, name) in [(ctrl, "Ctrl"), (shortcut.alt_key, "Alt"), (shortcut.shift_key, "Shift"), (meta, "Meta")] {
        if on {
            parts.push(name);
        }
    }
    parts.push(&key);
    parts.join("+")
}

/// `shortcutLabelForCommand` in the given context.
pub(super) fn label_for(rules: &[Rule], command: &str, context: &ShortcutContext) -> Option<String> {
    label_on(rules, command, context, MAC)
}

/// [`label_for`] as on macOS or elsewhere.
fn label_on(rules: &[Rule], command: &str, context: &ShortcutContext, mac: bool) -> Option<String> {
    effective_shortcut(rules, command, context, mac).map(|shortcut| format_label(&shortcut, mac))
}

/// `threadJumpIndexFromCommand`-style: `prefix.jump.N` as the 0-based index.
pub(super) fn jump_index(command: &str, prefix: &str) -> Option<usize> {
    let n: usize = command.strip_prefix(prefix)?.strip_prefix(".jump.")?.parse().ok()?;
    (1..=9).contains(&n).then(|| n - 1)
}

/// `projectScriptIdFromCommand`.
pub(super) fn script_id(command: &str) -> Option<&str> {
    command.strip_prefix("script.")?.strip_suffix(".run").filter(|id| !id.is_empty())
}

const RELOAD_TOAST_COOLDOWN: Duration = Duration::from_secs(2);

/// What a `keybindingsUpdated` event toasts (`KeybindingsUpdateToast.logic.ts`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Reload {
    Updated,
    Invalid(String),
}

/// A save and the server's file watcher each publish the same snapshot, so an identical
/// toast inside the cooldown is the same change twice.
#[derive(Default)]
pub(super) struct ReloadToasts {
    last: Option<(Option<String>, Instant)>,
}

impl ReloadToasts {
    pub(super) fn handle(&mut self, event: &Value, now: Instant) -> Option<Reload> {
        if event["type"] != "keybindingsUpdated" {
            return None;
        }
        let issue = event["payload"]["issues"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|issue| issue["kind"].as_str().is_some_and(|kind| kind.starts_with("keybindings.")))
            .map(|issue| issue["message"].as_str().unwrap_or_default().to_owned());
        if let Some((last, at)) = &self.last
            && *last == issue
            && now.duration_since(*at) < RELOAD_TOAST_COOLDOWN
        {
            return None;
        }
        self.last = Some((issue.clone(), now));
        Some(issue.map_or(Reload::Updated, Reload::Invalid))
    }
}

impl super::Workspace {
    /// Toasts the keybindings reload the last config batch reported (`startKeybindingsUpdateToasts`).
    pub(super) fn show_reload_toast(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(reload) = self.pending_reload.take() else { return };
        let notification = match reload {
            Reload::Updated => {
                Notification::success("Keybindings configuration reloaded successfully.").title("Keybindings updated")
            }
            Reload::Invalid(message) => {
                let workspace = cx.weak_entity();
                Notification::warning(message).title("Invalid keybindings configuration").action(move |_, _, _| {
                    let workspace = workspace.clone();
                    Button::new("open-keybindings-file").outline().xsmall().label("Open keybindings.json").on_click(
                        move |_, window, cx| {
                            let Some(workspace) = workspace.upgrade() else { return };
                            let workspace = workspace.read(cx);
                            let info = &workspace.primary_info();
                            let (client, path, editors) = (
                                workspace.client(),
                                info.keybindings_config_path.clone(),
                                info.available_editors.clone(),
                            );
                            super::pages::settings::keybindings::open_keybindings_file(
                                client, path, &editors, window, cx,
                            );
                        },
                    )
                })
            }
        };
        window.push_notification(notification, cx);
    }

    /// Recomputes the merged rules when the server's keybindings changed.
    pub(super) fn sync_keymap(&mut self, _cx: &mut Context<Self>) {
        if *self.installed_keybindings != *self.primary_info().keybindings {
            self.installed_keybindings = self.primary_info().keybindings.clone();
            self.keymap = Rc::from(keybindings::merge_with_defaults(&self.installed_keybindings));
        }
    }

    /// The when-clause context for a key pressed with `stack` as the focused element's contexts.
    pub(super) fn shortcut_context(
        &self,
        stack: &[KeyContext],
        window: &Window,
        cx: &Context<Self>,
    ) -> ShortcutContext {
        let composer_focus = self.composer.focus_handle(cx).is_focused(window);
        let shortcut = self.client_settings.values.get("sendShortcut").as_str().unwrap_or("enter");
        ShortcutContext {
            terminal_focus: focus_in(stack, TERMINAL_CONTEXT),
            terminal_open: self.terminal_open(),
            preview_focus: focus_in(stack, PREVIEW_CONTEXT),
            preview_open: matches!(self.active_surface(), Some(super::right_panel::Surface::Browser(_))),
            right_panel_open: self.right_panel_open(),
            editable_focus: focus_in(stack, INPUT_CONTEXT),
            model_picker_open: self.model_picker_open(),
            composer_focus,
            draft_thread_route: matches!(self.target, Some(super::Target::Draft { .. })),
            turn_running: self.turn_running(),
            usage_page_open: self.page == Some(super::pages::Page::Usage),
            composer_requires_modifier: composer_focus
                && super::send::requires_modifier(shortcut, &self.composer.read(cx).value()),
            combobox_focus: focus_in(stack, COMBOBOX_CONTEXT),
            layer_open: ["PopupMenu", "Popover", "Dialog"].iter().any(|layer| focus_in(stack, layer))
                || self.palette.is_some()
                || self.model_picker_open(),
        }
    }

    pub(super) fn turn_running(&self) -> bool {
        self.thread
            .as_ref()
            .and_then(|t| t.state.projection.as_ref())
            .is_some_and(super::super::model::ThreadProjection::is_running)
    }

    /// A command's shortcut label for a tooltip or menu, in the resting context (no terminal or preview focus).
    pub(super) fn shortcut_label(&self, command: &str) -> Option<String> {
        let context = ShortcutContext {
            terminal_open: self.terminal_open(),
            right_panel_open: self.right_panel_open(),
            usage_page_open: self.page == Some(super::pages::Page::Usage),
            ..ShortcutContext::default()
        };
        label_for(&self.keymap, command, &context)
    }

    /// `"{label} ({shortcut})"` for the terminal drawer's controls, labelled as if a terminal had focus.
    pub(super) fn with_terminal_shortcut(&self, label: &str, command: &str) -> String {
        let context =
            ShortcutContext { terminal_focus: true, terminal_open: self.terminal_open(), ..Default::default() };
        match label_for(&self.keymap, command, &context) {
            Some(shortcut) => format!("{label} ({shortcut})"),
            None => label.to_owned(),
        }
    }

    /// `isProjectPinShortcut`: the thread pin shortcut, which pins the highlighted row of a
    /// project list while its search field has focus.
    pub(super) fn is_pin_shortcut(&self, keystroke: &Keystroke) -> bool {
        let context = ShortcutContext { editable_focus: true, ..ShortcutContext::default() };
        resolve(&self.keymap, keystroke, &context) == Some("thread.pin")
    }

    /// `"{label} ({shortcut})"`, or the label alone when the command has no shortcut.
    pub(super) fn with_shortcut(&self, label: &str, command: &str) -> String {
        match self.shortcut_label(command) {
            Some(shortcut) => format!("{label} ({shortcut})"),
            None => label.to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keybindings::{compile, defaults, merge_with_defaults};
    use gpui_kit::Modifiers;

    fn key(text: &'static str) -> Keystroke {
        Keystroke::parse(text).unwrap()
    }

    #[test]
    fn the_last_rule_whose_condition_holds_wins() {
        let rules = defaults();
        let rest = ShortcutContext::default();
        let terminal = ShortcutContext { terminal_focus: true, ..rest };
        assert_eq!(resolve_on(rules, &key("ctrl-w"), &rest, false), Some("rightPanel.close"));
        assert_eq!(resolve_on(rules, &key("ctrl-w"), &terminal, false), Some("terminal.close"));
        assert_eq!(resolve_on(rules, &key("ctrl-n"), &rest, false), Some("chat.new"));
        assert_eq!(resolve_on(rules, &key("ctrl-n"), &terminal, false), Some("terminal.new"));
        assert_eq!(resolve_on(rules, &key("ctrl-d"), &rest, false), Some("diff.toggle"));
        assert_eq!(resolve_on(rules, &key("ctrl-k"), &terminal, false), None, "the palette yields to the terminal");
        assert_eq!(resolve_on(rules, &key("ctrl-j"), &terminal, false), Some("terminal.toggle"));
        let custom = merge_with_defaults(&[compile("mod+alt+y", "sidebar.toggle", None).unwrap()]);
        assert_eq!(resolve_on(&custom, &key("ctrl-alt-y"), &rest, false), Some("sidebar.toggle"));
        assert_eq!(resolve_on(&custom, &key("ctrl-b"), &rest, false), None, "the server's rule replaces the default");
        // `mod` is Cmd on macOS, so Ctrl alone no longer matches it there.
        assert_eq!(resolve_on(rules, &key("cmd-w"), &rest, true), Some("rightPanel.close"));
        assert_eq!(resolve_on(rules, &key("ctrl-w"), &rest, true), None);
        assert_eq!(resolve_on(&custom, &key("cmd-alt-y"), &rest, true), Some("sidebar.toggle"));
    }

    #[test]
    fn altgr_text_runs_no_shortcut_off_macos() {
        let rules = merge_with_defaults(&[
            compile("mod+alt+q", "sidebar.toggle", None).unwrap(),
            compile("mod+alt+e", "usage.open", None).unwrap(),
        ]);
        let rest = ShortcutContext::default();
        let typed = |text: &'static str, typed: &str| Keystroke { key_char: Some(typed.to_owned()), ..key(text) };
        assert_eq!(resolve_on(&rules, &typed("ctrl-alt-q", "@"), &rest, false), None, "German AltGr+Q types @");
        assert_eq!(resolve_on(&rules, &typed("ctrl-alt-e", "€"), &rest, false), None);
        assert_eq!(resolve_on(&rules, &typed("ctrl-alt-q", "q"), &rest, false), Some("sidebar.toggle"));
        assert_eq!(resolve_on(&rules, &key("ctrl-alt-q"), &rest, false), Some("sidebar.toggle"), "no text typed");
        assert_eq!(resolve_on(&rules, &typed("cmd-alt-q", "œ"), &rest, true), Some("sidebar.toggle"));
    }

    #[test]
    fn conditions_read_focus_and_panels() {
        let rules = defaults();
        let rest = ShortcutContext::default();
        assert_eq!(resolve_on(rules, &key("ctrl-t"), &rest, false), None);
        let editing = ShortcutContext { editable_focus: true, ..rest };
        assert_eq!(resolve_on(rules, &key("ctrl-shift-p"), &editing, false), Some("thread.pin"));
        assert_eq!(
            resolve_on(rules, &key("ctrl-shift-p"), &ShortcutContext { combobox_focus: true, ..editing }, false),
            None,
            "an open project list takes the pin shortcut"
        );
        assert_eq!(
            resolve_on(rules, &key("ctrl-t"), &ShortcutContext { right_panel_open: true, ..rest }, false),
            Some("rightPanel.new")
        );
        assert_eq!(resolve_on(rules, &key("ctrl-z"), &rest, false), Some("thread.undo"));
        assert_eq!(resolve_on(rules, &key("ctrl-z"), &ShortcutContext { editable_focus: true, ..rest }, false), None);
        assert_eq!(
            resolve_on(rules, &key("ctrl-r"), &ShortcutContext { preview_focus: true, ..rest }, false),
            Some("preview.refresh")
        );
        assert_eq!(resolve_on(rules, &key("ctrl-1"), &rest, false), Some("thread.jump.1"));
        let picker = ShortcutContext { model_picker_open: true, ..rest };
        assert_eq!(resolve_on(rules, &key("ctrl-1"), &picker, false), Some("modelPicker.jump.1"));
        assert_eq!(
            resolve_on(rules, &key("c"), &ShortcutContext { usage_page_open: true, ..rest }, false),
            Some("usage.cost")
        );
        assert_eq!(resolve_on(rules, &key("c"), &rest, false), None);
    }

    #[test]
    fn send_shortcuts_follow_the_composer_state() {
        let rules = defaults();
        let composer = ShortcutContext { composer_focus: true, ..ShortcutContext::default() };
        let running = ShortcutContext { turn_running: true, ..composer };
        let draft = ShortcutContext { draft_thread_route: true, ..composer };
        assert_eq!(resolve_on(rules, &key("ctrl-enter"), &composer, false), None);
        assert_eq!(resolve_on(rules, &key("ctrl-enter"), &running, false), Some("composer.sendAlternate"));
        assert_eq!(resolve_on(rules, &key("ctrl-enter"), &draft, false), Some("composer.sendBackground"));
        assert_eq!(resolve_on(rules, &key("ctrl-alt-enter"), &composer, false), Some("composer.sendAndNewThread"));
        let modifier = ShortcutContext { composer_requires_modifier: true, ..running };
        assert_eq!(resolve_on(rules, &key("ctrl-enter"), &modifier, false), None, "Mod+Enter sends");
        assert_eq!(resolve_on(rules, &key("ctrl-shift-enter"), &modifier, false), Some("composer.sendAlternate"));
    }

    #[test]
    fn keys_match_by_unshifted_key_or_typed_character() {
        let plus = compile("mod++", "preview.zoomIn", None).unwrap().shortcut;
        let typed_plus = Keystroke { key: "+".into(), key_char: Some("+".into()), modifiers: Modifiers::control() };
        assert!(matches(&typed_plus, &plus, false));
        let shifted_one = Keystroke {
            key: "1".into(),
            key_char: Some("!".into()),
            modifiers: Modifiers { control: true, shift: true, ..Default::default() },
        };
        assert!(matches(&shifted_one, &compile("mod+shift+1", "usage.period.day", None).unwrap().shortcut, false));
        let folded = Keystroke { key: "$".into(), key_char: None, modifiers: Modifiers::control() };
        assert!(matches(&folded, &compile("mod+shift+4", "usage.period.quarter", None).unwrap().shortcut, false));
        assert!(!matches(&folded, &compile("mod+4", "thread.jump.4", None).unwrap().shortcut, false));
        assert!(matches(&key("up"), &compile("arrowup", "x", None).unwrap().shortcut, false));
        assert!(matches(&key("cmd-k"), &compile("mod+k", "x", None).unwrap().shortcut, true));
        assert!(!matches(&key("ctrl-k"), &compile("mod+k", "x", None).unwrap().shortcut, true));
    }

    #[test]
    fn labels_show_the_live_shortcut() {
        let rules = defaults();
        let rest = ShortcutContext::default();
        assert_eq!(label_on(rules, "sidebar.toggle", &rest, false).as_deref(), Some("Ctrl+B"));
        assert_eq!(label_on(rules, "rightPanel.toggle", &rest, false).as_deref(), Some("Ctrl+Alt+B"));
        assert_eq!(label_on(rules, "terminal.close", &rest, false), None, "its condition does not hold");
        assert_eq!(label_on(rules, "workspace.focusNext", &rest, false).as_deref(), Some("Ctrl+F6"), "the newest rule");
        let shadowed = merge_with_defaults(&[compile("mod+b", "usage.open", None).unwrap()]);
        assert_eq!(label_on(&shadowed, "sidebar.toggle", &rest, false), None, "a later rule claims the chord");
        assert_eq!(label_on(rules, "sidebar.toggle", &rest, true).as_deref(), Some("\u{2318}B"));
        assert_eq!(label_on(rules, "rightPanel.toggle", &rest, true).as_deref(), Some("\u{2325}\u{2318}B"));
        let shortcut = compile("mod+shift+arrowup", "x", None).unwrap().shortcut;
        assert_eq!(format_label(&shortcut, true), "\u{21e7}\u{2318}Up");
        assert_eq!(format_label(&compile("esc", "x", None).unwrap().shortcut, false), "Esc");
    }

    #[test]
    fn pages_forward_the_shell_chords_the_keymap_resolves() {
        let chord =
            |command, key: &str, ctrl, shift, alt, meta| PageChord { command, key: key.into(), meta, ctrl, shift, alt };
        assert_eq!(
            page_chords_on(defaults(), false, false),
            [
                chord("workspace.focusNext", "f6", true, false, false, false),
                chord("workspace.focusNext", "f6", false, false, false, false),
                chord("workspace.focusPrevious", "f6", true, true, false, false),
                chord("workspace.focusPrevious", "f6", false, true, false, false),
                chord("sidebar.toggle", "b", true, false, false, false),
                chord("rightPanel.toggle", "b", true, false, true, false),
                chord("preview.focusUrl", "l", true, false, false, false),
            ],
            "every shortcut of each command, with `mod` as Ctrl; nothing binds toggleMaximized"
        );
        let mac = page_chords_on(defaults(), false, true);
        assert!(mac.contains(&chord("sidebar.toggle", "b", false, false, false, true)), "`mod` is Cmd on macOS");

        let custom = merge_with_defaults(&[
            compile("mod+shift+m", "rightPanel.toggleMaximized", None).unwrap(),
            compile("mod+l", "usage.open", None).unwrap(),
            compile("mod+alt+s", "sidebar.toggle", Some("terminalOpen")).unwrap(),
        ]);
        let commands = |terminal_open| {
            page_chords_on(&custom, terminal_open, false)
                .into_iter()
                .map(|chord| (chord.command, chord.key))
                .collect::<Vec<_>>()
        };
        let closed = commands(false);
        assert!(closed.contains(&("rightPanel.toggleMaximized", "m".into())), "a user binding joins the set");
        assert!(!closed.iter().any(|(command, _)| *command == "preview.focusUrl"), "a later rule claims its chord");
        assert!(!closed.contains(&("sidebar.toggle", "s".into())), "its condition needs an open terminal");
        assert!(commands(true).contains(&("sidebar.toggle", "s".into())));
    }

    #[test]
    fn reloads_toast_once_per_change() {
        let mut toasts = ReloadToasts::default();
        let start = Instant::now();
        let ok = serde_json::json!({"type": "keybindingsUpdated", "payload": {"issues": []}});
        let bad = serde_json::json!({"type": "keybindingsUpdated", "payload": {"issues": [
            {"kind": "settings.invalid", "message": "other"},
            {"kind": "keybindings.invalid-entry", "message": "Bad rule"},
        ]}});
        assert_eq!(toasts.handle(&ok, start), Some(Reload::Updated));
        assert_eq!(toasts.handle(&ok, start + Duration::from_millis(500)), None, "the watcher's echo");
        assert_eq!(toasts.handle(&bad, start + Duration::from_millis(600)), Some(Reload::Invalid("Bad rule".into())));
        assert_eq!(toasts.handle(&ok, start + Duration::from_secs(3)), Some(Reload::Updated));
        assert_eq!(toasts.handle(&serde_json::json!({"type": "settingsUpdated"}), start), None);
    }

    #[test]
    fn script_shortcuts_come_from_the_open_environment() {
        let rest = ShortcutContext::default();
        let primary = merge_with_defaults(&[compile("mod+shift+1", "script.local.run", None).unwrap()]);
        let remote = [
            compile("mod+shift+1", "script.remote.run", None).unwrap(),
            compile("mod+shift+2", "script.dev.run", None).unwrap(),
            compile("mod+shift+3", "sidebar.toggle", None).unwrap(),
            compile("mod+b", "script.shadowed.run", None).unwrap(),
        ];
        let resolve = |text| resolve_chat_on(&primary, &remote, &key(text), &rest, false);
        assert_eq!(resolve("ctrl-shift-1"), Some("script.remote.run"), "the primary's script rule yields");
        assert_eq!(resolve("ctrl-shift-2"), Some("script.dev.run"));
        assert_eq!(resolve("ctrl-shift-3"), None, "the open environment binds only scripts");
        assert_eq!(resolve("ctrl-b"), Some("sidebar.toggle"), "the primary's own commands win");
        assert_eq!(resolve_chat_on(&primary, &primary, &key("ctrl-shift-1"), &rest, false), Some("script.local.run"));
    }

    #[test]
    fn commands_carry_their_index_or_script() {
        assert_eq!(jump_index("thread.jump.3", "thread"), Some(2));
        assert_eq!(jump_index("modelPicker.jump.9", "modelPicker"), Some(8));
        assert_eq!(jump_index("thread.jump.0", "thread"), None);
        assert_eq!(script_id("script.dev.run"), Some("dev"));
        assert_eq!(script_id("script..run"), None);
    }
}
