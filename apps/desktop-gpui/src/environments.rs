//! Saved remote environments: the catalog this device keeps, pairing, and the connection
//! diagnosis Settings › Environments shows. Ports the bearer route of
//! `packages/client-runtime/src/connection/` (`onboarding.ts`, `compatibility.ts`,
//! `diagnosis.ts`, `presentation.ts`) and `resolveRemotePairingTarget` in
//! `packages/shared/src/remote.ts`. SSH and T3 Connect routes are not ported.
//!
//! Bearer tokens are stored encrypted with Windows DPAPI (the guarantee Electron's
//! `safeStorage` gives the desktop app's connection catalog).

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use url::Url;

use crate::backend::{self, Endpoint, LocalServer};

/// `ORCHESTRATION_PROTOCOL_VERSION` in `packages/contracts`.
const ORCHESTRATION_PROTOCOL_VERSION: u64 = 2;
/// `DEFAULT_REMOTE_REQUEST_TIMEOUT_MS` in `authorization/remote.ts`.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const RETRY_BASE_DELAY_MS: f64 = 1_000.;
const RETRY_MAX_DELAY_MS: f64 = 300_000.;

// ---------------------------------------------------------------------------------------------
// Pairing input

/// `parsePairingUrlFields` (`ConnectionsSettings.logic.ts`): a pasted pairing URL's host and code.
pub fn parse_pairing_url(input: &str) -> Option<(String, String)> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    let candidate = if has_scheme(trimmed, true) {
        trimmed.to_owned()
    } else if let Some(rest) = trimmed.strip_prefix("//") {
        format!("https://{rest}")
    } else {
        format!("https://{trimmed}")
    };
    let url = Url::parse(&candidate).ok()?;
    if let Some(hosted) = hosted_pairing_request(&url) {
        return Some(hosted);
    }
    Some((url.origin().ascii_serialization(), pairing_token(&url)?))
}

/// `parseRemotePairingFields`: the dialog's host and code, or a pairing URL pasted as the host.
pub fn parse_remote_pairing_fields(host: &str, code: &str) -> Result<(String, String), &'static str> {
    if let Some(parsed) = parse_pairing_url(host) {
        return Ok(parsed);
    }
    match (host.trim(), code.trim()) {
        ("", _) => Err("Enter a backend host."),
        (_, "") => Err("Enter a pairing code."),
        (host, code) => Ok((host.to_owned(), code.to_owned())),
    }
}

/// `resolveRemotePairingTarget` for a host and code: the server's HTTP base, without a trailing slash.
pub fn resolve_pairing_host(host: &str, code: &str) -> Result<String, &'static str> {
    let host = host.trim();
    if host.is_empty() {
        return Err("Enter a backend URL.");
    }
    let mut url = normalize_base_url(host)?;
    if code.trim().is_empty() {
        return Err("Enter a pairing code.");
    }
    let http = match url.scheme() {
        "ws" => Some("http"),
        "wss" => Some("https"),
        _ => None,
    };
    if let Some(scheme) = http {
        url.set_scheme(scheme).map_err(|()| "Backend URL is invalid.")?;
    }
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

fn normalize_base_url(raw: &str) -> Result<Url, &'static str> {
    const INVALID: &str = "Backend URL is invalid.";
    let trimmed = raw.trim().trim_start_matches('/');
    let input = if has_scheme(trimmed, false) { trimmed.to_owned() } else { format!("https://{trimmed}") };
    let mut url = Url::parse(&input).map_err(|_| INVALID)?;
    if !matches!(url.scheme(), "http" | "https" | "ws" | "wss") {
        return Err(INVALID);
    }
    url.set_path("/");
    url.set_query(None);
    url.set_fragment(None);
    Ok(url)
}

/// `^[a-zA-Z][a-zA-Z\d+-]*:\/\/`, with `.` allowed in the scheme when `dot`.
fn has_scheme(input: &str, dot: bool) -> bool {
    let Some(end) = input.find("://") else { return false };
    let mut chars = input[..end].chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || (dot && c == '.'))
}

fn first_param<'a>(
    pairs: impl Iterator<Item = (std::borrow::Cow<'a, str>, std::borrow::Cow<'a, str>)>,
    name: &str,
) -> Option<String> {
    let value = pairs.into_iter().find(|(key, _)| key == name)?.1;
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

/// `getPairingTokenFromUrl`: `#token=` first, then `?token=`.
fn pairing_token(url: &Url) -> Option<String> {
    let hash = url.fragment().unwrap_or_default();
    first_param(url::form_urlencoded::parse(hash.as_bytes()), "token")
        .or_else(|| first_param(url.query_pairs(), "token"))
}

/// `readHostedPairingRequest`: a hosted pairing page's `?host=` plus its token.
fn hosted_pairing_request(url: &Url) -> Option<(String, String)> {
    let host = first_param(url.query_pairs(), "host")?;
    Some((host, pairing_token(url)?))
}

// ---------------------------------------------------------------------------------------------
// Connection failures

/// `ConnectionTransientReason` and `ConnectionBlockedReason`, for the reasons a bearer route meets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    Network,
    Timeout,
    Transport,
    RemoteUnavailable,
    Authentication,
    Configuration,
    Permission,
    Unsupported,
}

