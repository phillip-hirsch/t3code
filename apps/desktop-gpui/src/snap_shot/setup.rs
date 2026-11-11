//! `DesktopSnapShotState` and the setup copy and rules the Settings page and its wizard read
//! from it (`SnapShotSettings.logic.ts`, `SnapShotSetupDialog.logic.ts`). Linux captures through
//! the desktop-neutral portals only, so the GNOME extension, KDE, Hyprland and Niri helper
//! branches have no state to take here.

use crate::keybindings::Shortcut as Chord;

/// `DesktopSnapShotState.mode`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Direct,
    Portal,
    Unavailable,
}

/// `linuxBackend`: the window screenshot portal, or a screenshot the user picks each time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(
    all(any(windows, target_os = "macos"), not(test)),
    expect(dead_code, reason = "only Linux captures through the window screenshot portal")
)]
pub enum LinuxBackend {
    ScreenshotPortal,
    Picker,
}

/// `macPermissions`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MacPermissions {
    pub screen_recording: bool,
    pub accessibility: bool,
}

/// A macOS permission the setup asks for (`allow-screen-recording`, `allow-accessibility`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MacPermission {
    ScreenRecording,
    Accessibility,
}

/// `PortalShortcutState`: the desktop's answer about the portal shortcut.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PortalShortcut {
    pub registered: bool,
    pub pending: bool,
    pub can_retry: Option<bool>,
    pub label: Option<String>,
    pub message: Option<String>,
}

impl PortalShortcut {
    /// `PortalCaptureShortcut`'s state before the desktop answers.
    pub fn waiting() -> Self {
        Self {
            pending: true,
            message: Some("Waiting for shortcut permission. Approve the desktop prompt if one appears.".into()),
            ..Self::default()
        }
    }

    /// The session is gone; a retry asks again.
    pub fn failed(message: impl Into<String>) -> Self {
        Self { can_retry: Some(true), message: Some(message.into()), ..Self::default() }
    }

    /// The desktop declined `BindShortcuts`.
    #[cfg(any(not(any(windows, target_os = "macos")), test))]
    pub fn denied(version: u32) -> Self {
        let message = if version >= 2 {
            "Shortcut permission wasn't granted. Open shortcut permissions to allow it."
        } else {
            "Shortcut permission wasn't granted. Allow T3 Code in your desktop's shortcut settings."
        };
        Self { can_retry: Some(version >= 2), message: Some(message.into()), ..Self::default() }
    }

    /// `bound`: the desktop's trigger for the shortcut, when it assigned one.
    #[cfg(any(not(any(windows, target_os = "macos")), test))]
    pub fn bound(label: Option<&str>, version: u32) -> Self {
        let label = label.map(str::trim).filter(|label| !label.is_empty());
        let message = match label {
            Some(label) => format!("Desktop shortcut: {label}"),
            None if version >= 2 => "No shortcut is assigned. Open shortcut permissions to choose one.".into(),
            None => "No shortcut is assigned. Choose one in your desktop's shortcut settings.".into(),
        };
        Self {
            registered: label.is_some(),
            pending: false,
            can_retry: Some(version >= 2),
            label: label.map(str::to_owned),
            message: Some(message),
        }
    }

    pub fn apply(self, state: &mut State) {
        state.shortcut_registered = self.registered;
        state.shortcut_pending = self.pending;
        state.shortcut_can_retry = self.can_retry;
        state.shortcut_label = self.label;
        state.shortcut_message = self.message;
    }
}

/// `DesktopSnapShotState`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct State {
    pub mode: Mode,
    pub windows: bool,
    /// `linuxDesktop`, as its display name.
    pub linux_desktop: Option<&'static str>,
    /// `None` in portal mode until the session bus answers.
    pub linux_backend: Option<LinuxBackend>,
    pub shortcut_registered: bool,
    pub shortcut_pending: bool,
    pub shortcut_can_retry: Option<bool>,
    /// The desktop's own name for the portal shortcut.
    pub shortcut_label: Option<String>,
    pub shortcut_message: Option<String>,
    /// Why capture can't run: a permission, an unsupported session, or the last failure.
    pub message: Option<String>,
    /// The shortcut fired since it was registered.
    pub shortcut_verified: bool,
    pub mac_permissions: Option<MacPermissions>,
}

