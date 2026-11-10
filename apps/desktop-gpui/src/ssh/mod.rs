//! SSH environments, as the desktop app runs them (`packages/ssh`, `DesktopSshEnvironment.ts`):
//! the system `ssh` resolves a host, starts or reuses T3 Code there, forwards a loopback port to
//! it and mints a pairing credential. Every call spawns processes, so it runs on the tokio runtime.

pub mod config;
mod scripts;
mod tunnel;

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::Duration;

use futures::channel::oneshot;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt as _;
use tokio::process::Command;

pub use tunnel::{app_version, disconnect, ensure, shutdown};

/// `DesktopSshEnvironmentTarget`: what `ssh` is told to connect to.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SshTarget {
    pub alias: String,
    pub hostname: String,
    pub username: Option<String>,
    pub port: Option<u16>,
}

impl SshTarget {
    /// A target for `alias` before ssh has resolved it.
    pub fn alias(alias: &str) -> Self {
        Self { alias: alias.to_owned(), hostname: alias.to_owned(), username: None, port: None }
    }

    /// `formatDesktopSshTarget`: `user@host:port`.
    pub fn display(&self) -> String {
        let authority = match &self.username {
            Some(user) => format!("{user}@{}", self.hostname),
            None => self.hostname.clone(),
        };
        match self.port {
            Some(port) => format!("{authority}:{port}"),
            None => authority,
        }
    }

    /// `sshTargetKey`: the JSON array `[alias, hostname, username, port]` routes are keyed by.
    pub fn key(&self) -> String {
        serde_json::json!([self.alias, self.hostname, self.username, self.port]).to_string()
    }

    /// `targetConnectionKey`: one tunnel and remote state directory per key.
    fn connection_key(&self) -> String {
        let port = self.port.map(|port| port.to_string()).unwrap_or_default();
        format!("{}\0{}\0{}\0{port}", self.alias, self.hostname, self.username.as_deref().unwrap_or_default())
    }

    /// `buildSshHostSpec`: `user@alias`, falling back to the hostname.
    fn host_spec(&self) -> Result<String, SshError> {
        let destination = match self.alias.trim() {
            "" => self.hostname.trim(),
            alias => alias,
        };
        if destination.is_empty() {
            return Err(SshError::new("SSH target is missing its alias/hostname."));
        }
        Ok(match &self.username {
            Some(user) => format!("{user}@{destination}"),
            None => destination.to_owned(),
        })
    }
}

/// Why an SSH step failed: the message the dialog and the connection status show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SshError {
    pub message: String,
}

impl SshError {
    fn new(message: impl Into<String>) -> Self {
        Self { message: message.into() }
    }

    /// `isSshAuthFailure`: the server refused every offered credential.
    fn is_auth_failure(&self) -> bool {
        let message = self.message.to_lowercase();
        let refused = message.match_indices("permission denied (").any(|(at, needle)| {
            let rest = &message[at + needle.len()..];
            ["publickey", "password", "keyboard-interactive", "hostbased", "gssapi-with-mic"]
                .iter()
                .any(|method| rest.starts_with(method))
                && rest.contains(')')
        });
        refused || message.contains("authentication failed") || message.contains("too many authentication failures")
    }

    /// `isSshAgentSigningFailure`: a local agent problem the account password cannot fix.
    fn is_agent_signing_failure(&self) -> bool {
        const FAILED: &str = "sign_and_send_pubkey: signing failed";
        self.message.match_indices(FAILED).any(|(at, _)| {
            let rest = &self.message[at + FAILED.len()..];
            rest.starts_with(": agent refused operation")
                || rest.starts_with(": communication with agent failed")
                || rest
                    .strip_prefix(" for ")
                    .is_some_and(|rest| rest.lines().next().is_some_and(|l| l.contains(" from agent:")))
        })
    }
}

