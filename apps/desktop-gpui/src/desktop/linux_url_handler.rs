//! `t3code://` links on Linux, as Electron's `DesktopLinuxUrlHandler` registers them: a hidden,
//! URL-only desktop entry that runs this executable with the link, made the scheme's default
//! handler with `xdg-mime`. Unlike Electron it never takes the scheme from another handler (such
//! as an installed Electron T3 Code): it claims it only while unset or already its own.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// This app's desktop entry, named after its app id ([`super::APP_ID`]); the launcher badge
/// names it too.
pub const DESKTOP_ENTRY: &str = "com.t3tools.t3code.gpui.desktop";
const SCHEME: &str = "t3code";
const DISPLAY_NAME: &str = "T3 Code";

/// Registers the handler on a background thread; failures are logged, never shown.
pub fn register() {
    let spawned = std::thread::Builder::new().name("url-handler".into()).spawn(|| {
        if let Err(error) = try_register() {
            log::warn!("could not register the {SCHEME}:// handler: {error}");
        }
    });
    if let Err(error) = spawned {
        log::warn!("could not start the {SCHEME}:// handler registration: {error}");
    }
}

fn try_register() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|error| error.to_string())?;
    let dir = applications_dir().ok_or("no home directory")?;
    let entry = dir.join(DESKTOP_ENTRY);
    let content = render(DISPLAY_NAME, &exe.to_string_lossy(), SCHEME);
    if std::fs::read_to_string(&entry).ok().as_deref() != Some(content.as_str()) {
        std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
        std::fs::write(&entry, &content).map_err(|error| error.to_string())?;
    }
    let mime = format!("x-scheme-handler/{SCHEME}");
    let current = output("xdg-mime", &["query", "default", &mime]).unwrap_or_default();
    let current = current.trim();
    if current.is_empty() || current == DESKTOP_ENTRY {
        output("xdg-mime", &["default", DESKTOP_ENTRY, &mime]).ok_or("xdg-mime default failed")?;
    } else {
        log::info!("{SCHEME}:// links stay with {current}");
    }
    // The MIME cache only speeds lookups up; a missing tool is fine.
    let _ = output("update-desktop-database", &[&dir.to_string_lossy()]);
    Ok(())
}

/// `$XDG_DATA_HOME/applications`, else `~/.local/share/applications`.
fn applications_dir() -> Option<PathBuf> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".local/share")))?;
    Some(data.join("applications"))
}

fn output(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    output.status.success().then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// `escapeDesktopEntryString`.
fn escape_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('\n', "\\n").replace('\r', "\\r").replace('\t', "\\t")
}

/// `escapeDesktopEntryExecArgument`: Exec values are unescaped, then unquoted, so escape in reverse.
fn escape_exec_argument(value: &str) -> String {
    let quoted =
        value.replace('\\', "\\\\").replace('`', "\\`").replace('$', "\\$").replace('"', "\\\"").replace('%', "%%");
    escape_string(&format!("\"{quoted}\""))
}

/// `escapeUrlHandlerExecTarget`: legacy xdg-open reads the first Exec word without unquoting it.
fn escape_exec_target(value: &str) -> String {
    if value.chars().any(|c| " \t\n\"'\\><~|&;$*?#()`".contains(c)) {
        escape_exec_argument(value)
    } else {
        escape_string(&value.replace('%', "%%"))
    }
}

/// `renderUrlHandlerDesktopEntry`.
fn render(display_name: &str, exec_target: &str, scheme: &str) -> String {
    [
        "[Desktop Entry]".to_owned(),
        "Type=Application".to_owned(),
        format!("Name={}", escape_string(display_name)),
        format!("Exec={} %U", escape_exec_target(exec_target)),
        "Terminal=false".to_owned(),
        "NoDisplay=true".to_owned(),
        "StartupNotify=false".to_owned(),
        format!("MimeType=x-scheme-handler/{scheme};"),
        String::new(),
    ]
    .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_entry_is_named_after_the_window_app_id() {
        assert_eq!(DESKTOP_ENTRY, format!("{}.desktop", super::super::APP_ID));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_window_icon_decodes() {
        assert!(super::super::window_icon().is_some());
    }

    #[test]
    fn entries_quote_exec_targets_like_electron() {
        let entry = render("T3 Code", "/opt/t3/t3-desktop", "t3code");
        assert!(entry.starts_with("[Desktop Entry]\nType=Application\nName=T3 Code\n"));
        assert!(entry.contains("\nExec=/opt/t3/t3-desktop %U\n"));
        assert!(entry.ends_with("NoDisplay=true\nStartupNotify=false\nMimeType=x-scheme-handler/t3code;\n"));
        assert_eq!(escape_exec_target("/home/me/my apps/t3"), r#""/home/me/my apps/t3""#);
        assert_eq!(escape_exec_target("/a/100%/t3"), "/a/100%%/t3");
        assert_eq!(escape_exec_target("/a/$HOME `x`"), r#""/a/\\$HOME \\`x\\`""#);
    }
}
