//! Where a pairing link's URL points and how it is shared (`pairingUrls.ts`,
//! `ConnectionsSettings.logic.ts` and `PairingLinkListRow`): the addresses the local server
//! is reached at, the full pairing URL for each, and the QR code another device scans.

use std::net::Ipv4Addr;
use std::rc::Rc;

use gpui_kit::{Bounds, Hsla, IntoElement, Styled as _, canvas, fill, point, px, size};
use url::Url;

use crate::desktop::exposure::ExposureView;
use crate::desktop::settings::ExposureMode;

/// `DEFAULT_HOSTED_APP_URL`.
const HOSTED_APP_URL: &str = "https://app.t3.codes";
const PAIRING_TOKEN_PARAM: &str = "token";

/// `AdvertisedEndpointReachability`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Reachability {
    Loopback,
    Lan,
    PrivateNetwork,
}

/// The `AdvertisedEndpoint` fields pairing reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct AdvertisedEndpoint {
    pub id: String,
    pub label: &'static str,
    /// Normalized: `http(s)://host:port/`.
    pub http_base_url: String,
    pub reachability: Reachability,
    /// `compatibility.hostedHttpsApp === "compatible"`.
    pub hosted_https: bool,
    /// `status !== "unavailable"`.
    pub available: bool,
    pub is_default: bool,
}

/// The endpoints Sharing & access offers for the local server (`visibleDesktopAdvertisedEndpoints`):
/// this machine, the LAN and tailnet addresses while network access is on, then Tailscale HTTPS.
pub(super) fn desktop_endpoints(view: &ExposureView) -> Vec<AdvertisedEndpoint> {
    let mut endpoints = Vec::new();
    if view.exposure.mode == ExposureMode::NetworkAccessible {
        let port = view.port;
        endpoints.push(AdvertisedEndpoint {
            id: format!("desktop-loopback:{port}"),
            label: "This machine",
            http_base_url: format!("http://127.0.0.1:{port}/"),
            reachability: Reachability::Loopback,
            hosted_https: false,
            available: true,
            is_default: false,
        });
        if let Some(url) = &view.exposure.endpoint_url {
            endpoints.push(AdvertisedEndpoint {
                id: format!("desktop-lan:{url}"),
                label: "Local network",
                http_base_url: normalize(url),
                reachability: Reachability::Lan,
                hosted_https: false,
                available: true,
                is_default: true,
            });
        }
        endpoints.extend(view.tailnet_ipv4.iter().map(|address: &Ipv4Addr| {
            let url = format!("http://{address}:{port}");
            AdvertisedEndpoint {
                id: format!("tailscale-ip:{url}"),
                label: "Tailscale IP",
                http_base_url: normalize(&url),
                reachability: Reachability::PrivateNetwork,
                hosted_https: false,
                available: true,
                is_default: false,
            }
        }));
    }
    if let Some(https) = &view.tailscale {
        endpoints.push(AdvertisedEndpoint {
            id: format!("tailscale-magicdns:{}", https.url),
            label: "Tailscale HTTPS",
            http_base_url: normalize(&https.url),
            reachability: Reachability::PrivateNetwork,
            hosted_https: https.available,
            available: https.available,
            is_default: false,
        });
    }
    endpoints
}

/// `normalizeHttpBaseUrl`: the origin with a `/` path.
fn normalize(url: &str) -> String {
    match Url::parse(url) {
        Ok(mut parsed) => {
            parsed.set_path("/");
            parsed.set_query(None);
            parsed.set_fragment(None);
            parsed.to_string()
        }
        Err(_) => url.to_owned(),
    }
}

/// `setPairingTokenOnUrl`: the token goes in the fragment, never to a server log.
fn with_token(mut url: Url, credential: &str) -> String {
    let query: Vec<(String, String)> =
        url.query_pairs().filter(|(key, _)| key != PAIRING_TOKEN_PARAM).map(|(k, v)| (k.into(), v.into())).collect();
    if query.is_empty() {
        url.set_query(None);
    } else {
        url.query_pairs_mut().clear().extend_pairs(query);
    }
    let fragment: String =
        url::form_urlencoded::Serializer::new(String::new()).append_pair(PAIRING_TOKEN_PARAM, credential).finish();
    url.set_fragment(Some(&fragment));
    url.to_string()
}

