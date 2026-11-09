//! Preview browser sessions: the wire shapes of `packages/contracts/src/preview.ts`,
//! the per-thread reducer of `apps/web/src/previewStateStore.ts`, and the address
//! bar's URL resolution from `packages/shared/src/preview.ts`.

use std::fmt::Write as _;

use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(tag = "_tag")]
pub enum NavStatus {
    #[default]
    Idle,
    Loading {
        url: String,
        #[serde(default)]
        title: String,
    },
    Success {
        url: String,
        #[serde(default)]
        title: String,
    },
    LoadFailed {
        url: String,
        #[serde(default)]
        title: String,
        code: i64,
        #[serde(default)]
        description: String,
        /// The navigation was a file the server's browser downloaded instead of showing.
        #[serde(default)]
        download: Option<FileDownload>,
    },
}

/// `PreviewFileDownload`.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FileDownload {
    pub id: String,
    pub file_name: String,
}

impl NavStatus {
    pub fn url(&self) -> Option<&str> {
        match self {
            NavStatus::Idle => None,
            NavStatus::Loading { url, .. } | NavStatus::Success { url, .. } | NavStatus::LoadFailed { url, .. } => {
                Some(url)
            }
        }
    }

    pub fn title(&self) -> Option<&str> {
        match self {
            NavStatus::Idle => None,
            NavStatus::Loading { title, .. }
            | NavStatus::Success { title, .. }
            | NavStatus::LoadFailed { title, .. } => Some(title).filter(|t| !t.is_empty()).map(String::as_str),
        }
    }

    pub fn to_json(&self) -> Value {
        match self {
            NavStatus::Idle => json!({"_tag": "Idle"}),
            NavStatus::Loading { url, title } => json!({"_tag": "Loading", "url": url, "title": title}),
            NavStatus::Success { url, title } => json!({"_tag": "Success", "url": url, "title": title}),
            NavStatus::LoadFailed { url, title, code, description, download } => {
                let mut status =
                    json!({"_tag": "LoadFailed", "url": url, "title": title, "code": code, "description": description});
                if let Some(download) = download {
                    status["download"] = json!({"id": download.id, "fileName": download.file_name});
                }
                status
            }
        }
    }
}

/// `PreviewAppearancePreference`: what `prefers-color-scheme` the page sees.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}

impl Appearance {
    pub const ALL: [Appearance; 3] = [Appearance::System, Appearance::Light, Appearance::Dark];

    pub fn as_str(self) -> &'static str {
        match self {
            Appearance::System => "system",
            Appearance::Light => "light",
            Appearance::Dark => "dark",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Appearance::System => "System",
            Appearance::Light => "Light",
            Appearance::Dark => "Dark",
        }
    }
}

/// `PREVIEW_ZOOM_LEVELS`, shared with desktop Chromium's zoom steps.
pub const ZOOM_LEVELS: [f64; 17] =
    [0.25, 0.33, 0.5, 0.67, 0.75, 0.8, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0, 4.0, 5.0];

/// The next zoom level in `direction` (-1, +1), or the default for 0, as the web's `stepServerZoom`.
pub fn step_zoom(current: f64, direction: i32) -> f64 {
    if direction == 0 {
        return 1.0;
    }
    let index = ZOOM_LEVELS.iter().position(|level| (level - current).abs() < 1e-9).unwrap_or(7);
    let next = if direction > 0 { (index + 1).min(ZOOM_LEVELS.len() - 1) } else { index.saturating_sub(1) };
    ZOOM_LEVELS[next]
}

/// `PreviewViewportSetting`: the page fills the panel, or gets a fixed CSS size.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(tag = "_tag", rename_all = "camelCase")]
pub enum Viewport {
    #[default]
    Fill,
    Freeform {
        width: u32,
        height: u32,
    },
    Preset {
        width: u32,
        height: u32,
        #[serde(rename = "presetId")]
        preset_id: String,
    },
}

impl Viewport {
    pub fn size(&self) -> Option<(f32, f32)> {
        match self {
            Viewport::Fill => None,
            Viewport::Freeform { width, height } | Viewport::Preset { width, height, .. } => {
                Some((*width as f32, *height as f32))
            }
        }
    }

    pub fn preset_id(&self) -> Option<&str> {
        match self {
            Viewport::Preset { preset_id, .. } => Some(preset_id),
            _ => None,
        }
    }

    pub fn preset(preset: &Preset) -> Value {
        json!({"_tag": "preset", "presetId": preset.id, "width": preset.width, "height": preset.height})
    }
}

pub struct Preset {
    pub id: &'static str,
    pub label: &'static str,
    pub width: u32,
    pub height: u32,
}

const fn preset(id: &'static str, label: &'static str, width: u32, height: u32) -> Preset {
    Preset { id, label, width, height }
}

/// `packages/shared/src/previewViewport.ts`, in Chrome DevTools' device order.
pub const PRESETS: [Preset; 17] = [
    preset("iphone-se", "iPhone SE", 375, 667),
    preset("iphone-xr", "iPhone XR", 414, 896),
    preset("iphone-12-pro", "iPhone 12 Pro", 390, 844),
    preset("iphone-14-pro-max", "iPhone 14 Pro Max", 430, 932),
    preset("pixel-7", "Pixel 7", 412, 915),
    preset("samsung-galaxy-s8-plus", "Samsung Galaxy S8+", 360, 740),
    preset("samsung-galaxy-s20-ultra", "Samsung Galaxy S20 Ultra", 412, 915),
    preset("ipad-mini", "iPad Mini", 768, 1024),
    preset("ipad-air", "iPad Air", 820, 1180),
    preset("ipad-pro", "iPad Pro", 1024, 1366),
    preset("surface-pro-7", "Surface Pro 7", 912, 1368),
    preset("surface-duo", "Surface Duo", 540, 720),
    preset("galaxy-z-fold-5", "Galaxy Z Fold 5", 344, 882),
    preset("asus-zenbook-fold", "Asus Zenbook Fold", 853, 1280),
    preset("samsung-galaxy-a51-71", "Samsung Galaxy A51/71", 412, 914),
    preset("nest-hub", "Nest Hub", 1024, 600),
    preset("nest-hub-max", "Nest Hub Max", 1280, 800),
];

/// A fixed viewport shrunk to fit `available`, never enlarged: its on-screen size and the scale,
/// which the page zoom also takes so the page still lays out at the viewport's CSS width.
/// The server's size is outside data: an empty one counts as 1px, so the scale is never NaN.
pub fn fit_viewport(viewport: (f32, f32), available: (f32, f32)) -> ((f32, f32), f32) {
    let (width, height) = (viewport.0.max(1.), viewport.1.max(1.));
    let scale = (available.0 / width).min(available.1 / height);
    let scale = if scale.is_finite() { scale.clamp(0.05, 1.) } else { 1. };
    ((width * scale, height * scale), scale)
}

/// Chromium's net errors by code, named as Electron's `did-fail-load` names them.
const NET_ERRORS: [(i64, &str); 26] = [
    (-2, "ERR_FAILED"),
    (-6, "ERR_FILE_NOT_FOUND"),
    (-100, "ERR_CONNECTION_CLOSED"),
    (-101, "ERR_CONNECTION_RESET"),
    (-102, "ERR_CONNECTION_REFUSED"),
    (-103, "ERR_CONNECTION_ABORTED"),
    (-105, "ERR_NAME_NOT_RESOLVED"),
    (-106, "ERR_INTERNET_DISCONNECTED"),
    (-107, "ERR_SSL_PROTOCOL_ERROR"),
    (-109, "ERR_ADDRESS_UNREACHABLE"),
    (-110, "ERR_SSL_CLIENT_AUTH_CERT_NEEDED"),
    (-117, "ERR_BAD_SSL_CLIENT_AUTH_CERT"),
    (-118, "ERR_CONNECTION_TIMED_OUT"),
    (-127, "ERR_PROXY_AUTH_REQUESTED"),
    (-130, "ERR_PROXY_CONNECTION_FAILED"),
    (-137, "ERR_NAME_RESOLUTION_FAILED"),
    (-200, "ERR_CERT_COMMON_NAME_INVALID"),
    (-201, "ERR_CERT_DATE_INVALID"),
    (-202, "ERR_CERT_AUTHORITY_INVALID"),
    (-206, "ERR_CERT_REVOKED"),
    (-300, "ERR_INVALID_URL"),
    (-302, "ERR_UNKNOWN_URL_SCHEME"),
    (-310, "ERR_TOO_MANY_REDIRECTS"),
    (-312, "ERR_UNSAFE_PORT"),
    (-320, "ERR_INVALID_RESPONSE"),
    (-324, "ERR_EMPTY_RESPONSE"),
];