impl State {
    /// Whether the web would have a state yet: a portal session answers asynchronously.
    pub fn known(&self) -> bool {
        self.mode != Mode::Portal || self.linux_backend.is_some() || self.message.is_some()
    }
}

/// `captureSetupBackend` for the backends this client has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    Direct,
    Portal,
    Picker,
}

/// `CaptureSetupStep`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Access,
    Shortcut,
}

/// Unsupported sessions' `message`.
#[cfg(any(not(any(windows, target_os = "macos")), test))]
pub const X11_UNAVAILABLE: &str = "SnapShots require a Wayland session. X11 capture is not supported.";
/// `WAYLAND_MODIFIER_PAIR_UNAVAILABLE_MESSAGE`.
pub const WAYLAND_PAIR_UNAVAILABLE: &str = "Modifier-pair shortcuts aren't available in this Wayland session. Choose another shortcut or use Take snapshot from the command palette.";
/// The portal check's answer for a chord it can bind.
pub const PORTAL_CHORD_AVAILABLE: &str = "Your desktop will confirm this shortcut when you save it.";
const MAC_SCREEN_RECORDING: &str = "Allow Screen Recording in System Settings, then restart T3 Code.";
const MAC_ACCESSIBILITY: &str = "Allow Accessibility in System Settings, then restart T3 Code.";
const MAC_BOTH: &str = "Allow Accessibility and Screen Recording in System Settings, then restart T3 Code.";

/// `captureSetupBackend`.
pub fn backend(state: &State) -> Backend {
    match (state.mode, state.linux_backend) {
        (Mode::Portal, Some(LinuxBackend::ScreenshotPortal)) => Backend::Portal,
        (Mode::Portal, _) => Backend::Picker,
        _ => Backend::Direct,
    }
}

/// `captureSetupDesktopName`.
pub fn desktop_name(state: &State) -> Option<&'static str> {
    (state.mode == Mode::Portal).then_some(state.linux_desktop).flatten()
}

/// `XDG_CURRENT_DESKTOP`'s first desktop the setup names.
#[cfg(any(not(any(windows, target_os = "macos")), test))]
pub fn linux_desktop(current_desktop: &str) -> Option<&'static str> {
    current_desktop.split(':').find_map(|name| match name.to_lowercase().as_str() {
        "gnome" => Some("GNOME"),
        "kde" => Some("KDE Plasma"),
        "niri" => Some("Niri"),
        "hyprland" => Some("Hyprland"),
        _ => None,
    })
}

/// `isWaylandSession`, given the session type, the display and the runtime directory's entries.
#[cfg(any(not(any(windows, target_os = "macos")), test))]
pub fn is_wayland_session<'a>(
    session_type: Option<&str>,
    wayland_display: Option<&str>,
    runtime_entries: impl IntoIterator<Item = &'a str>,
) -> bool {
    let session_type = session_type.map(str::to_lowercase);
    if session_type.as_deref() == Some("wayland") || wayland_display.is_some_and(|display| !display.is_empty()) {
        return true;
    }
    if session_type.as_deref() == Some("x11") {
        return false;
    }
    runtime_entries.into_iter().any(|name| {
        name.strip_prefix("wayland-").is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    })
}

/// `captureSetupAccessReady`.
pub fn access_ready(state: &State) -> bool {
    state.mode != Mode::Unavailable && state.message.is_none()
}

/// `captureSetupMacPermissionsReady`.
pub fn mac_permissions_ready(state: &State, include_accessibility: bool) -> bool {
    state.mac_permissions.is_none_or(|p| p.screen_recording && (!include_accessibility || p.accessibility))
}