/// `resolveDesktopPairingUrl`: `<endpoint>/pair#token=…`.
fn desktop_pairing_url(endpoint_url: &str, credential: &str) -> Option<String> {
    let mut url = Url::parse(endpoint_url).ok()?;
    url.set_path("/pair");
    Some(with_token(url, credential))
}

/// `resolveHostedPairingUrl`: the hosted app's `/pair?host=<endpoint>#token=…`, for HTTPS only.
fn hosted_pairing_url(endpoint_url: &str, credential: &str) -> Option<String> {
    if Url::parse(endpoint_url).ok()?.scheme() != "https" {
        return None;
    }
    let mut url = Url::parse(HOSTED_APP_URL).ok()?.join("/pair").ok()?;
    url.query_pairs_mut().append_pair("host", endpoint_url);
    Some(with_token(url, credential))
}

/// `resolveAdvertisedEndpointPairingUrl`.
fn endpoint_pairing_url(endpoint: &AdvertisedEndpoint, credential: &str) -> Option<String> {
    let hosted = endpoint.hosted_https.then(|| hosted_pairing_url(&endpoint.http_base_url, credential)).flatten();
    hosted.or_else(|| desktop_pairing_url(&endpoint.http_base_url, credential))
}

/// `isHostedAppPairingUrl`.
pub(super) fn is_hosted_app_pairing_url(value: &str) -> bool {
    Url::parse(value).is_ok_and(|url| url.path() == "/pair" && url.query_pairs().any(|(key, _)| key == "host"))
}

/// `selectPairingEndpoint` with no saved default: the default endpoint, else the first other
/// devices can reach, else one the hosted app can use.
pub(super) fn select_pairing_endpoint(endpoints: &[AdvertisedEndpoint]) -> Option<&AdvertisedEndpoint> {
    let available = || endpoints.iter().filter(|e| e.available);
    available()
        .find(|e| e.is_default)
        .or_else(|| available().find(|e| e.reachability != Reachability::Loopback))
        .or_else(|| available().find(|e| e.hosted_https))
}

/// `endpointShareHint`.
fn share_hint(endpoint: &AdvertisedEndpoint, url: &str) -> &'static str {
    if is_hosted_app_pairing_url(url) {
        return "Opens the hosted app, no install needed";
    }
    match endpoint.reachability {
        Reachability::Lan => "Devices on the same network",
        Reachability::PrivateNetwork => "Devices on your private network",
        Reachability::Loopback => "Clients on this machine",
    }
}

/// One endpoint a pairing link's URL and QR code can use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ShareOption {
    pub id: String,
    pub label: &'static str,
    pub url: String,
    pub detail: &'static str,
    /// `isQrShareableEndpoint`: a device scanning a loopback URL would dial itself.
    pub qr_shareable: bool,
}

/// How one pairing link with a known credential can be shared.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Share {
    /// `shareablePairingUrl`: the full URL to open, or `None` when only the code can be shared.
    pub url: Option<String>,
    pub options: Vec<ShareOption>,
    /// The option the panel shows (`selectQrEndpointOption`), by index into `options`.
    pub selected: Option<usize>,
}

impl Share {
    /// `qrPairingUrl`: the picked endpoint's URL, else the shareable one.
    pub fn qr_url(&self) -> Option<&str> {
        self.selected.map(|i| self.options[i].url.as_str()).or(self.url.as_deref())
    }

    /// `canRenderQrForSelection`.
    pub fn qr_shareable(&self) -> bool {
        self.selected.is_none_or(|i| self.options[i].qr_shareable)
    }
}