impl std::fmt::Display for SshError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// `parseManualDesktopSshTarget`: the dialog's host (which may carry `user@` and `:port`),
/// username and port fields.
pub fn parse_manual_target(host: &str, username: &str, port: &str) -> Result<SshTarget, &'static str> {
    const REQUIRED: &str = "SSH host or alias is required.";
    let raw = host.trim();
    if raw.is_empty() {
        return Err(REQUIRED);
    }
    let mut hostname = raw.to_owned();
    let mut username = Some(username.trim().to_owned()).filter(|u| !u.is_empty());
    let mut parsed_port: Option<i64> = None;
    if let Some(at) = hostname.rfind('@').filter(|&at| at > 0) {
        let inline = hostname[..at].trim().to_owned();
        hostname = hostname[at + 1..].trim().to_owned();
        if username.is_none() && !inline.is_empty() {
            username = Some(inline);
        }
    }
    let digits = |text: &str| !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
    let bracketed = hostname.strip_prefix('[').and_then(|rest| {
        let (inner, after) = rest.split_once(']')?;
        match after.strip_prefix(':') {
            None if after.is_empty() => Some((inner.trim().to_owned(), None)),
            Some(port) if digits(port) => Some((inner.trim().to_owned(), port.parse().ok())),
            _ => None,
        }
    });
    if let Some((inner, port)) = bracketed {
        hostname = inner;
        parsed_port = port;
    } else if let [name, port] = hostname.split(':').collect::<Vec<_>>()[..]
        && digits(port)
    {
        parsed_port = port.parse().ok();
        hostname = name.trim().to_owned();
    }
    let raw_port = port.trim();
    if !raw_port.is_empty() {
        // `Number.parseInt`: leading digits, so "22x" is 22 and "x" is invalid.
        let leading: String = raw_port.chars().take_while(char::is_ascii_digit).collect();
        parsed_port = Some(leading.parse().unwrap_or(-1));
    }
    if hostname.is_empty() {
        return Err(REQUIRED);
    }
    let port = match parsed_port {
        None => None,
        Some(port) => Some(u16::try_from(port).ok().filter(|&p| p > 0).ok_or("SSH port must be between 1 and 65535.")?),
    };
    Ok(SshTarget { alias: hostname.clone(), hostname, username, port })
}

/// `parseSshResolveOutput`: the `ssh -G` settings for `alias`.
fn parse_resolve_output(alias: &str, stdout: &str) -> SshTarget {
    let mut values = std::collections::HashMap::new();
    for line in stdout.lines() {
        let mut words = line.split_whitespace();
        let (Some(key), rest) = (words.next(), words.collect::<Vec<_>>().join(" ")) else { continue };
        if !rest.is_empty() {
            values.entry(key.to_owned()).or_insert(rest);
        }
    }
    SshTarget {
        alias: alias.to_owned(),
        hostname: values.remove("hostname").filter(|h| !h.is_empty()).unwrap_or_else(|| alias.to_owned()),
        username: values.remove("user").filter(|u| !u.is_empty()),
        port: values.get("port").and_then(|port| port.parse().ok()),
    }
}

/// `resolveSshTarget`: asks `ssh -G` what `alias` means; falls back to the alias itself.
pub async fn resolve_target(alias: &str) -> Result<SshTarget, SshError> {
    let alias = alias.trim();
    if alias.is_empty() {
        return Err(SshError::new("SSH host alias is required."));
    }
    let options = RunOptions { pre_host_args: vec!["-G".into()], ..RunOptions::default() };
    Ok(match run(&SshTarget::alias(alias), &options, &Auth::Batch).await {
        Ok(output) => parse_resolve_output(alias, &output.stdout),
        Err(error) => {
            log::debug!("ssh -G {alias} failed, using the alias as is: {error}");
            SshTarget::alias(alias)
        }
    })
}

/// How an ssh process authenticates.
#[derive(Clone, Debug)]
enum Auth {
    /// Keys and agent only (`BatchMode=yes`).
    Batch,
    /// A password or verification code from the in-app prompt, handed over through
    /// `SSH_ASKPASS`; `None` lets ssh ask with no answer ready, which fails loudly.
    Secret(Option<String>),
}

#[derive(Default)]
struct RunOptions {
    pre_host_args: Vec<String>,
    remote_args: Vec<String>,
    stdin: Option<String>,
    timeout: Option<Duration>,
}

struct Output {
    stdout: String,
}

const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_ERROR_OUTPUT: usize = 4_000;

/// `ssh` is a real executable everywhere (`ssh.exe` on Windows), so it is spawned directly.
fn ssh_command() -> Command {
    let mut command = Command::new(if cfg!(windows) { "ssh.exe" } else { "ssh" });
    command.kill_on_drop(true);
    // A console program started from a GUI app would open a console window.
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    command
}

