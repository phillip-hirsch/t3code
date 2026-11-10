//! `SshEnvironmentManager` (`packages/ssh/src/tunnel.ts`): one loopback forward per target to
//! the T3 server it starts or reuses there, with the in-app password prompt when keys fail.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use futures::FutureExt as _;
use futures::future::BoxFuture;
use serde::Deserialize;
use sha2::Digest as _;
use tokio::io::AsyncReadExt as _;

use super::{
    Auth, RunOptions, SshError, SshTarget, apply_auth, base_args, request_password, resolve_target, run, scripts,
};

const DEFAULT_REMOTE_PORT: u16 = 3773;
const REMOTE_PORT_SCAN_WINDOW: u16 = 200;
const SSH_READY_TIMEOUT: Duration = Duration::from_secs(20);
const SSH_READY_PROBE_TIMEOUT: Duration = Duration::from_secs(1);
const TUNNEL_STDERR_TAIL_CHARS: usize = 16_384;
const REMOTE_READY_TIMEOUT_MS: u64 = 60_000;
const REMOTE_LAUNCH_TIMEOUT: Duration = Duration::from_secs(90);
// Cold archive launches download and extract ~70 MB and may wait on an install lock.
const REMOTE_ARCHIVE_CHECKSUMS_SECONDS: u32 = 30;
const REMOTE_ARCHIVE_DOWNLOAD_SECONDS: u32 = 240;
const REMOTE_ARCHIVE_LOCK_WAIT_SECONDS: u32 = 360;
const REMOTE_ARCHIVE_LAUNCH_TIMEOUT: Duration = Duration::from_mins(15);
const REMOTE_REUSE_READY_TIMEOUT_MS: u64 = 2_000;
/// How long quitting waits for the remote servers this run started to stop.
const QUIT_STOP_WAIT: Duration = Duration::from_secs(2);
/// `CLI_RELEASE_DEFAULT_BASE_URL` in `packages/shared/src/cliRelease.ts`.
const RELEASE_BASE_URL: &str = "https://github.com/pingdotgg/t3code/releases/download";

/// `DesktopSshEnvironmentBootstrap`: the forwarded server and, when asked, a pairing credential.
pub struct Bootstrap {
    pub target: SshTarget,
    /// `http://127.0.0.1:<port>`, no trailing slash.
    pub http_base: String,
    pub pairing_token: Option<String>,
}

/// A running `ssh -N -L` forward.
struct Tunnel {
    target: SshTarget,
    http_base: String,
    child: tokio::process::Child,
    #[cfg(windows)]
    _job: Option<win32job::Job>,
}

impl Tunnel {
    fn close(mut self) {
        drop(self.child.start_kill());
    }
}

#[derive(Default)]
struct Manager {
    tunnels: Mutex<HashMap<String, Tunnel>>,
    /// One lock per target, so a reconnect cannot reuse a server while a stop is pending.
    locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// Passwords from the prompt, kept in memory for the run to reuse for the same target.
    secrets: Mutex<HashMap<String, String>>,
}

static MANAGER: LazyLock<Manager> = LazyLock::new(Manager::default);

fn locked<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Manager {
    fn lock_for(&self, key: &str) -> Arc<tokio::sync::Mutex<()>> {
        locked(&self.locks).entry(key.to_owned()).or_default().clone()
    }

    fn secret(&self, key: &str) -> Option<String> {
        locked(&self.secrets).get(key).cloned()
    }
}

/// What runs `t3` on the remote: the release archive of this app's version, or in a development
/// build a source checkout there (`T3CODE_DEV_REMOTE_T3_SERVER_ENTRY_PATH`), as Electron's
/// `resolveDesktopSshCliRunner` picks.
struct Runner {
    node_script: String,
    archive_version: String,
}

impl Runner {
    fn current() -> Self {
        let node_script = std::env::var("T3CODE_DEV_REMOTE_T3_SERVER_ENTRY_PATH")
            .ok()
            .filter(|path| cfg!(debug_assertions) && !path.trim().is_empty())
            .unwrap_or_default();
        Self { node_script: node_script.trim().to_owned(), archive_version: app_version() }
    }

    fn uses_node(&self) -> bool {
        !self.node_script.is_empty()
    }
}