/// The pairing URLs for `credential` over `endpoints`, falling back to the server's
/// `endpoint_url` when no endpoint is advertised. `picked` is the endpoint the user chose.
pub(super) fn share(
    endpoints: &[AdvertisedEndpoint],
    endpoint_url: Option<&str>,
    credential: &str,
    picked: Option<&str>,
) -> Share {
    let default = select_pairing_endpoint(endpoints);
    let fallback = || {
        let endpoint_url = endpoint_url.filter(|url| !url.is_empty())?;
        hosted_pairing_url(endpoint_url, credential).or_else(|| desktop_pairing_url(endpoint_url, credential))
    };
    let url = default.and_then(|endpoint| endpoint_pairing_url(endpoint, credential)).or_else(fallback);
    let options: Vec<ShareOption> = endpoints
        .iter()
        .filter(|endpoint| endpoint.available)
        .filter_map(|endpoint| {
            let url = endpoint_pairing_url(endpoint, credential)?;
            Some(ShareOption {
                id: endpoint.id.clone(),
                label: endpoint.label,
                detail: share_hint(endpoint, &url),
                url,
                qr_shareable: endpoint.reachability != Reachability::Loopback,
            })
        })
        .collect();
    let position = |id: &str| options.iter().position(|option| option.id == id);
    let selected = picked
        .and_then(position)
        .or_else(|| default.and_then(|endpoint| position(&endpoint.id)))
        .or_else(|| options.iter().position(|option| option.qr_shareable))
        .or((!options.is_empty()).then_some(0));
    Share { url, options, selected }
}

/// A QR code's modules, row by row.
pub(super) struct Qr {
    width: usize,
    dark: Vec<bool>,
}

/// `QRCodeSvg level="M"`; `None` for text too long to encode.
pub(super) fn qr(text: &str) -> Option<Rc<Qr>> {
    let code = qrcode::QrCode::with_error_correction_level(text, qrcode::EcLevel::M).ok()?;
    let dark = code.to_colors().into_iter().map(|color| color == qrcode::Color::Dark).collect();
    Some(Rc::new(Qr { width: code.width(), dark }))
}

