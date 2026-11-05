//! The `t3` command: a launcher in `<T3 home>/bin` that runs this app's server entry as the
//! CLI, and putting it on PATH. `apps/desktop/src/app/DesktopCliShim.ts` and
//! `DesktopCliCommand.ts`; the launcher runs Node with the server entry where Electron runs
//! its own executable as Node.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};

use crate::backend::LaunchConfig;

/// `MARKER`: a launcher without it was not written by T3 Code and is left alone.
pub const MARKER: &str = "Written by T3 Code: runs the desktop app's bundled t3 CLI.";
const MOVED: &str = "T3 Code has moved or been removed. Open the app once to update this command.";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CliState {
    pub supported: bool,
    pub installed_path: Option<PathBuf>,
    pub on_path: bool,
    pub shadowed_by: Option<PathBuf>,
}

impl CliState {
    /// `CliCommandSettingsRow`'s description.
    pub fn description(&self) -> String {
        match (&self.shadowed_by, &self.installed_path) {
            (Some(other), _) => {
                format!("Another t3 at {} runs first in a new terminal. Remove it to use T3 Code's.", other.display())
            }
            (None, None) => "Run T3 Code's CLI as `t3` from any terminal.".into(),
            (None, Some(path)) if self.on_path => {
                format!("Installed at {}. Open a new terminal to use it.", path.display())
            }
            (None, Some(path)) => format!(
                "Installed at {}, which is not on your PATH yet. Add its folder to your PATH to run `t3`.",
                path.display()
            ),
        }
    }
}

/// What the launcher starts.
pub struct Target {
    pub node: PathBuf,
    pub entry: PathBuf,
    pub home: PathBuf,
}

impl Target {
    /// The server this app would start, with Node resolved to a full path so the launcher
    /// works from any shell.
    pub fn resolve() -> Option<Self> {
        Self::from_config(&LaunchConfig::from_env().ok()?)
    }

    pub fn from_config(config: &LaunchConfig) -> Option<Self> {
        let node = if config.node.is_absolute() { config.node.clone() } else { which(&config.node)? };
        let entry = std::fs::canonicalize(&config.server_entry).ok()?;
        Some(Self { node, entry: strip_verbatim(entry), home: config.home.clone() })
    }

    pub fn launcher(&self) -> PathBuf {
        self.home.join("bin").join(if cfg!(windows) { "t3.cmd" } else { "t3" })
    }
}

/// `\\?\C:\…` from `canonicalize` confuses cmd.exe.
fn strip_verbatim(path: PathBuf) -> PathBuf {
    match path.to_str().and_then(|p| p.strip_prefix(r"\\?\")) {
        Some(rest) if !rest.starts_with("UNC") => PathBuf::from(rest),
        _ => path,
    }
}

fn which(program: &Path) -> Option<PathBuf> {
    let extensions: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT").unwrap_or_else(|_| ".EXE;.CMD;.BAT".into()).split(';').map(str::to_owned).collect()
    } else {
        vec![String::new()]
    };
    std::env::split_paths(&std::env::var_os("PATH")?).find_map(|dir| {
        extensions.iter().map(|ext| dir.join(format!("{}{ext}", program.display()))).find(|path| path.is_file())
    })
}