/// WebView2's `COREWEBVIEW2_WEB_ERROR_STATUS` as a Chromium net error; `None` for a cancelled load.
fn webview2_net_error(status: i64) -> Option<i64> {
    Some(match status {
        1 => -200,
        2 => -201,
        3 => -117,
        4 => -206,
        // `CERTIFICATE_IS_INVALID`: what a self-signed development certificate reports.
        5 => -202,
        6 => -109,
        7 => -118,
        8 => -320,
        9 => -103,
        10 => -101,
        11 => -106,
        12 => -102,
        13 => -105,
        14 => return None,
        15 => -310,
        18 => -127,
        _ => -2,
    })
}

/// `WKWebView`'s `NSURLErrorDomain` and `WebKitErrorDomain` errors as a Chromium net error.
fn webkit_cocoa_net_error(domain: &str, code: i64) -> Option<i64> {
    Some(match (domain, code) {
        ("NSURLErrorDomain", -999) => return None,
        ("NSURLErrorDomain", -1000) => -300,
        ("NSURLErrorDomain", -1001) => -118,
        ("NSURLErrorDomain", -1002) => -302,
        ("NSURLErrorDomain", -1003 | -1006) => -105,
        ("NSURLErrorDomain", -1004) => -102,
        ("NSURLErrorDomain", -1005) => -100,
        ("NSURLErrorDomain", -1007) => -310,
        ("NSURLErrorDomain", -1009 | -1020) => -106,
        ("NSURLErrorDomain", -1011) => -320,
        ("NSURLErrorDomain", -1014) => -324,
        ("NSURLErrorDomain", -1200) => -107,
        ("NSURLErrorDomain", -1201 | -1204) => -201,
        ("NSURLErrorDomain", -1202 | -1203) => -202,
        ("NSURLErrorDomain", -1205) => -117,
        ("NSURLErrorDomain", -1206) => -110,
        // A response WebKit hands off (a download) or a plug-in takes, which Chromium aborts.
        ("WebKitErrorDomain", 100 | 102 | 204) => return None,
        ("WebKitErrorDomain", 101) => -302,
        ("WebKitErrorDomain", 103) => -312,
        _ => -2,
    })
}

/// `WebKitGTK`'s `GError` domains as a Chromium net error.
fn webkit_gtk_net_error(domain: &str, code: i64) -> Option<i64> {
    Some(match (domain, code) {
        ("WebKitNetworkError", 302) | ("g-io-error-quark", 19) => return None,
        ("WebKitPolicyError", 100 | 102) => return None,
        ("WebKitNetworkError", 301) | ("WebKitPolicyError", 101) => -302,
        ("WebKitNetworkError", 303) => -6,
        ("WebKitPolicyError", 103) => -312,
        ("g-io-error-quark", 24) => -118,
        ("g-io-error-quark", 28) | ("g-resolver-error-quark", 0 | 2) => -105,
        ("g-resolver-error-quark", 1) => -137,
        ("g-io-error-quark", 37 | 38) => -109,
        ("g-io-error-quark", 39) => -102,
        ("g-io-error-quark", 40) => -130,
        ("g-io-error-quark", 44) => -100,
        ("g-tls-error-quark", 2) => -202,
        ("g-tls-error-quark", 5) => -110,
        ("g-tls-error-quark", _) => -107,
        _ => -2,
    })
}

/// The net error Electron's `did-fail-load` reports for a main-frame navigation the platform's web
/// view failed (WebView2, `WKWebView` or `WebKitGTK`), so `LoadFailed` reads the same to agents, other
/// clients and the error page. `None` for what Electron leaves alone: an aborted load (superseded,
/// stopped or a download, `ERR_ABORTED`), and an HTTP error, whose page the server sent and shows.
pub fn net_error(error: &wry::PageLoadError) -> Option<(i64, &'static str)> {
    if error.http_status.is_some_and(|status| !(300..400).contains(&status)) {
        return None;
    }
    let domain = error.domain.as_str();
    let code = match domain {
        "COREWEBVIEW2_WEB_ERROR_STATUS" => webview2_net_error(error.code),
        "NSURLErrorDomain" | "WebKitErrorDomain" => webkit_cocoa_net_error(domain, error.code),
        _ => webkit_gtk_net_error(domain, error.code),
    }?;
    let name = NET_ERRORS.iter().find(|(known, _)| *known == code).map_or("ERR_FAILED", |(_, name)| name);
    Some((code, name))
}

/// Reports the top page's URL and back/forward state over wry's IPC on load and on each history
/// change, through Chromium's Navigation API (WebView2; WebKit lacks it and reports nothing).
pub const HISTORY_SCRIPT: &str = r#"(() => {
  if (window !== window.top || !window.navigation || !window.ipc) return;
  const post = () => window.ipc.postMessage(JSON.stringify({ t3History: { url: location.href, back: navigation.canGoBack, forward: navigation.canGoForward } }));
  post();
  navigation.addEventListener("currententrychange", post);
  addEventListener("pageshow", post);
})();"#;

/// [`HISTORY_SCRIPT`]'s message. The page can post one too, so its URL is only trusted within the
/// page's own origin.
#[derive(Clone, Debug, PartialEq)]
pub struct PageHistory {
    pub url: String,
    pub back: bool,
    pub forward: bool,
}

pub fn history_message(body: &str) -> Option<PageHistory> {
    let message: serde_json::Value = serde_json::from_str(body).ok()?;
    let history = message.get("t3History")?;
    Some(PageHistory {
        url: history.get("url")?.as_str()?.to_owned(),
        back: history.get("back")?.as_bool()?,
        forward: history.get("forward")?.as_bool()?,
    })
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RevealRequest {
    pub id: String,
    #[serde(default)]
    pub force: bool,
}

/// `PreviewSessionSnapshot`: one browser tab of a thread.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub thread_id: String,
    pub tab_id: String,
    #[serde(default)]
    pub nav_status: NavStatus,
    #[serde(default)]
    pub can_go_back: bool,
    #[serde(default)]
    pub can_go_forward: bool,
    #[serde(default)]
    pub viewport: Viewport,
    #[serde(default)]
    pub color_scheme: Option<Appearance>,
    #[serde(default)]
    pub zoom_factor: Option<f64>,
    /// `"desktop"` (or absent) is a tab this client hosts; `"server"` runs in the server's Chromium.
    #[serde(default)]
    pub runtime: Option<String>,
    #[serde(default)]
    pub headless: bool,
    /// Fixed at open: the profile whose cookies the tab uses.
    #[serde(default)]
    pub profile_id: Option<String>,
    #[serde(default)]
    pub reveal: Option<bool>,
    #[serde(default)]
    pub reveal_request: Option<RevealRequest>,
    #[serde(default)]
    pub updated_at: String,
}

impl Session {
    pub fn server_hosted(&self) -> bool {
        self.runtime.as_deref() == Some("server")
    }
}

/// A preview tab's sessions for one thread, reconciled from `preview.list` and
/// `subscribePreviewEvents` like the web's `ThreadPreviewState`.
#[derive(Clone, Debug, Default)]
pub struct ThreadPreviews {
    /// In open order, so tabs keep their place.
    pub sessions: Vec<Session>,
    pub active_tab: Option<String>,
    server_epoch: Option<String>,
    revision: u64,
    pub list_loaded: bool,
    /// Tabs whose close is in flight (`suppressedTabIds`): their snapshots stay out until it settles.
    closing: std::collections::HashSet<String>,
}

/// A tab taken out by [`ThreadPreviews::begin_close`], to put back if the close fails.
#[derive(Debug)]
pub struct ClosingTab {
    tab_id: String,
    restore: Option<(usize, Session)>,
}

/// What an applied event asks of the caller.
#[derive(Debug, PartialEq, Eq)]
pub enum Applied {
    Ignored,
    Changed,
    /// The server restarted: its list replaces ours.
    Relist,
}

impl ThreadPreviews {
    pub fn active(&self) -> Option<&Session> {
        let tab = self.active_tab.as_deref()?;
        self.sessions.iter().find(|s| s.tab_id == tab)
    }

    pub fn get(&self, tab_id: &str) -> Option<&Session> {
        self.sessions.iter().find(|s| s.tab_id == tab_id)
    }