impl Reason {
    fn as_str(self) -> &'static str {
        match self {
            Reason::Network => "network",
            Reason::Timeout => "timeout",
            Reason::Transport => "transport",
            Reason::RemoteUnavailable => "remote-unavailable",
            Reason::Authentication => "authentication",
            Reason::Configuration => "configuration",
            Reason::Permission => "permission",
            Reason::Unsupported => "unsupported",
        }
    }

    /// A `ConnectionBlockedError`: the supervisor stops retrying until asked.
    pub fn blocks(self) -> bool {
        matches!(self, Reason::Authentication | Reason::Configuration | Reason::Permission | Reason::Unsupported)
    }
}

/// `ConnectionHop`, for a direct route.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hop {
    Device,
    Server,
}

/// `ConnectionAttemptError` for a direct route.
#[derive(Clone, Debug, PartialEq)]
pub struct Failure {
    pub reason: Reason,
    pub detail: String,
    pub hop: Option<Hop>,
    pub server_update_required: bool,
}

impl Failure {
    fn new(reason: Reason, detail: impl Into<String>, hop: Option<Hop>) -> Self {
        Self { reason, detail: detail.into(), hop, server_update_required: false }
    }

    /// `mapRemoteEnvironmentError` for an HTTP status the server declares.
    pub fn http(status: u16, url: &str) -> Self {
        let (reason, detail) = match status {
            401 => (Reason::Authentication, "The environment credential is invalid.".to_owned()),
            403 => (Reason::Permission, "The environment credential does not grant the required access.".to_owned()),
            400 => (Reason::Configuration, "The environment rejected the authentication request.".to_owned()),
            404 => (Reason::Configuration, "The environment endpoint could not be found.".to_owned()),
            500 => (Reason::RemoteUnavailable, "The environment could not authorize the connection.".to_owned()),
            _ => (
                Reason::RemoteUnavailable,
                format!("Remote environment endpoint {url} returned undeclared status {status}."),
            ),
        };
        Self::new(reason, detail, Some(Hop::Server))
    }

    /// `RemoteEnvironmentAuthTimeoutError` / `RemoteEnvironmentAuthFetchError`.
    fn request(error: reqwest::Error, url: &str) -> Self {
        if error.is_timeout() {
            let detail =
                format!("Remote environment endpoint {url} timed out after {}ms.", REQUEST_TIMEOUT.as_millis());
            return Self::new(Reason::Timeout, detail, Some(Hop::Server));
        }
        // reqwest repeats the URL in its message; the root cause says what went wrong.
        let mut cause: &dyn std::error::Error = &error;
        while let Some(source) = cause.source() {
            cause = source;
        }
        let detail = format!("Failed to fetch remote environment endpoint {url} ({cause}).");
        Self::new(Reason::Network, detail, Some(Hop::Server))
    }

    fn invalid_json(url: &str) -> Self {
        let detail = format!("Remote environment endpoint returned an invalid response from {url}.");
        Self::new(Reason::RemoteUnavailable, detail, Some(Hop::Server))
    }

    /// `credentialMissingError`.
    pub fn credential_missing(environment: &SavedEnvironment) -> Self {
        let detail = format!("Connection credential {} is unavailable.", environment.connection_id());
        Self::new(Reason::Authentication, detail, Some(Hop::Device))
    }

    /// `environmentMismatchError`.
    pub fn environment_mismatch(expected: &str, actual: &str) -> Self {
        Self::new(Reason::Configuration, format!("Connected environment {actual} does not match {expected}."), None)
    }

    /// A socket that failed to open or closed.
    pub fn transport(detail: impl Into<String>) -> Self {
        Self::new(Reason::Transport, detail, Some(Hop::Server))
    }
}

// ---------------------------------------------------------------------------------------------
// Descriptor and pairing

/// `ExecutionEnvironmentDescriptor`, the fields this client reads.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Descriptor {
    pub environment_id: String,
    pub label: String,
    #[serde(default)]
    pub server_version: Option<String>,
    #[serde(default)]
    pub orchestration_protocol_version: Option<u64>,
    #[serde(default)]
    pub platform: Value,
    #[serde(default)]
    pub capabilities: Value,
}

