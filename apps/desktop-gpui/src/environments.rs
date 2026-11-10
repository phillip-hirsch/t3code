//! Saved remote environments: the catalog this device keeps, pairing, and the connection
//! diagnosis Settings › Environments shows. Ports the bearer route of
//! `packages/client-runtime/src/connection/` (`onboarding.ts`, `compatibility.ts`,
//! `diagnosis.ts`, `presentation.ts`, `routes.ts`) and `resolveRemotePairingTarget` in
//! `packages/shared/src/remote.ts`. A saved environment keeps direct and SSH routes; T3 Connect
//! routes and routes learned from the server are not ported.
//!
//! Bearer tokens are stored encrypted by [`crate::secure_storage`] (the guarantee Electron's
//! `safeStorage` gives the desktop app's connection catalog).

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use futures::FutureExt as _;
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tokio::sync::{Notify, watch};
use url::Url;

use crate::backend::{self, Endpoint, LocalServer};
use crate::pull_request_routing as routing;
use crate::secure_storage as secure;
use crate::ssh::SshTarget;

/// `ORCHESTRATION_PROTOCOL_VERSION` in `packages/contracts`.
const ORCHESTRATION_PROTOCOL_VERSION: u64 = 2;
/// `DEFAULT_REMOTE_REQUEST_TIMEOUT_MS` in `authorization/remote.ts`.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const RETRY_BASE_DELAY_MS: f64 = 1_000.;
const RETRY_MAX_DELAY_MS: f64 = 300_000.;
const ROUTING_KEY: &str = "githubRoutingPermissions";

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
    EndpointUnavailable,
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
            Reason::EndpointUnavailable => "endpoint-unavailable",
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
    /// The HTTP status the server declared, when one did.
    pub http_status: Option<u16>,
}

impl Failure {
    fn new(reason: Reason, detail: impl Into<String>, hop: Option<Hop>) -> Self {
        Self { reason, detail: detail.into(), hop, server_update_required: false, http_status: None }
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
        Self { http_status: Some(status), ..Self::new(reason, detail, Some(Hop::Server)) }
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

    /// `credentialMissingError` for route `id`, or why its stored credential could not be decrypted.
    pub fn credential_missing(id: &str, credential: &Credential) -> Self {
        let detail = match credential {
            Credential::Locked { error, .. } => {
                let store = if cfg!(target_os = "macos") {
                    "the Keychain"
                } else if cfg!(windows) {
                    "your Windows profile"
                } else {
                    "the keyring"
                };
                format!(
                    "Connection credential {id} is saved, but this device could not decrypt it ({error}). It is kept: make {store} available to T3 Code, then retry."
                )
            }
            Credential::Missing | Credential::Token(_) => format!("Connection credential {id} is unavailable."),
        };
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

    /// No saved route answered (`endpoint-unavailable`).
    pub fn no_route_answered(label: &str) -> Self {
        Self::new(Reason::EndpointUnavailable, format!("{label} did not answer on any saved route."), None)
    }

    /// `sshPreparationError`: a cancelled prompt blocks; anything else is retried.
    pub fn ssh(message: &str) -> Self {
        if message.to_lowercase().contains("cancel") {
            return Self::new(Reason::Authentication, message, Some(Hop::Device));
        }
        Self::new(Reason::RemoteUnavailable, format!("Could not prepare the SSH environment: {message}"), None)
    }

    /// A blocked configuration problem, such as a route reaching another machine.
    pub fn configuration(detail: impl Into<String>) -> Self {
        Self::new(Reason::Configuration, detail, None)
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

/// `differentMachineError`: a route paired for one environment reached another.
pub fn different_machine(label: &str, what: &str) -> String {
    format!("That {what} reaches {label}, a different machine. Add it as its own environment instead.")
}

/// `assertKnownAddress`: a new environment's address must not add a route to a saved one.
fn assert_known_address(saved: &[SavedEnvironment], environment_id: &str, route: &Route) -> Result<(), String> {
    match saved.iter().find(|saved| saved.environment_id == environment_id) {
        Some(known) if !known.routes.iter().any(|r| r.address_key() == route.address_key()) => Err(format!(
            "That address claims to be {}, which is already saved. Add it as a route from that environment's settings instead.",
            known.label
        )),
        _ => Ok(()),
    }
}

/// `preparePairingRegistration` for a host and pairing code. `saved` is the catalog, so a code
/// for an address that would add a second route to a saved environment is refused unspent;
/// with `expected`, the code adds a route to that environment and any other one is refused.
/// Runs on the tokio runtime; errors are the dialog's message.
pub async fn pair(
    host: String,
    code: String,
    saved: Vec<SavedEnvironment>,
    expected: Option<String>,
) -> Result<SavedEnvironment, String> {
    let http_base = resolve_pairing_host(&host, &code)?;
    let descriptor = fetch_descriptor(&http_base).await.map_err(|failure| failure.detail)?;
    // Checked before redeeming the one-time code, so a wrong link is not spent.
    let route = Route::direct(http_base.clone(), Credential::Missing);
    match &expected {
        Some(expected) if *expected != descriptor.environment_id => {
            return Err(different_machine(&descriptor.label, "address"));
        }
        Some(_) => {}
        None => assert_known_address(&saved, &descriptor.environment_id, &route)?,
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
        enabled: true,
        routes: vec![Route { credential: Credential::Token(token), ..route }],
    })
}

/// An environment paired over SSH: the server the forward reaches and the token it issued.
pub struct ProvisionedSsh {
    pub environment_id: String,
    pub label: String,
    /// The target as ssh resolved it.
    pub target: SshTarget,
    pub http_base: String,
    pub token: String,
}

/// `provisionDesktopSshEnvironment`: starts or reuses T3 Code on `target`, forwards to it and
/// pairs with a credential minted there. With `expected`, a different machine is refused
/// before its credential is spent. Runs on the tokio runtime.
pub async fn provision_ssh(target: &SshTarget, expected: Option<&str>) -> Result<ProvisionedSsh, Failure> {
    let bootstrap = crate::ssh::ensure(target, true).await.map_err(|error| Failure::ssh(&error.message))?;
    let Some(pairing_token) = bootstrap.pairing_token else {
        let detail = "The SSH environment did not issue a pairing credential.";
        return Err(Failure::new(Reason::Authentication, detail, Some(Hop::Server)));
    };
    let descriptor = fetch_descriptor(&bootstrap.http_base).await?;
    if expected.is_some_and(|expected| expected != descriptor.environment_id) {
        return Err(Failure::configuration(different_machine(&descriptor.label, "host")));
    }
    let token = redeem_pairing_code(&bootstrap.http_base, &pairing_token).await.map_err(|failure| {
        // A reused server on another build can fail to decode the grant this build's runner
        // minted, and answers 500 on every retry.
        let version = crate::ssh::app_version();
        match &descriptor.server_version {
            Some(server) if failure.http_status == Some(500) && *server != version => {
                let label = &descriptor.label;
                Failure::configuration(format!(
                    "{label} runs T3 Code {server}, but this desktop is {version}, and that server could not accept its pairing. Restart the T3 server on {label} so it runs {version}, then retry."
                ))
            }
            _ => failure,
        }
    })?;
    let label = match descriptor.label.trim() {
        "" => bootstrap.target.alias.clone(),
        label => label.to_owned(),
    };
    Ok(ProvisionedSsh {
        environment_id: descriptor.environment_id,
        label,
        target: bootstrap.target,
        http_base: bootstrap.http_base,
        token,
    })
}

/// `registerSsh`'s preparation: an environment reached over SSH, for the add dialog to save.
pub async fn pair_ssh(target: SshTarget, expected: Option<String>) -> Result<SavedEnvironment, String> {
    let provisioned = provision_ssh(&target, expected.as_deref()).await.map_err(|failure| failure.detail)?;
    Ok(SavedEnvironment {
        environment_id: provisioned.environment_id,
        label: provisioned.label,
        enabled: true,
        routes: vec![Route {
            address: Address::Ssh(provisioned.target),
            credential: Credential::Token(provisioned.token),
        }],
    })
}

/// `fetchRemoteSessionState`: whether `token` is still signed in there, and what it may do.
pub async fn session_grants(http_base: &str, token: &str) -> Result<(bool, Vec<String>), Failure> {
    let url = format!("{http_base}/api/auth/session");
    let request = backend::http_client().get(&url).bearer_auth(token).timeout(REQUEST_TIMEOUT);
    let response = request.send().await.map_err(|error| Failure::request(error, &url))?;
    let status = response.status();
    if !status.is_success() {
        return Err(Failure::http(status.as_u16(), &url));
    }
    let body: Value = response.json().await.map_err(|_| Failure::invalid_json(&url))?;
    let granted = [&body["permissions"], &body["scopes"]]
        .into_iter()
        .find_map(Value::as_array)
        .map(|scopes| scopes.iter().filter_map(Value::as_str).map(str::to_owned).collect())
        .unwrap_or_default();
    Ok((body["authenticated"] == Value::Bool(true), granted))
}

// ---------------------------------------------------------------------------------------------
// Routes

/// `ConnectionRouteKind`, for the routes this client saves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RouteKind {
    Loopback,
    Lan,
    Tailnet,
    Vpn,
    Public,
    Ssh,
}

impl RouteKind {
    /// `ROUTE_KIND_RANK`: nearer routes go first.
    fn rank(self) -> u8 {
        match self {
            RouteKind::Loopback => 0,
            RouteKind::Lan => 1,
            RouteKind::Tailnet | RouteKind::Vpn => 2,
            RouteKind::Public => 3,
            RouteKind::Ssh => 4,
        }
    }
}

/// Where a route reaches the environment.
#[derive(Clone, Debug, PartialEq)]
pub enum Address {
    /// `https://host:port`, no trailing slash.
    Direct(String),
    /// A loopback forward the system ssh opens (`SshConnectionProfile`).
    Ssh(SshTarget),
}

/// `ConnectionRoute`: one way to reach a saved environment, with the token paired over it.
#[derive(Clone, Debug, PartialEq)]
pub struct Route {
    pub address: Address,
    pub credential: Credential,
}

impl Route {
    pub fn direct(http_base: String, credential: Credential) -> Self {
        Self { address: Address::Direct(http_base), credential }
    }

    /// `connectionRouteId`: `bearer:<id>:<origin>` or `ssh:<id>:<target key>`.
    pub fn id(&self, environment_id: &str) -> String {
        match &self.address {
            Address::Direct(http_base) => {
                let origin = Url::parse(http_base).map(|url| url.origin().ascii_serialization()).unwrap_or_default();
                format!("bearer:{environment_id}:{origin}")
            }
            Address::Ssh(target) => format!("ssh:{environment_id}:{}", target.key()),
        }
    }

    pub fn http_base(&self) -> Option<&str> {
        match &self.address {
            Address::Direct(http_base) => Some(http_base),
            Address::Ssh(_) => None,
        }
    }

    fn hostname(&self) -> Option<String> {
        let url = Url::parse(self.http_base()?).ok()?;
        Some(normalize_hostname(url.host_str()?))
    }

    /// `connectionRouteKind`.
    pub fn kind(&self) -> RouteKind {
        if matches!(self.address, Address::Ssh(_)) {
            return RouteKind::Ssh;
        }
        match self.hostname() {
            None => RouteKind::Public,
            Some(host) if is_loopback_host(&host) => RouteKind::Loopback,
            Some(host) if is_tailnet_host(&host) => RouteKind::Tailnet,
            Some(host) if is_shared_address_space_host(&host) => RouteKind::Vpn,
            Some(host) if is_private_network_host(&host) => RouteKind::Lan,
            Some(_) => RouteKind::Public,
        }
    }

    /// `connectionRouteLabel`.
    pub fn label(&self) -> String {
        match (self.kind(), &self.address) {
            (RouteKind::Loopback, _) => "This device".into(),
            (RouteKind::Lan, _) => "LAN".into(),
            (RouteKind::Tailnet, _) => "Tailscale".into(),
            (RouteKind::Vpn, _) => "VPN".into(),
            (RouteKind::Ssh, Address::Ssh(target)) => match &target.username {
                Some(user) => format!("SSH {user}@{}", target.hostname),
                None => format!("SSH {}", target.hostname),
            },
            (RouteKind::Ssh, _) => "SSH".into(),
            (RouteKind::Public, _) => self.hostname().unwrap_or_else(|| "Remote link".into()),
        }
    }

    /// `connectionRouteAddress`: the base URL, or the SSH alias.
    pub fn address_text(&self) -> String {
        match &self.address {
            Address::Direct(http_base) => http_base.clone(),
            Address::Ssh(target) => target.alias.clone(),
        }
    }

    /// `routeAddressKey`: two routes with the same key reach the same address.
    fn address_key(&self) -> String {
        match &self.address {
            Address::Direct(http_base) => format!("bearer:{}", http_base.trim_end_matches('/')),
            Address::Ssh(target) => format!("ssh:{}", target.key()),
        }
    }
}

/// `pendingRouteOrder`: an order still being saved, while it lists the saved routes and
/// differs from their saved order.
pub fn pending_route_order<'a>(pending: Option<&'a [String]>, saved: &[String]) -> Option<&'a [String]> {
    let pending = pending.filter(|pending| pending.len() == saved.len())?;
    let listed = pending.iter().all(|id| saved.contains(id));
    (listed && pending != saved).then_some(pending)
}