    /// `freshlyRevealedServerTabs`: server tabs an agent revealed since `previous`, new ones or
    /// ones with a new reveal request, each with whether the reveal is forced. Nothing is fresh
    /// before the first list loads: that list is the baseline, so a reload never reopens
    /// previews the user dismissed.
    pub fn fresh_reveals(&self, previous: &Self) -> Vec<(String, bool)> {
        if !previous.list_loaded {
            return Vec::new();
        }
        self.sessions
            .iter()
            .filter(|session| session.server_hosted() && session.reveal == Some(true))
            .filter(|session| {
                let before = previous.get(&session.tab_id);
                match &session.reveal_request {
                    Some(request) => before.and_then(|b| b.reveal_request.as_ref()).is_none_or(|b| b.id != request.id),
                    None => before.is_none(),
                }
            })
            .map(|session| (session.tab_id.clone(), session.reveal_request.as_ref().is_some_and(|r| r.force)))
            .collect()
    }

    /// Keeps a newer local copy when the incoming one is older (`updatedAt` is ISO-8601).
    fn upsert(&mut self, session: Session) {
        if self.closing.contains(&session.tab_id) {
            return;
        }
        match self.sessions.iter_mut().find(|s| s.tab_id == session.tab_id) {
            Some(existing) if existing.updated_at > session.updated_at => {}
            Some(existing) => *existing = session,
            None => self.sessions.push(session),
        }
    }

    /// `preview.list`'s result (`reconcilePreviewServerSessions`).
    pub fn apply_list(&mut self, result: &Value) -> Result<(), serde_json::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct ListResult {
            sessions: Vec<Session>,
            server_epoch: String,
            revision: u64,
        }
        let list = ListResult::deserialize(result)?;
        let same_server = self.server_epoch.as_deref() == Some(list.server_epoch.as_str());
        if same_server && list.revision < self.revision {
            return Ok(());
        }
        let previous = std::mem::take(&mut self.sessions);
        if !same_server {
            self.closing.clear();
        }
        self.closing.retain(|tab| list.sessions.iter().any(|s| &s.tab_id == tab));
        for session in list.sessions.into_iter().filter(|s| !self.closing.contains(&s.tab_id)) {
            let newer = previous
                .iter()
                .find(|s| same_server && s.tab_id == session.tab_id && s.updated_at > session.updated_at);
            self.sessions.push(newer.cloned().unwrap_or(session));
        }
        if self.active().is_none() {
            self.active_tab = self.latest().map(|s| s.tab_id.clone());
        }
        self.server_epoch = Some(list.server_epoch);
        self.revision = list.revision;
        self.list_loaded = true;
        Ok(())
    }

    fn latest(&self) -> Option<&Session> {
        self.sessions.iter().max_by(|a, b| a.updated_at.cmp(&b.updated_at))
    }

    /// One `subscribePreviewEvents` item for this thread (`applyPreviewServerEvent`).
    pub fn apply_event(&mut self, event: &Value) -> Result<Applied, serde_json::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Event {
            #[serde(rename = "type")]
            kind: String,
            tab_id: String,
            server_epoch: String,
            revision: u64,
            #[serde(default)]
            created_at: String,
            #[serde(default)]
            snapshot: Option<Session>,
            #[serde(default)]
            url: String,
            #[serde(default)]
            title: String,
            #[serde(default)]
            code: i64,
            #[serde(default)]
            description: String,
            #[serde(default)]
            download: Option<FileDownload>,
        }
        let event = Event::deserialize(event)?;
        if self.server_epoch.as_deref().is_some_and(|epoch| epoch != event.server_epoch) {
            return Ok(Applied::Relist);
        }
        if event.revision < self.revision {
            return Ok(Applied::Ignored);
        }
        match event.kind.as_str() {
            "opened" | "navigated" | "resized" => {
                let Some(snapshot) = event.snapshot else { return Ok(Applied::Ignored) };
                if self.closing.contains(&snapshot.tab_id) {
                    return Ok(Applied::Ignored);
                }
                if event.kind == "opened" || self.active_tab.is_none() {
                    self.active_tab = Some(snapshot.tab_id.clone());
                }
                self.upsert(snapshot);
            }
            "failed" => {
                let Some(session) = self.sessions.iter_mut().find(|s| s.tab_id == event.tab_id) else {
                    return Ok(Applied::Ignored);
                };
                session.nav_status = NavStatus::LoadFailed {
                    url: event.url,
                    title: event.title,
                    code: event.code,
                    description: event.description,
                    download: event.download,
                };
                session.updated_at = event.created_at;
            }
            "closed" => {
                self.remove(&event.tab_id);
                self.closing.remove(&event.tab_id);
            }
            _ => return Ok(Applied::Ignored),
        }
        self.server_epoch = Some(event.server_epoch);
        self.revision = event.revision;
        Ok(Applied::Changed)
    }

    /// A snapshot a request returned (`applyPreviewServerSnapshot`); `focus` makes it the active tab.
    pub fn apply_snapshot(&mut self, snapshot: Session, focus: bool) {
        if self.closing.contains(&snapshot.tab_id) {
            return;
        }
        if focus || self.active().is_none() {
            self.active_tab = Some(snapshot.tab_id.clone());
        }
        self.upsert(snapshot);
    }

    /// `beginPreviewSessionClose`: the tab leaves the list while `preview.close` runs.
    pub fn begin_close(&mut self, tab_id: &str) -> ClosingTab {
        let restore = self.sessions.iter().position(|s| s.tab_id == tab_id).map(|i| (i, self.sessions[i].clone()));
        self.remove(tab_id);
        self.closing.insert(tab_id.to_owned());
        ClosingTab { tab_id: tab_id.to_owned(), restore }
    }

    /// `cancelPreviewSessionClose`: a failed close puts the tab back in its place, focused.
    pub fn cancel_close(&mut self, closing: ClosingTab) {
        if !self.closing.remove(&closing.tab_id) {
            return;
        }
        let Some((index, session)) = closing.restore else { return };
        self.active_tab = Some(session.tab_id.clone());
        if self.get(&session.tab_id).is_none() {
            self.sessions.insert(index.min(self.sessions.len()), session);
        }
    }

    pub fn remove(&mut self, tab_id: &str) {
        let Some(index) = self.sessions.iter().position(|s| s.tab_id == tab_id) else { return };
        self.sessions.remove(index);
        if self.active_tab.as_deref() == Some(tab_id) {
            // The neighbour takes focus, as closing a browser tab does.
            self.active_tab =
                self.sessions.get(index.min(self.sessions.len().saturating_sub(1))).map(|s| s.tab_id.clone());
        }
    }
}

const LOOPBACK_PREFIXES: [&str; 5] = ["localhost", "127.0.0.1", "0.0.0.0", "[::]", "[::1]"];
const KNOWN_NON_WEB_SCHEMES: [&str; 14] = [
    "about",
    "blob",
    "chrome",
    "data",
    "file",
    "ftp",
    "javascript",
    "mailto",
    "sms",
    "ssh",
    "tel",
    "view-source",
    "ws",
    "wss",
];

/// The DuckDuckGo template, the web's default `browserSearchEngine`.
pub const DEFAULT_SEARCH_TEMPLATE: &str = "https://duckduckgo.com/?q=%s";

/// `resolveSearchUrlTemplate`: the engine's template, or a valid custom one, else DuckDuckGo.
pub fn search_template(settings: &crate::settings::Settings) -> String {
    let template = match settings.get("browserSearchEngine").as_str().unwrap_or_default() {
        "google" => "https://www.google.com/search?q=%s",
        "bing" => "https://www.bing.com/search?q=%s",
        "brave" => "https://search.brave.com/search?q=%s",
        "kagi" => "https://kagi.com/search?q=%s",
        "ecosia" => "https://www.ecosia.org/search?q=%s",
        "custom" => {
            let custom = settings.get("browserCustomSearchUrl").as_str().unwrap_or_default().trim();
            if valid_search_template(custom) { custom } else { DEFAULT_SEARCH_TEMPLATE }
        }
        _ => DEFAULT_SEARCH_TEMPLATE,
    };
    template.to_owned()
}

/// `isValidSearchUrlTemplate`: an http(s) URL whose `%s` sits after the authority.
pub fn valid_search_template(template: &str) -> bool {
    let fill = |query: &str| url::Url::parse(&template.replace("%s", &encode_uri_component(query))).ok();
    let authority = |url: &url::Url| {
        (url.username().to_owned(), url.password().map(str::to_owned), url.host_str().map(str::to_owned), url.port())
    };
    match (template.contains("%s"), fill("a"), fill("b")) {
        (true, Some(a), Some(b)) => matches!(a.scheme(), "http" | "https") && authority(&a) == authority(&b),
        _ => false,
    }
}