impl Descriptor {
    pub fn machine(&self) -> Option<String> {
        self.platform["machine"].as_str().map(str::to_owned)
    }

    /// `orchestrationProtocolCompatibilityError`.
    pub fn compatibility_error(&self) -> Option<Failure> {
        let version = self.orchestration_protocol_version.unwrap_or(1);
        if version == ORCHESTRATION_PROTOCOL_VERSION {
            return None;
        }
        let label = &self.label;
        Some(if version > ORCHESTRATION_PROTOCOL_VERSION {
            let detail = format!(
                "This client is not supported by this server. Update your app or use a compatible release to connect to {label}."
            );
            Failure::new(Reason::Unsupported, detail, None)
        } else {
            let detail = format!("This client requires a newer server. Update T3 Code on {label} to connect.");
            Failure {
                server_update_required: self.can_self_update(),
                ..Failure::new(Reason::Unsupported, detail, None)
            }
        })
    }

    fn can_self_update(&self) -> bool {
        match self.capabilities["serverSelfUpdate"].as_str() {
            None => false,
            Some("desktop-managed") => self.capabilities["desktopAppUpdate"] == Value::Bool(true),
            Some(_) => true,
        }
    }
}

/// `fetchRemoteEnvironmentDescriptor`. Runs on the tokio runtime.
pub async fn fetch_descriptor(http_base: &str) -> Result<Descriptor, Failure> {
    let url = format!("{http_base}/.well-known/t3/environment");
    let request = backend::http_client().get(&url).timeout(REQUEST_TIMEOUT);
    let response = request.send().await.map_err(|error| Failure::request(error, &url))?;
    let status = response.status();
    if !status.is_success() {
        return Err(Failure::http(status.as_u16(), &url));
    }
    response.json().await.map_err(|error| {
        log::warn!("{url}: {error}");
        Failure::invalid_json(&url)
    })
}

/// `bootstrapRemoteBearerSession`: redeems a pairing code for a bearer session.
async fn redeem_pairing_code(http_base: &str, code: &str) -> Result<String, Failure> {
    let url = format!("{http_base}/oauth/token");
    let os = if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    };
    let form = [
        ("grant_type", "urn:ietf:params:oauth:grant-type:token-exchange"),
        ("subject_token", code),
        ("subject_token_type", "urn:t3:params:oauth:token-type:environment-bootstrap"),
        ("requested_token_type", "urn:ietf:params:oauth:token-type:access_token"),
        ("client_label", "T3 Code Desktop (GPUI)"),
        ("client_device_type", "desktop"),
        ("client_os", os),
    ];
    let request = backend::http_client().post(&url).form(&form).timeout(REQUEST_TIMEOUT);
    let response = request.send().await.map_err(|error| Failure::request(error, &url))?;
    let status = response.status();
    if !status.is_success() {
        return Err(Failure::http(status.as_u16(), &url));
    }
    let body: Value = response.json().await.map_err(|error| {
        log::warn!("{url}: {error}");
        Failure::invalid_json(&url)
    })?;
    body["access_token"].as_str().map(str::to_owned).ok_or_else(|| Failure::invalid_json(&url))
}

/// `preparePairingRegistration` for a host and pairing code. `saved` is the catalog, so a code
/// for an address that would add a second route to a saved environment is refused unspent.
/// Runs on the tokio runtime; errors are the dialog's message.
pub async fn pair(host: String, code: String, saved: Vec<SavedEnvironment>) -> Result<SavedEnvironment, String> {
    let http_base = resolve_pairing_host(&host, &code)?;
    let descriptor = fetch_descriptor(&http_base).await.map_err(|failure| failure.detail)?;
    if let Some(known) = saved.iter().find(|saved| saved.environment_id == descriptor.environment_id)
        && known.http_base != http_base
    {
        return Err(format!(
            "That address claims to be {}, which is already saved. Add it as a route from that environment's settings instead.",
            known.label
        ));
    }
    // An outdated server is still saved so it can be updated from this client.
    if let Some(failure) = descriptor.compatibility_error()
        && !failure.server_update_required
    {
        return Err(failure.detail);
    }
    let token = redeem_pairing_code(&http_base, code.trim()).await.map_err(|failure| failure.detail)?;
    Ok(SavedEnvironment {
        environment_id: descriptor.environment_id,
        label: descriptor.label,
        http_base,
        enabled: true,
        token: Some(token),
    })
}

// ---------------------------------------------------------------------------------------------
// Catalog