/// `arrayMove`: `order` with the item at `from` moved to `to`.
pub fn move_route(order: &[String], from: usize, to: usize) -> Vec<String> {
    let mut moved = order.to_vec();
    if from < moved.len() && to < moved.len() {
        let item = moved.remove(from);
        moved.insert(to, item);
    }
    moved
}

/// `upsertRoute`: replaces the route with the same id, else inserts the new one first among
/// its kind, so a freshly paired address connects before an older one of the same kind.
fn upsert_route(routes: &mut Vec<Route>, route: Route, environment_id: &str) {
    let id = route.id(environment_id);
    if let Some(existing) = routes.iter_mut().find(|existing| existing.id(environment_id) == id) {
        *existing = route;
        return;
    }
    let (kind, rank) = (route.kind(), route.kind().rank());
    let index = routes.iter().position(|existing| existing.kind().rank() > rank || existing.kind() == kind);
    routes.insert(index.unwrap_or(routes.len()), route);
}

/// `normalizeHostname`: lower case, no IPv6 brackets or trailing dots.
fn normalize_hostname(host: &str) -> String {
    host.to_lowercase().trim_start_matches('[').trim_end_matches(']').trim_end_matches('.').to_owned()
}

fn ipv4(host: &str) -> Option<[u8; 4]> {
    let parts: Vec<_> = host.split('.').collect();
    if parts.len() != 4 {
        return None;
    }
    let mut octets = [0; 4];
    for (octet, part) in octets.iter_mut().zip(parts) {
        // `Number(part)`: plain decimal digits only.
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        *octet = part.parse().ok()?;
    }
    Some(octets)
}

/// `isLocalLoopbackHost`.
fn is_loopback_host(host: &str) -> bool {
    host == "localhost" || host == "::1" || ipv4(host).is_some_and(|ip| ip[0] == 127)
}

/// `isTailnetHost`: MagicDNS or Tailscale's `fd7a:115c:a1e0::/48`.
fn is_tailnet_host(host: &str) -> bool {
    host.ends_with(".ts.net")
        || host.parse::<std::net::Ipv6Addr>().is_ok_and(|ip| ip.segments()[..3] == [0xfd7a, 0x115c, 0xa1e0])
}

/// `isSharedAddressSpaceHost`: 100.64.0.0/10, which any VPN may use.
fn is_shared_address_space_host(host: &str) -> bool {
    ipv4(host).is_some_and(|ip| ip[0] == 100 && (64..=127).contains(&ip[1]))
}

/// `isPrivateNetworkHost`.
fn is_private_network_host(host: &str) -> bool {
    let local_name = host.ends_with(".localhost")
        || host.ends_with(".local")
        || host == "home.arpa"
        || host.ends_with(".home.arpa")
        || host.ends_with(".ts.net")
        || (!host.contains('.') && !host.contains(':'));
    if host == "::" || is_loopback_host(host) || local_name {
        return true;
    }
    let mapped = host.strip_prefix("::ffff:").and_then(|rest| {
        ipv4(rest).or_else(|| host.parse::<std::net::Ipv6Addr>().ok()?.to_ipv4_mapped().map(|ip| ip.octets()))
    });
    if let Some(ip) = ipv4(host).or(mapped) {
        return matches!(ip, [0 | 10 | 127, ..] | [192, 168, ..] | [169, 254, ..])
            || (ip[0] == 100 && (64..=127).contains(&ip[1]))
            || (ip[0] == 172 && (16..=31).contains(&ip[1]))
            || (ip[0] == 198 && (18..=19).contains(&ip[1]));
    }
    let first = host.split(':').next().unwrap_or_default();
    host.contains(':')
        && (1..=4).contains(&first.len())
        && u16::from_str_radix(first, 16).is_ok_and(|hextet| hextet & 0xfe00 == 0xfc00 || hextet & 0xffc0 == 0xfe80)
}

// ---------------------------------------------------------------------------------------------
// Catalog

/// A paired environment: one identity, reached over its routes, preferred first
/// (`ConnectionCatalogEntry` with `alternateRoutes`).
#[derive(Clone, Debug, PartialEq)]
pub struct SavedEnvironment {
    pub environment_id: String,
    pub label: String,
    pub enabled: bool,
    /// Never empty: removing the last route forgets the environment.
    pub routes: Vec<Route>,
}

/// A saved environment's bearer token as this device holds it. `Debug` never prints the token.
#[derive(Clone, PartialEq)]
pub enum Credential {
    /// Nothing is stored.
    Missing,
    Token(String),
    /// Stored, but this run could not decrypt it: the keyring was locked, denied access or is
    /// unavailable. Saving writes `stored` back unchanged, so a later run can still read it.
    Locked {
        stored: String,
        error: String,
    },
}

