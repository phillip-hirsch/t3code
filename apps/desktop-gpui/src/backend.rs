//! Starts the local T3 server the way the Electron desktop does, then gets a
//! bearer token for it. Mirrors `apps/desktop/src/backend/DesktopBackendManager.ts`.

use std::net::{Ipv4Addr, SocketAddrV4, TcpListener};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use serde::Deserialize;
use serde_json::json;
use tokio::io::AsyncWriteExt as _;
use tokio::process::{Child, Command};

const FIRST_PORT: u16 = 3773;
const READY_TIMEOUT: Duration = Duration::from_secs(180);
/// `SERVER_EXIT_CODE_STATE_DIR_OWNED` in `packages/contracts/src/desktopBootstrap.ts`.
const EXIT_STATE_DIR_OWNED: i32 = 78;

/// Where to reach a server, and how to authenticate.
#[derive(Clone, Debug)]
pub struct Endpoint {
    /// `http://host:port`, no trailing slash.
    pub http_base: String,
    pub bearer_token: String,
}

impl Endpoint {
    pub fn ws_url(&self) -> String {
        let rest = self.http_base.strip_prefix("http").unwrap_or(&self.http_base);
        format!("ws{rest}/ws")
    }
}

/// A server this process started. Dropping it kills the server.
pub struct LocalServer {
    child: Child,
    #[cfg(windows)]
    _job: Option<win32job::Job>,
}

/// Keeps a [`LocalServer`] alive on the runtime. Dropping the handle stops it.
pub struct ServerHandle {
    pub exited: futures::channel::oneshot::Receiver<String>,
    _stop: futures::channel::oneshot::Sender<()>,
}

impl LocalServer {
    pub fn supervise(self, runtime: &tokio::runtime::Handle) -> ServerHandle {
        let (exited_tx, exited) = futures::channel::oneshot::channel();
        let (stop, stop_rx) = futures::channel::oneshot::channel::<()>();
        runtime.spawn(async move {
            // Move the whole server in: capturing only `child` would drop the
            // Windows job here, and closing the job kills the server.
            let mut server = self;
            tokio::select! {
                status = server.child.wait() => {
                    let reason = match status {
                        Ok(status) => format!("The local server stopped ({status})."),
                        Err(error) => format!("Lost track of the local server: {error}"),
                    };
                    let _ = exited_tx.send(reason);
                }
                // Sender dropped: the app is done with the server. `kill_on_drop` ends it.
                _ = stop_rx => {}
            }
        });
        ServerHandle { exited, _stop: stop }
    }
}

pub struct LaunchConfig {
    pub node: PathBuf,
    pub server_entry: PathBuf,
    pub home: PathBuf,
}

impl LaunchConfig {
    /// `T3_NODE_BIN`, `T3_SERVER_ENTRY` and `T3CODE_HOME` override the defaults.
    pub fn from_env() -> Result<Self> {
        let node = std::env::var_os("T3_NODE_BIN").map(PathBuf::from).unwrap_or_else(|| "node".into());
        let server_entry = match std::env::var_os("T3_SERVER_ENTRY") {
            Some(path) => PathBuf::from(path),
            None => default_server_entry()?,
        };
        let home = match std::env::var_os("T3CODE_HOME") {
            Some(path) => PathBuf::from(path),
            None => default_home()?,
        };
        Ok(Self { node, server_entry, home })
    }
}

/// The server sources next to this crate: built bundle first, then the TS entry.
fn default_server_entry() -> Result<PathBuf> {
    let server = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../server");
    [server.join("dist/bin.mjs"), server.join("src/bin.ts")]
        .into_iter()
        .find(|path| path.exists())
        .ok_or_else(|| anyhow!("no server entry under {}; set T3_SERVER_ENTRY", server.display()))
}

/// Separate from the Electron app's `~/.t3` until this client reaches parity,
/// so both can run at once without fighting over the state directory.
fn default_home() -> Result<PathBuf> {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .ok_or_else(|| anyhow!("no home directory; set T3CODE_HOME"))?;
    Ok(PathBuf::from(home).join(".t3-gpui"))
}

/// Uses `T3_SERVER_URL` + `T3_SERVER_TOKEN` when set, otherwise starts a local server.
pub async fn resolve_endpoint() -> Result<(Endpoint, Option<LocalServer>)> {
    if let (Ok(url), Ok(token)) = (std::env::var("T3_SERVER_URL"), std::env::var("T3_SERVER_TOKEN")) {
        let endpoint = Endpoint { http_base: url.trim_end_matches('/').to_owned(), bearer_token: token };
        wait_ready(&endpoint.http_base, None).await?;
        return Ok((endpoint, None));
    }
    let (endpoint, server) = launch(LaunchConfig::from_env()?).await?;
    Ok((endpoint, Some(server)))
}