/// `baseSshArgs`.
fn base_args(target: &SshTarget, batch: bool) -> Vec<String> {
    let mut args = vec![
        "-o".into(),
        format!("BatchMode={}", if batch { "yes" } else { "no" }),
        "-o".into(),
        "ConnectTimeout=10".into(),
    ];
    if let Some(port) = target.port {
        args.extend(["-p".into(), port.to_string()]);
    }
    args
}

/// Applies `auth` to an ssh command: `buildSshChildEnvironment`.
fn apply_auth(command: &mut Command, auth: &Auth) -> Result<(), SshError> {
    let Auth::Secret(secret) = auth else { return Ok(()) };
    let askpass = askpass_helper()
        .map_err(|error| SshError::new(format!("Failed to prepare SSH authentication helpers. ({error})")))?;
    command.env("SSH_ASKPASS", askpass).env("SSH_ASKPASS_REQUIRE", "force");
    if let Some(secret) = secret {
        command.env("T3_SSH_AUTH_SECRET", secret);
    }
    // OpenSSH only consults SSH_ASKPASS with a display set, outside Windows.
    if cfg!(not(windows)) && std::env::var_os("DISPLAY").is_none() {
        command.env("DISPLAY", "t3code");
    }
    Ok(())
}

/// `runSshCommand`: one ssh invocation with a script on stdin, its output, or why it failed.
async fn run(target: &SshTarget, options: &RunOptions, auth: &Auth) -> Result<Output, SshError> {
    let host_spec = target.host_spec()?;
    let mut args = base_args(target, matches!(auth, Auth::Batch));
    // Cold installs need a long command budget; a lost connection must fail sooner.
    args.extend(["-o", "ServerAliveInterval=15", "-o", "ServerAliveCountMax=3"].map(String::from));
    args.extend(options.pre_host_args.iter().cloned());
    args.extend(["--".into(), host_spec.clone()]);
    args.extend(options.remote_args.iter().cloned());
    let mut command = ssh_command();
    apply_auth(&mut command, auth)?;
    command.args(&args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| SshError::new(format!("Failed to spawn SSH command for {host_spec}. ({error})")))?;
    let mut stdin = child.stdin.take();
    let script = options.stdin.clone();
    let feed = async move {
        if let (Some(stdin), Some(script)) = (stdin.as_mut(), script) {
            // A remote that exits early closes the pipe; its exit status says why.
            drop(stdin.write_all(script.as_bytes()).await);
        }
        drop(stdin);
    };
    let timeout = options.timeout.unwrap_or(DEFAULT_COMMAND_TIMEOUT);
    let finished = tokio::time::timeout(timeout, async {
        let ((), output) = tokio::join!(feed, child.wait_with_output());
        output
    })
    .await;
    let output = match finished {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => return Err(SshError::new(format!("Failed to run SSH command for {host_spec}. ({error})"))),
        Err(_) => return Err(SshError::new(format!("SSH command timed out after {}ms.", timeout.as_millis()))),
    };
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    if output.status.success() {
        return Ok(Output { stdout });
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let code = output.status.code().map_or_else(|| "signal".to_owned(), |code| code.to_string());
    log::warn!("ssh {host_spec} failed (exit {code}): {}", stderr.trim());
    let message = match (stderr.trim(), redact(stdout.trim())) {
        ("", stdout) if stdout.is_empty() => format!("SSH command failed for {host_spec} (exit {code})."),
        ("", stdout) => stdout,
        (stderr, _) => stderr.to_owned(),
    };
    Err(SshError::new(message))
}

/// `redactSshErrorOutput`: hides credential values and keeps the start of long output.
fn redact(output: &str) -> String {
    const KEYS: [&str; 5] = ["access_token", "bearertoken", "credential", "pairingtoken", "token"];
    let mut redacted = String::with_capacity(output.len());
    let mut rest = output;
    while let Some(start) = rest.find('"') {
        let (before, quoted) = rest.split_at(start);
        redacted.push_str(before);
        let Some(end) = quoted[1..].find('"').map(|end| end + 2) else {
            rest = quoted;
            break;
        };
        let (key, after) = quoted.split_at(end);
        redacted.push_str(key);
        rest = after;
        let name = key.trim_matches('"').to_lowercase();
        let value = after.trim_start().strip_prefix(':').map(str::trim_start);
        if let Some(value) = value.filter(|_| KEYS.contains(&name.as_str()))
            && let Some(inner) = value.strip_prefix('"')
            && let Some(close) = inner.find('"')
        {
            redacted.push_str(&after[..after.len() - value.len()]);
            redacted.push_str("\"[redacted]\"");
            rest = &inner[close + 1..];
        }
    }
    redacted.push_str(rest);
    match redacted.char_indices().nth(MAX_ERROR_OUTPUT) {
        Some((cut, _)) => format!("{}\n[truncated]", &redacted[..cut]),
        None => redacted,
    }
}

/// The askpass helper OpenSSH runs for the in-app prompt's answer; written once per run into a
/// private temp directory. It prints `T3_SSH_AUTH_SECRET` and never shows a dialog itself.
fn askpass_helper() -> std::io::Result<PathBuf> {
    static HELPER: OnceLock<Result<PathBuf, String>> = OnceLock::new();
    HELPER
        .get_or_init(|| write_askpass_helper().map_err(|error| error.to_string()))
        .clone()
        .map_err(std::io::Error::other)
}

fn write_askpass_helper() -> std::io::Result<PathBuf> {
    let suffix = getrandom::u64().map_err(|error| std::io::Error::other(error.to_string()))?;
    let directory = std::env::temp_dir().join(format!("t3code-ssh-runtime-{suffix:016x}")).join("t3code-ssh-askpass");
    std::fs::create_dir_all(&directory)?;
    #[cfg(windows)]
    {
        std::fs::write(directory.join("ssh-askpass.ps1"), scripts::ASKPASS_WINDOWS_SCRIPT)?;
        let launcher = directory.join("ssh-askpass.cmd");
        std::fs::write(&launcher, scripts::ASKPASS_WINDOWS_LAUNCHER)?;
        Ok(launcher)
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
        let launcher = directory.join("ssh-askpass.sh");
        std::fs::write(&launcher, scripts::ASKPASS_POSIX_SCRIPT)?;
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o700))?;
        Ok(launcher)
    }
}