/// `captureSetupShortcutReady`.
pub fn shortcut_ready(state: &State, unsaved: bool) -> bool {
    !unsaved && access_ready(state) && (state.shortcut_registered || state.shortcut_pending)
}

/// `captureSetupInitialStep`: `None` resumes where setup left off.
pub fn initial_step(state: &State, requested: Option<Step>) -> Step {
    if !access_ready(state) {
        return Step::Access;
    }
    match requested {
        // Disabled backends have not checked permissions; pickers need an explanation, not an
        // active-window access claim.
        None if (state.mode == Mode::Direct && !state.shortcut_registered) || backend(state) == Backend::Picker => {
            Step::Access
        }
        None => Step::Shortcut,
        Some(step) => step,
    }
}

/// `snapShotStatus`.
pub fn status(state: Option<&State>, enabled: bool) -> String {
    let Some(state) = state else { return "Checking snapshots…".into() };
    if state.mode == Mode::Unavailable {
        return state.message.clone().unwrap_or_else(|| "Not supported on this platform.".into());
    }
    if !enabled {
        return "Turn this on to set up snapshots.".into();
    }
    setup_summary(state, enabled).into()
}

/// `snapShotSetupSummary`.
pub fn setup_summary(state: &State, enabled: bool) -> &'static str {
    if state.message.is_some() {
        "Capture needs attention"
    } else if backend(state) == Backend::Picker {
        "Manual capture only. You'll choose a window each time"
    } else if !enabled {
        "Enable capture to continue"
    } else if state.shortcut_pending {
        "Waiting for shortcut permission"
    } else if state.shortcut_verified {
        "Ready to capture"
    } else if state.shortcut_registered {
        if state.shortcut_label.is_some() { "Ready to capture" } else { "Shortcut saved" }
    } else {
        "Finish shortcut setup"
    }
}

/// `snapShotShortcutStatus`.
pub fn shortcut_status(state: Option<&State>) -> Option<String> {
    let state = state?;
    if state.shortcut_pending {
        Some("Approve the shortcut permission prompt to continue.".into())
    } else if state.shortcut_registered {
        (state.mode != Mode::Portal).then(|| "Shortcut saved.".into())
    } else {
        state.shortcut_message.clone()
    }
}

/// `snapShotSetupButtonLabel`.
pub fn setup_button_label(state: Option<&State>) -> String {
    let Some(state) = state else { return "Continue setup".into() };
    if access_ready(state) {
        return "Manage capture".into();
    }
    desktop_name(state).map_or_else(|| "Continue setup".into(), |desktop| format!("Set up {desktop} capture"))
}

/// `snapShotSetupComplete`.
pub fn setup_complete(state: Option<&State>, include_accessibility: bool) -> bool {
    let Some(state) = state else { return false };
    state.windows
        || (state.mac_permissions.is_some()
            && access_ready(state)
            && mac_permissions_ready(state, include_accessibility)
            && state.shortcut_registered)
}

/// `snapShotFeedbackUnavailableMessage`: no portal backend here flashes or animates.
pub fn feedback_unavailable(state: Option<&State>) -> Option<&'static str> {
    state.filter(|state| state.mode == Mode::Portal).map(|_| "Capture effects aren't available on this desktop.")
}

/// `snapShotDescription`.
pub fn description(state: Option<&State>) -> &'static str {
    if state.is_some_and(|state| state.mode == Mode::Portal && backend(state) == Backend::Picker) {
        "Automatic capture isn't available here. Choose a window instead."
    } else {
        "Capture a window and attach it to your current draft."
    }
}

/// `snapShotAccessibilityUnavailableMessage`.
pub fn accessibility_unavailable(state: Option<&State>) -> Option<&'static str> {
    state
        .filter(|state| state.mode == Mode::Portal && state.linux_backend.is_some())
        .map(|_| "This desktop only provides a screenshot.")
}