impl fmt::Debug for Credential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => f.write_str("Missing"),
            Self::Token(_) => f.write_str("Token(<redacted>)"),
            Self::Locked { error, .. } => f.debug_struct("Locked").field("error", error).finish_non_exhaustive(),
        }
    }
}

impl Credential {
    /// Tries a locked credential's decryption again, e.g. once the keyring is unlocked.
    /// Blocks on the platform store, so it runs off the UI thread.
    pub fn reopen(&self) -> Option<Self> {
        match self {
            Self::Locked { stored, .. } => Some(open_credential(stored.clone(), secure::unprotect)),
            Self::Missing | Self::Token(_) => None,
        }
    }
}

impl SavedEnvironment {
    pub fn route(&self, route_id: &str) -> Option<&Route> {
        self.routes.iter().find(|route| route.id(&self.environment_id) == route_id)
    }

    pub fn route_ids(&self) -> Vec<String> {
        self.routes.iter().map(|route| route.id(&self.environment_id)).collect()
    }

    /// `environmentMcpUrl`: on the connected route's address first, then the saved order. An
    /// SSH forward is a loopback port of this run, so it never names one.
    pub fn mcp_url(&self, connected_route: Option<&str>) -> Option<String> {
        let connected = connected_route.and_then(|id| self.route(id));
        connected.into_iter().chain(&self.routes).find_map(Route::http_base).map(|base| format!("{base}/mcp"))
    }

    /// `environmentTransportLabel`: how the row says it is reached.
    pub fn transport_label(&self, active_route: Option<&str>) -> String {
        if self.routes.len() > 1 {
            return match active_route.and_then(|id| self.route(id)) {
                Some(active) => format!("via {}", active.label()),
                None => self.routes[0].label(),
            };
        }
        match self.routes.first().map(|route| &route.address) {
            Some(Address::Ssh(target)) => format!("SSH {}", target.display()),
            Some(Address::Direct(http_base)) => http_base.clone(),
            None => "Remote link".into(),
        }
    }
}

/// What this device keeps: the saved environments and the desktop app's
/// `localEnvironmentEnabled` setting.
#[derive(Clone, Debug, PartialEq)]
pub struct Catalog {
    pub local_environment_enabled: bool,
    pub environments: Vec<SavedEnvironment>,
    /// This device's pull request routing grants (`githubRoutingPermissions`, the web catalog's
    /// field name), including the primary environment's.
    pub pull_request_routing: Vec<routing::Stored>,
    /// The document as read. Saving writes back its fields this build does not know and the
    /// entries it could not decode, so an older or newer build loses nothing.
    stored: Map<String, Value>,
}

impl Default for Catalog {
    fn default() -> Self {
        Self {
            local_environment_enabled: true,
            environments: Vec::new(),
            pull_request_routing: Vec::new(),
            stored: Map::new(),
        }
    }
}

/// The catalog the app shows and edits, from [`Catalog::open`].
pub struct Opened {
    pub catalog: Catalog,
    /// The file could not be read, so saving would destroy it: saves are refused.
    pub read_only: bool,
    /// What went wrong reading the file, for Settings › Environments.
    pub problem: Option<String>,
}

/// Why `environments.json` could not be used.
#[derive(Debug, PartialEq)]
enum Damage {
    /// Not a catalog: invalid JSON, or the wrong shape.
    Corrupt(String),
    /// Not readable at all.
    Unreadable(String),
}

/// One saved environment as `environments.json` holds it. `routes` lists every route, preferred
/// first; an entry from before routes holds one direct route in `httpBaseUrl` and
/// `encryptedToken`. Saving mirrors the first direct route there, so an older build still
/// connects, and leaves them out without one, so an older build keeps the entry untouched.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredEnvironment {
    environment_id: String,
    label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    http_base_url: Option<String>,
    #[serde(default = "enabled_default")]
    enabled: bool,
    /// The bearer token, encrypted by [`secure`], hex.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    encrypted_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    routes: Option<Vec<StoredRoute>>,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
enum StoredRoute {
    Direct {
        http_base_url: String,
        #[serde(default)]
        encrypted_token: String,
    },
    Ssh {
        alias: String,
        hostname: String,
        #[serde(default)]
        username: Option<String>,
        #[serde(default)]
        port: Option<u16>,
        #[serde(default)]
        encrypted_token: String,
    },
}

fn enabled_default() -> bool {
    true
}

impl StoredEnvironment {
    /// The entry, when this build can read it and it has a route.
    fn decode(entry: &Value) -> Option<Self> {
        let decoded = Self::deserialize(entry)
            .inspect_err(|error| log::warn!("keeping a saved environment this build cannot read: {error}"))
            .ok()?;
        let has_route =
            decoded.routes.as_ref().is_some_and(|routes| !routes.is_empty()) || decoded.http_base_url.is_some();
        has_route.then_some(decoded)
    }

    /// Its routes, reading a pre-routes entry as one direct route.
    fn into_routes(self, unprotect: impl Fn(&[u8]) -> Result<Vec<u8>, String>) -> Vec<Route> {
        let stored = match self.routes.filter(|routes| !routes.is_empty()) {
            Some(routes) => routes,
            None => vec![StoredRoute::Direct {
                http_base_url: self.http_base_url.unwrap_or_default(),
                encrypted_token: self.encrypted_token.unwrap_or_default(),
            }],
        };
        stored
            .into_iter()
            .map(|route| match route {
                StoredRoute::Direct { http_base_url, encrypted_token } => Route {
                    address: Address::Direct(http_base_url.trim_end_matches('/').to_owned()),
                    credential: open_credential(encrypted_token, &unprotect),
                },
                StoredRoute::Ssh { alias, hostname, username, port, encrypted_token } => Route {
                    address: Address::Ssh(SshTarget { alias, hostname, username, port }),
                    credential: open_credential(encrypted_token, &unprotect),
                },
            })
            .collect()
    }
}

/// A credential as stored: encrypted hex, kept as read when locked, empty when missing.
fn seal(credential: &Credential, protect: &impl Fn(&[u8]) -> Result<Vec<u8>, String>) -> anyhow::Result<String> {
    Ok(match credential {
        Credential::Token(token) => {
            let sealed =
                protect(token.as_bytes()).map_err(|error| anyhow::anyhow!("could not encrypt a token: {error}"))?;
            backend::hex(&sealed)
        }
        Credential::Locked { stored, .. } => stored.clone(),
        Credential::Missing => String::new(),
    })
}

impl Catalog {
    fn path() -> Option<PathBuf> {
        Some(crate::settings::userdata_dir()?.join("environments.json"))
    }

    /// Reads `<T3 home>/userdata/environments.json` to look something up; a missing or
    /// unusable file is empty. Blocks on the file and the platform store.
    pub fn load() -> Self {
        let Some(path) = Self::path() else { return Self::default() };
        read_at(&path, secure::unprotect)
            .unwrap_or_else(|damage| {
                log::warn!("{}: {damage:?}", path.display());
                None
            })
            .unwrap_or_default()
    }

    /// Reads the catalog to show and edit, once per run. A file that is not a catalog is moved
    /// aside to `environments.corrupt-<ms>.json` first, so a save cannot destroy it; one that
    /// cannot be read makes the catalog read-only. Blocks on the file and the platform store.
    pub fn open() -> Opened {
        match Self::path() {
            Some(path) => open_at(&path, secure::unprotect, crate::timestamp::now_ms()),
            None => Opened { catalog: Self::default(), read_only: false, problem: None },
        }
    }