/// `DesktopSshPasswordPromptRequest`: the in-app prompt for a password or verification code.
pub struct PasswordPrompt {
    pub destination: String,
    pub username: Option<String>,
    pub prompt: String,
    /// When the prompt stops waiting, in epoch ms.
    pub expires_at_ms: i64,
    /// The answer, or `None` when cancelled.
    pub reply: oneshot::Sender<Option<String>>,
}

/// `DEFAULT_SSH_PASSWORD_PROMPT_TIMEOUT_MS`.
const PROMPT_TIMEOUT: Duration = Duration::from_secs(180);

fn prompt_channel() -> &'static (async_channel::Sender<PasswordPrompt>, async_channel::Receiver<PasswordPrompt>) {
    static CHANNEL: OnceLock<(async_channel::Sender<PasswordPrompt>, async_channel::Receiver<PasswordPrompt>)> =
        OnceLock::new();
    CHANNEL.get_or_init(async_channel::unbounded)
}

/// The prompts the app shows; any window may take one.
pub fn password_prompts() -> async_channel::Receiver<PasswordPrompt> {
    prompt_channel().1.clone()
}

/// Hands back a prompt a closing window took, for another window to show.
pub fn requeue(prompt: PasswordPrompt) {
    drop(prompt_channel().0.try_send(prompt));
}