/// A paired environment (`BearerConnectionTarget` + profile + credential).
#[derive(Clone, Debug, PartialEq)]
pub struct SavedEnvironment {
    pub environment_id: String,
    pub label: String,
    /// `https://host:port`, no trailing slash.
    pub http_base: String,
    pub enabled: bool,
    /// `None` when the stored credential could not be decrypted.
    pub token: Option<String>,
}

impl SavedEnvironment {
    /// `bearerConnectionId`.
    pub fn connection_id(&self) -> String {
        let origin = Url::parse(&self.http_base).map(|url| url.origin().ascii_serialization()).unwrap_or_default();
        format!("bearer:{}:{origin}", self.environment_id)
    }

    /// `environmentMcpUrl` for a bearer route.
    pub fn mcp_url(&self) -> String {
        format!("{}/mcp", self.http_base)
    }

    pub fn endpoint(&self) -> Option<Endpoint> {
        Some(Endpoint::bearer(&self.http_base, self.token.clone()?))
    }
}

/// What this device keeps: the saved environments and the desktop app's
/// `localEnvironmentEnabled` setting.
#[derive(Clone, Debug, PartialEq)]
pub struct Catalog {
    pub local_environment_enabled: bool,
    pub environments: Vec<SavedEnvironment>,
}

impl Default for Catalog {
    fn default() -> Self {
        Self { local_environment_enabled: true, environments: Vec::new() }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredEnvironment {
    environment_id: String,
    label: String,
    http_base_url: String,
    #[serde(default = "enabled_default")]
    enabled: bool,
    /// The bearer token, DPAPI-encrypted, hex.
    #[serde(default)]
    encrypted_token: String,
}

fn enabled_default() -> bool {
    true
}

impl Catalog {
    fn path() -> Option<PathBuf> {
        Some(crate::settings::userdata_dir()?.join("environments.json"))
    }

    /// Reads `<T3 home>/userdata/environments.json`; a missing or unreadable file is empty.
    pub fn load() -> Self {
        Self::path()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            .map(|document| Self::from_document(&document))
            .unwrap_or_default()
    }

    fn from_document(document: &Value) -> Self {
        let environments = document["environments"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|entry| serde_json::from_value::<StoredEnvironment>(entry.clone()).ok())
            .map(|stored| SavedEnvironment {
                token: unhex(&stored.encrypted_token)
                    .and_then(|blob| secure::unprotect(&blob).ok())
                    .and_then(|bytes| String::from_utf8(bytes).ok()),
                environment_id: stored.environment_id,
                label: stored.label,
                http_base: stored.http_base_url,
                enabled: stored.enabled,
            })
            .collect();
        Self { local_environment_enabled: document["localEnvironmentEnabled"] != Value::Bool(false), environments }
    }

    fn document(&self) -> anyhow::Result<Value> {
        let environments = self
            .environments
            .iter()
            .map(|environment| {
                let encrypted_token = match &environment.token {
                    Some(token) => {
                        let sealed = secure::protect(token.as_bytes())
                            .map_err(|error| anyhow::anyhow!("could not encrypt a token: {error}"))?;
                        backend::hex(&sealed)
                    }
                    None => String::new(),
                };
                serde_json::to_value(StoredEnvironment {
                    environment_id: environment.environment_id.clone(),
                    label: environment.label.clone(),
                    http_base_url: environment.http_base.clone(),
                    enabled: environment.enabled,
                    encrypted_token,
                })
                .context("could not encode an environment")
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(
            json!({ "version": 1, "localEnvironmentEnabled": self.local_environment_enabled, "environments": environments }),
        )
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let path = Self::path().context("No T3 home directory.")?;
        crate::settings::write_atomically(&path, &self.document()?).with_context(|| format!("writing {path:?}"))
    }

    pub fn get(&self, environment_id: &str) -> Option<&SavedEnvironment> {
        self.environments.iter().find(|environment| environment.environment_id == environment_id)
    }

    pub fn get_mut(&mut self, environment_id: &str) -> Option<&mut SavedEnvironment> {
        self.environments.iter_mut().find(|environment| environment.environment_id == environment_id)
    }

    /// `EnvironmentRegistry.register` for one bearer route: replaces a saved entry with the same id.
    pub fn upsert(&mut self, environment: SavedEnvironment) {
        match self.get_mut(&environment.environment_id) {
            Some(saved) => *saved = environment,
            None => self.environments.push(environment),
        }
    }
}

/// `prepareBearerConnectionUpdate`'s label check.
pub fn validate_label(label: &str) -> Result<String, &'static str> {
    let label = label.trim();
    if label.is_empty() { Err("Environment label cannot be empty.") } else { Ok(label.to_owned()) }
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if text.is_empty() || text.len() % 2 != 0 {
        return None;
    }
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok()).collect()
}

#[cfg(windows)]
mod secure {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
    };

    pub fn protect(data: &[u8]) -> Result<Vec<u8>, String> {
        let input = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr().cast_mut() };
        let mut output = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
        // SAFETY: `input` points at `data`, which outlives the call; DPAPI only reads it and
        // allocates `output`, which `take` frees.
        let ok = unsafe {
            CryptProtectData(
                &input,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        };
        take(ok, output)
    }

    pub fn unprotect(data: &[u8]) -> Result<Vec<u8>, String> {
        let input = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr().cast_mut() };
        let mut output = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
        // SAFETY: as in `protect`.
        let ok = unsafe {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        };
        take(ok, output)
    }

