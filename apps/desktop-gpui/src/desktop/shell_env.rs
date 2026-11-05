//! The login shell's environment for the server this app starts, like Electron's
//! `DesktopShellEnvironment` on macOS and Linux: an app opened from Finder or a desktop
//! launcher inherits launchd's or the session's bare `PATH`, so `node` and the provider CLIs
//! would not be found. Windows needs none of it; the server reads the user PATH itself there.

use std::collections::HashMap;
use std::time::Duration;

/// `LOGIN_SHELL_ENV_NAMES`.
const LOGIN_SHELL_ENV_NAMES: &[&str] = &[
    "PATH",
    "DBUS_SESSION_BUS_ADDRESS",
    "DISPLAY",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "SSH_AUTH_SOCK",
    "HOMEBREW_PREFIX",
    "HOMEBREW_CELLAR",
    "HOMEBREW_REPOSITORY",
    "XDG_CONFIG_HOME",
    "XDG_CURRENT_DESKTOP",
    "XDG_DATA_HOME",
    "XDG_RUNTIME_DIR",
    "XDG_SESSION_DESKTOP",
    "XDG_SESSION_TYPE",
    "WAYLAND_DISPLAY",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "NODE_EXTRA_CA_CERTS",
    "T3CODE_TELEMETRY_ENABLED",
    "T3CODE_GITHUB_APP_CLIENT_ID",
    "__MISE_DIFF",
    "JAVA_HOME",
];

/// The shell's value wins over the inherited one.
const SHELL_PREFERRED: &[&str] =
    &["DBUS_SESSION_BUS_ADDRESS", "XDG_CURRENT_DESKTOP", "XDG_SESSION_DESKTOP", "XDG_SESSION_TYPE", "__MISE_DIFF"];

/// The shell's value fills in only what the app didn't inherit.
const FILL_IN: &[&str] = &[
    "DISPLAY",
    "HOMEBREW_PREFIX",
    "HOMEBREW_CELLAR",
    "HOMEBREW_REPOSITORY",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_RUNTIME_DIR",
    "WAYLAND_DISPLAY",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "NODE_EXTRA_CA_CERTS",
    "JAVA_HOME",
    "T3CODE_TELEMETRY_ENABLED",
    "T3CODE_GITHUB_APP_CLIENT_ID",
    "SSH_AUTH_SOCK",
];

const LOCALE_NAMES: &[&str] = &["LANG", "LC_ALL", "LC_CTYPE"];
/// `FALLBACK_LC_CTYPE`: launchd sets no locale, and pbcopy then decodes text as MacRoman.
const FALLBACK_LC_CTYPE: &str = "en_US.UTF-8";
const LOGIN_SHELL_TIMEOUT: Duration = Duration::from_secs(5);
const LAUNCHCTL_TIMEOUT: Duration = Duration::from_secs(2);

/// What the server's environment changes to; `prepared` is whether the shell gave a `PATH`
/// (`shellEnvironmentPrepared`, which spares the server its own probe).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Patch {
    pub vars: Vec<(String, String)>,
    pub prepared: bool,
}

impl Patch {
    fn var(&self, name: &str) -> Option<&str> {
        self.vars.iter().find(|(key, _)| key == name).map(|(_, value)| value.as_str())
    }
}

static PROBED: tokio::sync::OnceCell<Patch> = tokio::sync::OnceCell::const_new();

/// The server's environment, probed once per run like Electron's startup patch of
/// `process.env`. Takes up to a few seconds the first time; run it off the UI thread.
pub async fn probe() -> &'static Patch {
    PROBED.get_or_init(probe_login_shell).await
}

/// The `PATH` that programs the app starts are found on: the probed one, else the app's own.
pub fn search_path() -> Option<std::ffi::OsString> {
    PROBED.get().and_then(|patch| patch.var("PATH")).map(Into::into).or_else(|| std::env::var_os("PATH"))
}

/// Probes the login shell (then `launchctl` on macOS) and works out the server's environment.
async fn probe_login_shell() -> Patch {
    if cfg!(windows) {
        return Patch::default();
    }
    let inherited = |name: &str| std::env::var(name).ok();
    let mut shell = HashMap::new();
    for candidate in shell_candidates(std::env::var("SHELL").ok().as_deref()) {
        shell = read_login_shell(&candidate).await;
        if shell.get("PATH").is_some_and(|path| !path.trim().is_empty()) {
            break;
        }
        log::warn!("could not read PATH from the login shell {candidate}");
    }
    let launchctl = if cfg!(target_os = "macos") && !shell.contains_key("PATH") {
        run(tokio::process::Command::new("/bin/launchctl").args(["getenv", "PATH"]), LAUNCHCTL_TIMEOUT).await
    } else {
        None
    };
    let runtime_bus = linux_session_bus();
    patch(&shell, launchctl.as_deref().map(str::trim), &inherited, runtime_bus.as_deref())
}