/// `resolveLinkTarget`: whether a clicked link opens in a browser tab rather than the system
/// browser. A modified or middle click always goes to the system browser.
pub fn link_opens_in_app(url: &str, modified: bool, settings: &crate::settings::Settings, can_open: bool) -> bool {
    !modified
        && can_open
        && settings.get("browserLinkTarget").as_str() == Some("app")
        && url::Url::parse(url).is_ok_and(|url| matches!(url.scheme(), "http" | "https"))
}

fn explicit_scheme(input: &str) -> Option<String> {
    let colon = input.find(':')?;
    let scheme = &input[..colon];
    let mut chars = scheme.chars();
    (chars.next()?.is_ascii_alphabetic() && chars.all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c)))
        .then(|| scheme.to_ascii_lowercase())
}

fn has_scheme_with_authority(input: &str) -> bool {
    explicit_scheme(input).is_some_and(|scheme| input[scheme.len()..].starts_with("://"))
}

fn is_loopback_prefixed(input: &str) -> bool {
    let lower = input.to_ascii_lowercase();
    LOOPBACK_PREFIXES.iter().any(|host| {
        lower.strip_prefix(host).is_some_and(|rest| rest.is_empty() || rest.starts_with(':') || rest.starts_with('/'))
    })
}

/// `normalizePreviewUrl`: an http(s) URL, defaulting loopback hosts to http and the rest to https.
pub fn normalize_url(raw: &str) -> Result<String, &'static str> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("Enter a URL.");
    }
    if explicit_scheme(trimmed).is_some_and(|scheme| KNOWN_NON_WEB_SCHEMES.contains(&scheme.as_str())) {
        return Err("Enter an http or https URL.");
    }
    let candidate = if has_scheme_with_authority(trimmed) {
        trimmed.to_owned()
    } else {
        format!("{}://{trimmed}", if is_loopback_prefixed(trimmed) { "http" } else { "https" })
    };
    let parsed = url::Url::parse(&candidate).map_err(|_| "Enter an http or https URL.")?;
    match parsed.scheme() {
        "http" | "https" => Ok(parsed.into()),
        _ => Err("Enter an http or https URL."),
    }
}

/// `resolveAddressBarInput`: a URL when the input looks like one, otherwise a search.
pub fn resolve_address(input: &str, search_template: &str) -> Result<String, &'static str> {
    let trimmed = input.trim();
    if trimmed.is_empty() || has_scheme_with_authority(trimmed) {
        return normalize_url(trimmed);
    }
    if !trimmed.contains(char::is_whitespace) {
        if let Some(scheme) = explicit_scheme(trimmed) {
            let bare_host_port = {
                let rest = &trimmed[scheme.len() + 1..];
                let digits = rest.chars().take_while(char::is_ascii_digit).count();
                digits > 0 && rest[digits..].chars().next().is_none_or(|c| "/?#".contains(c))
            };
            if KNOWN_NON_WEB_SCHEMES.contains(&scheme.as_str()) || !bare_host_port {
                if scheme != "http" && scheme != "https" {
                    return Err("Enter an http or https URL.");
                }
                return normalize_url(trimmed);
            }
        }
        let authority = trimmed.split(['/', '?', '#']).next().unwrap_or_default();
        let authority = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
        let host = match authority.rsplit_once(':') {
            Some((host, port)) if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => host,
            _ => authority,
        };
        let has_port = host.len() < authority.len();
        let ipv4 = host.split('.').count() == 4
            && host.split('.').all(|part| (1..=3).contains(&part.len()) && part.chars().all(|c| c.is_ascii_digit()));
        let bracketed = host.starts_with('[') && host.ends_with(']');
        let dotted = {
            let labels: Vec<&str> = host.trim_end_matches('.').split('.').collect();
            labels.len() >= 2 && labels.iter().all(|label| !label.is_empty())
        };
        if (has_port || host.eq_ignore_ascii_case("localhost") || ipv4 || bracketed || dotted)
            && let Ok(url) = normalize_url(trimmed)
        {
            return Ok(url);
        }
    }
    Ok(search_template.replace("%s", &encode_uri_component(trimmed)))
}

/// JavaScript's `encodeURIComponent`.
fn encode_uri_component(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
            out.push(byte as char);
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

/// `normalizeHostname`.
fn normalize_hostname(host: &str) -> String {
    host.to_ascii_lowercase().trim_start_matches('[').trim_end_matches(']').trim_end_matches('.').to_owned()
}

/// `isLocalLoopbackHost`.
fn is_local_loopback_host(host: &str) -> bool {
    let host = normalize_hostname(host);
    host == "localhost" || host == "::1" || host.parse::<std::net::Ipv4Addr>().is_ok_and(|ip| ip.octets()[0] == 127)
}

/// `isPrivateNetworkHost` (`packages/shared/src/hostClassification.ts`).
fn is_private_network_host(host: &str) -> bool {
    let host = normalize_hostname(host);
    if host == "::"
        || host == "home.arpa"
        || is_local_loopback_host(&host)
        || [".localhost", ".local", ".home.arpa", ".ts.net"].iter().any(|suffix| host.ends_with(suffix))
        || (!host.contains('.') && !host.contains(':'))
    {
        return true;
    }
    let private_v4 = |ip: std::net::Ipv4Addr| {
        let [a, b, ..] = ip.octets();
        matches!(a, 0 | 10 | 127)
            || (a == 100 && (64..=127).contains(&b))
            || (a == 172 && (16..=31).contains(&b))
            || (a == 192 && b == 168)
            || (a == 169 && b == 254)
            || (a == 198 && (18..=19).contains(&b))
    };
    if let Ok(ip) = host.parse::<std::net::Ipv4Addr>() {
        return private_v4(ip);
    }
    match host.parse::<std::net::Ipv6Addr>() {
        Ok(ip) => ip
            .to_ipv4_mapped()
            .map_or_else(|| (ip.segments()[0] & 0xfe00) == 0xfc00 || (ip.segments()[0] & 0xffc0) == 0xfe80, private_v4),
        Err(_) => false,
    }
}

/// `resolveDiscoveredServerUrl`: a loopback server URL from the environment's scan, pointed at the
/// environment's host when this computer's loopback is another machine's (`environment_base` is
/// its HTTP base then; `None` for this app's own server). What it can't map stays as given.
pub fn resolve_discovered_url(raw: &str, environment_base: Option<&str>) -> String {
    let Ok(normalized) = normalize_url(raw) else { return raw.to_owned() };
    let Ok(mut parsed) = url::Url::parse(&normalized) else { return raw.to_owned() };
    let loopback =
        parsed.host_str().is_some_and(|host| matches!(host, "localhost" | "127.0.0.1" | "0.0.0.0" | "[::1]"));
    let Some(base) = environment_base.filter(|_| loopback) else { return normalized };
    let Some(environment_host) = url::Url::parse(base).ok().and_then(|url| url.host_str().map(normalize_hostname))
    else {
        return raw.to_owned();
    };
    // Past the private network this needs the planned authenticated preview gateway.
    if !is_private_network_host(&environment_host) {
        return raw.to_owned();
    }
    // localhost lets the browser reach loopback servers bound to either IPv4 or IPv6.
    let host = if is_local_loopback_host(&environment_host) {
        "localhost".to_owned()
    } else if environment_host.contains(':') {
        format!("[{environment_host}]")
    } else {
        environment_host
    };
    let port = parsed.port_or_known_default();
    if parsed.set_host(Some(&host)).is_err() || parsed.set_port(port).is_err() {
        return raw.to_owned();
    }
    parsed.into()
}

/// `previewPdfAssetUrl`: a PDF of the server's signed assets that a server tab opened, as this
/// client fetches it. The server's browser may reach it on its host's loopback, so such a URL goes
/// through this client's own connection to the environment instead.
pub fn pdf_asset_url(raw: &str, http_base: &str) -> Option<String> {
    let url = url::Url::parse(raw).ok()?;
    let base = url::Url::parse(http_base).ok()?;
    let path = url.path();
    let pdf = path.len() > 4 && path.get(path.len() - 4..).is_some_and(|ext| ext.eq_ignore_ascii_case(".pdf"));
    if !matches!(url.scheme(), "http" | "https") || !path.starts_with("/api/assets/") || !pdf {
        return None;
    }
    let host = url.host_str().unwrap_or_default();
    if url.origin() != base.origin() && !is_local_loopback_host(host) && host != "0.0.0.0" {
        return Some(url.into());
    }
    let mut local = base.join(path).ok()?;
    local.set_query(url.query());
    local.set_fragment(url.fragment());
    Some(local.into())
}

/// The address bar's text: the page URL, or nothing for a blank tab.
pub fn display_url(session: &Session) -> &str {
    session.nav_status.url().filter(|url| *url != "about:blank").unwrap_or_default()
}

/// `PREVIEW_VIEWPORT_{MIN,MAX}_DIMENSION` and `PREVIEW_VIEWPORT_MAX_AREA`.
pub const VIEWPORT_MIN_DIMENSION: i64 = 240;
pub const VIEWPORT_MAX_DIMENSION: i64 = 3840;
const VIEWPORT_MAX_AREA: i64 = 3840 * 2160;
/// The size "Responsive" starts at when switched to from "Fill panel" (`RESPONSIVE_SEED_SIZE`).
const RESPONSIVE_SEED: (u32, u32) = (1280, 800);

impl Viewport {
    pub fn to_json(&self) -> Value {
        match self {
            Viewport::Fill => json!({"_tag": "fill"}),
            Viewport::Freeform { width, height } => json!({"_tag": "freeform", "width": width, "height": height}),
            Viewport::Preset { width, height, preset_id } => {
                json!({"_tag": "preset", "presetId": preset_id, "width": width, "height": height})
            }
        }
    }

    /// The Settings select's value: `fill`, a known preset's id, else `responsive` (`viewportSelectValue`).
    pub fn choice(&self) -> &str {
        match self {
            Viewport::Fill => "fill",
            Viewport::Preset { preset_id, .. } if PRESETS.iter().any(|p| p.id == preset_id) => preset_id,
            _ => "responsive",
        }
    }

    /// The size the Settings fields show; "Fill panel" shows none.
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "viewport sizes are whole pixels")]
    fn shown_size(&self) -> (u32, u32) {
        self.size().map_or(RESPONSIVE_SEED, |(w, h)| (w as u32, h as u32))
    }

    /// The viewport a Settings select pick saves (`selectViewport`): responsive keeps the size.
    pub fn chosen(&self, choice: &str) -> Option<Viewport> {
        let (width, height) = self.shown_size();
        match choice {
            "fill" => Some(Viewport::Fill),
            "responsive" => Some(Viewport::Freeform { width, height }),
            id => PRESETS.iter().find(|p| p.id == id).map(|p| Viewport::Preset {
                width: p.width,
                height: p.height,
                preset_id: p.id.to_owned(),
            }),
        }
    }

    /// A typed width or height (`commitDimension`): whole, in range and under the area cap, else nothing.
    pub fn with_dimension(&self, width_axis: bool, value: i64) -> Option<Viewport> {
        if !(VIEWPORT_MIN_DIMENSION..=VIEWPORT_MAX_DIMENSION).contains(&value) {
            return None;
        }
        let (width, height) = self.shown_size();
        let value = u32::try_from(value).ok()?;
        let (width, height) = if width_axis { (value, height) } else { (width, value) };
        if i64::from(width) * i64::from(height) > VIEWPORT_MAX_AREA
            || self.size() == Some((width as f32, height as f32))
        {
            return None;
        }
        Some(Viewport::Freeform { width, height })
    }

    /// Width and height swapped, keeping a preset a preset.
    pub fn rotated(&self) -> Viewport {
        match self.clone() {
            Viewport::Fill => Viewport::Fill,
            Viewport::Freeform { width, height } => Viewport::Freeform { width: height, height: width },
            Viewport::Preset { width, height, preset_id } => {
                Viewport::Preset { width: height, height: width, preset_id }
            }
        }
    }
}