fn shell_word(value: &str) -> String {
    format!("'{}'", value.replace('\'', r#"'"'"'"#))
}

/// cmd.exe expands `%` even inside quotes; Windows paths cannot contain `"`.
fn cmd_text(value: &str) -> String {
    value.replace('%', "%%")
}

/// `renderCliShim`: this install's home and paths are embedded so `sudo`, which clears the
/// environment, runs the same install.
pub fn render(target: &Target, launcher: &Path, windows: bool) -> String {
    let (node, entry, launcher, home) = (
        target.node.to_string_lossy(),
        target.entry.to_string_lossy(),
        launcher.to_string_lossy(),
        target.home.to_string_lossy(),
    );
    if windows {
        let utf8 = [&node, &entry, &launcher, &home].iter().any(|value| !value.is_ascii());
        let restore: &[&str] = if utf8 { &["chcp %t3_codepage% >nul"] } else { &[] };
        let mut lines = vec![
            "@echo off".to_owned(),
            format!("rem {MARKER}"),
            // Delayed expansion would rewrite `!name!` in paths and arguments.
            "setlocal EnableExtensions DisableDelayedExpansion".into(),
        ];
        if utf8 {
            // Ctrl-C skips codepage restoration, so switch only for non-ASCII install paths.
            lines.push(r#"for /f "tokens=2 delims=:." %%c in ('chcp') do set "t3_codepage=%%c""#.into());
            lines.push("chcp 65001 >nul".into());
        }
        lines.push(format!(r#"set "T3CODE_CLI_PATH={}""#, cmd_text(&launcher)));
        lines.push(format!(r#"if not defined T3CODE_HOME set "T3CODE_HOME={}""#, cmd_text(&home)));
        // A goto, not a parenthesized block: "Program Files (x86)" would close the block early.
        lines.push(format!(r#"if exist "{}" goto run"#, cmd_text(&node)));
        lines.extend(restore.iter().map(|line| (*line).to_owned()));
        lines.push(format!("echo {MOVED} 1>&2"));
        lines.push("exit /b 127".into());
        lines.push(":run".into());
        lines.push(format!(r#""{}" "{}" %*"#, cmd_text(&node), cmd_text(&entry)));
        lines.push(r#"set "t3_exit=%ERRORLEVEL%""#.into());
        lines.extend(restore.iter().map(|line| (*line).to_owned()));
        lines.push("exit /b %t3_exit%".into());
        lines.push(String::new());
        return lines.join("\r\n");
    }
    [
        "#!/bin/sh".to_owned(),
        format!("# {MARKER}"),
        format!("export T3CODE_CLI_PATH={}", shell_word(&launcher)),
        format!("home={}", shell_word(&home)),
        r#"export T3CODE_HOME="${T3CODE_HOME:-$home}""#.into(),
        format!("app={}", shell_word(&node)),
        r#"if [ ! -x "$app" ]; then"#.into(),
        format!(r#"  echo "{MOVED}" >&2"#),
        "  exit 127".into(),
        "fi".into(),
        format!(r#"exec "$app" {} "$@""#, shell_word(&entry)),
        String::new(),
    ]
    .join("\n")
}

/// `DesktopCliShim.install`: writes the launcher unless another program owns that path.
/// It lives in the app's own home; PATH is untouched.
pub fn write_launcher(target: &Target) -> Result<PathBuf> {
    let launcher = target.launcher();
    let content = render(target, &launcher, cfg!(windows));
    let existing = std::fs::read_to_string(&launcher).ok();
    if existing.as_deref().is_some_and(|text| !text.contains(MARKER)) {
        bail!("leaving a t3 launcher the app did not write at {}", launcher.display());
    }
    if existing.as_deref() != Some(content.as_str()) {
        let dir = launcher.parent().context("launcher directory")?;
        std::fs::create_dir_all(dir)?;
        let staging = launcher.with_extension(format!("{}.tmp", std::process::id()));
        std::fs::write(&staging, &content)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o755))?;
        }
        std::fs::rename(&staging, &launcher)?;
        log::info!("installed t3 launcher at {}", launcher.display());
    }
    Ok(launcher)
}

#[cfg(any(windows, test))]
fn same_windows_path(left: &str, right: &str) -> bool {
    let trim = |value: &str| value.trim_end_matches(['\\', '/']).to_lowercase();
    trim(left) == trim(right)
}

#[cfg(any(windows, test))]
fn path_entries(value: &str, separator: char) -> Vec<&str> {
    value.split(separator).filter(|entry| !entry.is_empty()).collect()
}

/// The user PATH with `dir` appended, or `None` when it is already there.
#[cfg(any(windows, test))]
pub fn with_path_entry(user_path: &str, dir: &str) -> Option<String> {
    let entries = path_entries(user_path, ';');
    (!entries.iter().any(|entry| same_windows_path(entry, dir))).then(|| [entries, vec![dir]].concat().join(";"))
}

/// The user PATH without `dir`, or `None` when it was not there.
#[cfg(any(windows, test))]
pub fn without_path_entry(user_path: &str, dir: &str) -> Option<String> {
    let entries = path_entries(user_path, ';');
    let kept: Vec<&str> = entries.iter().copied().filter(|entry| !same_windows_path(entry, dir)).collect();
    (kept.len() != entries.len()).then(|| kept.join(";"))
}

#[cfg(not(windows))]
fn is_our_launcher(path: &Path) -> bool {
    // The launcher is a few KB; never read a large binary another `t3` names.
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() <= 16_384)
        && std::fs::read_to_string(path).is_ok_and(|text| text.contains(MARKER))
}

#[cfg(windows)]
mod platform {
    use std::process::Command;

    use anyhow::{Result, bail};
    use base64::Engine as _;

    /// `WINDOWS_USER_PATH_SCRIPT`: keeps REG_EXPAND_SZ and `%VAR%`s, tells Explorer with
    /// WM_SETTINGCHANGE, and a failed read exits nonzero so PATH is never replaced wholesale.
    const USER_PATH_SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
if ($env:T3_SET -eq '1') {
  $key = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey('Environment')
  $kind = [Microsoft.Win32.RegistryValueKind]::ExpandString
  if ($key.GetValueNames() -contains 'Path' -and $key.GetValueKind('Path') -eq 'String') {
    $kind = [Microsoft.Win32.RegistryValueKind]::String
  }
  $key.SetValue('Path', $env:T3_PATH, $kind)
  Add-Type -Namespace T3 -Name Env -MemberDefinition '[DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern System.IntPtr SendMessageTimeout(System.IntPtr hWnd, uint Msg, System.UIntPtr wParam, string lParam, uint fuFlags, uint uTimeout, out System.UIntPtr lpdwResult);'
  $result = [System.UIntPtr]::Zero
  [void][T3.Env]::SendMessageTimeout([System.IntPtr]0xffff, 0x1A, [System.UIntPtr]::Zero, 'Environment', 2, 5000, [ref]$result)
} else {
  $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment')
  $value = if ($key) { $key.GetValue('Path', '', 'DoNotExpandEnvironmentNames') } else { '' }
  [Console]::Out.Write([Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes([string]$value)))
}
"#;

    fn powershell(set: Option<&str>) -> Result<String> {
        let root = std::env::var("SystemRoot").or_else(|_| std::env::var("WINDIR"));
        let exe = match root {
            Ok(root) => format!(r"{root}\System32\WindowsPowerShell\v1.0\powershell.exe"),
            Err(_) => "powershell.exe".into(),
        };
        let mut command = Command::new(exe);
        command.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", USER_PATH_SCRIPT]);
        if let Some(value) = set {
            command.env("T3_SET", "1").env("T3_PATH", value);
        }
        let output = command.stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null()).output()?;
        if !output.status.success() {
            bail!("powershell exited with {}", output.status);
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    pub fn read_user_path() -> Result<String> {
        let encoded =
            powershell(None).map_err(|_| anyhow::anyhow!("Could not read your PATH, so it was left unchanged."))?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded.trim())
            .map_err(|_| anyhow::anyhow!("Could not read your PATH, so it was left unchanged."))?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    pub fn write_user_path(value: &str) -> Result<()> {
        powershell(Some(value)).map(|_| ()).map_err(|_| anyhow::anyhow!("Could not update your PATH."))
    }
}

/// `ownedPathMarker`: records that the PATH entry is ours to remove.
#[cfg(windows)]
fn owned_path_marker(target: &Target) -> PathBuf {
    target.home.join("userdata").join("cli-command-path-entry")
}

#[cfg(not(windows))]
fn unix_candidates() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let mut dirs = Vec::new();
    if cfg!(target_os = "macos") {
        dirs.extend([PathBuf::from("/opt/homebrew/bin"), PathBuf::from("/usr/local/bin")]);
    }
    dirs.extend([home.join(".local/bin"), home.join("bin")]);
    dirs
}

/// `DesktopCliCommand.state`. Blocking: run it off the UI thread.
pub fn state() -> CliState {
    let Some(target) = Target::resolve() else { return CliState::default() };
    let launcher = target.launcher();
    #[cfg(windows)]
    {
        let bin = launcher.parent().map(|dir| dir.to_string_lossy().into_owned()).unwrap_or_default();
        let installed = owned_path_marker(&target).exists()
            && platform::read_user_path().is_ok_and(|path| with_path_entry(&path, &bin).is_none());
        let installed_path = installed.then_some(launcher);
        CliState { supported: true, on_path: installed_path.is_some(), installed_path, shadowed_by: None }
    }
    #[cfg(not(windows))]
    {
        let installed_path = unix_candidates()
            .into_iter()
            .map(|dir| dir.join("t3"))
            .find(|link| link.is_symlink() && is_our_launcher(link));
        let first = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .map(|dir| dir.join("t3"))
            .find(|candidate| candidate.is_file());
        let ours = |path: &PathBuf| {
            is_our_launcher(path) || std::fs::canonicalize(path).ok() == std::fs::canonicalize(&launcher).ok()
        };
        let shadowed_by = first.clone().filter(|path| !ours(path));
        CliState { supported: true, on_path: first.as_ref().is_some_and(ours), installed_path, shadowed_by }
    }
}

/// `DesktopCliCommand.install`: the launcher, then its folder on the user PATH (Windows) or a
/// `t3` link in a user bin folder (elsewhere). Blocking.
pub fn install() -> Result<CliState> {
    let target = Target::resolve().context("The t3 command needs this app's server and Node.")?;
    let launcher = write_launcher(&target)
        .map_err(|_| anyhow::anyhow!("Could not set up the t3 launcher at {}.", target.launcher().display()))?;
    #[cfg(windows)]
    {
        let bin = launcher.parent().map(|dir| dir.to_string_lossy().into_owned()).unwrap_or_default();
        if let Some(next) = with_path_entry(&platform::read_user_path()?, &bin) {
            platform::write_user_path(&next)?;
            std::fs::write(owned_path_marker(&target), format!("{bin}\n"))
                .map_err(|_| anyhow::anyhow!("Added t3 to your PATH but could not record it."))?;
        }
    }
    #[cfg(not(windows))]
    {
        let current = state();
        if let Some(existing) = &current.installed_path {
            if std::fs::read_link(existing).ok().as_deref() == Some(launcher.as_path()) {
                return Ok(current);
            }
            std::fs::remove_file(existing).map_err(|_| anyhow::anyhow!("Could not replace {}.", existing.display()))?;
        }
        if let Some(other) = state().shadowed_by {
            bail!(
                "Another t3 at {} runs first in a new terminal. Remove it, or run the launcher directly at {}.",
                other.display(),
                launcher.display()
            );
        }
        let on_path: Vec<PathBuf> = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).collect();
        let (first, rest): (Vec<_>, Vec<_>) = unix_candidates().into_iter().partition(|dir| on_path.contains(dir));
        let linked = first.into_iter().chain(rest).any(|dir| {
            std::fs::create_dir_all(&dir).is_ok() && std::os::unix::fs::symlink(&launcher, dir.join("t3")).is_ok()
        });
        if !linked {
            bail!(
                "Another t3 command is already installed, or no folder on your PATH is writable. Run the launcher directly at {}.",
                launcher.display()
            );
        }
    }
    Ok(state())
}

/// `DesktopCliCommand.uninstall`: removes only what `install` added. Blocking.
pub fn uninstall() -> Result<CliState> {
    let target = Target::resolve().context("The t3 command needs this app's server and Node.")?;
    #[cfg(windows)]
    {
        let marker = owned_path_marker(&target);
        if marker.exists() {
            let bin = target.launcher().parent().map(|dir| dir.to_string_lossy().into_owned()).unwrap_or_default();
            if let Some(kept) = without_path_entry(&platform::read_user_path()?, &bin) {
                platform::write_user_path(&kept)?;
            }
            let _ = std::fs::remove_file(marker);
        }
    }
    #[cfg(not(windows))]
    {
        let _ = &target;
        for link in unix_candidates().into_iter().map(|dir| dir.join("t3")) {
            if link.is_symlink() && is_our_launcher(&link) {
                std::fs::remove_file(&link).map_err(|_| anyhow::anyhow!("Could not remove {}.", link.display()))?;
            }
        }
    }
    Ok(state())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> Target {
        Target {
            node: r"C:\Program Files (x86)\nodejs\node.exe".into(),
            entry: r"H:\t3\apps\server\dist\bin.mjs".into(),
            home: r"C:\Users\me\100%\.t3-gpui".into(),
        }
    }

    #[test]
    fn windows_launchers_quote_paths_for_cmd() {
        let text = render(&target(), Path::new(r"C:\Users\me\100%\.t3-gpui\bin\t3.cmd"), true);
        assert!(text.starts_with("@echo off\r\nrem Written by T3 Code"));
        assert!(text.contains(r#"if not defined T3CODE_HOME set "T3CODE_HOME=C:\Users\me\100%%\.t3-gpui""#));
        assert!(text.contains(r#"if exist "C:\Program Files (x86)\nodejs\node.exe" goto run"#));
        assert!(text.contains(r#""C:\Program Files (x86)\nodejs\node.exe" "H:\t3\apps\server\dist\bin.mjs" %*"#));
        // ASCII paths keep the console's code page.
        assert!(!text.contains("chcp"));
        let mut unicode = target();
        unicode.home = r"C:\Users\zoë\.t3-gpui".into();
        assert!(render(&unicode, Path::new("t3.cmd"), true).contains("chcp 65001 >nul"));
    }

    #[test]
    fn unix_launchers_escape_single_quotes() {
        let target =
            Target { node: "/usr/bin/node".into(), entry: "/opt/it's/bin.mjs".into(), home: "/home/me/.t3".into() };
        let text = render(&target, Path::new("/home/me/.t3/bin/t3"), false);
        assert!(text.contains(r#"exec "$app" '/opt/it'"'"'s/bin.mjs' "$@""#));
        assert!(text.contains("export T3CODE_HOME=\"${T3CODE_HOME:-$home}\""));
    }

    #[test]
    fn path_edits_compare_windows_folders_loosely() {
        assert_eq!(with_path_entry(r"C:\a;C:\B\", r"c:\b"), None);
        assert_eq!(with_path_entry(r"C:\a;;", r"C:\b").as_deref(), Some(r"C:\a;C:\b"));
        assert_eq!(without_path_entry(r"C:\a;C:\B\;C:\c", r"c:\b").as_deref(), Some(r"C:\a;C:\c"));
        assert_eq!(without_path_entry(r"C:\a", r"c:\b"), None);
    }

    #[test]
    fn descriptions_match_the_settings_row() {
        let mut state = CliState { supported: true, ..Default::default() };
        assert_eq!(state.description(), "Run T3 Code's CLI as `t3` from any terminal.");
        state.installed_path = Some("/x/t3".into());
        assert_eq!(
            state.description(),
            "Installed at /x/t3, which is not on your PATH yet. Add its folder to your PATH to run `t3`."
        );
        state.on_path = true;
        assert_eq!(state.description(), "Installed at /x/t3. Open a new terminal to use it.");
    }
}