/// `listLoginShellCandidates`: `$SHELL`, then the platform's default shell.
fn shell_candidates(shell: Option<&str>) -> Vec<String> {
    let fallback = if cfg!(target_os = "macos") { "/bin/zsh" } else { "/bin/bash" };
    let mut candidates: Vec<String> = Vec::new();
    for candidate in [shell.map(str::trim).filter(|shell| !shell.is_empty()), Some(fallback)].into_iter().flatten() {
        if !candidates.iter().any(|seen| seen == candidate) {
            candidates.push(candidate.to_owned());
        }
    }
    candidates
}

/// `buildEnvironmentCaptureCommand` for POSIX shells: each value between markers.
fn capture_script(names: &[&str]) -> String {
    names
        .iter()
        .map(|name| {
            format!(
                "printf '%s\\n' '__T3CODE_ENV_{name}_START__'; printenv {name} || true; printf '%s\\n' '__T3CODE_ENV_{name}_END__'"
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// `extractEnvironmentValue`.
fn extract(output: &str, name: &str) -> Option<String> {
    let start = format!("__T3CODE_ENV_{name}_START__");
    let end = format!("__T3CODE_ENV_{name}_END__");
    let after = &output[output.find(&start)? + start.len()..];
    let value = &after[..after.find(&end)?];
    let value = value.strip_prefix("\r\n").or_else(|| value.strip_prefix('\n')).unwrap_or(value);
    let value = value.strip_suffix("\r\n").or_else(|| value.strip_suffix('\n')).unwrap_or(value);
    (!value.is_empty()).then(|| value.to_owned())
}

/// `readEnvironmentFromLoginShell`: an interactive login shell, so profiles that set `PATH` run.
async fn read_login_shell(shell: &str) -> HashMap<String, String> {
    let mut command = tokio::process::Command::new(shell);
    command.args(["-ilc", &capture_script(LOGIN_SHELL_ENV_NAMES)]);
    let Some(output) = run(&mut command, LOGIN_SHELL_TIMEOUT).await else { return HashMap::new() };
    LOGIN_SHELL_ENV_NAMES.iter().filter_map(|name| Some(((*name).to_owned(), extract(&output, name)?))).collect()
}

/// Runs `command` with no input and returns its stdout, or `None` when it fails or overruns.
async fn run(command: &mut tokio::process::Command, timeout: Duration) -> Option<String> {
    command.stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null()).kill_on_drop(true);
    let output = tokio::time::timeout(timeout, command.output()).await.ok()?.ok()?;
    output.status.success().then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// `mergePathEntries` with `:`: the preferred entries first, each once.
fn merge_paths(preferred: Option<&str>, inherited: Option<&str>) -> Option<String> {
    let mut merged: Vec<&str> = Vec::new();
    for entry in [preferred, inherited].into_iter().flatten().flat_map(|path| path.split(':')).map(str::trim) {
        if !entry.is_empty() && !merged.contains(&entry) {
            merged.push(entry);
        }
    }
    (!merged.is_empty()).then(|| merged.join(":"))
}

/// The Linux session bus under the user's runtime directory, when the app inherited none.
fn linux_session_bus() -> Option<String> {
    if !cfg!(target_os = "linux") || std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some() {
        return None;
    }
    let runtime = std::env::var("XDG_RUNTIME_DIR").ok()?;
    let bus = std::path::Path::new(&runtime).join("bus");
    bus.exists().then(|| format!("unix:path={}", bus.display()))
}

/// `installPosixEnvironment`'s rules, given the shell's values, `launchctl`'s PATH and what the
/// app inherited.
fn patch(
    shell: &HashMap<String, String>,
    launchctl_path: Option<&str>,
    inherited: &dyn Fn(&str) -> Option<String>,
    session_bus: Option<&str>,
) -> Patch {
    let present = |value: &Option<String>| value.as_deref().is_some_and(|value| !value.trim().is_empty());
    let shell_value = |name: &str| shell.get(name).filter(|value| !value.trim().is_empty());
    let mut vars = Vec::new();
    let preferred = shell_value("PATH").map(String::as_str).or(launchctl_path.filter(|path| !path.is_empty()));
    if let Some(path) = merge_paths(preferred, inherited("PATH").as_deref()) {
        vars.push(("PATH".to_owned(), path));
    }
    for name in SHELL_PREFERRED {
        if let Some(value) = shell_value(name) {
            vars.push(((*name).to_owned(), value.clone()));
        }
    }
    for name in FILL_IN {
        if !present(&inherited(name))
            && let Some(value) = shell_value(name)
        {
            vars.push(((*name).to_owned(), value.clone()));
        }
    }
    if cfg!(target_os = "macos") && LOCALE_NAMES.iter().all(|name| !present(&inherited(name))) {
        let locale: Vec<(String, String)> = LOCALE_NAMES
            .iter()
            .filter_map(|name| Some(((*name).to_owned(), shell_value(name)?.trim().to_owned())))
            .collect();
        if locale.is_empty() {
            vars.push(("LC_CTYPE".to_owned(), FALLBACK_LC_CTYPE.to_owned()));
        }
        vars.extend(locale);
    }
    if let Some(bus) = session_bus
        && !vars.iter().any(|(name, _)| name == "DBUS_SESSION_BUS_ADDRESS")
    {
        vars.push(("DBUS_SESSION_BUS_ADDRESS".to_owned(), bus.to_owned()));
    }
    Patch { vars, prepared: shell_value("PATH").is_some() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_come_back_between_their_markers() {
        let output = "motd\n__T3CODE_ENV_PATH_START__\n/opt/homebrew/bin:/usr/bin\n__T3CODE_ENV_PATH_END__\n\
                      __T3CODE_ENV_LANG_START__\n__T3CODE_ENV_LANG_END__\n";
        assert_eq!(extract(output, "PATH").as_deref(), Some("/opt/homebrew/bin:/usr/bin"));
        assert_eq!(extract(output, "LANG"), None, "an unset variable");
        assert_eq!(extract(output, "HOME"), None);
        assert!(capture_script(&["PATH"]).contains("printenv PATH || true"));
    }

    #[test]
    fn the_shell_path_leads_and_inherited_entries_follow_once() {
        assert_eq!(
            merge_paths(Some("/opt/homebrew/bin:/usr/bin"), Some("/usr/bin:/bin: ")).as_deref(),
            Some("/opt/homebrew/bin:/usr/bin:/bin")
        );
        assert_eq!(merge_paths(None, None), None);
        assert_eq!(shell_candidates(Some(" /bin/fish ")).first().map(String::as_str), Some("/bin/fish"));
    }

    #[test]
    fn the_patch_follows_electrons_rules() {
        let shell: HashMap<String, String> = [
            ("PATH", "/opt/homebrew/bin:/usr/bin"),
            ("SSH_AUTH_SOCK", "/tmp/agent"),
            ("XDG_SESSION_TYPE", "wayland"),
            ("JAVA_HOME", "/jdk"),
        ]
        .into_iter()
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect();
        let inherited = |name: &str| match name {
            "PATH" => Some("/usr/bin:/bin".to_owned()),
            "JAVA_HOME" => Some("/mine".to_owned()),
            "XDG_SESSION_TYPE" => Some("x11".to_owned()),
            _ => None,
        };
        let patch = patch(&shell, None, &inherited, None);
        let get = |name: &str| patch.vars.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str());
        assert!(patch.prepared);
        assert_eq!(get("PATH"), Some("/opt/homebrew/bin:/usr/bin:/bin"));
        assert_eq!(get("SSH_AUTH_SOCK"), Some("/tmp/agent"), "filled in");
        assert_eq!(get("JAVA_HOME"), None, "an inherited value stays");
        assert_eq!(get("XDG_SESSION_TYPE"), Some("wayland"), "the shell's session wins");

        let empty = HashMap::new();
        let fallback = super::patch(&empty, Some("/usr/local/bin"), &inherited, Some("unix:path=/run/user/1/bus"));
        assert!(!fallback.prepared, "launchctl's PATH is not the shell's");
        assert_eq!(fallback.vars[0], ("PATH".to_owned(), "/usr/local/bin:/usr/bin:/bin".to_owned()));
        assert!(
            fallback.vars.contains(&("DBUS_SESSION_BUS_ADDRESS".to_owned(), "unix:path=/run/user/1/bus".to_owned()))
        );
    }
}