/// This app's version: release packaging sets `T3CODE_APP_VERSION`; a source build runs the
/// server next to it, whose version names the matching release archive.
pub fn app_version() -> String {
    if let Some(version) = option_env!("T3CODE_APP_VERSION") {
        return version.to_owned();
    }
    let package: serde_json::Value =
        serde_json::from_str(include_str!("../../../server/package.json")).unwrap_or_default();
    package["version"].as_str().unwrap_or_default().to_owned()
}

/// `EXACT_ARCHIVE_VERSION`: one exact `SemVer` version, since it names a directory the runner replaces.
fn exact_version(version: &str) -> bool {
    let (core, pre) = match version.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (version, None),
    };
    let number = |part: &str| {
        !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()) && (part == "0" || !part.starts_with('0'))
    };
    let parts: Vec<_> = core.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|part| number(part))
        && pre.is_none_or(|pre| {
            pre.split('.').all(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
        })
}

fn single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// `applyScriptPlaceholders`, in order.
fn fill(template: &str, replacements: &[(&str, String)]) -> String {
    replacements
        .iter()
        .fold(template.to_owned(), |script, (token, value)| script.replace(&format!("@@{token}@@"), value))
}

/// `buildRemoteNodeEnvScript` without an engine range: this client never pins one, so the
/// check never runs.
fn node_env_script() -> String {
    let script = fill(
        scripts::NODE_ENV_SCRIPT,
        &[("T3_NODE_ENGINE_RANGE", single_quote("")), ("T3_NODE_ENGINE_CHECK_SCRIPT", "process.exit(0);".into())],
    );
    script.trim_end_matches('\n').to_owned()
}

/// `buildRemoteT3RunnerScript`.
fn runner_script(runner: &Runner) -> Result<String, SshError> {
    if runner.node_script.is_empty() && runner.archive_version.is_empty() {
        return Err(SshError::new("A remote t3 runner needs an archive version or a node script path."));
    }
    if !runner.archive_version.is_empty() && !exact_version(&runner.archive_version) {
        return Err(SshError::new(format!(
            "'{}' is not an exact t3 version and cannot name a runtime directory.",
            runner.archive_version
        )));
    }
    let script = fill(
        scripts::RUNNER_SCRIPT,
        &[
            ("T3_NODE_SCRIPT_PATH", single_quote(&runner.node_script)),
            ("T3_ARCHIVE_VERSION", single_quote(&runner.archive_version)),
            ("T3_RELEASE_BASE_URL", single_quote(RELEASE_BASE_URL)),
            ("T3_ARCHIVE_LOCK_WAIT_SECONDS", REMOTE_ARCHIVE_LOCK_WAIT_SECONDS.to_string()),
            ("T3_ARCHIVE_DOWNLOAD_SECONDS", REMOTE_ARCHIVE_DOWNLOAD_SECONDS.to_string()),
            ("T3_ARCHIVE_CHECKSUMS_SECONDS", REMOTE_ARCHIVE_CHECKSUMS_SECONDS.to_string()),
            ("T3_NODE_ENV_SCRIPT", node_env_script()),
        ],
    );
    Ok(script.trim_end_matches('\n').to_owned())
}

fn launch_script(runner: &Runner) -> Result<String, SshError> {
    Ok(fill(
        scripts::LAUNCH_SCRIPT,
        &[
            ("T3_ARCHIVE_MODE", if runner.uses_node() { "0" } else { "1" }.into()),
            ("T3_NODE_ENV_SCRIPT", node_env_script()),
            ("T3_RUNNER_SCRIPT", runner_script(runner)?),
            ("T3_PICK_PORT_SCRIPT", scripts::PICK_PORT_SCRIPT.trim_end_matches('\n').into()),
            ("T3_WAIT_READY_SCRIPT", scripts::WAIT_READY_SCRIPT.trim_end_matches('\n').into()),
            ("T3_DEFAULT_REMOTE_PORT", DEFAULT_REMOTE_PORT.to_string()),
            ("T3_REMOTE_PORT_SCAN_WINDOW", REMOTE_PORT_SCAN_WINDOW.to_string()),
            ("T3_READY_TIMEOUT_MS", REMOTE_READY_TIMEOUT_MS.to_string()),
            ("T3_REUSE_READY_TIMEOUT_MS", REMOTE_REUSE_READY_TIMEOUT_MS.to_string()),
            ("T3_READY_PROBE_TIMEOUT_MS", SSH_READY_PROBE_TIMEOUT.as_millis().to_string()),
        ],
    ))
}