/// `packages/contracts/src/browserProfile.ts`.
pub const DEFAULT_PROFILE_ID: &str = "default";
pub const INCOGNITO_PROFILE_ID: &str = "incognito";
pub const PROFILE_MAX_COUNT: usize = 24;
pub const PROFILE_NAME_MAX_LENGTH: usize = 48;

#[derive(Clone, Copy, Debug, Default, Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ProfileKind {
    #[default]
    Persistent,
    Incognito,
}

/// A browser profile: its own cookies and logins, for this client's tabs and the server's browser.
#[derive(Clone, Debug, Deserialize, serde::Serialize, PartialEq, Eq)]
pub struct Profile {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub kind: ProfileKind,
}

impl Profile {
    pub fn built_in(&self) -> bool {
        self.id == DEFAULT_PROFILE_ID || self.id == INCOGNITO_PROFILE_ID
    }
}

/// The stored `browserProfiles`, the user's own. An entry this build can't read is skipped, not the whole list.
pub fn user_profiles(settings: &crate::settings::Settings) -> Vec<Profile> {
    stored_profiles(settings).iter().filter_map(|value| Profile::deserialize(value).ok()).collect()
}

/// The stored entries as written. Edits start from these, so entries and fields this build can't read survive.
fn stored_profiles(settings: &crate::settings::Settings) -> &[Value] {
    settings.get("browserProfiles").as_array().map_or(&[], Vec::as_slice)
}

/// No room for another profile: the stored list, readable or not, is at the contract's limit.
pub fn profiles_full(settings: &crate::settings::Settings) -> bool {
    stored_profiles(settings).len() >= PROFILE_MAX_COUNT
}

/// `browserProfiles` with `profile` appended, or `None` when the list is full.
pub fn profiles_with_added(settings: &crate::settings::Settings, profile: &Profile) -> Option<Value> {
    if profiles_full(settings) {
        return None;
    }
    let stored = stored_profiles(settings);
    let added = serde_json::to_value(profile).expect("a profile is plain strings");
    Some(Value::Array(stored.iter().cloned().chain([added]).collect()))
}

/// `browserProfiles` with profile `id` renamed to `name`.
pub fn profiles_with_renamed(settings: &crate::settings::Settings, id: &str, name: &str) -> Value {
    let mut stored = stored_profiles(settings).to_vec();
    for entry in stored.iter_mut().filter(|entry| entry["id"] == id) {
        entry["name"] = name.into();
    }
    Value::Array(stored)
}

/// `browserProfiles` without profile `id`.
pub fn profiles_without(settings: &crate::settings::Settings, id: &str) -> Value {
    stored_profiles(settings).iter().filter(|entry| entry["id"] != id).cloned().collect()
}

/// `resolveBrowserProfiles`: the built-ins, then the user's without ids taken, all persistent.
pub fn resolve_profiles(user: &[Profile]) -> Vec<Profile> {
    let built_in = |id: &str, name: &str, kind| Profile { id: id.into(), name: name.into(), kind };
    let mut resolved = vec![
        built_in(DEFAULT_PROFILE_ID, "Default", ProfileKind::Persistent),
        built_in(INCOGNITO_PROFILE_ID, "Incognito", ProfileKind::Incognito),
    ];
    for profile in user {
        if resolved.iter().all(|p| p.id != profile.id) {
            resolved.push(Profile { kind: ProfileKind::Persistent, ..profile.clone() });
        }
    }
    resolved
}

/// `BrowserDefaults.profileId`: `browserDefaultProfileId` when it names a listed, non-incognito profile.
pub fn default_profile_id(settings: &crate::settings::Settings) -> String {
    let wanted = settings.get("browserDefaultProfileId").as_str().unwrap_or_default();
    resolve_profiles(&user_profiles(settings))
        .into_iter()
        .find(|p| p.id == wanted && p.kind != ProfileKind::Incognito)
        .map_or_else(|| DEFAULT_PROFILE_ID.to_owned(), |p| p.id)
}

/// `base`, else the first free `base 2`, `base 3`, … among `profiles`' names.
pub fn unique_profile_name(base: &str, profiles: &[Profile]) -> String {
    let taken = |name: &str| profiles.iter().any(|p| p.name == name);
    if !taken(base) {
        return base.to_owned();
    }
    (2..).map(|index| format!("{base} {index}")).find(|name| !taken(name)).unwrap_or_default()
}

/// Where this client's tabs of a profile keep their data, like Electron's session partitions.
#[derive(Debug, PartialEq, Eq)]
pub enum Partition {
    /// The browser's own folder, where tabs lived before profiles.
    Default,
    /// In memory, gone when the app closes.
    Incognito,
    /// A folder of its own, named by a hash so any id makes a short, valid name.
    Folder(String),
}

pub fn partition(profile_id: Option<&str>) -> Partition {
    use sha2::Digest as _;
    match profile_id {
        None | Some(DEFAULT_PROFILE_ID) => Partition::Default,
        Some(INCOGNITO_PROFILE_ID) => Partition::Incognito,
        Some(id) => Partition::Folder(crate::hex::lower_hex(&sha2::Sha256::digest(id.as_bytes())[..16])),
    }
}