    fn take(ok: i32, output: CRYPT_INTEGER_BLOB) -> Result<Vec<u8>, String> {
        if ok == 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        // SAFETY: on success DPAPI filled `output` with `cbData` bytes it allocated with LocalAlloc.
        let bytes = unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec();
        // SAFETY: freed once, with the allocator DPAPI documents.
        unsafe { LocalFree(output.pbData.cast()) };
        Ok(bytes)
    }
}

#[cfg(not(windows))]
mod secure {
    const UNAVAILABLE: &str = "Secure credential storage is not available on this platform.";

    pub fn protect(_: &[u8]) -> Result<Vec<u8>, String> {
        Err(UNAVAILABLE.into())
    }

    pub fn unprotect(_: &[u8]) -> Result<Vec<u8>, String> {
        Err(UNAVAILABLE.into())
    }
}

/// The workspace's environment: `T3_SERVER_URL`, else the local server, or the first enabled
/// saved environment while the local environment is off (Electron then starts no server).
pub async fn resolve_workspace_endpoint() -> anyhow::Result<(Endpoint, Option<LocalServer>)> {
    let overridden = std::env::var_os("T3_SERVER_URL").is_some() && std::env::var_os("T3_SERVER_TOKEN").is_some();
    if !overridden {
        let catalog = Catalog::load();
        if !catalog.local_environment_enabled {
            let endpoint = catalog
                .environments
                .iter()
                .filter(|environment| environment.enabled)
                .find_map(SavedEnvironment::endpoint);
            let endpoint = endpoint.context(
                "The local environment is off. Turn it on in Settings › Environments, or add an environment there.",
            )?;
            return Ok((endpoint, None));
        }
    }
    backend::resolve_endpoint().await
}

// ---------------------------------------------------------------------------------------------
// Supervisor state and presentation

/// `SupervisorConnectionPhase`, without the network probe's `offline`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    #[default]
    Available,
    Connecting,
    Backoff,
    Connected,
    Blocked,
}

/// `SupervisorConnectionState`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ConnectionState {
    pub phase: Phase,
    pub attempt: u32,
    pub last_failure: Option<Failure>,
    /// Epoch ms.
    pub retry_at: Option<i64>,
}

/// `retryDelayMs`: exponential from 1 s to 5 min, half of it jittered by `random` in 0..=1.
pub fn retry_delay_ms(failure_count: u32, random: f64) -> u64 {
    let ceiling = RETRY_MAX_DELAY_MS.min(RETRY_BASE_DELAY_MS * 2f64.powi(failure_count.min(30) as i32 + 1));
    (ceiling / 2. + ceiling / 2. * random).round() as u64
}

/// `EnvironmentConnectionPhase` (`presentConnectionState`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Presented {
    Available,
    Connecting,
    Reconnecting,
    Connected,
    Error,
    Unsupported,
}

impl ConnectionState {
    pub fn presented(&self) -> Presented {
        match self.phase {
            Phase::Available => Presented::Available,
            Phase::Connecting if self.attempt <= 1 && self.last_failure.is_none() => Presented::Connecting,
            Phase::Connecting | Phase::Backoff => Presented::Reconnecting,
            Phase::Connected => Presented::Connected,
            Phase::Blocked if self.last_failure.as_ref().is_some_and(|f| f.reason == Reason::Unsupported) => {
                Presented::Unsupported
            }
            Phase::Blocked => Presented::Error,
        }
    }

    fn error(&self) -> Option<&str> {
        match self.phase {
            Phase::Connecting | Phase::Backoff | Phase::Blocked => {
                self.last_failure.as_ref().map(|f| f.detail.as_str())
            }
            _ => None,
        }
    }

    /// `connectionStatusText`.
    pub fn status_text(&self) -> String {
        match (self.presented(), self.error()) {
            (Presented::Available, _) => "Available".into(),
            (Presented::Connecting, _) => "Connecting...".into(),
            (Presented::Reconnecting, Some(error)) => format!("Failed to connect. Reconnecting... Reason: {error}"),
            (Presented::Reconnecting, None) => "Reconnecting...".into(),
            (Presented::Connected, _) => "Connected".into(),
            (Presented::Unsupported, _) => "Client not supported".into(),
            (Presented::Error, Some(error)) => format!("Connection failed. Reason: {error}"),
            (Presented::Error, None) => "Connection failed".into(),
        }
    }