/// `remoteStateKey`: the remote state directory's name for `target`.
fn state_key(target: &SshTarget) -> String {
    let digest = sha2::Sha256::digest(target.connection_key().as_bytes());
    crate::backend::hex(&digest)[..16].to_owned()
}

/// `extractJsonObject` + decode: the last JSON object a remote script printed.
fn decode_output<T: for<'de> Deserialize<'de>>(stdout: &str) -> Option<T> {
    let trimmed = stdout.trim();
    serde_json::from_str(trimmed).ok().or_else(|| {
        let start = trimmed.find('{')?;
        serde_json::Deserializer::from_str(&trimmed[start..]).into_iter::<T>().next()?.ok()
    })
}

fn has_output(stdout: &str) -> bool {
    stdout.lines().any(|line| !line.trim().is_empty())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LaunchResult {
    remote_port: f64,
}

#[derive(Deserialize)]
struct PairingResult {
    credential: String,
}

/// `launchOrReuseRemoteServer`: the remote server's loopback port.
async fn launch(target: &SshTarget, auth: Auth, runner: &Runner) -> Result<u16, SshError> {
    let options = RunOptions {
        remote_args: vec!["sh".into(), "-l".into(), "-s".into(), "--".into(), state_key(target)],
        stdin: Some(launch_script(runner)?),
        timeout: Some(if runner.uses_node() { REMOTE_LAUNCH_TIMEOUT } else { REMOTE_ARCHIVE_LAUNCH_TIMEOUT }),
        ..RunOptions::default()
    };
    let output = run(target, &options, &auth).await?;
    if !has_output(&output.stdout) {
        return Err(SshError::new("SSH launch did not return a remote port."));
    }
    let launched: LaunchResult =
        decode_output(&output.stdout).ok_or_else(|| SshError::new("SSH launch returned unparseable output."))?;
    let port = launched.remote_port;
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "checked to be a whole port")]
    let whole = (port.fract() == 0. && (1.0..=65_535.).contains(&port)).then_some(port as u16);
    whole.ok_or_else(|| SshError::new(format!("SSH launch returned an invalid remote port: {port}.")))
}

/// `issueRemotePairingToken`: a one-time pairing credential from the remote's `t3`.
async fn mint_pairing_token(target: &SshTarget, auth: Auth, runner: &Runner) -> Result<String, SshError> {
    let script = fill(
        scripts::PAIRING_SCRIPT,
        &[("T3_STATE_KEY", state_key(target)), ("T3_RUNNER_SCRIPT", runner_script(runner)?)],
    );
    let options = RunOptions {
        remote_args: vec!["sh".into(), "-s".into()],
        stdin: Some(script),
        timeout: (!runner.uses_node()).then_some(REMOTE_ARCHIVE_LAUNCH_TIMEOUT),
        ..RunOptions::default()
    };
    let output = run(target, &options, &auth).await?;
    if !has_output(&output.stdout) {
        return Err(SshError::new("SSH pairing did not return a credential."));
    }
    let paired: PairingResult =
        decode_output(&output.stdout).ok_or_else(|| SshError::new("SSH pairing returned unparseable output."))?;
    match paired.credential.trim() {
        "" => Err(SshError::new("SSH pairing command returned an invalid credential.")),
        _ => Ok(paired.credential),
    }
}

/// `stopRemoteServer`: stops the server this client started there; an adopted one keeps running.
async fn stop_remote(target: &SshTarget, auth: Auth) -> Result<(), SshError> {
    let options = RunOptions {
        remote_args: vec!["sh".into(), "-s".into()],
        stdin: Some(fill(scripts::STOP_SCRIPT, &[("T3_STATE_KEY", state_key(target))])),
        ..RunOptions::default()
    };
    run(target, &options, &auth).await.map(drop)
}

/// The end of the remote server's log, for diagnosing a forward that never answered.
async fn remote_log_tail(target: &SshTarget, auth: Auth) -> String {
    let options = RunOptions {
        remote_args: vec!["sh".into(), "-s".into()],
        stdin: Some(fill(scripts::LOG_TAIL_SCRIPT, &[("T3_STATE_KEY", state_key(target))])),
        timeout: Some(Duration::from_secs(10)),
        ..RunOptions::default()
    };
    run(target, &options, &auth).await.map(|output| output.stdout.trim().to_owned()).unwrap_or_default()
}