/// Asks the app for the password for `target`; `None` when the user cancelled.
async fn request_password(target: &SshTarget, attempt: u32) -> Result<String, SshError> {
    let host_spec = target.host_spec()?;
    let destination = match target.alias.trim() {
        "" => target.hostname.trim().to_owned(),
        alias => alias.to_owned(),
    };
    let (reply, answer) = oneshot::channel();
    let expires_at_ms =
        crate::timestamp::now_ms().saturating_add(i64::try_from(PROMPT_TIMEOUT.as_millis()).unwrap_or(i64::MAX));
    let prompt = PasswordPrompt {
        destination: destination.clone(),
        username: target.username.clone(),
        prompt: format!("Enter the SSH password or verification code for {host_spec}."),
        expires_at_ms,
        reply,
    };
    log::info!("ssh password prompt {attempt} for {host_spec}");
    if prompt_channel().0.send(prompt).await.is_err() {
        return Err(SshError::new("T3 Code window is not available for SSH authentication."));
    }
    match tokio::time::timeout(PROMPT_TIMEOUT, answer).await {
        Ok(Ok(Some(password))) => Ok(password),
        Ok(Ok(None)) => Err(SshError::new(format!("SSH authentication cancelled for {host_spec}."))),
        Ok(Err(_)) => Err(SshError::new("SSH authentication was cancelled because the app window closed.")),
        Err(_) => Err(SshError::new(format!("SSH authentication timed out for {destination}."))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(alias: &str, hostname: &str, username: Option<&str>, port: Option<u16>) -> SshTarget {
        SshTarget { alias: alias.into(), hostname: hostname.into(), username: username.map(Into::into), port }
    }

    #[test]
    fn manual_targets_take_inline_users_and_ports() {
        assert_eq!(parse_manual_target(" devbox ", "", ""), Ok(target("devbox", "devbox", None, None)));
        assert_eq!(parse_manual_target("me@box:2222", "", ""), Ok(target("box", "box", Some("me"), Some(2222))));
        // The username field wins over an inline one; the port field over an inline port.
        assert_eq!(parse_manual_target("me@box:2222", "root", "22"), Ok(target("box", "box", Some("root"), Some(22))));
        assert_eq!(parse_manual_target("[::1]:2200", "", ""), Ok(target("::1", "::1", None, Some(2200))));
        assert_eq!(parse_manual_target("fe80::1", "", ""), Ok(target("fe80::1", "fe80::1", None, None)));
        assert_eq!(parse_manual_target("  ", "", ""), Err("SSH host or alias is required."));
        assert_eq!(parse_manual_target("me@", "", ""), Err("SSH host or alias is required."));
        assert_eq!(parse_manual_target("box", "", "70000"), Err("SSH port must be between 1 and 65535."));
        assert_eq!(parse_manual_target("box", "", "0"), Err("SSH port must be between 1 and 65535."));
        assert_eq!(parse_manual_target("box", "", "x"), Err("SSH port must be between 1 and 65535."));
    }

    #[test]
    fn ssh_g_output_resolves_the_first_values() {
        let out = "user me\nhostname 10.1.2.3\nport 2022\nhostname later\nidentityfile ~/.ssh/id\n";
        assert_eq!(parse_resolve_output("box", out), target("box", "10.1.2.3", Some("me"), Some(2022)));
        assert_eq!(parse_resolve_output("box", ""), target("box", "box", None, None));
    }

    #[test]
    fn targets_display_and_key_like_the_web() {
        let full = target("box", "10.0.0.1", Some("me"), Some(22));
        assert_eq!(full.display(), "me@10.0.0.1:22");
        assert_eq!(full.key(), r#"["box","10.0.0.1","me",22]"#);
        assert_eq!(target("box", "box", None, None).key(), r#"["box","box",null,null]"#);
        assert_eq!(full.host_spec(), Ok("me@box".into()));
        assert_eq!(target(" ", "h", None, None).host_spec(), Ok("h".into()));
    }

    #[test]
    fn auth_failures_are_told_from_other_errors() {
        let failed = |message: &str| SshError::new(message).is_auth_failure();
        assert!(failed("me@box: Permission denied (publickey,password)."));
        assert!(failed("Received disconnect: Too many authentication failures"));
        assert!(!failed("ssh: connect to host box port 22: Connection refused"));
        assert!(!failed("Permission denied (other)"));
        let agent = |message: &str| SshError::new(message).is_agent_signing_failure();
        assert!(agent("sign_and_send_pubkey: signing failed for ED25519 \"k\" from agent: agent refused operation"));
        assert!(agent("sign_and_send_pubkey: signing failed: agent refused operation"));
        assert!(!agent("sign_and_send_pubkey: signing failed for RSA"));
    }

    #[test]
    fn error_output_hides_credentials() {
        assert_eq!(
            redact(r#"{"credential": "secret", "other":"x", "token":"t"}"#),
            r#"{"credential": "[redacted]", "other":"x", "token":"[redacted]"}"#
        );
        assert!(redact(&"é".repeat(MAX_ERROR_OUTPUT + 5)).ends_with("\n[truncated]"));
    }
}
