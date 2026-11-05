//! Preview browser sessions: the wire shapes of `packages/contracts/src/preview.ts`,
//! the per-thread reducer of `apps/web/src/previewStateStore.ts`, and the address
//! bar's URL resolution from `packages/shared/src/preview.ts`.

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
    },
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
            NavStatus::LoadFailed { url, title, code, description } => {
                json!({"_tag": "LoadFailed", "url": url, "title": title, "code": code, "description": description})
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
    let index = ZOOM_LEVELS.iter().position(|level| (level - current).abs() < 1e-9).unwrap_or(7) as i32;
    ZOOM_LEVELS[(index + direction).clamp(0, ZOOM_LEVELS.len() as i32 - 1) as usize]
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
pub fn fit_viewport(viewport: (f32, f32), available: (f32, f32)) -> ((f32, f32), f32) {
    let scale = (available.0 / viewport.0).min(available.1 / viewport.1).clamp(0.05, 1.);
    ((viewport.0 * scale, viewport.1 * scale), scale)
}

/// A WebView2 `COREWEBVIEW2_WEB_ERROR_STATUS` as the Chromium net error Electron reports
/// (`did-fail-load`), so `LoadFailed` reads the same to agents and other clients. `None`
/// when the navigation did not fail: a cancel, or an HTTP error page that still loaded.
pub fn web_error(status: i32) -> Option<(i64, &'static str)> {
    Some(match status {
        1 => (-200, "ERR_CERT_COMMON_NAME_INVALID"),
        2 => (-201, "ERR_CERT_DATE_INVALID"),
        3 => (-117, "ERR_BAD_SSL_CLIENT_AUTH_CERT"),
        4 => (-206, "ERR_CERT_REVOKED"),
        5 => (-202, "ERR_CERT_AUTHORITY_INVALID"),
        6 => (-109, "ERR_ADDRESS_UNREACHABLE"),
        7 => (-118, "ERR_CONNECTION_TIMED_OUT"),
        8 => (-320, "ERR_INVALID_RESPONSE"),
        9 => (-103, "ERR_CONNECTION_ABORTED"),
        10 => (-101, "ERR_CONNECTION_RESET"),
        11 => (-106, "ERR_INTERNET_DISCONNECTED"),
        12 => (-102, "ERR_CONNECTION_REFUSED"),
        13 => (-105, "ERR_NAME_NOT_RESOLVED"),
        15 => (-310, "ERR_TOO_MANY_REDIRECTS"),
        16 => (-2, "ERR_FAILED"),
        _ => return None,
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

    /// Keeps a newer local copy when the incoming one is older (`updatedAt` is ISO-8601).
    fn upsert(&mut self, session: Session) {
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
        for session in list.sessions {
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
                };
                session.updated_at = event.created_at;
            }
            "closed" => self.remove(&event.tab_id),
            _ => return Ok(Applied::Ignored),
        }
        self.server_epoch = Some(event.server_epoch);
        self.revision = event.revision;
        Ok(Applied::Changed)
    }

    /// A snapshot a request returned (`applyPreviewServerSnapshot`); `focus` makes it the active tab.
    pub fn apply_snapshot(&mut self, snapshot: Session, focus: bool) {
        if focus || self.active().is_none() {
            self.active_tab = Some(snapshot.tab_id.clone());
        }
        self.upsert(snapshot);
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
        if has_port || host.eq_ignore_ascii_case("localhost") || ipv4 || bracketed || dotted {
            if let Ok(url) = normalize_url(trimmed) {
                return Ok(url);
            }
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
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
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
        let (width, height) = if width_axis { (value as u32, height) } else { (width, value as u32) };
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

/// The stored `browserProfiles`, the user's own.
pub fn user_profiles(settings: &crate::settings::Settings) -> Vec<Profile> {
    serde_json::from_value(settings.get("browserProfiles").clone()).unwrap_or_default()
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
        Some(id) => {
            Partition::Folder(sha2::Sha256::digest(id.as_bytes())[..16].iter().map(|b| format!("{b:02x}")).collect())
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn session(tab: &str, updated_at: &str) -> Value {
        json!({
            "threadId": "t1", "tabId": tab, "updatedAt": updated_at,
            "navStatus": {"_tag": "Success", "url": format!("https://{tab}.test/"), "title": tab},
            "canGoBack": false, "canGoForward": false,
        })
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
    }

    #[test]
    fn maps_webview2_errors_to_chromium_net_errors() {
        assert_eq!(web_error(13), Some((-105, "ERR_NAME_NOT_RESOLVED")));
        assert_eq!(web_error(12), Some((-102, "ERR_CONNECTION_REFUSED")));
        assert_eq!(web_error(14), None, "a canceled navigation is not a failure");
        assert_eq!(web_error(0), None, "an HTTP error page still loaded");
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

        let closed = json!({"type": "closed", "threadId": "t1", "tabId": "c", "createdAt": "5", "serverEpoch": "e1",
            "revision": 7});
        previews.apply_event(&closed).unwrap();
        assert_eq!(previews.active_tab.as_deref(), Some("b"), "the neighbour takes focus");

        let restarted = json!({"type": "closed", "threadId": "t1", "tabId": "b", "createdAt": "6", "serverEpoch": "e2",
            "revision": 1});
        assert_eq!(previews.apply_event(&restarted).unwrap(), Applied::Relist);
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