/// `waitForHttpReady`: polls `http_base` until it answers 2xx.
async fn wait_http_ready(http_base: &str, timeout: Duration) -> Result<(), SshError> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let probe = crate::backend::http_client().get(format!("{http_base}/")).timeout(SSH_READY_PROBE_TIMEOUT).send();
        if probe.await.is_ok_and(|response| response.status().is_success()) {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(SshError::new(format!(
                "Timed out waiting {}ms for backend readiness at {http_base}/.",
                timeout.as_millis()
            )));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// A free loopback port for the forward.
fn reserve_local_port() -> Result<u16, SshError> {
    std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .and_then(|listener| listener.local_addr())
        .map(|address| address.port())
        .map_err(|error| SshError::new(format!("Could not reserve a local port for the SSH tunnel. ({error})")))
}

/// Keeps the end of what the tunnel writes to stderr, at a char boundary.
fn keep_tail(tail: &mut String, chunk: &str) {
    tail.push_str(chunk);
    let excess = tail.chars().count().saturating_sub(TUNNEL_STDERR_TAIL_CHARS);
    if excess > 0 {
        let cut = tail.char_indices().nth(excess).map_or(tail.len(), |(at, _)| at);
        tail.drain(..cut);
    }
}

/// `startSshTunnel`: `ssh -N -L`, ready once the forwarded server answers.
async fn start_tunnel(target: &SshTarget, remote_port: u16, local_port: u16, auth: Auth) -> Result<Tunnel, SshError> {
    let host_spec = target.host_spec()?;
    let mut args = base_args(target, matches!(auth, Auth::Batch));
    for option in [
        "ExitOnForwardFailure=yes",
        "ControlMaster=no",
        "ControlPath=none",
        "ControlPersist=no",
        "ServerAliveInterval=15",
        "ServerAliveCountMax=3",
    ] {
        args.extend(["-o".into(), option.into()]);
    }
    args.extend(["-n".into(), "-N".into(), "-L".into(), format!("{local_port}:127.0.0.1:{remote_port}")]);
    args.extend(["--".into(), host_spec]);
    let mut command = super::ssh_command();
    apply_auth(&mut command, &auth)?;
    command.args(&args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| SshError::new(format!("Failed to spawn SSH tunnel for {}. ({error})", target.alias)))?;
    #[cfg(windows)]
    let job = crate::backend::contain(&child);
    let tail = Arc::new(Mutex::new(String::new()));
    let drain = child.stderr.take().map(|mut stderr| {
        let tail = tail.clone();
        tokio::spawn(async move {
            let mut buffer = [0u8; 4096];
            while let Ok(read) = stderr.read(&mut buffer).await {
                if read == 0 {
                    break;
                }
                keep_tail(&mut locked(&tail), &String::from_utf8_lossy(&buffer[..read]));
            }
        })
    });
    let http_base = format!("http://127.0.0.1:{local_port}");
    let ready = tokio::select! {
        ready = wait_http_ready(&http_base, SSH_READY_TIMEOUT) => ready,
        status = child.wait() => {
            // Diagnostics drain until the pipe closes, so the tail holds the real cause.
            if let Some(drain) = drain {
                drop(drain.await);
            }
            let code = status.ok().and_then(|status| status.code()).map_or_else(|| "signal".into(), |c| c.to_string());
            let stderr = locked(&tail).trim().to_owned();
            log::warn!("ssh tunnel to {} exited ({code}): {stderr}", target.alias);
            Err(SshError::new(if stderr.is_empty() {
                format!("SSH tunnel exited unexpectedly for {} (exit {code}).", target.alias)
            } else {
                stderr
            }))
        }
    };
    if let Err(error) = ready {
        drop(child.start_kill());
        let log = remote_log_tail(target, auth).await;
        log::warn!("ssh tunnel to {} did not become ready: {error}; remote log:\n{log}", target.alias);
        return Err(error);
    }
    log::info!("ssh tunnel ready: {} -> 127.0.0.1:{local_port}", target.alias);
    Ok(Tunnel {
        target: target.clone(),
        http_base,
        child,
        #[cfg(windows)]
        _job: job,
    })
}

/// `runWithSshAuth`: keys and agent first, then up to two in-app prompts for a password or
/// verification code, which this run then reuses for the target.
async fn with_auth<T>(
    key: &str,
    target: &SshTarget,
    operation: impl Fn(Auth) -> BoxFuture<'static, Result<T, SshError>>,
) -> Result<T, SshError> {
    let mut secret = MANAGER.secret(key);
    let mut prompts = 0;
    loop {
        let auth = match &secret {
            None => Auth::Batch,
            Some(secret) => Auth::Secret(Some(secret.clone())),
        };
        let error = match operation(auth).await {
            Ok(value) => return Ok(value),
            Err(error) if !error.is_auth_failure() => return Err(error),
            Err(error) => error,
        };
        log::warn!("ssh authentication failed for {}: {error}", target.alias);
        if secret.take().is_some() {
            locked(&MANAGER.secrets).remove(key);
        }
        if prompts >= 2 || error.is_agent_signing_failure() {
            return Err(error);
        }
        prompts += 1;
        let password = request_password(target, prompts).await?;
        locked(&MANAGER.secrets).insert(key.to_owned(), password.clone());
        secret = Some(password);
    }
}

/// `ensureTunnelEntry`: reuses a forward that still answers, else launches and forwards anew.
async fn ensure_tunnel(key: &str, target: &SshTarget, runner: &Arc<Runner>) -> Result<(SshTarget, String), SshError> {
    let existing = locked(&MANAGER.tunnels).get(key).map(|tunnel| (tunnel.target.clone(), tunnel.http_base.clone()));
    if let Some((target, http_base)) = existing {
        if wait_http_ready(&http_base, Duration::from_secs(2)).await.is_ok() {
            return Ok((target, http_base));
        }
        log::warn!("ssh tunnel to {} went stale; starting another", target.alias);
        // Closing a stale forward must not stop the remote server.
        if let Some(stale) = locked(&MANAGER.tunnels).remove(key) {
            stale.close();
        }
    }
    let remote_port = with_auth(key, target, |auth| {
        let (target, runner) = (target.clone(), runner.clone());
        async move { launch(&target, auth, &runner).await }.boxed()
    })
    .await?;
    let local_port = reserve_local_port()?;
    let tunnel = with_auth(key, target, |auth| {
        let target = target.clone();
        async move { start_tunnel(&target, remote_port, local_port, auth).await }.boxed()
    })
    .await?;
    let ready = (tunnel.target.clone(), tunnel.http_base.clone());
    if let Some(replaced) = locked(&MANAGER.tunnels).insert(key.to_owned(), tunnel) {
        replaced.close();
    }
    Ok(ready)
}

/// `resolveSshTarget` with the fields the user typed kept over what ssh resolved.
async fn resolved(target: &SshTarget) -> Result<SshTarget, SshError> {
    let alias = if target.alias.trim().is_empty() { &target.hostname } else { &target.alias };
    let base = resolve_target(alias).await?;
    Ok(SshTarget {
        username: target.username.clone().or(base.username.clone()),
        port: target.port.or(base.port),
        ..base
    })
}

/// `ensureEnvironment`: starts or reuses the server on `target` and forwards to it; with
/// `issue_pairing_token`, also mints a one-time pairing credential there.
pub async fn ensure(target: &SshTarget, issue_pairing_token: bool) -> Result<Bootstrap, SshError> {
    let target = resolved(target).await?;
    let key = target.connection_key();
    let lock = MANAGER.lock_for(&key);
    let _held = lock.lock().await;
    let runner = Arc::new(Runner::current());
    let (target, http_base) = ensure_tunnel(&key, &target, &runner).await?;
    let pairing_token = if issue_pairing_token {
        let token = with_auth(&key, &target, |auth| {
            let (target, runner) = (target.clone(), runner.clone());
            async move { mint_pairing_token(&target, auth, &runner).await }.boxed()
        })
        .await?;
        Some(token)
    } else {
        None
    };
    Ok(Bootstrap { target, http_base, pairing_token })
}

/// `disconnectEnvironment`: closes the forward and stops the server this client started there.
pub async fn disconnect(target: &SshTarget) -> Result<(), SshError> {
    let target = resolved(target).await?;
    let key = target.connection_key();
    let lock = MANAGER.lock_for(&key);
    let _held = lock.lock().await;
    if let Some(tunnel) = locked(&MANAGER.tunnels).remove(&key) {
        tunnel.close();
    }
    with_auth(&key, &target, |auth| {
        let target = target.clone();
        async move { stop_remote(&target, auth).await }.boxed()
    })
    .await
}

/// Closes every forward as the app quits and asks each server this run started to stop, as the
/// desktop app's manager scope does; waits at most [`QUIT_STOP_WAIT`]. Blocks the caller.
pub fn shutdown() {
    let tunnels: Vec<(String, Tunnel)> = locked(&MANAGER.tunnels).drain().collect();
    if tunnels.is_empty() {
        return;
    }
    let stops: Vec<_> = tunnels
        .into_iter()
        .map(|(key, tunnel)| {
            let target = tunnel.target.clone();
            tunnel.close();
            let auth = MANAGER.secret(&key).map_or(Auth::Batch, |secret| Auth::Secret(Some(secret)));
            async move {
                if let Err(error) = stop_remote(&target, auth).await {
                    log::warn!("could not stop the T3 server on {}: {error}", target.alias);
                }
            }
        })
        .collect();
    let stopped = crate::runtime().block_on(tokio::time::timeout(QUIT_STOP_WAIT, futures::future::join_all(stops)));
    if stopped.is_err() {
        log::warn!("remote T3 servers were still stopping when the app quit");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_versions_must_be_exact() {
        for good in ["0.1.7", "1.20.3", "0.1.8-nightly.20261109.1", "1.0.0-rc-1"] {
            assert!(exact_version(good), "{good} is exact");
        }
        for bad in ["", "0.1", "01.2.3", "1.2.3-", "1.2.3-a..b", "1.2.3/../x", "1.2.3-a b", "v1.2.3"] {
            assert!(!exact_version(bad), "{bad:?} is not exact");
        }
    }

    #[test]
    fn scripts_fill_every_placeholder() {
        let runner = Runner { node_script: String::new(), archive_version: "0.1.7".into() };
        let launch = launch_script(&runner).expect("a launch script");
        assert!(!launch.contains("@@"), "unfilled placeholder in the launch script");
        assert!(launch.contains("T3_ARCHIVE_MODE=1"));
        assert!(launch.contains("T3_ARCHIVE_VERSION='0.1.7'"));
        assert!(launch.contains(&format!("T3_RELEASE_BASE_URL='{RELEASE_BASE_URL}'")));
        let node = Runner { node_script: "/src/it's/bin.mjs".into(), archive_version: String::new() };
        let launch = launch_script(&node).expect("a launch script");
        assert!(launch.contains(r"T3_NODE_SCRIPT_PATH='/src/it'\''s/bin.mjs'"));
        assert!(launch.contains("T3_ARCHIVE_MODE=0"));
        let none = Runner { node_script: String::new(), archive_version: String::new() };
        assert!(launch_script(&none).is_err());
        let bad = Runner { node_script: String::new(), archive_version: "../x".into() };
        assert!(runner_script(&bad).is_err());
    }

    #[test]
    fn state_keys_hash_the_target() {
        let target = SshTarget { alias: "box".into(), hostname: "10.0.0.1".into(), username: None, port: Some(22) };
        let key = state_key(&target);
        assert_eq!(key.len(), 16);
        assert!(key.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_ne!(key, state_key(&SshTarget { port: None, ..target }));
    }

    #[test]
    fn remote_output_decodes_past_shell_noise() {
        let launched: LaunchResult =
            decode_output("motd line\n{\"remotePort\":3774,\"serverKind\":\"managed\"}\n").expect("a launch result");
        assert!((launched.remote_port - 3774.).abs() < f64::EPSILON);
        let paired: PairingResult = decode_output("{\"credential\":\"abc\"}").expect("a pairing result");
        assert_eq!(paired.credential, "abc");
        assert!(decode_output::<PairingResult>("nothing here").is_none());
    }

    #[test]
    fn stderr_tails_keep_char_boundaries() {
        let mut tail = String::new();
        keep_tail(&mut tail, &"é".repeat(TUNNEL_STDERR_TAIL_CHARS));
        keep_tail(&mut tail, "end");
        assert_eq!(tail.chars().count(), TUNNEL_STDERR_TAIL_CHARS);
        assert!(tail.ends_with("éend"));
    }
}