/// `preview.reportProfiles`' input (`BrowserProfileReporter`): the server's browser takes this
/// client's profiles, default profile and recording frame rate for the tabs agents open.
pub fn profile_report(settings: &crate::settings::Settings) -> Value {
    json!({
        "profiles": settings.get("browserProfiles"),
        "defaultProfileId": settings.get("browserDefaultProfileId"),
        "recordingFrameRate": settings.get("browserRecordingFrameRate"),
    })
}

/// `resolveDiscoveredServerUrl`: a loopback server another machine discovered, at that
/// machine's address. `environment_base` is the hosting environment's `httpBaseUrl`, `None` for
/// this machine's own server, which reaches loopback directly.
pub fn resolve_discovered_server_url(raw: &str, environment_base: Option<&str>) -> String {
    let Ok(normalized) = normalize_url(raw) else { return raw.to_owned() };
    let Ok(mut parsed) = url::Url::parse(&normalized) else { return raw.to_owned() };
    let loopback = matches!(parsed.host_str(), Some("localhost" | "127.0.0.1" | "0.0.0.0" | "[::1]"));
    let Some(base) = environment_base.filter(|_| loopback) else { return normalized };
    let Some(host) = url::Url::parse(base).ok().and_then(|base| base.host_str().map(normalize_hostname)) else {
        return raw.to_owned();
    };
    // `resolveEnvironmentPortTarget` reaches only private-network servers directly.
    if !is_private_network_host(&host) {
        return raw.to_owned();
    }
    let port = parsed.port_or_known_default();
    // localhost lets the browser reach loopback servers bound to either IPv4 or IPv6.
    let host = if is_local_loopback_host(&host) {
        "localhost".to_owned()
    } else if host.contains(':') {
        format!("[{host}]")
    } else {
        host
    };
    if parsed.set_host(Some(&host)).is_err() || parsed.set_port(port).is_err() {
        return raw.to_owned();
    }
    parsed.into()
}

#[cfg(test)]
mod tests {
    #[test]
    fn pdf_asset_urls_go_through_this_clients_connection() {
        let base = "http://env.test:3773";
        assert_eq!(
            pdf_asset_url("http://127.0.0.1:3773/api/assets/t/a.PDF?sig=1#page=2", base).as_deref(),
            Some("http://env.test:3773/api/assets/t/a.PDF?sig=1#page=2"),
            "the host's loopback is reached through the environment"
        );
        assert_eq!(
            pdf_asset_url("https://cdn.test/api/assets/a.pdf", base).as_deref(),
            Some("https://cdn.test/api/assets/a.pdf"),
            "another public origin stays as it is"
        );
        assert_eq!(pdf_asset_url("http://env.test:3773/files/a.pdf", base), None, "not a signed asset");
        assert_eq!(pdf_asset_url("http://env.test:3773/api/assets/a.png", base), None);
        assert_eq!(pdf_asset_url("file:///api/assets/a.pdf", base), None);
    }

    use super::*;

    #[test]
    fn discovered_servers_open_at_the_machine_that_hosts_them() {
        let resolve = resolve_discovered_server_url;
        // This machine reaches its own loopback servers.
        assert_eq!(resolve("localhost:5173/app?x=1", None), "http://localhost:5173/app?x=1");
        assert_eq!(resolve("http://127.0.0.1:3000", Some("http://192.168.1.20:3773")), "http://192.168.1.20:3000/");
        assert_eq!(
            resolve("http://[::1]:8080/a#b", Some("http://box.tail1234.ts.net:3773")),
            "http://box.tail1234.ts.net:8080/a#b"
        );
        assert_eq!(
            resolve("http://localhost:4000", Some("http://[fd7a:115c:a1e0::1]:3773")),
            "http://[fd7a:115c:a1e0::1]:4000/"
        );
        assert_eq!(resolve("http://localhost:4000", Some("http://127.0.0.1:3773")), "http://localhost:4000/");
        // A public address needs a preview gateway, so the raw URL opens.
        assert_eq!(resolve("http://localhost:4000", Some("https://t3.example.com")), "http://localhost:4000");
        // A server already bound to a reachable address opens as it is.
        assert_eq!(resolve("http://192.168.1.20:4000", Some("http://10.0.0.2:3773")), "http://192.168.1.20:4000/");
    }

    fn session(tab: &str, updated_at: &str) -> Value {
        json!({
            "threadId": "t1", "tabId": tab, "updatedAt": updated_at,
            "navStatus": {"_tag": "Success", "url": format!("https://{tab}.test/"), "title": tab},
            "canGoBack": false, "canGoForward": false,
        })
    }

    #[test]
    fn agent_reveals_are_fresh_once_per_tab_or_request() {
        let server = |tab: &str, request: Value| {
            let mut value = session(tab, "2026-01-01T00:00:00Z");
            value["runtime"] = "server".into();
            value["reveal"] = true.into();
            value["revealRequest"] = request;
            serde_json::from_value::<Session>(value).expect("session")
        };
        let previews = |loaded: bool, sessions: Vec<Session>| ThreadPreviews {
            sessions,
            list_loaded: loaded,
            ..ThreadPreviews::default()
        };
        let quiet = server("a", Value::Null);
        let asked = server("b", json!({"id": "r1"}));
        let forced = server("b", json!({"id": "r2", "force": true}));
        let mut local = serde_json::from_value::<Session>(session("c", "2026-01-01T00:00:00Z")).expect("session");
        local.reveal = Some(true);

        let before = previews(true, vec![]);
        let after = previews(true, vec![quiet.clone(), asked, local]);
        assert_eq!(after.fresh_reveals(&before), [("a".to_owned(), false), ("b".to_owned(), false)]);
        assert!(after.fresh_reveals(&previews(false, vec![])).is_empty(), "the first list is the baseline");
        assert!(after.fresh_reveals(&after).is_empty(), "already shown");
        let again = previews(true, vec![quiet, forced]);
        assert_eq!(again.fresh_reveals(&after), [("b".to_owned(), true)], "a new request reveals again");
    }

    #[test]
    fn discovered_servers_resolve_against_the_environment_host() {
        let local = resolve_discovered_url("localhost:5173", None);
        assert_eq!(local, "http://localhost:5173/", "this app's own server keeps loopback");
        let lan = Some("http://192.168.1.20:3773");
        assert_eq!(
            resolve_discovered_url("http://localhost:5173/app?x=1#h", lan),
            "http://192.168.1.20:5173/app?x=1#h"
        );
        assert_eq!(resolve_discovered_url("0.0.0.0:3000", lan), "http://192.168.1.20:3000/");
        assert_eq!(resolve_discovered_url("http://[::1]:8080/", lan), "http://192.168.1.20:8080/");
        assert_eq!(
            resolve_discovered_url("http://localhost/", lan),
            "http://192.168.1.20/",
            "the default port stays implicit"
        );
        assert_eq!(
            resolve_discovered_url("http://localhost:5173/", Some("https://box.tail1.ts.net")),
            "http://box.tail1.ts.net:5173/"
        );
        assert_eq!(
            resolve_discovered_url("http://localhost:5173/", Some("http://[fd7a:115c::5]:3773")),
            "http://[fd7a:115c::5]:5173/"
        );
        assert_eq!(
            resolve_discovered_url("http://127.0.0.1:5173/", Some("http://127.0.0.1:4100")),
            "http://localhost:5173/",
            "a tunnel's loopback"
        );
        assert_eq!(
            resolve_discovered_url("http://localhost:5173/", Some("https://t3.example.com")),
            "http://localhost:5173/",
            "a public host has no route yet"
        );
        assert_eq!(
            resolve_discovered_url("http://10.0.0.5:5173/", lan),
            "http://10.0.0.5:5173/",
            "only loopback URLs move"
        );
        assert!(
            is_private_network_host("nas")
                && is_private_network_host("::ffff:10.1.2.3")
                && is_private_network_host("fe80::1")
        );
        assert!(!is_private_network_host("8.8.8.8") && !is_private_network_host("2001:db8::1"));
    }

    #[test]
    fn resolves_addresses_like_the_web() {
        let search = DEFAULT_SEARCH_TEMPLATE;
        let cases = [
            ("localhost:5173", "http://localhost:5173/"),
            ("127.0.0.1:3000/a?b=1", "http://127.0.0.1:3000/a?b=1"),
            ("example.com", "https://example.com/"),
            ("https://example.com/x", "https://example.com/x"),
            ("myhost:8080", "https://myhost:8080/"),
            ("hello world", "https://duckduckgo.com/?q=hello%20world"),
            ("rust", "https://duckduckgo.com/?q=rust"),
            ("a&b", "https://duckduckgo.com/?q=a%26b"),
        ];
        for (input, expected) in cases {
            assert_eq!(resolve_address(input, search).as_deref(), Ok(expected), "{input}");
        }
        for rejected in ["mailto:a@b.c", "file:///etc/passwd", "javascript:alert(1)", ""] {
            assert!(resolve_address(rejected, search).is_err(), "{rejected}");
        }
    }

