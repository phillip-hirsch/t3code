//! How far the local server reaches: this machine only, or every interface with a LAN address
//! to advertise, plus Tailscale Serve. `apps/desktop/src/backend/DesktopServerExposure.ts` and
//! `tailscaleEndpointProvider.ts`.

use std::net::{Ipv4Addr, UdpSocket};
use std::time::Duration;

use super::settings::{DesktopSettings, ExposureMode};

const LOOPBACK_HOST: &str = "127.0.0.1";
const LAN_BIND_HOST: &str = "0.0.0.0";
const TAILSCALE_PROBE_TIMEOUT: Duration = Duration::from_millis(2_500);

/// `isTailscaleIpv4Address`: Tailscale's CGNAT range, 100.64.0.0/10.
pub fn is_tailscale_ipv4(address: Ipv4Addr) -> bool {
    let [a, b, ..] = address.octets();
    a == 100 && (64..=127).contains(&b)
}

/// The address the OS sends outbound traffic from, which is what other devices on the
/// network reach. Connecting a UDP socket picks the route without sending anything.
pub fn route_ipv4() -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    // TEST-NET-1: routable on any network, never answered.
    socket.connect((Ipv4Addr::new(192, 0, 2, 1), 9)).ok()?;
    match socket.local_addr().ok()?.ip() {
        std::net::IpAddr::V4(address) if !address.is_unspecified() => Some(address),
        _ => None,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Exposure {
    pub mode: ExposureMode,
    pub bind_host: &'static str,
    /// `http://<lan address>:<port>`, the default way in from other devices.
    pub endpoint_url: Option<String>,
    pub advertised_host: Option<String>,
}

/// `resolveRuntimeState`: network access needs a LAN address to advertise or a Tailscale
/// address to be reached on; without either it falls back to this machine only.
pub fn resolve(mode: ExposureMode, port: u16, route: Option<Ipv4Addr>, host_override: Option<&str>) -> Exposure {
    let local =
        Exposure { mode: ExposureMode::LocalOnly, bind_host: LOOPBACK_HOST, endpoint_url: None, advertised_host: None };
    if mode == ExposureMode::LocalOnly {
        return local;
    }
    // `isUsableLanIpv4Address`.
    let usable = |a: &Ipv4Addr| !a.is_loopback() && !a.is_link_local() && !is_tailscale_ipv4(*a);
    let advertised = host_override
        .map(str::trim)
        .filter(|host| !host.is_empty())
        .map(str::to_owned)
        .or_else(|| route.filter(usable).map(|a| a.to_string()));
    if advertised.is_none() && !route.is_some_and(is_tailscale_ipv4) {
        return local;
    }
    Exposure {
        mode,
        bind_host: LAN_BIND_HOST,
        endpoint_url: advertised.as_ref().map(|host| format!("http://{host}:{port}")),
        advertised_host: advertised,
    }
}

/// What Settings > Sharing & access shows, read off the UI thread when the page opens.
#[derive(Clone)]
pub struct ExposureView {
    pub exposure: Exposure,
    pub tailscale: Option<TailscaleHttps>,
}

/// What the server is started with: the bind host and Tailscale Serve.
pub fn for_launch(settings: &DesktopSettings, port: u16) -> (Exposure, bool, u16) {
    let host_override = std::env::var("T3CODE_DESKTOP_LAN_HOST").ok();
    let route = (settings.server_exposure_mode == ExposureMode::NetworkAccessible).then(route_ipv4).flatten();
    let exposure = resolve(settings.server_exposure_mode, port, route, host_override.as_deref());
    (exposure, settings.tailscale_serve_enabled, settings.tailscale_serve_port)
}

/// `buildTailscaleHttpsBaseUrl`.
pub fn tailscale_https_url(magic_dns_name: &str, serve_port: u16) -> String {
    match serve_port {
        443 => format!("https://{magic_dns_name}/"),
        port => format!("https://{magic_dns_name}:{port}/"),
    }
}

/// `parseTailscaleStatus`'s MagicDNS name: `Self.DNSName` without its trailing dot.
pub fn magic_dns_name(status_json: &str) -> Option<String> {
    let status: serde_json::Value = serde_json::from_str(status_json).ok()?;
    let name = status.pointer("/Self/DNSName")?.as_str()?.trim().trim_end_matches('.');
    (!name.is_empty()).then(|| name.to_owned())
}

/// The Tailscale HTTPS endpoint and whether Tailscale Serve answers on it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TailscaleHttps {
    pub url: String,
    pub available: bool,
}