    /// `unprotect` decrypts a stored token ([`secure::unprotect`] outside tests). The error
    /// says why `document` is not a catalog.
    fn from_document(document: Value, unprotect: impl Fn(&[u8]) -> Result<Vec<u8>, String>) -> Result<Self, String> {
        let Value::Object(stored) = document else { return Err("the file is not a JSON object".into()) };
        let entries = match stored.get("environments") {
            None => &[][..],
            Some(Value::Array(entries)) => entries.as_slice(),
            Some(_) => return Err("`environments` is not a list".into()),
        };
        let environments = entries
            .iter()
            .filter_map(StoredEnvironment::decode)
            .map(|entry| SavedEnvironment {
                environment_id: entry.environment_id.clone(),
                label: entry.label.clone(),
                enabled: entry.enabled,
                routes: entry.into_routes(&unprotect),
            })
            .collect();
        let local_environment_enabled = stored.get("localEnvironmentEnabled") != Some(&Value::Bool(false));
        let pull_request_routing = stored
            .get(ROUTING_KEY)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|grant| routing::Stored::deserialize(grant).ok())
            .collect();
        Ok(Self { local_environment_enabled, environments, pull_request_routing, stored })
    }

    /// One environment's entry; `read` is the entry as read, whose unknown fields stay.
    fn entry(
        environment: &SavedEnvironment,
        read: Option<&Map<String, Value>>,
        protect: &impl Fn(&[u8]) -> Result<Vec<u8>, String>,
    ) -> anyhow::Result<Value> {
        let mut mirror = None;
        let routes = environment
            .routes
            .iter()
            .map(|route| {
                let encrypted_token = seal(&route.credential, protect)?;
                Ok(match &route.address {
                    Address::Direct(http_base) => {
                        mirror.get_or_insert_with(|| (http_base.clone(), encrypted_token.clone()));
                        StoredRoute::Direct { http_base_url: http_base.clone(), encrypted_token }
                    }
                    Address::Ssh(target) => StoredRoute::Ssh {
                        alias: target.alias.clone(),
                        hostname: target.hostname.clone(),
                        username: target.username.clone(),
                        port: target.port,
                        encrypted_token,
                    },
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let (http_base_url, encrypted_token) = mirror.unzip();
        let known = serde_json::to_value(StoredEnvironment {
            environment_id: environment.environment_id.clone(),
            label: environment.label.clone(),
            http_base_url,
            enabled: environment.enabled,
            encrypted_token,
            routes: Some(routes),
        })
        .context("could not encode an environment")?;
        let mut entry = read.cloned().unwrap_or_default();
        // A mirror this save leaves out must not survive from the entry as read.
        entry.remove("httpBaseUrl");
        entry.remove("encryptedToken");
        if let Value::Object(known) = known {
            entry.extend(known);
        }
        Ok(Value::Object(entry))
    }

    /// `protect` encrypts a token ([`secure::protect`] outside tests).
    fn document(&self, protect: impl Fn(&[u8]) -> Result<Vec<u8>, String>) -> anyhow::Result<Value> {
        let read: &[Value] = self.stored.get("environments").and_then(Value::as_array).map_or(&[], Vec::as_slice);
        let id = |entry: &Value| entry.get("environmentId").and_then(Value::as_str).map(str::to_owned);
        let decodes = |entry: &Value| StoredEnvironment::decode(entry).is_some();
        let mut environments = self
            .environments
            .iter()
            .map(|environment| {
                // Start from the entry as read, so fields this build does not know stay.
                let read = read
                    .iter()
                    .find(|entry| id(entry).as_ref() == Some(&environment.environment_id) && decodes(entry))
                    .and_then(Value::as_object);
                Self::entry(environment, read, &protect)
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        // Entries this build could not decode stay as read, unless one it saves replaced it.
        let undecoded =
            read.iter().filter(|entry| !decodes(entry) && id(entry).is_none_or(|id| self.get(&id).is_none()));
        environments.extend(undecoded.cloned());
        let mut document = self.stored.clone();
        document.entry("version").or_insert(json!(1));
        document.insert("localEnvironmentEnabled".into(), json!(self.local_environment_enabled));
        document.insert("environments".into(), Value::Array(environments));
        if self.pull_request_routing.is_empty() {
            document.remove(ROUTING_KEY);
        } else {
            document.insert(ROUTING_KEY.into(), serde_json::to_value(&self.pull_request_routing)?);
        }
        Ok(Value::Object(document))
    }

    /// Writes the catalog. Blocks on the file and the platform store.
    pub fn save(&self) -> anyhow::Result<()> {
        let path = Self::path().context("No T3 home directory.")?;
        crate::settings::write_atomically(&path, &self.document(secure::protect)?)
            .with_context(|| format!("writing {}", path.display()))
    }

    pub fn get(&self, environment_id: &str) -> Option<&SavedEnvironment> {
        self.environments.iter().find(|environment| environment.environment_id == environment_id)
    }

    pub fn get_mut(&mut self, environment_id: &str) -> Option<&mut SavedEnvironment> {
        self.environments.iter_mut().find(|environment| environment.environment_id == environment_id)
    }

    /// `removeConnectionFromCatalog`: forgets the environment and the grants it was given.
    pub fn remove(&mut self, environment_id: &str) {
        self.environments.retain(|environment| environment.environment_id != environment_id);
        self.pull_request_routing.retain(|grant| grant.environment_id != environment_id);
    }

    /// `EnvironmentRegistry.register` for a paired route. A new environment is added; a saved
    /// one gains the route (replacing one to the same address) and is switched on. With
    /// `known_address_only`, a route to a new address of a saved environment is refused: that
    /// is "Add route", not "Add environment".
    pub fn register(&mut self, paired: SavedEnvironment, known_address_only: bool) -> Result<(), String> {
        let Some(saved) = self.get_mut(&paired.environment_id) else {
            self.environments.push(paired);
            return Ok(());
        };
        let id = saved.environment_id.clone();
        for route in paired.routes {
            if known_address_only {
                assert_known_address(std::slice::from_ref(saved), &id, &route)?;
            }
            let (key, route_id) = (route.address_key(), route.id(&id));
            saved.routes.retain(|existing| existing.address_key() != key || existing.id(&id) == route_id);
            upsert_route(&mut saved.routes, route, &id);
        }
        saved.enabled = true;
        Ok(())
    }

    /// `EnvironmentRegistry.removeRoute`; the last route takes the environment with it.
    /// Returns the removed route.
    pub fn remove_route(&mut self, environment_id: &str, route_id: &str) -> Option<Route> {
        let saved = self.get_mut(environment_id)?;
        let index = saved.routes.iter().position(|route| route.id(environment_id) == route_id)?;
        let removed = saved.routes.remove(index);
        if saved.routes.is_empty() {
            self.remove(environment_id);
        }
        Some(removed)
    }

    /// `EnvironmentRegistry.reorderRoutes`: `route_ids` must list every saved route once.
    pub fn reorder_routes(&mut self, environment_id: &str, route_ids: &[String]) -> Result<(), &'static str> {
        let saved = self.get_mut(environment_id).ok_or("The environment is no longer saved.")?;
        let ids = saved.route_ids();
        let unique: std::collections::HashSet<_> = route_ids.iter().collect();
        if route_ids.len() != ids.len() || unique.len() != ids.len() || !ids.iter().all(|id| unique.contains(id)) {
            return Err("The route order must list every saved route once.");
        }
        let mut routes: Vec<Option<Route>> = std::mem::take(&mut saved.routes).into_iter().map(Some).collect();
        saved.routes = route_ids
            .iter()
            .filter_map(|id| ids.iter().position(|saved| saved == id).and_then(|index| routes[index].take()))
            .collect();
        Ok(())
    }
}

/// A stored `encryptedToken`: empty is no credential; one that does not decrypt stays as stored.
fn open_credential(stored: String, unprotect: impl Fn(&[u8]) -> Result<Vec<u8>, String>) -> Credential {
    if stored.is_empty() {
        return Credential::Missing;
    }
    let opened = unhex(&stored)
        .ok_or_else(|| "The stored credential is not valid hex.".to_owned())
        .and_then(|blob| unprotect(&blob))
        .and_then(|bytes| String::from_utf8(bytes).map_err(|_| "The stored credential is not text.".to_owned()));
    match opened {
        Ok(token) => Credential::Token(token),
        Err(error) => Credential::Locked { stored, error },
    }
}

/// The catalog at `path`; `None` when there is no file.
fn read_at(path: &Path, unprotect: impl Fn(&[u8]) -> Result<Vec<u8>, String>) -> Result<Option<Catalog>, Damage> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(Damage::Unreadable(error.to_string())),
    };
    let document = serde_json::from_slice(&bytes).map_err(|error| Damage::Corrupt(error.to_string()))?;
    Catalog::from_document(document, unprotect).map(Some).map_err(Damage::Corrupt)
}

/// [`Catalog::open`] for the file at `path`; `now_ms` names a corrupt file moved aside.
fn open_at(path: &Path, unprotect: impl Fn(&[u8]) -> Result<Vec<u8>, String>, now_ms: i64) -> Opened {
    let kept = "Changes to environments are not saved, so the file is kept.";
    let (read_only, problem) = match read_at(path, unprotect) {
        Ok(catalog) => return Opened { catalog: catalog.unwrap_or_default(), read_only: false, problem: None },
        Err(Damage::Unreadable(error)) => {
            log::warn!("could not read {}: {error}", path.display());
            (true, format!("Saved environments could not be read ({error}). {kept}"))
        }
        Err(Damage::Corrupt(error)) => {
            log::warn!("{} is not an environments catalog: {error}", path.display());
            let aside = path.with_file_name(format!("environments.corrupt-{now_ms}.json"));
            match std::fs::rename(path, &aside) {
                Ok(()) => (
                    false,
                    format!(
                        "Saved environments could not be read ({error}). The file was moved to {}.",
                        aside.display()
                    ),
                ),
                Err(rename) => {
                    log::warn!("could not move {} aside: {rename}", path.display());
                    (true, format!("Saved environments could not be read ({error}). {kept}"))
                }
            }
        }
    };
    Opened { catalog: Catalog::default(), read_only, problem: Some(problem) }
}

/// `prepareBearerConnectionUpdate`'s label check.
pub fn validate_label(label: &str) -> Result<String, &'static str> {
    let label = label.trim();
    if label.is_empty() { Err("Environment label cannot be empty.") } else { Ok(label.to_owned()) }
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if text.is_empty() || !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok()).collect()
}