    #[test]
    fn search_engine_and_link_target_follow_the_settings() {
        let settings =
            |patch: Value| crate::settings::Settings::layered(&crate::settings::contract().client_defaults, &patch);
        let template = |engine: &str, custom: &str| {
            search_template(&settings(json!({"browserSearchEngine": engine, "browserCustomSearchUrl": custom})))
        };
        assert_eq!(template("duckduckgo", ""), DEFAULT_SEARCH_TEMPLATE);
        assert_eq!(template("kagi", ""), "https://kagi.com/search?q=%s");
        assert_eq!(template("custom", " https://s.test/find?q=%s "), "https://s.test/find?q=%s");
        assert_eq!(template("custom", "https://%s.test/"), DEFAULT_SEARCH_TEMPLATE, "the query picks the host");
        assert_eq!(template("custom", "https://s.test/find"), DEFAULT_SEARCH_TEMPLATE, "no %s");
        assert_eq!(template("custom", "ftp://s.test/?q=%s"), DEFAULT_SEARCH_TEMPLATE);

        let app = settings(json!({"browserLinkTarget": "app"}));
        assert!(link_opens_in_app("https://example.com/", false, &app, true));
        assert!(!link_opens_in_app("https://example.com/", true, &app, true), "ctrl or middle click");
        assert!(!link_opens_in_app("https://example.com/", false, &app, false), "no thread to host it");
        assert!(!link_opens_in_app("mailto:a@b.c", false, &app, true));
        assert!(!link_opens_in_app("https://example.com/", false, &settings(json!({})), true), "system default");
    }

    #[test]
    fn settings_viewport_picks_and_typed_sizes_follow_the_web() {
        let ipad = Viewport::Preset { width: 820, height: 1180, preset_id: "ipad-air".into() };
        assert_eq!((Viewport::Fill.choice(), ipad.choice()), ("fill", "ipad-air"));
        let unknown = Viewport::Preset { width: 1, height: 2, preset_id: "gone".into() };
        assert_eq!(unknown.choice(), "responsive");
        assert_eq!(Viewport::Fill.chosen("responsive"), Some(Viewport::Freeform { width: 1280, height: 800 }));
        assert_eq!(ipad.chosen("responsive"), Some(Viewport::Freeform { width: 820, height: 1180 }));
        assert_eq!(ipad.chosen("fill"), Some(Viewport::Fill));
        assert_eq!(Viewport::Fill.chosen("ipad-air"), Some(ipad.clone()));
        assert_eq!(Viewport::Fill.chosen("nope"), None);

        assert_eq!(ipad.with_dimension(true, 900), Some(Viewport::Freeform { width: 900, height: 1180 }));
        assert_eq!(ipad.with_dimension(false, 239), None, "below the minimum");
        assert_eq!(ipad.with_dimension(true, 820), None, "unchanged");
        let wide = Viewport::Freeform { width: 3840, height: 2160 };
        assert_eq!(wide.with_dimension(false, 2161), None, "over the area cap");
        assert_eq!(
            ipad.rotated(),
            Viewport::Preset { width: 1180, height: 820, preset_id: "ipad-air".into() },
            "a rotated preset stays a preset"
        );
        assert_eq!(serde_json::from_value::<Viewport>(ipad.to_json()).unwrap(), ipad);
    }

    #[test]
    fn one_unreadable_profile_neither_hides_nor_drops_the_others() {
        let settings =
            |patch: Value| crate::settings::Settings::layered(&crate::settings::contract().client_defaults, &patch);
        let work = json!({"id": "profile-work", "name": "Work", "kind": "persistent", "color": "teal"});
        let future = json!({"id": "profile-new", "name": "New", "kind": "sandboxed"});
        let stored = settings(json!({"browserProfiles": [work, {"id": "nameless"}, future]}));
        let ids: Vec<String> = user_profiles(&stored).into_iter().map(|p| p.id).collect();
        assert_eq!(ids, ["profile-work"]);

        let renamed = profiles_with_renamed(&stored, "profile-work", "Job");
        assert_eq!(
            renamed,
            json!([{"id": "profile-work", "name": "Job", "kind": "persistent", "color": "teal"}, {"id": "nameless"}, future])
        );
        let extra = Profile { id: "profile-b".into(), name: "B".into(), kind: ProfileKind::Persistent };
        let added = profiles_with_added(&stored, &extra).unwrap();
        assert_eq!(added.as_array().unwrap().len(), 4);
        assert_eq!(added[1], json!({"id": "nameless"}));
        assert_eq!(profiles_without(&stored, "profile-work"), json!([{"id": "nameless"}, future]));

        let full = settings(json!({"browserProfiles": vec![json!({"id": "x"}); PROFILE_MAX_COUNT]}));
        assert_eq!(profiles_with_added(&full, &extra), None);
    }

    #[test]
    fn profiles_resolve_like_the_contract_and_partition_their_tabs() {
        let settings =
            |patch: Value| crate::settings::Settings::layered(&crate::settings::contract().client_defaults, &patch);
        let work = json!({"id": "profile-work", "name": "Work", "kind": "persistent"});
        let stored = settings(json!({
            "browserProfiles": [{"id": "default", "name": "Hijacked"}, {"id": "x", "name": "X", "kind": "incognito"}, work],
            "browserDefaultProfileId": "profile-work",
        }));
        let resolved = resolve_profiles(&user_profiles(&stored));
        let ids: Vec<&str> = resolved.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["default", "incognito", "x", "profile-work"]);
        assert_eq!(resolved[0].name, "Default", "built-ins can't be replaced");
        assert_eq!(resolved[2].kind, ProfileKind::Persistent, "only the built-in incognito is ephemeral");
        assert_eq!(default_profile_id(&stored), "profile-work");
        assert_eq!(default_profile_id(&settings(json!({"browserDefaultProfileId": "incognito"}))), "default");
        assert_eq!(default_profile_id(&settings(json!({"browserDefaultProfileId": "gone"}))), "default");

        assert_eq!(unique_profile_name("Work", &resolved), "Work 2");
        assert_eq!(unique_profile_name("New profile", &resolved), "New profile");

        assert_eq!(partition(None), Partition::Default);
        assert_eq!(partition(Some("incognito")), Partition::Incognito);
        let Partition::Folder(folder) = partition(Some("profile-work")) else { panic!() };
        assert_eq!(folder.len(), 32);
        assert_ne!(partition(Some("profile-home")), Partition::Folder(folder));