/// `resolveTailscaleMagicDnsAdvertisedEndpoint`: reads `tailscale status --json`, then probes
/// the HTTPS URL when Serve is on. Blocking: run it off the UI thread. `None` when Tailscale is
/// not running here.
pub fn tailscale_https(serve_enabled: bool, serve_port: u16) -> Option<TailscaleHttps> {
    let executable = if cfg!(windows) { "tailscale.exe" } else { "tailscale" };
    let mut command = std::process::Command::new(executable);
    // From Finder the app's own PATH is bare; look where the login shell would.
    #[cfg(not(windows))]
    if let Some(path) = crate::desktop::shell_env::search_path() {
        command.env("PATH", path);
    }
    let output = command
        .args(["status", "--json"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let name = magic_dns_name(&String::from_utf8_lossy(&output.stdout))?;
    let url = tailscale_https_url(&name, serve_port);
    let available = serve_enabled && probe(&url);
    Some(TailscaleHttps { url, available })
}

/// `probeTailscaleHttpsEndpoint`.
fn probe(base_url: &str) -> bool {
    let url = format!("{base_url}.well-known/t3/environment");
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build() else { return false };
    runtime.block_on(async {
        let Ok(client) = reqwest::Client::builder().timeout(TAILSCALE_PROBE_TIMEOUT).build() else { return false };
        client.get(url).send().await.is_ok_and(|response| response.status().is_success())
    })
}

/// `missingTailscaleEndpointDescription`.
pub fn missing_tailscale_description(mode: ExposureMode, serve_enabled: bool) -> &'static str {
    if mode != ExposureMode::NetworkAccessible && !serve_enabled {
        "Turn on Network access to detect Tailscale."
    } else {
        "Start Tailscale to set up HTTPS access through MagicDNS."
    }
}

/// The port field of the Tailscale HTTPS dialog: digits from 1 to 65535.
pub fn parse_serve_port(input: &str) -> Option<u16> {
    let trimmed = input.trim();
    (!trimmed.is_empty() && trimmed.bytes().all(|b| b.is_ascii_digit()))
        .then(|| trimmed.parse::<u16>().ok())
        .flatten()
        .filter(|port| *port >= 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_access_needs_an_address_to_be_reached_on() {
        let lan = Some(Ipv4Addr::new(192, 168, 1, 20));
        assert_eq!(resolve(ExposureMode::LocalOnly, 3773, lan, None).bind_host, "127.0.0.1");
        let open = resolve(ExposureMode::NetworkAccessible, 3773, lan, None);
        assert_eq!(open.bind_host, "0.0.0.0");
        assert_eq!(open.endpoint_url.as_deref(), Some("http://192.168.1.20:3773"));
        // Only a Tailscale address: reachable on the tailnet, nothing to advertise on the LAN.
        let tailnet = resolve(ExposureMode::NetworkAccessible, 3773, Some(Ipv4Addr::new(100, 101, 2, 3)), None);
        assert_eq!((tailnet.mode, tailnet.endpoint_url), (ExposureMode::NetworkAccessible, None));
        // No address at all falls back to this machine.
        let offline = resolve(ExposureMode::NetworkAccessible, 3773, Some(Ipv4Addr::new(169, 254, 0, 1)), None);
        assert_eq!(offline.mode, ExposureMode::LocalOnly);
        let named = resolve(ExposureMode::NetworkAccessible, 3773, None, Some(" box.local "));
        assert_eq!(named.endpoint_url.as_deref(), Some("http://box.local:3773"));
    }

    #[test]
    fn tailscale_urls_and_ports() {
        assert!(is_tailscale_ipv4(Ipv4Addr::new(100, 64, 0, 1)) && !is_tailscale_ipv4(Ipv4Addr::new(100, 128, 0, 1)));
        assert_eq!(tailscale_https_url("box.tail1.ts.net", 443), "https://box.tail1.ts.net/");
        assert_eq!(tailscale_https_url("box.tail1.ts.net", 8443), "https://box.tail1.ts.net:8443/");
        assert_eq!(magic_dns_name(r#"{"Self":{"DNSName":"box.tail1.ts.net."}}"#).as_deref(), Some("box.tail1.ts.net"));
        assert_eq!(magic_dns_name(r#"{"Self":{"DNSName":""}}"#), None);
        assert_eq!(parse_serve_port(" 8443 "), Some(8443));
        for bad in ["0", "65536", "-1", "4.5", ""] {
            assert_eq!(parse_serve_port(bad), None, "{bad}");
        }
    }
}