/// Draws `qr` in a `side`-pixel square with a quiet zone of `margin` modules, one quad per
/// run of dark modules. Edges are rounded to whole pixels so adjacent rows leave no seams.
pub(super) fn qr_element(qr: Rc<Qr>, side: f32, margin: usize, color: Hsla) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |bounds: Bounds<gpui_kit::Pixels>, (), window, _| {
            let cells = qr.width + margin * 2;
            #[expect(clippy::cast_precision_loss, reason = "a QR code is at most 177 modules wide")]
            let module = side / cells as f32;
            #[expect(clippy::cast_precision_loss, reason = "a QR code is at most 177 modules wide")]
            let edge = |index: usize| (index as f32 * module).round();
            for row in 0..qr.width {
                let modules = &qr.dark[row * qr.width..(row + 1) * qr.width];
                let mut col = 0;
                while col < qr.width {
                    if !modules[col] {
                        col += 1;
                        continue;
                    }
                    let start = col;
                    while col < qr.width && modules[col] {
                        col += 1;
                    }
                    let (x0, x1) = (edge(start + margin), edge(col + margin));
                    let (y0, y1) = (edge(row + margin), edge(row + 1 + margin));
                    let origin = point(bounds.origin.x + px(x0), bounds.origin.y + px(y0));
                    window.paint_quad(fill(Bounds::new(origin, size(px(x1 - x0), px(y1 - y0))), color));
                }
            }
        },
    )
    .size(px(side))
    .flex_none()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::exposure::{Exposure, TailscaleHttps};

    fn view(mode: ExposureMode, lan: Option<&str>, tailnet: &[Ipv4Addr], https: Option<(&str, bool)>) -> ExposureView {
        ExposureView {
            exposure: Exposure {
                mode,
                bind_host: "0.0.0.0",
                endpoint_url: lan.map(str::to_owned),
                advertised_host: None,
            },
            tailscale: https.map(|(url, available)| TailscaleHttps { url: url.into(), available }),
            port: 3773,
            tailnet_ipv4: tailnet.to_vec(),
        }
    }

    #[test]
    fn pairing_urls_carry_the_token_in_the_fragment() {
        assert_eq!(
            desktop_pairing_url("http://192.168.1.20:3773/", "ABC123").as_deref(),
            Some("http://192.168.1.20:3773/pair#token=ABC123")
        );
        assert_eq!(
            hosted_pairing_url("https://box.tail1.ts.net/", "ABC123").as_deref(),
            Some("https://app.t3.codes/pair?host=https%3A%2F%2Fbox.tail1.ts.net%2F#token=ABC123")
        );
        assert_eq!(hosted_pairing_url("http://192.168.1.20:3773/", "ABC123"), None, "the hosted app needs HTTPS");
        assert!(is_hosted_app_pairing_url("https://app.t3.codes/pair?host=x#token=y"));
        assert!(!is_hosted_app_pairing_url("http://192.168.1.20:3773/pair#token=y"));
    }

    #[test]
    fn the_lan_address_is_shared_by_default_and_loopback_gets_no_qr() {
        let tailnet = Ipv4Addr::new(100, 101, 2, 3);
        let endpoints = desktop_endpoints(&view(
            ExposureMode::NetworkAccessible,
            Some("http://192.168.1.20:3773"),
            &[tailnet],
            Some(("https://box.tail1.ts.net/", true)),
        ));
        let ids: Vec<&str> = endpoints.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "desktop-loopback:3773",
                "desktop-lan:http://192.168.1.20:3773",
                "tailscale-ip:http://100.101.2.3:3773",
                "tailscale-magicdns:https://box.tail1.ts.net/",
            ]
        );
        let shared = share(&endpoints, Some("http://192.168.1.20:3773"), "CODE", None);
        assert_eq!(shared.url.as_deref(), Some("http://192.168.1.20:3773/pair#token=CODE"));
        assert_eq!(shared.selected, Some(1));
        let details: Vec<&str> = shared.options.iter().map(|o| o.detail).collect();
        assert_eq!(
            details,
            [
                "Clients on this machine",
                "Devices on the same network",
                "Devices on your private network",
                "Opens the hosted app, no install needed",
            ]
        );
        let local = share(&endpoints, None, "CODE", Some("desktop-loopback:3773"));
        assert_eq!(local.qr_url(), Some("http://127.0.0.1:3773/pair#token=CODE"));
        assert!(!local.qr_shareable(), "another device would dial itself");
        let hosted = share(&endpoints, None, "CODE", Some("tailscale-magicdns:https://box.tail1.ts.net/"));
        assert!(hosted.qr_url().is_some_and(is_hosted_app_pairing_url));
    }

    #[test]
    fn without_network_access_only_a_working_tailscale_https_is_shared() {
        let local_only = view(ExposureMode::LocalOnly, None, &[], Some(("https://box.tail1.ts.net/", false)));
        let endpoints = desktop_endpoints(&local_only);
        assert_eq!(endpoints.len(), 1);
        assert_eq!(share(&endpoints, None, "CODE", None), Share::default(), "Serve is not answering: code only");
        let serving =
            desktop_endpoints(&view(ExposureMode::LocalOnly, None, &[], Some(("https://box.tail1.ts.net/", true))));
        let shared = share(&serving, None, "CODE", None);
        assert!(shared.url.as_deref().is_some_and(is_hosted_app_pairing_url));
        assert!(shared.qr_shareable());
    }

    #[test]
    fn an_unadvertised_server_shares_its_endpoint_url_or_only_the_code() {
        assert_eq!(share(&[], None, "CODE", None).url, None);
        let https = share(&[], Some("https://t3.example.com"), "CODE", None);
        assert!(https.url.as_deref().is_some_and(is_hosted_app_pairing_url));
        assert_eq!(
            share(&[], Some("http://box:3773"), "CODE", None).url.as_deref(),
            Some("http://box:3773/pair#token=CODE")
        );
        assert!(share(&[], Some("http://box:3773"), "CODE", None).qr_shareable());
    }

    #[test]
    fn qr_codes_encode_the_url() {
        let code = qr("http://192.168.1.20:3773/pair#token=ABCDEFGHJKLM").expect("short text encodes");
        assert_eq!(code.dark.len(), code.width * code.width);
        // Finder pattern: the top-left corner module is dark.
        assert!(code.dark[0]);
    }
}