/// `macPermissionMessage`: what capture still needs before it can run.
pub fn mac_permission_message(permissions: MacPermissions, include_accessibility: bool) -> Option<&'static str> {
    let accessibility = !include_accessibility || permissions.accessibility;
    match (permissions.screen_recording, accessibility) {
        (false, false) => Some(MAC_BOTH),
        (true, false) => Some(MAC_ACCESSIBILITY),
        (false, true) => Some(MAC_SCREEN_RECORDING),
        (true, true) => None,
    }
}

/// `MAC_PERMISSION_MESSAGES`: a state message that clears once permission is granted.
pub fn is_mac_permission_message(message: &str) -> bool {
    [MAC_SCREEN_RECORDING, MAC_ACCESSIBILITY, MAC_BOTH].contains(&message)
}

/// `portalShortcutTrigger`: the chord as the shortcuts specification writes it.
pub fn portal_trigger(chord: &Chord) -> Result<String, &'static str> {
    let key = chord.key.to_lowercase();
    let named = match key.as_str() {
        " " => "space",
        "escape" | "esc" => "Escape",
        "enter" => "Return",
        "tab" => "Tab",
        "backspace" => "BackSpace",
        "delete" => "Delete",
        "insert" => "Insert",
        "home" => "Home",
        "end" => "End",
        "pageup" => "Page_Up",
        "pagedown" => "Page_Down",
        "arrowup" | "up" => "Up",
        "arrowdown" | "down" => "Down",
        "arrowleft" | "left" => "Left",
        "arrowright" | "right" => "Right",
        "+" => "plus",
        "-" => "minus",
        "=" => "equal",
        "," => "comma",
        "." => "period",
        "/" => "slash",
        ";" => "semicolon",
        "'" => "apostrophe",
        "[" => "bracketleft",
        "]" => "bracketright",
        "\\" => "backslash",
        "`" => "grave",
        "!" => "exclam",
        "@" => "at",
        "#" => "numbersign",
        "$" => "dollar",
        "%" => "percent",
        "^" => "asciicircum",
        "&" => "ampersand",
        "*" => "asterisk",
        "(" => "parenleft",
        ")" => "parenright",
        "_" => "underscore",
        ":" => "colon",
        "\"" => "quotedbl",
        "{" => "braceleft",
        "}" => "braceright",
        "|" => "bar",
        "<" => "less",
        ">" => "greater",
        "?" => "question",
        "~" => "asciitilde",
        _ => "",
    };
    let function = key
        .strip_prefix('f')
        .and_then(|n| n.parse::<u8>().ok().filter(|n| (1..=24).contains(n) && !key[1..].starts_with('0')));
    let keysym = if !named.is_empty() {
        named.to_owned()
    } else if (key.len() == 1 && key.bytes().all(|b| b.is_ascii_alphanumeric())) || function.is_some() {
        key.to_uppercase()
    } else {
        return Err("This key isn't supported as a Wayland capture shortcut. Choose another key.");
    };
    let modifiers = [
        (chord.ctrl_key || chord.mod_key, "CTRL"),
        (chord.alt_key, "ALT"),
        (chord.shift_key, "SHIFT"),
        (chord.meta_key, "LOGO"),
    ];
    let mut parts: Vec<String> = modifiers.into_iter().filter(|(on, _)| *on).map(|(_, name)| name.to_owned()).collect();
    parts.push(keysym);
    Ok(parts.join("+"))
}