pub async fn launch(config: LaunchConfig) -> Result<(Endpoint, LocalServer)> {
    std::fs::create_dir_all(&config.home).with_context(|| format!("create {}", config.home.display()))?;
    let port = pick_port()?;
    let bootstrap_token = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());

    // Bootstrap goes over stdin (fd 0): Rust cannot hand fd 3 to a child on Windows.
    let mut command = Command::new(&config.node);
    command
        .arg(&config.server_entry)
        .args(["--bootstrap-fd", "0"])
        .current_dir(&config.home)
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    for key in ["T3CODE_PORT", "T3CODE_MODE", "T3CODE_NO_BROWSER", "T3CODE_HOST", "T3CODE_HOME"] {
        command.env_remove(key);
    }
    let mut child = command
        .spawn()
        .with_context(|| format!("start {} {}", config.node.display(), config.server_entry.display()))?;
    #[cfg(windows)]
    let job = contain(&child);

    let bootstrap = json!({
        "mode": "desktop",
        "noBrowser": true,
        "port": port,
        "t3Home": config.home,
        "host": "127.0.0.1",
        "desktopBootstrapToken": bootstrap_token,
        "tailscaleServeEnabled": false,
        "tailscaleServePort": 443,
    });
    let mut stdin = child.stdin.take().context("server stdin")?;
    stdin.write_all(format!("{bootstrap}\n").as_bytes()).await?;
    stdin.flush().await?;
    drop(stdin);

    let http_base = format!("http://127.0.0.1:{port}");
    let mut server = LocalServer {
        child,
        #[cfg(windows)]
        _job: job,
    };
    tokio::select! {
        ready = wait_ready(&http_base, Some(READY_TIMEOUT)) => ready?,
        status = server.child.wait() => {
            let code = status.ok().and_then(|s| s.code());
            if code == Some(EXIT_STATE_DIR_OWNED) {
                bail!("another T3 server already owns {}", config.home.display());
            }
            bail!("server exited before it was ready (code {code:?})");
        }
    }
    let bearer_token = exchange_token(&http_base, &bootstrap_token).await?;
    Ok((Endpoint { http_base, bearer_token }, server))
}

/// First port from 3773 that is free on loopback and the wildcard address,
/// matching `apps/desktop/src/app/DesktopApp.ts`.
fn pick_port() -> Result<u16> {
    (FIRST_PORT..FIRST_PORT + 200)
        .find(|port| {
            TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, *port)).is_ok()
                && TcpListener::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, *port)).is_ok()
        })
        .ok_or_else(|| anyhow!("no free port from {FIRST_PORT}"))
}

#[derive(Deserialize)]
struct EnvironmentDescriptor {
    #[serde(rename = "orchestrationProtocolVersion")]
    orchestration_protocol_version: Option<u32>,
}

async fn wait_ready(http_base: &str, timeout: Option<Duration>) -> Result<()> {
    let client = reqwest::Client::builder().timeout(Duration::from_secs(1)).build()?;
    let url = format!("{http_base}/.well-known/t3/environment");
    let deadline = tokio::time::Instant::now() + timeout.unwrap_or(Duration::from_secs(15));
    loop {
        if let Ok(response) = client.get(&url).send().await {
            if response.status().is_success() {
                let descriptor: EnvironmentDescriptor = response.json().await.context("environment descriptor")?;
                let version = descriptor.orchestration_protocol_version.unwrap_or(1);
                if version != 2 {
                    bail!("server speaks orchestration protocol {version}; this client needs 2");
                }
                return Ok(());
            }
        }
        if tokio::time::Instant::now() > deadline {
            bail!("server at {http_base} did not become ready");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[derive(Deserialize)]
struct AccessToken {
    access_token: String,
}

/// One exchange per launch: a second desktop-bootstrap exchange revokes the first session.
async fn exchange_token(http_base: &str, bootstrap_token: &str) -> Result<String> {
    let os = if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    };
    let form = [
        ("grant_type", "urn:ietf:params:oauth:grant-type:token-exchange"),
        ("subject_token", bootstrap_token),
        ("subject_token_type", "urn:t3:params:oauth:token-type:environment-bootstrap"),
        ("requested_token_type", "urn:ietf:params:oauth:token-type:access_token"),
        ("client_label", "T3 Code Desktop (GPUI)"),
        ("client_device_type", "desktop"),
        ("client_os", os),
    ];
    let response = reqwest::Client::new()
        .post(format!("{http_base}/oauth/token"))
        .form(&form)
        .send()
        .await
        .context("token exchange request")?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        bail!("token exchange failed ({status}): {body}");
    }
    Ok(response.json::<AccessToken>().await.context("token exchange response")?.access_token)
}

/// The server does not watch its parent on Windows; a kill-on-close job ends it with us.
#[cfg(windows)]
fn contain(child: &Child) -> Option<win32job::Job> {
    let handle = child.raw_handle()?;
    let mut limits = win32job::ExtendedLimitInfo::new();
    limits.limit_kill_on_job_close();
    let job = win32job::Job::create_with_limit_info(&limits).ok()?;
    job.assign_process(handle as isize).ok()?;
    Some(job)
}