        let report = profile_report(&stored);
        assert_eq!(report["defaultProfileId"], "profile-work");
        assert_eq!(report["recordingFrameRate"], 30);
        assert_eq!(report["profiles"][2], work);
    }

    #[test]
    fn fits_fixed_viewports_without_enlarging_them() {
        assert_eq!(fit_viewport((375., 667.), (500., 900.)), ((375., 667.), 1.));
        assert_eq!(fit_viewport((820., 1180.), (410., 1180.)), ((410., 590.), 0.5));
        assert_eq!(fit_viewport((0., 0.), (0., 0.)), ((0.05, 0.05), 0.05), "an empty viewport never makes a NaN size");
        assert_eq!(fit_viewport((375., 667.), (f32::NAN, f32::NAN)), ((375., 667.), 1.));
    }

    #[test]
    fn native_load_failures_read_as_chromium_net_errors() {
        let failure = |domain: &str, code: i64, http_status: Option<i32>| {
            net_error(&wry::PageLoadError {
                url: "https://a.test/".into(),
                domain: domain.into(),
                code,
                description: String::new(),
                http_status,
            })
        };
        // WebView2.
        let webview2 = |status, http| failure("COREWEBVIEW2_WEB_ERROR_STATUS", status, http);
        assert_eq!(webview2(13, None), Some((-105, "ERR_NAME_NOT_RESOLVED")));
        assert_eq!(webview2(12, None), Some((-102, "ERR_CONNECTION_REFUSED")));
        assert_eq!(webview2(5, None), Some((-202, "ERR_CERT_AUTHORITY_INVALID")));
        assert_eq!(webview2(15, Some(302)), Some((-310, "ERR_TOO_MANY_REDIRECTS")), "a redirect loop still failed");
        assert_eq!(webview2(0, None), Some((-2, "ERR_FAILED")));
        assert_eq!(webview2(14, None), None, "a cancelled load is ERR_ABORTED, which Electron ignores");
        assert_eq!(webview2(0, Some(404)), None, "the server's own error page shows");
        // WKWebView.
        assert_eq!(failure("NSURLErrorDomain", -1004, None), Some((-102, "ERR_CONNECTION_REFUSED")));
        assert_eq!(failure("NSURLErrorDomain", -1003, None), Some((-105, "ERR_NAME_NOT_RESOLVED")));
        assert_eq!(failure("NSURLErrorDomain", -1202, None), Some((-202, "ERR_CERT_AUTHORITY_INVALID")));
        assert_eq!(failure("NSURLErrorDomain", -1009, None), Some((-106, "ERR_INTERNET_DISCONNECTED")));
        assert_eq!(failure("NSURLErrorDomain", -999, None), None, "superseded");
        assert_eq!(failure("WebKitErrorDomain", 102, None), None, "handed off as a download");
        assert_eq!(failure("NSPOSIXErrorDomain", 61, None), Some((-2, "ERR_FAILED")));
        // WebKitGTK.
        assert_eq!(failure("g-io-error-quark", 39, None), Some((-102, "ERR_CONNECTION_REFUSED")));
        assert_eq!(failure("g-resolver-error-quark", 0, None), Some((-105, "ERR_NAME_NOT_RESOLVED")));
        assert_eq!(failure("g-io-error-quark", 24, None), Some((-118, "ERR_CONNECTION_TIMED_OUT")));
        assert_eq!(failure("g-tls-error-quark", 2, None), Some((-202, "ERR_CERT_AUTHORITY_INVALID")));
        assert_eq!(failure("WebKitPolicyError", 103, None), Some((-312, "ERR_UNSAFE_PORT")));
        assert_eq!(failure("WebKitNetworkError", 302, None), None, "cancelled");
        assert_eq!(failure("WebKitPolicyError", 102, None), None, "interrupted by a download");
        assert_eq!(failure("g-io-error-quark", 19, None), None, "cancelled");
    }

    #[test]
    fn reads_the_history_scripts_message() {
        assert_eq!(
            history_message(r#"{"t3History":{"url":"https://a.test/#b","back":true,"forward":false}}"#),
            Some(PageHistory { url: "https://a.test/#b".into(), back: true, forward: false })
        );
        assert_eq!(history_message(r#"{"t3History":{"url":"https://a.test/","back":true}}"#), None);
        assert_eq!(history_message("not json"), None);
        assert_eq!(history_message(r#"{"other":1}"#), None);
    }

    #[test]
    fn decodes_viewport_settings() {
        let fixed: Viewport =
            serde_json::from_value(json!({"_tag": "preset", "presetId": "ipad-air", "width": 820, "height": 1180}))
                .unwrap();
        assert_eq!(fixed.size(), Some((820., 1180.)));
        assert_eq!(fixed.preset_id(), Some("ipad-air"));
        let fill: Viewport = serde_json::from_value(json!({"_tag": "fill"})).unwrap();
        assert_eq!(fill.size(), None);
    }

    #[test]
    fn steps_zoom_through_the_shared_levels() {
        assert_eq!(step_zoom(1.0, 1), 1.1);
        assert_eq!(step_zoom(1.0, -1), 0.9);
        assert_eq!(step_zoom(5.0, 1), 5.0);
        assert_eq!(step_zoom(0.25, -1), 0.25);
        assert_eq!(step_zoom(1.5, 0), 1.0);
        assert_eq!(step_zoom(1.23, 1), 1.1, "an unknown factor steps from the default");
    }

    #[test]
    fn reconciles_list_and_events() {
        let mut previews = ThreadPreviews::default();
        previews
            .apply_list(
                &json!({"sessions": [session("a", "1"), session("b", "2")], "serverEpoch": "e1", "revision": 4}),
            )
            .unwrap();
        assert_eq!(previews.active_tab.as_deref(), Some("b"), "the most recent tab is active");

        let opened = json!({"type": "opened", "threadId": "t1", "tabId": "c", "createdAt": "3", "serverEpoch": "e1",
            "revision": 5, "snapshot": session("c", "3")});
        assert_eq!(previews.apply_event(&opened).unwrap(), Applied::Changed);
        assert_eq!(previews.active_tab.as_deref(), Some("c"));

        let stale = json!({"type": "closed", "threadId": "t1", "tabId": "a", "createdAt": "0", "serverEpoch": "e1",
            "revision": 3});
        assert_eq!(previews.apply_event(&stale).unwrap(), Applied::Ignored);

        let failed = json!({"type": "failed", "threadId": "t1", "tabId": "c", "createdAt": "4", "serverEpoch": "e1",
            "revision": 6, "url": "https://c.test/", "title": "", "code": -105, "description": "ERR_NAME_NOT_RESOLVED"});
        previews.apply_event(&failed).unwrap();
        assert!(matches!(previews.active().unwrap().nav_status, NavStatus::LoadFailed { code: -105, .. }));

        let downloaded = json!({"type": "failed", "threadId": "t1", "tabId": "c", "createdAt": "4", "serverEpoch": "e1",
            "revision": 6, "url": "https://c.test/a.zip", "title": "", "code": -3, "description": "ERR_ABORTED",
            "download": {"id": "d1", "fileName": "a.zip"}});
        previews.apply_event(&downloaded).unwrap();
        let status = &previews.active().unwrap().nav_status;
        let download = FileDownload { id: "d1".into(), file_name: "a.zip".into() };
        assert!(matches!(status, NavStatus::LoadFailed { download: Some(d), .. } if *d == download));
        assert_eq!(status.to_json()["download"], json!({"id": "d1", "fileName": "a.zip"}));

        let closed = json!({"type": "closed", "threadId": "t1", "tabId": "c", "createdAt": "5", "serverEpoch": "e1",
            "revision": 7});
        previews.apply_event(&closed).unwrap();
        assert_eq!(previews.active_tab.as_deref(), Some("b"), "the neighbour takes focus");

        let restarted = json!({"type": "closed", "threadId": "t1", "tabId": "b", "createdAt": "6", "serverEpoch": "e2",
            "revision": 1});
        assert_eq!(previews.apply_event(&restarted).unwrap(), Applied::Relist);
    }

    #[test]
    fn a_failed_close_restores_the_tab_and_a_closing_tab_ignores_updates() {
        let mut previews = ThreadPreviews::default();
        let list = |revision: u64| json!({"sessions": [session("a", "1"), session("b", "2"), session("c", "3")], "serverEpoch": "e1", "revision": revision});
        previews.apply_list(&list(1)).unwrap();
        let closing = previews.begin_close("b");
        assert!(previews.get("b").is_none());
        let navigated = json!({"type": "navigated", "threadId": "t1", "tabId": "b", "createdAt": "4", "serverEpoch": "e1",
            "revision": 2, "snapshot": session("b", "4")});
        assert_eq!(previews.apply_event(&navigated).unwrap(), Applied::Ignored, "a closing tab stays out");
        previews.apply_list(&list(3)).unwrap();
        assert!(previews.get("b").is_none(), "and out of a list");
        previews.active_tab = Some("a".into());
        previews.cancel_close(closing);
        let order: Vec<&str> = previews.sessions.iter().map(|s| s.tab_id.as_str()).collect();
        assert_eq!(order, ["a", "b", "c"], "it returns to its place");
        assert_eq!(previews.active_tab.as_deref(), Some("b"));

        let closing = previews.begin_close("c");
        let closed = json!({"type": "closed", "threadId": "t1", "tabId": "c", "createdAt": "5", "serverEpoch": "e1",
            "revision": 4});
        previews.apply_event(&closed).unwrap();
        previews.cancel_close(closing);
        assert!(previews.get("c").is_none(), "a close the server confirmed stays closed");
        let reopened = json!({"type": "opened", "threadId": "t1", "tabId": "c", "createdAt": "6",
            "serverEpoch": "e1", "revision": 5, "snapshot": session("c", "6")});
        previews.apply_event(&reopened).unwrap();
        assert!(previews.get("c").is_some(), "the id is usable again");
    }

    #[test]
    fn a_newer_local_copy_survives_an_older_list() {
        let mut previews = ThreadPreviews::default();
        previews.apply_list(&json!({"sessions": [session("a", "1")], "serverEpoch": "e1", "revision": 1})).unwrap();
        let newer: Session = serde_json::from_value(session("a", "9")).unwrap();
        previews.apply_snapshot(newer.clone(), true);
        previews.apply_list(&json!({"sessions": [session("a", "2")], "serverEpoch": "e1", "revision": 2})).unwrap();
        assert_eq!(previews.get("a"), Some(&newer));
    }
}