/// The workspace's environment: `T3_SERVER_URL`, else the local server, or the first enabled
/// saved environment while the local environment is off (Electron then starts no server).
pub async fn resolve_workspace_endpoint() -> anyhow::Result<(Endpoint, Option<LocalServer>)> {
    let overridden = std::env::var_os("T3_SERVER_URL").is_some() && std::env::var_os("T3_SERVER_TOKEN").is_some();
    if !overridden {
        // File IO and decrypting every token would block one of the runtime's two workers.
        let catalog = tokio::task::spawn_blocking(Catalog::load).await.map_err(|error| {
            log::error!("reading saved environments failed: {error}");
            anyhow::anyhow!("Saved environments could not be read: {error}")
        })?;
        if !catalog.local_environment_enabled {
            return Ok((remote_workspace_endpoint(&catalog).await?, None));
        }
    }
    backend::resolve_endpoint().await
}

/// The routes the workspace may start through while the local environment is off: every
/// enabled environment's signed-in routes, in saved order.
fn remote_workspace_routes(catalog: &Catalog) -> Vec<(&str, &Address, &str)> {
    let enabled = catalog.environments.iter().filter(|environment| environment.enabled);
    enabled
        .flat_map(|environment| {
            environment.routes.iter().filter_map(|route| match &route.credential {
                Credential::Token(token) => Some((environment.environment_id.as_str(), &route.address, token.as_str())),
                Credential::Missing | Credential::Locked { .. } => None,
            })
        })
        .collect()
}

/// The first of [`remote_workspace_routes`] that answers; an SSH route starts its forward first.
async fn remote_workspace_endpoint(catalog: &Catalog) -> anyhow::Result<Endpoint> {
    let mut unreachable = None;
    for (environment_id, address, token) in remote_workspace_routes(catalog) {
        let http_base = match address {
            Address::Direct(http_base) => http_base.clone(),
            Address::Ssh(target) => match ssh_workspace_base(environment_id, target).await {
                Ok(http_base) => http_base,
                Err(error) => {
                    log::warn!("could not start through {}: {error}", target.alias);
                    unreachable = Some(error);
                    continue;
                }
            },
        };
        return Ok(Endpoint::bearer(&http_base, token.to_owned()));
    }
    match unreachable {
        // Retried: the host may answer on the next attempt.
        Some(error) => Err(anyhow::anyhow!(error)),
        None => Err(backend::Unrecoverable(
            "The local environment is off. Turn it on in Settings › Environments, or add an environment there.".into(),
        )
        .into()),
    }
}

/// `prepareDesktopSshEnvironment` with the saved token: the forward to `target`, once it
/// answers as `environment_id`.
async fn ssh_workspace_base(environment_id: &str, target: &SshTarget) -> Result<String, String> {
    let bootstrap = crate::ssh::ensure(target, false).await.map_err(|error| error.message)?;
    // Identity first, before the saved token is sent anywhere.
    let descriptor = fetch_descriptor(&bootstrap.http_base).await.map_err(|failure| failure.detail)?;
    if descriptor.environment_id != environment_id {
        return Err(different_machine(&descriptor.label, "host"));
    }
    Ok(bootstrap.http_base)
}

/// The workspace's environment for the whole app run, started once and shared by every
/// window, as Electron's backend manager outlives its windows: closing the window leaves this
/// app's server and its agents running. Like `DesktopBackendManager`, a server that exits or
/// fails to start is started again with a backoff; the server stops when the app quits.
#[derive(Clone)]
pub struct PrimaryBackend {
    shared: Arc<Shared>,
    status: watch::Receiver<BackendStatus>,
}

struct Shared {
    status: watch::Sender<BackendStatus>,
    /// Wakes a supervisor that waits to restart; see [`PrimaryBackend::restart`].
    restart: Notify,
}

#[derive(Clone)]
pub enum BackendStatus {
    Starting,
    Ready(Endpoint),
    /// It went down, with why; a restart is scheduled.
    Restarting(String),
    /// It keeps failing to start, or cannot be started again (another server owns the T3
    /// home). Only the first kind is still retried on its own.
    Failed(String),
}

/// One start of the backend, as [`supervise`] sees it.
enum Start {
    /// `exited` resolves with why this app's server stopped, or `None` once the app quits. It
    /// is `None` for a server this app does not run.
    Ready {
        endpoint: Endpoint,
        exited: Option<BoxFuture<'static, Option<String>>>,
    },
    /// `retry` is false when starting again cannot help.
    Failed {
        reason: String,
        retry: bool,
    },
    Quit,
}

/// `INITIAL_RESTART_DELAY` and `MAX_RESTART_DELAY` in `DesktopBackendManager.ts`.
const INITIAL_RESTART_DELAY: Duration = Duration::from_millis(500);
const MAX_RESTART_DELAY: Duration = Duration::from_secs(10);
/// `STARTUP_FAILURE_RESTART_ATTEMPTS`: restarts without readiness before the failure shows.
const STARTUP_FAILURE_RESTART_ATTEMPTS: u32 = 3;

impl gpui_kit::Global for PrimaryBackend {}

impl PrimaryBackend {
    pub fn start(runtime: &tokio::runtime::Handle) -> Self {
        let backend = Self::new();
        runtime.spawn(supervise(backend.shared.clone(), start_workspace));
        backend
    }

    fn new() -> Self {
        let (status, receiver) = watch::channel(BackendStatus::Starting);
        Self { shared: Arc::new(Shared { status, restart: Notify::new() }), status: receiver }
    }

    /// The status now; [`Self::changed`] then waits for the next one.
    pub fn status(&mut self) -> BackendStatus {
        self.status.borrow_and_update().clone()
    }

    pub async fn changed(&mut self) {
        // `shared` holds the sender, so the channel never closes.
        let _ = self.status.changed().await;
    }

    /// Starts a backend that is down again now, skipping the backoff (Reconnect, a new window).
    pub fn restart(&self) {
        let down = self.shared.status.send_if_modified(|status| {
            let down = matches!(status, BackendStatus::Restarting(_) | BackendStatus::Failed(_));
            if down {
                *status = BackendStatus::Starting;
            }
            down
        });
        if down {
            self.shared.restart.notify_waiters();
        }
    }
}

/// [`resolve_workspace_endpoint`] for [`supervise`].
async fn start_workspace() -> Start {
    if backend::quitting_now() {
        return Start::Quit;
    }
    match resolve_workspace_endpoint().await {
        Ok((endpoint, server)) => Start::Ready { endpoint, exited: server.map(|server| server.run().boxed()) },
        Err(_) if backend::quitting_now() => Start::Quit,
        Err(error) => Start::Failed {
            retry: error.downcast_ref::<backend::Unrecoverable>().is_none(),
            reason: format!("{error:#}"),
        },
    }
}