    /// `savedBackendStatus`: the row's status and whether it reads as an error.
    pub fn saved_status(&self, enabled: bool) -> (String, bool) {
        let presented = self.presented();
        if !enabled && presented != Presented::Unsupported {
            return ("Off".into(), false);
        }
        match (presented, self.error()) {
            (Presented::Connected, _) => ("Connected".into(), false),
            (Presented::Connecting, _) => ("Connecting".into(), false),
            (Presented::Reconnecting, Some(error)) => (format!("Reconnecting: {error}"), true),
            (Presented::Reconnecting, None) => ("Reconnecting".into(), true),
            (Presented::Unsupported, _) => ("Client not supported".into(), false),
            (Presented::Error, Some(error)) => (format!("Connection failed: {error}"), true),
            (Presented::Error, None) => ("Connection failed".into(), true),
            (Presented::Available, _) => ("Not connected".into(), false),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HopResult {
    Ok,
    Failed,
    Unchecked,
}

/// `ConnectionDiagnosis` for a direct route.
#[derive(Clone, Debug, PartialEq)]
pub struct Diagnosis {
    pub headline: String,
    pub hops: Vec<(String, HopResult)>,
    pub steps: Vec<String>,
    pub facts: Vec<(&'static str, String)>,
}

/// `presentConnectionDiagnosis` for a direct route; `None` when nothing failed. The caller
/// asks only for an enabled environment that is not connected or unsupported.
pub fn diagnose(host: &str, state: &ConnectionState, server_version: Option<&str>) -> Option<Diagnosis> {
    let failure = state.last_failure.as_ref()?;
    let outdated = failure.server_update_required;
    // A failure that names no hop was guessed to be the server, so nothing before it was probed.
    let (hop, guessed) = match (outdated, failure.hop) {
        (true, _) => (Hop::Server, false),
        (false, Some(hop)) => (hop, false),
        (false, None) => (Hop::Server, true),
    };
    let refused = matches!(failure.reason, Reason::Authentication | Reason::Permission);
    let headline = match hop {
        _ if outdated => format!("{host} runs an older T3 Code"),
        Hop::Device => "This device could not prepare the connection".into(),
        Hop::Server if refused => format!("T3 Code on {host} did not accept this device"),
        Hop::Server => format!("T3 Code on {host} did not answer"),
    };
    let steps = match hop {
        _ if outdated => vec![format!("Update T3 Code on {host}.")],
        Hop::Device => vec![
            "Restart T3 Code on this device.".into(),
            format!("If it keeps failing, remove {host} from this device and add it again."),
        ],
        Hop::Server => {
            vec![
                format!("Make sure T3 Code is running on {host}."),
                "Retry. If it keeps failing, copy the diagnostics.".into(),
            ]
        }
    };
    let failed = match hop {
        Hop::Device => 0,
        Hop::Server => 1,
    };
    let hops = ["This device".to_owned(), format!("T3 Code on {host}")]
        .into_iter()
        .enumerate()
        .map(|(index, label)| {
            let result = if index == failed {
                HopResult::Failed
            } else if index < failed && !guessed {
                HopResult::Ok
            } else {
                HopResult::Unchecked
            };
            (label, result)
        })
        .collect();
    let mut facts = Vec::new();
    let mut add = |label: &'static str, value: Option<String>| {
        if let Some(value) = value.filter(|value| !value.is_empty()) {
            facts.push((label, value));
        }
    };
    add("Cause", Some(if outdated { "outdated".into() } else { failure.reason.as_str().into() }));
    add("Attempts", (state.attempt > 0).then(|| state.attempt.to_string()));
    add("Next retry", state.retry_at.and_then(iso_time));
    add("Route", Some("Direct".into()));
    add("App version", Some(env!("CARGO_PKG_VERSION").into()));
    add("Host version", server_version.map(str::to_owned));
    add("Error", Some(failure.detail.clone()));
    Some(Diagnosis { headline, hops, steps, facts })
}

/// `DateTime.formatIso`.
fn iso_time(epoch_ms: i64) -> Option<String> {
    chrono::DateTime::from_timestamp_millis(epoch_ms)
        .map(|time| time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
}

/// `connectionDiagnosisText`: one plain-text block for bug reports.
pub fn diagnosis_text(host: &str, diagnosis: &Diagnosis) -> String {
    let mut lines = vec![format!("T3 Code connection to {host}: {}", diagnosis.headline), String::new()];
    lines.extend(diagnosis.hops.iter().map(|(label, result)| {
        let mark = match result {
            HopResult::Ok => "ok",
            HopResult::Failed => "FAILED",
            HopResult::Unchecked => "not checked",
        };
        format!("{label}: {mark}")
    }));
    lines.push(String::new());
    lines.extend(diagnosis.facts.iter().map(|(label, value)| format!("{label}: {value}")));
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairing_urls_fill_host_and_code() {
        assert_eq!(
            parse_pairing_url("http://127.0.0.1:3999/pair#token=ABC123"),
            Some(("http://127.0.0.1:3999".into(), "ABC123".into()))
        );
        assert_eq!(parse_pairing_url("box.ts.net/?token=XY"), Some(("https://box.ts.net".into(), "XY".into())));
        // A hosted pairing page names the backend in `host`.
        assert_eq!(
            parse_pairing_url("https://app.t3.codes/pair?host=box.example.com&label=Box#token=T1"),
            Some(("box.example.com".into(), "T1".into()))
        );
        // An empty hash token falls back to the query.
        assert_eq!(parse_pairing_url("https://h/pair?token=Q#token="), Some(("https://h".into(), "Q".into())));
        assert_eq!(parse_pairing_url("backend.example.com"), None);
        assert_eq!(parse_pairing_url("   "), None);
    }

    #[test]
    fn remote_pairing_fields_need_a_host_and_a_code() {
        assert_eq!(parse_remote_pairing_fields("", "CODE"), Err("Enter a backend host."));
        assert_eq!(parse_remote_pairing_fields("box", " "), Err("Enter a pairing code."));
        assert_eq!(parse_remote_pairing_fields(" box ", " C "), Ok(("box".into(), "C".into())));
        assert_eq!(parse_remote_pairing_fields("http://h:1/#token=T", ""), Ok(("http://h:1".into(), "T".into())));
    }

    #[test]
    fn pairing_hosts_normalize_to_an_http_base() {
        assert_eq!(resolve_pairing_host("box.example.com", "C"), Ok("https://box.example.com".into()));
        assert_eq!(resolve_pairing_host("//box:8080/some/path?x=1#y", "C"), Ok("https://box:8080".into()));
        assert_eq!(resolve_pairing_host("http://127.0.0.1:3999/", "C"), Ok("http://127.0.0.1:3999".into()));
        assert_eq!(resolve_pairing_host("wss://box.example.com", "C"), Ok("https://box.example.com".into()));
        assert_eq!(resolve_pairing_host("ws://box:80", "C"), Ok("http://box".into()));
        assert_eq!(resolve_pairing_host("ftp://box", "C"), Err("Backend URL is invalid."));
        assert_eq!(resolve_pairing_host("  ", "C"), Err("Enter a backend URL."));
        assert_eq!(resolve_pairing_host("box", ""), Err("Enter a pairing code."));
    }

    fn descriptor(version: Option<u64>, capabilities: Value) -> Descriptor {
        Descriptor {
            environment_id: "env".into(),
            label: "Box".into(),
            server_version: None,
            orchestration_protocol_version: version,
            platform: Value::Null,
            capabilities,
        }
    }

    #[test]
    fn protocol_compatibility_matches_the_web() {
        assert_eq!(descriptor(Some(2), json!({})).compatibility_error(), None);
        let newer = descriptor(Some(3), json!({})).compatibility_error().unwrap();
        assert_eq!(newer.reason, Reason::Unsupported);
        assert!(newer.detail.starts_with("This client is not supported by this server."));
        let older = descriptor(None, json!({})).compatibility_error().unwrap();
        assert_eq!(older.detail, "This client requires a newer server. Update T3 Code on Box to connect.");
        assert!(!older.server_update_required);
        assert!(
            descriptor(Some(1), json!({"serverSelfUpdate": "npm"}))
                .compatibility_error()
                .unwrap()
                .server_update_required
        );
        let managed = json!({"serverSelfUpdate": "desktop-managed"});
        assert!(!descriptor(Some(1), managed).compatibility_error().unwrap().server_update_required);
        let managed = json!({"serverSelfUpdate": "desktop-managed", "desktopAppUpdate": true});
        assert!(descriptor(Some(1), managed).compatibility_error().unwrap().server_update_required);
    }

    #[test]
    fn retry_delays_double_up_to_five_minutes() {
        assert_eq!(retry_delay_ms(0, 0.), 1_000);
        assert_eq!(retry_delay_ms(0, 1.), 2_000);
        assert_eq!(retry_delay_ms(2, 0.5), 6_000);
        assert_eq!(retry_delay_ms(20, 1.), 300_000);
        assert_eq!(retry_delay_ms(u32::MAX, 0.), 150_000);
    }

    fn state(phase: Phase, attempt: u32, failure: Option<Failure>) -> ConnectionState {
        ConnectionState { phase, attempt, last_failure: failure, retry_at: None }
    }

    #[test]
    fn saved_status_reads_like_the_web_row() {
        let refused = Failure::http(401, "x");
        assert_eq!(state(Phase::Connected, 1, None).saved_status(false), ("Off".into(), false));
        assert_eq!(state(Phase::Connecting, 1, None).saved_status(true), ("Connecting".into(), false));
        assert_eq!(
            state(Phase::Backoff, 2, Some(Failure::transport("closed"))).saved_status(true),
            ("Reconnecting: closed".into(), true)
        );
        assert_eq!(
            state(Phase::Blocked, 1, Some(refused.clone())).saved_status(true),
            ("Connection failed: The environment credential is invalid.".into(), true)
        );
        assert_eq!(
            state(Phase::Blocked, 1, Some(refused)).status_text(),
            "Connection failed. Reason: The environment credential is invalid."
        );
        let unsupported = descriptor(Some(3), json!({})).compatibility_error();
        assert_eq!(state(Phase::Blocked, 1, unsupported).saved_status(false), ("Client not supported".into(), false));
    }

    #[test]
    fn diagnosis_names_the_failed_hop() {
        assert_eq!(diagnose("Box", &state(Phase::Connecting, 1, None), None), None);
        let refused = state(Phase::Blocked, 1, Some(Failure::http(401, "x")));
        let diagnosis = diagnose("Box", &refused, Some("1.2.3")).unwrap();
        assert_eq!(diagnosis.headline, "T3 Code on Box did not accept this device");
        assert_eq!(
            diagnosis.hops,
            vec![("This device".into(), HopResult::Ok), ("T3 Code on Box".into(), HopResult::Failed)]
        );
        assert_eq!(diagnosis.facts[0], ("Cause", "authentication".into()));
        assert!(diagnosis.facts.contains(&("Host version", "1.2.3".into())));

        let mut backoff = state(Phase::Backoff, 3, Some(Failure::environment_mismatch("a", "b")));
        backoff.retry_at = Some(0);
        let diagnosis = diagnose("Box", &backoff, None).unwrap();
        assert_eq!(diagnosis.headline, "T3 Code on Box did not answer");
        // No hop was named, so the device was never checked.
        assert_eq!(diagnosis.hops[0].1, HopResult::Unchecked);
        assert!(diagnosis.facts.contains(&("Next retry", "1970-01-01T00:00:00.000Z".into())));
        assert!(diagnosis.facts.contains(&("Attempts", "3".into())));

        let saved = SavedEnvironment {
            environment_id: "e".into(),
            label: "Box".into(),
            http_base: "https://box:4000".into(),
            enabled: true,
            token: None,
        };
        let missing = state(Phase::Blocked, 1, Some(Failure::credential_missing(&saved)));
        let diagnosis = diagnose("Box", &missing, None).unwrap();
        assert_eq!(diagnosis.headline, "This device could not prepare the connection");
        assert!(
            diagnosis
                .facts
                .contains(&("Error", "Connection credential bearer:e:https://box:4000 is unavailable.".into()))
        );
        let text = diagnosis_text("Box", &diagnosis);
        assert!(text.starts_with("T3 Code connection to Box: This device could not prepare the connection\n\nThis device: FAILED\nT3 Code on Box: not checked\n\nCause: authentication"));
    }

    #[cfg(windows)]
    #[test]
    fn catalog_documents_round_trip_with_encrypted_tokens() {
        let catalog = Catalog {
            local_environment_enabled: false,
            environments: vec![SavedEnvironment {
                environment_id: "env-1".into(),
                label: "Box".into(),
                http_base: "https://box".into(),
                enabled: false,
                token: Some("secret-token".into()),
            }],
        };
        let document = catalog.document().unwrap();
        assert!(!document.to_string().contains("secret-token"));
        assert_eq!(Catalog::from_document(&document), catalog);
        // An unreadable credential keeps the entry, without its token.
        let mut tampered = document.clone();
        tampered["environments"][0]["encryptedToken"] = json!("00ff");
        assert_eq!(Catalog::from_document(&tampered).environments[0].token, None);
        assert_eq!(Catalog::from_document(&json!({})), Catalog::default());
    }

    #[test]
    fn labels_must_not_be_empty() {
        assert_eq!(validate_label("  Box "), Ok("Box".into()));
        assert_eq!(validate_label("  "), Err("Environment label cannot be empty."));
    }
}