/// `parseDesktopSnapShotShortcut`: a desktop's shortcut label as a chord, when it reads as one.
pub fn chord_from_desktop_label(label: &str) -> Option<Chord> {
    let text = label.trim();
    let text = text.get(..6).filter(|p| p.eq_ignore_ascii_case("press ")).map_or(text, |_| text[6..].trim_start());
    if text.is_empty() {
        return None;
    }
    // GTK accelerators: `<Control><Shift>2`.
    let input = match text.rfind('>') {
        Some(end) if text.starts_with('<') && end + 1 < text.len() => {
            let modifiers = &text[..=end];
            let parts: Vec<&str> = modifiers.trim_matches(['<', '>']).split("><").collect();
            if parts.iter().any(|part| part.is_empty() || part.contains(['<', '>'])) {
                return None;
            }
            format!("{}+{}", parts.join("+"), &text[end + 1..])
        }
        _ => text.to_owned(),
    };
    let input = input
        .split('+')
        .map(|part| match part.to_lowercase().as_str() {
            "super" | "win" => "meta".to_owned(),
            "primary" | "control" => "ctrl".to_owned(),
            _ => part.to_owned(),
        })
        .collect::<Vec<_>>()
        .join("+");
    if input != "+" && input.ends_with('+') && !input.ends_with("++") {
        return None;
    }
    let mut chord = crate::keybindings::parse_shortcut(&input.to_lowercase())?;
    chord.key = match chord.key.as_str() {
        "return" => "enter",
        "page_up" => "pageup",
        "page_down" => "pagedown",
        "left" => "arrowleft",
        "right" => "arrowright",
        "up" => "arrowup",
        "down" => "arrowdown",
        "plus" => "+",
        "minus" => "-",
        key => key,
    }
    .to_owned();
    let named = [
        "enter",
        "tab",
        "escape",
        "backspace",
        "delete",
        "insert",
        "home",
        "end",
        "pageup",
        "pagedown",
        "arrowleft",
        "arrowright",
        "arrowup",
        "arrowdown",
        "pause",
        "printscreen",
    ];
    let single =
        chord.key.chars().count() == 1 && !chord.key.contains(char::is_whitespace) && !chord.key.contains(['<', '>']);
    let function =
        chord.key.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()).is_some_and(|n| (1..=24).contains(&n));
    (chord.key == " " || single || function || named.contains(&chord.key.as_str())).then_some(chord)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn portal(backend: LinuxBackend) -> State {
        State { mode: Mode::Portal, linux_backend: Some(backend), ..State::default() }
    }

    fn mac(screen_recording: bool, accessibility: bool) -> State {
        State { mac_permissions: Some(MacPermissions { screen_recording, accessibility }), ..State::default() }
    }

    #[test]
    fn status_reads_like_the_web_for_each_capture_mode() {
        let mut state = State::default();
        assert_eq!(status(None, true), "Checking snapshots…");
        assert_eq!(status(Some(&state), false), "Turn this on to set up snapshots.");
        assert_eq!(status(Some(&state), true), "Finish shortcut setup");
        state.shortcut_registered = true;
        assert_eq!(status(Some(&state), true), "Shortcut saved");
        assert_eq!(shortcut_status(Some(&state)).as_deref(), Some("Shortcut saved."));
        state.shortcut_verified = true;
        assert_eq!(status(Some(&state), true), "Ready to capture");
        state.message = Some("failed".into());
        assert_eq!(status(Some(&state), true), "Capture needs attention");
        let failed = State { shortcut_message: Some("taken".into()), ..State::default() };
        assert_eq!(shortcut_status(Some(&failed)).as_deref(), Some("taken"));

        let x11 = State { mode: Mode::Unavailable, message: Some(X11_UNAVAILABLE.into()), ..State::default() };
        assert_eq!(status(Some(&x11), true), X11_UNAVAILABLE);
        assert_eq!(
            status(Some(&State { mode: Mode::Unavailable, ..State::default() }), true),
            "Not supported on this platform."
        );

        let mut wayland = portal(LinuxBackend::ScreenshotPortal);
        wayland.shortcut_pending = true;
        assert_eq!(status(Some(&wayland), true), "Waiting for shortcut permission");
        assert_eq!(
            shortcut_status(Some(&wayland)).as_deref(),
            Some("Approve the shortcut permission prompt to continue.")
        );
        wayland.shortcut_pending = false;
        wayland.shortcut_registered = true;
        wayland.shortcut_label = Some("Ctrl+Shift+2".into());
        assert_eq!(status(Some(&wayland), true), "Ready to capture");
        assert_eq!(shortcut_status(Some(&wayland)), None, "the portal names its own shortcut");
        assert_eq!(
            status(Some(&portal(LinuxBackend::Picker)), true),
            "Manual capture only. You'll choose a window each time"
        );
    }

    #[test]
    fn portal_sessions_are_unknown_until_the_bus_answers() {
        let checking = State { mode: Mode::Portal, ..State::default() };
        assert!(!checking.known());
        assert!(portal(LinuxBackend::Picker).known());
        assert!(State { message: Some("no bus".into()), ..checking }.known());
        assert!(State::default().known());
    }

    #[test]
    fn setup_completes_on_windows_at_once_and_on_macos_with_permissions_and_a_shortcut() {
        assert!(setup_complete(Some(&State { windows: true, ..State::default() }), true));
        assert!(!setup_complete(None, false));
        let mut state = mac(true, false);
        assert!(!setup_complete(Some(&state), false), "no shortcut yet");
        state.shortcut_registered = true;
        assert!(setup_complete(Some(&state), false));
        assert!(!setup_complete(Some(&state), true), "app text needs Accessibility");
        assert!(!setup_complete(Some(&portal(LinuxBackend::ScreenshotPortal)), false), "Linux always offers Manage");
    }

    #[test]
    fn the_setup_button_and_wizard_start_where_access_stands() {
        assert_eq!(setup_button_label(None), "Continue setup");
        assert_eq!(setup_button_label(Some(&mac(true, true))), "Manage capture");
        let broken =
            State { linux_desktop: Some("GNOME"), message: Some("no portal".into()), ..portal(LinuxBackend::Picker) };
        assert_eq!(setup_button_label(Some(&broken)), "Set up GNOME capture");
        assert_eq!(initial_step(&broken, Some(Step::Shortcut)), Step::Access);

        let direct = State::default();
        assert_eq!(initial_step(&direct, None), Step::Access, "no shortcut registered yet");
        let registered = State { shortcut_registered: true, ..State::default() };
        assert_eq!(initial_step(&registered, None), Step::Shortcut);
        assert_eq!(initial_step(&portal(LinuxBackend::Picker), None), Step::Access);
        assert_eq!(initial_step(&portal(LinuxBackend::ScreenshotPortal), None), Step::Shortcut);
        assert_eq!(initial_step(&portal(LinuxBackend::Picker), Some(Step::Shortcut)), Step::Shortcut);
    }

    #[test]
    fn the_shortcut_step_waits_on_access_and_unsaved_edits() {
        let failed = State { message: Some("x".into()), ..State::default() };
        let pending = State { shortcut_pending: true, ..portal(LinuxBackend::ScreenshotPortal) };
        assert!(shortcut_ready(&pending, false));
        assert!(!shortcut_ready(&pending, true), "an unsaved edit must be saved first");
        assert!(!shortcut_ready(&failed, false));
        assert!(mac_permissions_ready(&State::default(), true));
        assert!(mac_permissions_ready(&mac(true, false), false));
        assert!(!mac_permissions_ready(&mac(true, false), true));
    }

    #[test]
    fn portal_backends_disable_effects_and_app_text() {
        let picker = portal(LinuxBackend::Picker);
        assert_eq!(description(Some(&picker)), "Automatic capture isn't available here. Choose a window instead.");
        assert_eq!(
            description(Some(&portal(LinuxBackend::ScreenshotPortal))),
            "Capture a window and attach it to your current draft."
        );
        assert_eq!(accessibility_unavailable(Some(&picker)), Some("This desktop only provides a screenshot."));
        assert_eq!(accessibility_unavailable(Some(&State::default())), None);
        assert_eq!(feedback_unavailable(Some(&picker)), Some("Capture effects aren't available on this desktop."));
        assert_eq!(feedback_unavailable(Some(&State::default())), None);
    }

    #[test]
    fn mac_permission_messages_name_what_is_missing() {
        let permissions = |screen_recording, accessibility| MacPermissions { screen_recording, accessibility };
        assert_eq!(mac_permission_message(permissions(true, false), false), None);
        assert_eq!(mac_permission_message(permissions(true, false), true), Some(MAC_ACCESSIBILITY));
        assert_eq!(mac_permission_message(permissions(false, true), true), Some(MAC_SCREEN_RECORDING));
        assert_eq!(mac_permission_message(permissions(false, false), true), Some(MAC_BOTH));
        assert!(is_mac_permission_message(MAC_BOTH));
        assert!(!is_mac_permission_message("Could not capture the active window."));
    }

    #[test]
    fn wayland_sessions_are_found_like_electron() {
        assert!(is_wayland_session(Some("Wayland"), None, []));
        assert!(is_wayland_session(Some("x11"), Some("wayland-0"), []));
        assert!(!is_wayland_session(Some("x11"), None, ["wayland-0"]));
        assert!(is_wayland_session(None, None, ["bus", "wayland-1"]));
        assert!(!is_wayland_session(None, None, ["wayland-1.lock", "wayland-"]));
        assert_eq!(linux_desktop("ubuntu:GNOME"), Some("GNOME"));
        assert_eq!(linux_desktop("KDE"), Some("KDE Plasma"));
        assert_eq!(linux_desktop("XFCE"), None);
    }

    #[test]
    fn chords_become_portal_triggers() {
        let trigger = |input: &str| portal_trigger(&crate::keybindings::parse_shortcut(input).expect("a chord"));
        assert_eq!(trigger("mod+shift+2").as_deref(), Ok("CTRL+SHIFT+2"));
        assert_eq!(trigger("alt+meta+k").as_deref(), Ok("ALT+LOGO+K"));
        assert_eq!(trigger("ctrl+f12").as_deref(), Ok("CTRL+F12"));
        assert_eq!(trigger("ctrl+arrowup").as_deref(), Ok("CTRL+Up"));
        assert_eq!(trigger("ctrl+space").as_deref(), Ok("CTRL+space"));
        assert!(trigger("ctrl+printscreen").is_err());
    }

    #[test]
    fn portal_answers_become_shortcut_state() {
        let mut state = portal(LinuxBackend::ScreenshotPortal);
        PortalShortcut::waiting().apply(&mut state);
        assert!(state.shortcut_pending && !state.shortcut_registered);
        PortalShortcut::bound(Some(" Ctrl+Shift+2 "), 2).apply(&mut state);
        assert!(state.shortcut_registered && !state.shortcut_pending);
        assert_eq!(state.shortcut_label.as_deref(), Some("Ctrl+Shift+2"));
        assert_eq!(state.shortcut_message.as_deref(), Some("Desktop shortcut: Ctrl+Shift+2"));
        let unassigned = PortalShortcut::bound(Some(""), 1);
        assert!(!unassigned.registered);
        assert_eq!(unassigned.can_retry, Some(false), "version 1 has no configuration dialog");
        assert!(unassigned.message.is_some_and(|m| m.contains("desktop's shortcut settings")));
        assert_eq!(PortalShortcut::denied(2).can_retry, Some(true));
        assert_eq!(PortalShortcut::failed("gone").can_retry, Some(true));
    }

    #[test]
    fn desktop_labels_read_as_chords() {
        let parsed =
            |label: &str| chord_from_desktop_label(label).map(|c| (c.key, c.ctrl_key, c.shift_key, c.meta_key));
        assert_eq!(parsed("Ctrl+Shift+2"), Some(("2".into(), true, true, false)));
        assert_eq!(parsed("Press <Control><Shift>Page_Up"), Some(("pageup".into(), true, true, false)));
        assert_eq!(parsed("Super+Return"), Some(("enter".into(), false, false, true)));
        assert_eq!(parsed("Ctrl+"), None);
        assert_eq!(parsed(""), None);
        assert_eq!(parsed("Ctrl+Mouse4"), None);
    }
}