/// Starts the backend and keeps it running: `scheduleRestart` in `DesktopBackendManager.ts`.
async fn supervise<F, Fut>(shared: Arc<Shared>, mut start: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Start>,
{
    // Restarts since the server was last ready (`restartAttempt`).
    let mut attempt = 0;
    loop {
        let reason = match start().await {
            Start::Quit => return,
            Start::Ready { endpoint, exited } => {
                log::info!("server ready at {}", endpoint.http_base);
                attempt = 0;
                shared.status.send_replace(BackendStatus::Ready(endpoint));
                let Some(exited) = exited else { return };
                let Some(reason) = exited.await else { return };
                reason
            }
            Start::Failed { reason, retry: false } => {
                log::error!("{reason}");
                // Created before the status changes, so a restart that sees it wakes this.
                let restart = shared.restart.notified();
                shared.status.send_replace(BackendStatus::Failed(reason));
                restart.await;
                continue;
            }
            Start::Failed { reason, retry: true } => reason,
        };
        attempt += 1;
        let delay = restart_delay(attempt);
        log::warn!("{reason} Restarting in {delay:?}.");
        let restart = shared.restart.notified();
        shared.status.send_replace(if attempt >= STARTUP_FAILURE_RESTART_ATTEMPTS {
            BackendStatus::Failed(reason)
        } else {
            BackendStatus::Restarting(reason)
        });
        tokio::select! {
            () = tokio::time::sleep(delay) => {}
            () = restart => {}
        }
    }
}

/// `calculateRestartDelay` for the `attempt`th restart since the server was ready.
fn restart_delay(attempt: u32) -> Duration {
    INITIAL_RESTART_DELAY.saturating_mul(1 << attempt.saturating_sub(1).min(5)).min(MAX_RESTART_DELAY)
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
#[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "a delay capped at the maximum backoff")]
pub fn retry_delay_ms(failure_count: u32, random: f64) -> u64 {
    let ceiling =
        RETRY_MAX_DELAY_MS.min(RETRY_BASE_DELAY_MS * 2f64.powi(i32::try_from(failure_count.min(30)).unwrap_or(30) + 1));
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

    /// `connectionStatusTitle`: [`Self::status_text`] without the failure's detail.
    pub fn status_title(&self) -> String {
        match (self.presented(), self.error()) {
            (Presented::Reconnecting, Some(_)) => "Failed to connect. Reconnecting...".into(),
            (Presented::Error, _) => "Connection failed".into(),
            _ => self.status_text(),
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

    /// Runs [`supervise`] over `starts`, one per start; it panics when they run out.
    fn supervised(starts: Vec<Start>) -> PrimaryBackend {
        let backend = PrimaryBackend::new();
        let mut starts = std::collections::VecDeque::from(starts);
        tokio::spawn(supervise(backend.shared.clone(), move || {
            std::future::ready(starts.pop_front().expect("no start left"))
        }));
        backend
    }

    /// A server at `http_base` that runs until `exit` sends why, or drops for a quit.
    fn running(http_base: &str) -> (Start, tokio::sync::oneshot::Sender<String>) {
        let (exit, exited) = tokio::sync::oneshot::channel::<String>();
        let endpoint = Endpoint::bearer(http_base, String::new());
        (Start::Ready { endpoint, exited: Some(async { exited.await.ok() }.boxed()) }, exit)
    }

    fn failed(reason: &str, retry: bool) -> Start {
        Start::Failed { reason: reason.into(), retry }
    }

    /// The next status, as `ready <base>`, `restarting <why>`, `failed <why>` or `starting`.
    async fn next(backend: &mut PrimaryBackend) -> String {
        backend.changed().await;
        describe(&backend.status())
    }

    fn describe(status: &BackendStatus) -> String {
        match status {
            BackendStatus::Starting => "starting".into(),
            BackendStatus::Ready(endpoint) => format!("ready {}", endpoint.http_base),
            BackendStatus::Restarting(reason) => format!("restarting {reason}"),
            BackendStatus::Failed(reason) => format!("failed {reason}"),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_stopped_server_restarts_after_the_backoff() {
        let (first, crash) = running("http://a");
        let (second, _keep) = running("http://b");
        let mut backend = supervised(vec![first, second]);
        assert_eq!(describe(&backend.status()), "starting");
        assert_eq!(next(&mut backend).await, "ready http://a");
        crash.send("crashed".into()).unwrap();
        assert_eq!(next(&mut backend).await, "restarting crashed");
        let down = tokio::time::Instant::now();
        assert_eq!(next(&mut backend).await, "ready http://b");
        assert_eq!(down.elapsed(), INITIAL_RESTART_DELAY);
    }

    #[tokio::test(start_paused = true)]
    async fn failed_first_starts_back_off_then_show_and_restart_on_request() {
        let (server, _keep) = running("http://a");
        let starts = vec![failed("no node", true), failed("no node", true), failed("no node", true), server];
        let mut backend = supervised(starts);
        let started = tokio::time::Instant::now();
        assert_eq!(next(&mut backend).await, "restarting no node");
        assert_eq!(next(&mut backend).await, "restarting no node");
        assert_eq!(started.elapsed(), Duration::from_millis(500));
        // The third restart without readiness shows the failure, as Electron's startup failure.
        assert_eq!(next(&mut backend).await, "failed no node");
        assert_eq!(started.elapsed(), Duration::from_millis(1_500));
        // Reconnect skips the 2 s still to wait.
        backend.restart();
        assert_eq!(describe(&backend.status()), "starting");
        assert_eq!(next(&mut backend).await, "ready http://a");
        assert_eq!(started.elapsed(), Duration::from_millis(1_500));
    }

    #[tokio::test(start_paused = true)]
    async fn an_unrecoverable_start_waits_for_a_restart_request() {
        let (server, _keep) = running("http://a");
        let mut backend = supervised(vec![failed("home owned", false), server]);
        assert_eq!(next(&mut backend).await, "failed home owned");
        tokio::time::sleep(MAX_RESTART_DELAY * 6).await;
        assert_eq!(describe(&backend.status()), "failed home owned");
        backend.restart();
        assert_eq!(describe(&backend.status()), "starting");
        assert_eq!(next(&mut backend).await, "ready http://a");
        // A restart request while it runs changes nothing.
        backend.restart();
        assert_eq!(describe(&backend.status()), "ready http://a");
    }

    #[tokio::test(start_paused = true)]
    async fn quitting_stops_supervision() {
        let (server, quit) = running("http://a");
        let mut backend = supervised(vec![server]);
        assert_eq!(next(&mut backend).await, "ready http://a");
        drop(quit);
        tokio::time::sleep(MAX_RESTART_DELAY * 6).await;
        assert_eq!(describe(&backend.status()), "ready http://a");
    }

    #[test]
    fn restart_delays_double_up_to_ten_seconds() {
        let delays: Vec<_> = (1..=7).map(|attempt| restart_delay(attempt).as_millis()).collect();
        assert_eq!(delays, [500, 1_000, 2_000, 4_000, 8_000, 10_000, 10_000]);
    }

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
            state(Phase::Blocked, 1, Some(refused.clone())).status_text(),
            "Connection failed. Reason: The environment credential is invalid."
        );
        assert_eq!(state(Phase::Blocked, 1, Some(refused)).status_title(), "Connection failed");
        let backoff = state(Phase::Backoff, 2, Some(Failure::transport("closed")));
        assert_eq!(backoff.status_title(), "Failed to connect. Reconnecting...");
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

        let route = Route::direct("https://box:4000".into(), Credential::Missing);
        let id = route.id("e");
        let missing = state(Phase::Blocked, 1, Some(Failure::credential_missing(&id, &route.credential)));
        let diagnosis = diagnose("Box", &missing, None).unwrap();
        assert_eq!(diagnosis.headline, "This device could not prepare the connection");
        assert!(
            diagnosis
                .facts
                .contains(&("Error", "Connection credential bearer:e:https://box:4000 is unavailable.".into()))
        );
        let text = diagnosis_text("Box", &diagnosis);
        assert!(text.starts_with("T3 Code connection to Box: This device could not prepare the connection\n\nThis device: FAILED\nT3 Code on Box: not checked\n\nCause: authentication"));

        // A credential the keyring would not open says so, rather than that none is saved.
        let locked = Credential::Locked { stored: "00".into(), error: "Could not use the Keychain: denied".into() };
        let detail = Failure::credential_missing(&id, &locked).detail;
        assert!(detail.contains("is saved, but this device could not decrypt it (Could not use the Keychain: denied)"));
    }

    fn read(document: &Value, unprotect: impl Fn(&[u8]) -> Result<Vec<u8>, String>) -> Catalog {
        Catalog::from_document(document.clone(), unprotect).unwrap()
    }

    /// Stand-ins for the platform store: a reversible "encryption", and a keyring that refuses.
    #[expect(clippy::unnecessary_wraps, reason = "the signature of `secure::protect`")]
    fn seal(data: &[u8]) -> Result<Vec<u8>, String> {
        Ok(data.iter().map(|byte| byte ^ 0x5a).collect())
    }

    fn denied(_: &[u8]) -> Result<Vec<u8>, String> {
        Err("Could not use the Keychain: denied".into())
    }

    fn saved(id: &str, credential: Credential) -> SavedEnvironment {
        SavedEnvironment {
            environment_id: id.into(),
            label: id.into(),
            enabled: true,
            routes: vec![Route::direct(format!("https://{id}"), credential)],
        }
    }

    fn credential(environment: &SavedEnvironment) -> &Credential {
        &environment.routes[0].credential
    }

    #[test]
    fn credentials_the_keyring_cannot_open_survive_saves_of_other_changes() {
        let catalog = Catalog {
            local_environment_enabled: true,
            environments: vec![saved("a", Credential::Token("token-a".into())), saved("b", Credential::Missing)],
            ..Catalog::default()
        };
        let document = catalog.document(seal).unwrap();
        assert!(!document.to_string().contains("token-a"));
        assert_eq!(read(&document, seal).environments, catalog.environments);
        let stored = document["environments"][0]["encryptedToken"].clone();

        // This run's keyring refuses: the token is locked, not gone.
        let mut locked = read(&document, denied);
        let error = "Could not use the Keychain: denied".to_owned();
        let expected = Credential::Locked { stored: stored.as_str().unwrap().into(), error };
        assert_eq!(*credential(&locked.environments[0]), expected);
        assert_eq!(*credential(&locked.environments[1]), Credential::Missing);
        assert!(remote_workspace_routes(&locked).is_empty(), "a locked token opens no session");

        // Rename it, switch it off and pair another environment, then save.
        locked.environments[0].label = "Renamed".into();
        locked.environments[0].enabled = false;
        locked.register(saved("c", Credential::Token("token-c".into())), true).unwrap();
        let saved_again = locked.document(seal).unwrap();
        assert_eq!(saved_again["environments"][0]["encryptedToken"], stored);
        assert_eq!(saved_again["environments"][0]["routes"][0]["encryptedToken"], stored);
        assert_eq!(saved_again["environments"][1]["encryptedToken"], "");

        // A later run whose keyring opens reads every token back.
        let reopened = read(&saved_again, seal);
        let tokens: Vec<_> = reopened.environments.iter().map(|e| credential(e).clone()).collect();
        assert_eq!(
            tokens,
            [Credential::Token("token-a".into()), Credential::Missing, Credential::Token("token-c".into())]
        );
        assert_eq!(reopened.environments[0].label, "Renamed");
    }

    #[test]
    fn unreadable_stored_credentials_are_kept_as_stored() {
        for stored in ["zz", "0", "00ff"] {
            let document = json!({ "environments": [{
                "environmentId": "a", "label": "A", "httpBaseUrl": "https://a", "encryptedToken": stored,
            }] });
            let catalog = read(&document, seal);
            assert!(
                matches!(credential(&catalog.environments[0]), Credential::Locked { stored: kept, .. } if kept == stored)
            );
            assert_eq!(catalog.document(seal).unwrap()["environments"][0]["encryptedToken"], stored);
        }
        assert_eq!(read(&json!({}), seal), Catalog::default());
    }

    #[test]
    fn entries_and_fields_this_build_cannot_read_survive_a_save() {
        let document = json!({
            "version": 1,
            "futureSetting": { "x": 1 },
            "environments": [
                { "environmentId": "a", "label": "A", "httpBaseUrl": "https://a", "icon": ["r"] },
                { "environmentId": "b", "httpBaseUrl": "https://b", "kind": "ssh" },
                { "environmentId": "c", "label": 7 },
                { "environmentId": "d", "label": "D", "routes": [{ "type": "relay" }] },
                { "environmentId": "e", "label": "E" },
                "not an entry",
            ],
        });
        let mut catalog = read(&document, seal);
        assert_eq!(catalog.environments, [SavedEnvironment { label: "A".into(), ..saved("a", Credential::Missing) }]);

        catalog.get_mut("a").unwrap().label = "Renamed".into();
        let written = catalog.document(seal).unwrap();
        assert_eq!(written["futureSetting"], json!({ "x": 1 }));
        let entries = written["environments"].as_array().unwrap();
        assert_eq!(entries[0]["label"], "Renamed");
        assert_eq!(entries[0]["icon"], json!(["r"]));
        assert_eq!(entries[1..], document["environments"].as_array().unwrap()[1..]);

        // Pairing "c" again replaces its unreadable entry; removing "a" drops it.
        catalog.register(saved("c", Credential::Missing), false).unwrap();
        catalog.environments.retain(|environment| environment.environment_id != "a");
        let written = catalog.document(seal).unwrap();
        let ids: Vec<_> =
            written["environments"].as_array().unwrap().iter().map(|e| e["environmentId"].clone()).collect();
        assert_eq!(ids, [json!("c"), json!("b"), json!("d"), json!("e"), Value::Null]);
    }

    #[test]
    fn pull_request_routing_grants_round_trip_and_drop_with_their_environment() {
        let grant = |id: &str| routing::Stored {
            environment_id: id.into(),
            connection_key: format!("key-{id}"),
            permission: routing::Permission::ReadWrite,
        };
        let document = json!({ "githubRoutingPermissions": [
            { "environmentId": "a", "connectionKey": "key-a", "permission": "read-write" },
            { "environmentId": "b", "connectionKey": "key-b", "permission": "sideways" },
        ] });
        let mut catalog = read(&document, seal);
        assert_eq!(catalog.pull_request_routing, [grant("a")]);
        assert_eq!(
            catalog.document(seal).unwrap()["githubRoutingPermissions"],
            json!([
                { "environmentId": "a", "connectionKey": "key-a", "permission": "read-write" },
            ])
        );
        catalog.register(saved("a", Credential::Missing), false).unwrap();
        catalog.remove("a");
        assert!(catalog.environments.is_empty());
        assert!(catalog.document(seal).unwrap().get("githubRoutingPermissions").is_none());

        let mut catalog = read(&document, seal);
        let environment = saved("a", Credential::Missing);
        let route_id = environment.routes[0].id("a");
        catalog.register(environment, false).unwrap();
        assert!(catalog.remove_route("a", &route_id).is_some());
        assert!(catalog.environments.is_empty());
        assert!(catalog.pull_request_routing.is_empty(), "the last route takes the grants with it");
    }

    #[test]
    fn a_catalog_file_that_cannot_be_used_is_never_saved_over() {
        let dir = std::env::temp_dir().join(format!("t3-gpui-catalog-{}", std::process::id()));
        drop(std::fs::remove_dir_all(&dir));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("environments.json");

        let opened = open_at(&path, seal, 0);
        assert!(!opened.read_only && opened.problem.is_none());
        assert_eq!(opened.catalog, Catalog::default());

        // Not a catalog: moved aside byte for byte, and the list starts empty.
        for (ms, corrupt) in (1..).zip(["{\"environments\": [", "[1, 2]", "{\"environments\": 3}", ""]) {
            std::fs::write(&path, corrupt).unwrap();
            let opened = open_at(&path, seal, ms);
            assert!(!opened.read_only);
            assert!(opened.problem.unwrap().contains(&format!("environments.corrupt-{ms}.json")));
            assert!(!path.exists());
            assert_eq!(std::fs::read_to_string(dir.join(format!("environments.corrupt-{ms}.json"))).unwrap(), corrupt);
        }

        // Unreadable (here a directory): left in place, and saves are refused.
        std::fs::create_dir(&path).unwrap();
        let opened = open_at(&path, seal, 9);
        assert!(opened.read_only && opened.problem.is_some());
        assert!(path.is_dir());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn debug_output_never_shows_a_token() {
        let environment = saved("a", Credential::Token("secret-token".into()));
        let catalog = Catalog { environments: vec![environment.clone()], ..Catalog::default() };
        for text in [format!("{environment:?}"), format!("{catalog:?}"), format!("{:#?}", credential(&environment))] {
            assert!(!text.contains("secret-token"), "{text}");
        }
        assert!(format!("{environment:?}").contains("credential: Token(<redacted>)"));
        let locked = Credential::Locked { stored: "abcd".into(), error: "denied".into() };
        assert_eq!(format!("{locked:?}"), r#"Locked { error: "denied", .. }"#);
    }

    /// DPAPI keeps its master keys in the user's profile (`%APPDATA%\Microsoft\Protect`). A
    /// sandbox running as a user without a loaded profile fails every call with OS error 2.
    /// A failure with the profile readable is a real failure, so the test still runs then.
    #[cfg(windows)]
    fn dpapi_unavailable() -> Option<String> {
        let error = secure::protect(b"probe").err()?;
        let keys = std::env::var_os("APPDATA").map(|appdata| PathBuf::from(appdata).join("Microsoft").join("Protect"));
        let profile_readable = keys.is_some_and(|keys| std::fs::read_dir(keys).is_ok());
        (!profile_readable).then_some(error)
    }

    #[cfg(windows)]
    #[test]
    fn catalog_documents_round_trip_with_encrypted_tokens() {
        if let Some(error) = dpapi_unavailable() {
            eprintln!("skipped: DPAPI has no user profile here ({error})");
            return;
        }
        let catalog = Catalog {
            local_environment_enabled: false,
            environments: vec![SavedEnvironment {
                enabled: false,
                ..saved("env-1", Credential::Token("secret-token".into()))
            }],
            ..Catalog::default()
        };
        let document = catalog.document(secure::protect).unwrap();
        assert!(!document.to_string().contains("secret-token"));
        let reread = read(&document, secure::unprotect);
        assert_eq!((reread.local_environment_enabled, reread.environments), (false, catalog.environments));
        // A credential that was locked opens once the store works again.
        let stored = document["environments"][0]["encryptedToken"].as_str().unwrap().to_owned();
        let locked = Credential::Locked { stored, error: "locked".into() };
        assert_eq!(locked.reopen(), Some(Credential::Token("secret-token".into())));
        assert_eq!(Credential::Missing.reopen(), None);
        // A credential DPAPI cannot open is kept as stored.
        let mut tampered = document;
        tampered["environments"][0]["routes"][0]["encryptedToken"] = json!("00ff");
        let reread = read(&tampered, secure::unprotect);
        assert!(matches!(credential(&reread.environments[0]), Credential::Locked { stored, .. } if stored == "00ff"));
    }

    fn ssh_route(alias: &str, credential: Credential) -> Route {
        let target = SshTarget {
            alias: alias.into(),
            hostname: format!("{alias}.lan"),
            username: Some("me".into()),
            port: None,
        };
        Route { address: Address::Ssh(target), credential }
    }

    fn addresses(environment: &SavedEnvironment) -> Vec<String> {
        environment.routes.iter().map(Route::address_text).collect()
    }

    #[test]
    fn with_the_local_environment_off_the_workspace_starts_through_any_signed_in_route() {
        let mut ssh_only = saved("s", Credential::Missing);
        ssh_only.routes = vec![ssh_route("box", Credential::Token("token-s".into()))];
        let off = SavedEnvironment { enabled: false, ..saved("off", Credential::Token("token-off".into())) };
        let catalog = Catalog {
            local_environment_enabled: false,
            environments: vec![off, saved("signed-out", Credential::Missing), ssh_only],
            ..Catalog::default()
        };
        let routes = remote_workspace_routes(&catalog);
        let ssh = Address::Ssh(SshTarget {
            alias: "box".into(),
            hostname: "box.lan".into(),
            username: Some("me".into()),
            port: None,
        });
        assert_eq!(routes, [("s", &ssh, "token-s")], "an SSH-only environment is a way in");
    }

    #[test]
    fn entries_saved_before_routes_read_as_one_direct_route_and_keep_working_in_older_builds() {
        let token = backend::hex(&seal(b"token-a").unwrap());
        let document = json!({ "environments": [{
            "environmentId": "a", "label": "A", "httpBaseUrl": "https://a/", "encryptedToken": token, "enabled": false,
        }] });
        let mut catalog = read(&document, seal);
        let migrated =
            SavedEnvironment { enabled: false, label: "A".into(), ..saved("a", Credential::Token("token-a".into())) };
        assert_eq!(catalog.environments, [migrated]);

        // Saved with an SSH route ahead of it: `routes` lists both, and the top-level fields
        // an older build reads mirror the direct route.
        catalog.environments[0].routes.insert(0, ssh_route("box", Credential::Token("token-s".into())));
        let written = catalog.document(seal).unwrap();
        let entry = &written["environments"][0];
        assert_eq!(entry["httpBaseUrl"], "https://a");
        assert_eq!(entry["routes"][0]["type"], "ssh");
        assert_eq!(entry["routes"][0]["username"], "me");
        assert_eq!(
            entry["routes"][1],
            json!({ "type": "direct", "httpBaseUrl": "https://a", "encryptedToken": entry["encryptedToken"] })
        );
        assert_eq!(read(&written, seal).environments, catalog.environments);

        // Without a direct route, the mirror goes, so an older build leaves the entry alone.
        catalog.environments[0].routes.truncate(1);
        let written = catalog.document(seal).unwrap();
        let entry = written["environments"][0].as_object().unwrap();
        assert!(!entry.contains_key("httpBaseUrl") && !entry.contains_key("encryptedToken"));
        assert_eq!(read(&written, seal).environments, catalog.environments);
    }

    #[test]
    fn routes_rank_by_kind_and_label_like_the_web() {
        let kind = |base: &str| Route::direct(base.into(), Credential::Missing).kind();
        assert_eq!(kind("http://127.0.0.1:3773"), RouteKind::Loopback);
        assert_eq!(kind("http://[::1]:3773"), RouteKind::Loopback);
        assert_eq!(kind("http://192.168.1.4:3773"), RouteKind::Lan);
        assert_eq!(kind("http://devbox:3773"), RouteKind::Lan);
        assert_eq!(kind("http://[fe80::1]:3773"), RouteKind::Lan);
        assert_eq!(kind("https://box.tail1.ts.net"), RouteKind::Tailnet);
        assert_eq!(kind("http://[fd7a:115c:a1e0::1]"), RouteKind::Tailnet);
        assert_eq!(kind("http://100.100.1.2:3773"), RouteKind::Vpn);
        assert_eq!(kind("https://box.example.com"), RouteKind::Public);
        assert_eq!(kind("http://8.8.8.8"), RouteKind::Public);
        assert_eq!(ssh_route("box", Credential::Missing).kind(), RouteKind::Ssh);

        let label = |base: &str| Route::direct(base.into(), Credential::Missing).label();
        assert_eq!(label("http://localhost:3773"), "This device");
        assert_eq!(label("https://box.example.com"), "box.example.com");
        assert_eq!(ssh_route("box", Credential::Missing).label(), "SSH me@box.lan");
        assert_eq!(ssh_route("box", Credential::Missing).address_text(), "box");
        assert_eq!(ssh_route("box", Credential::Missing).id("e"), r#"ssh:e:["box","box.lan","me",null]"#);
        assert_eq!(Route::direct("https://box:443".into(), Credential::Missing).id("e"), "bearer:e:https://box");
    }

    #[test]
    fn registering_adds_routes_under_one_identity() {
        let mut catalog =
            Catalog { environments: vec![saved("e", Credential::Token("public".into()))], ..Catalog::default() };
        catalog.get_mut("e").unwrap().enabled = false;
        catalog.get_mut("e").unwrap().label = "Renamed".into();
        let route = |base: &str, token: &str| SavedEnvironment {
            routes: vec![Route::direct(base.into(), Credential::Token(token.into()))],
            ..saved("e", Credential::Missing)
        };

        // "Add environment" refuses a new address of a saved environment, unspent.
        let refused = catalog.register(route("http://192.168.1.4:3773", "lan"), true);
        assert_eq!(
            refused,
            Err("That address claims to be Renamed, which is already saved. Add it as a route from that environment's settings instead.".into())
        );
        // "Add route" takes it, nearest kind first, and switches the environment on.
        catalog.register(route("http://192.168.1.4:3773", "lan"), false).unwrap();
        catalog.register(route("https://box.tail1.ts.net", "tail"), false).unwrap();
        let mut ssh = saved("e", Credential::Missing);
        ssh.routes = vec![ssh_route("box", Credential::Token("ssh".into()))];
        catalog.register(ssh, false).unwrap();
        // A second LAN address goes first among LAN routes, so its fresh credential connects.
        catalog.register(route("http://10.0.0.9:3773", "lan2"), false).unwrap();
        let environment = catalog.get("e").unwrap();
        assert_eq!(
            addresses(environment),
            ["http://10.0.0.9:3773", "http://192.168.1.4:3773", "https://e", "https://box.tail1.ts.net", "box"]
        );
        assert!(environment.enabled);
        assert_eq!(environment.label, "Renamed");

        // Pairing an address again replaces its route in place, with the new token.
        catalog.register(route("https://e/", "public2"), true).unwrap();
        let environment = catalog.get("e").unwrap();
        assert_eq!(environment.routes.len(), 5);
        assert_eq!(environment.routes[2].credential, Credential::Token("public2".into()));

        // A different environment is its own entry.
        catalog.register(saved("other", Credential::Missing), true).unwrap();
        assert_eq!(catalog.environments.len(), 2);
    }

    #[test]
    fn routes_reorder_and_the_last_one_takes_the_environment() {
        let mut catalog = Catalog { environments: vec![saved("e", Credential::Missing)], ..Catalog::default() };
        catalog.get_mut("e").unwrap().routes.push(ssh_route("box", Credential::Missing));
        let ids = catalog.get("e").unwrap().route_ids();
        let reversed: Vec<_> = ids.iter().rev().cloned().collect();
        catalog.reorder_routes("e", &reversed).unwrap();
        assert_eq!(addresses(catalog.get("e").unwrap()), ["box", "https://e"]);
        for bad in [vec![ids[0].clone()], vec![ids[0].clone(), ids[0].clone()], vec![ids[0].clone(), "x".into()]] {
            assert!(catalog.reorder_routes("e", &bad).is_err(), "{bad:?}");
        }
        assert_eq!(addresses(catalog.get("e").unwrap()), ["box", "https://e"]);

        let removed = catalog.remove_route("e", &ids[1]).unwrap();
        assert_eq!(removed.address_text(), "box");
        assert_eq!(addresses(catalog.get("e").unwrap()), ["https://e"]);
        assert!(catalog.remove_route("e", "missing").is_none());
        catalog.remove_route("e", &ids[0]).unwrap();
        assert!(catalog.get("e").is_none());
    }

    #[test]
    fn rows_name_the_route_in_use_and_mcp_skips_forwards() {
        let mut environment = saved("e", Credential::Missing);
        assert_eq!(environment.transport_label(None), "https://e");
        assert_eq!(environment.mcp_url(None).as_deref(), Some("https://e/mcp"));
        environment.routes.insert(0, ssh_route("box", Credential::Missing));
        let ssh_id = environment.routes[0].id("e");
        assert_eq!(environment.transport_label(None), "SSH me@box.lan");
        assert_eq!(environment.transport_label(Some(&environment.routes[1].id("e"))), "via LAN");
        assert_eq!(environment.mcp_url(Some(&ssh_id)).as_deref(), Some("https://e/mcp"));
        environment.routes.truncate(1);
        assert_eq!(environment.transport_label(None), "SSH me@box.lan");
        assert_eq!(environment.mcp_url(None), None);
    }

    #[test]
    fn a_pending_route_order_shows_until_saved_or_stale() {
        let ids = |list: &[&str]| list.iter().map(|id| (*id).to_owned()).collect::<Vec<_>>();
        let saved = ids(&["a", "b", "c"]);
        let moved = move_route(&saved, 2, 0);
        assert_eq!(moved, ids(&["c", "a", "b"]));
        assert_eq!(move_route(&saved, 0, 2), ids(&["b", "c", "a"]));
        assert_eq!(move_route(&saved, 0, 9), saved);
        assert_eq!(pending_route_order(Some(&moved), &saved), Some(moved.as_slice()));
        // Saved in that order, or a route added or removed meanwhile: the saved order shows.
        assert_eq!(pending_route_order(Some(&moved), &moved), None);
        assert_eq!(pending_route_order(Some(&moved), &ids(&["a", "b"])), None);
        assert_eq!(pending_route_order(Some(&moved), &ids(&["a", "b", "d"])), None);
        assert_eq!(pending_route_order(None, &saved), None);
    }

    #[test]
    fn labels_must_not_be_empty() {
        assert_eq!(validate_label("  Box "), Ok("Box".into()));
        assert_eq!(validate_label("  "), Err("Environment label cannot be empty."));
    }
}
