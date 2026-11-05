//! The desktop app's own settings, `<T3 home>/userdata/desktop-settings.json`, in the
//! document format of `apps/desktop/src/settings/DesktopAppSettings.ts`: only values
//! that differ from the defaults are written.

use std::path::PathBuf;

use serde_json::{Map, Value, json};

/// `MIN_MAIN_WINDOW_SIZE`; smaller saved bounds are ignored.
pub const MIN_WINDOW_SIZE: (i32, i32) = (840, 620);
/// `DEFAULT_MAIN_WINDOW_SIZE`.
pub const DEFAULT_WINDOW_SIZE: (i32, i32) = (1100, 780);
const DEFAULT_TAILSCALE_SERVE_PORT: u16 = 443;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowRect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl WindowRect {
    /// `normalizeMainWindowBounds`: integers at least the minimum size, else nothing.
    fn parse(value: &Value) -> Option<Self> {
        let int = |key: &str| {
            let number = value.get(key)?.as_f64()?;
            (number.fract() == 0.0 && number.abs() < f64::from(i32::MAX)).then_some(number as i32)
        };
        let rect = Self { x: int("x")?, y: int("y")?, width: int("width")?, height: int("height")? };
        (rect.width >= MIN_WINDOW_SIZE.0 && rect.height >= MIN_WINDOW_SIZE.1).then_some(rect)
    }

    /// `windowFitsWithinDisplay`.
    pub fn fits_within(&self, display: &WindowRect) -> bool {
        self.x >= display.x
            && self.y >= display.y
            && self.x + self.width <= display.x + display.width
            && self.y + self.height <= display.y + display.height
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExposureMode {
    LocalOnly,
    NetworkAccessible,
}

impl ExposureMode {
    pub fn id(self) -> &'static str {
        match self {
            Self::LocalOnly => "local-only",
            Self::NetworkAccessible => "network-accessible",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DesktopSettings {
    pub local_environment_enabled: bool,
    pub main_window_bounds: Option<WindowRect>,
    pub main_window_maximized: bool,
    pub server_exposure_mode: ExposureMode,
    pub tailscale_serve_enabled: bool,
    pub tailscale_serve_port: u16,
    pub update_channel: String,
    pub update_channel_configured_by_user: bool,
    pub wsl_backend_enabled: bool,
    pub wsl_distro: Option<String>,
    pub wsl_only: bool,
    /// Keys the desktop app knows that this client does not use (e.g. `linuxPasswordStore`).
    other: Map<String, Value>,
}

impl Default for DesktopSettings {
    fn default() -> Self {
        Self {
            local_environment_enabled: true,
            main_window_bounds: None,
            main_window_maximized: false,
            server_exposure_mode: ExposureMode::LocalOnly,
            tailscale_serve_enabled: false,
            tailscale_serve_port: DEFAULT_TAILSCALE_SERVE_PORT,
            update_channel: "latest".into(),
            update_channel_configured_by_user: false,
            wsl_backend_enabled: false,
            wsl_distro: None,
            wsl_only: false,
            other: Map::new(),
        }
    }
}

const KNOWN_KEYS: [&str; 12] = [
    "localEnvironmentEnabled",
    "mainWindowBounds",
    "mainWindowMaximized",
    "serverExposureMode",
    "tailscaleServeEnabled",
    "tailscaleServePort",
    "updateChannel",
    "updateChannelConfiguredByUser",
    "wslBackendEnabled",
    "wslMode",
    "wslDistro",
    "wslOnly",
];

impl DesktopSettings {
    pub fn path() -> Option<PathBuf> {
        Some(crate::settings::t3_home()?.join("userdata").join("desktop-settings.json"))
    }

    /// A missing or unreadable file is the defaults.
    pub fn load() -> Self {
        Self::path()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            .map(|document| Self::from_document(&document))
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(path) = Self::path() else { return };
        if let Err(error) = crate::settings::write_atomically(&path, &self.to_document()) {
            log::warn!("could not save {}: {error}", path.display());
        }
    }

    /// `normalizeDesktopSettingsDocument`.
    pub fn from_document(document: &Value) -> Self {
        let flag = |key: &str| document.get(key).and_then(Value::as_bool);
        let defaults = Self::default();
        let main_window_bounds = document.get("mainWindowBounds").and_then(WindowRect::parse);
        let update_channel = document
            .get("updateChannel")
            .and_then(Value::as_str)
            .filter(|channel| matches!(*channel, "latest" | "nightly"));
        // Settings from before the flag existed only chose nightly on purpose.
        let legacy = document.get("updateChannelConfiguredByUser").is_none();
        let configured =
            flag("updateChannelConfiguredByUser") == Some(true) || (legacy && update_channel == Some("nightly"));
        let port = document
            .get("tailscaleServePort")
            .and_then(Value::as_u64)
            .and_then(|port| u16::try_from(port).ok())
            .filter(|port| *port >= 1)
            .unwrap_or(DEFAULT_TAILSCALE_SERVE_PORT);
        let distro =
            document.get("wslDistro").and_then(Value::as_str).filter(|name| valid_distro_name(name)).map(str::to_owned);
        let other = document
            .as_object()
            .map(|map| map.iter().filter(|(key, _)| !KNOWN_KEYS.contains(&key.as_str())))
            .into_iter()
            .flatten()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        Self {
            local_environment_enabled: flag("localEnvironmentEnabled") != Some(false),
            main_window_bounds,
            main_window_maximized: main_window_bounds.is_some() && flag("mainWindowMaximized") == Some(true),
            server_exposure_mode: match document.get("serverExposureMode").and_then(Value::as_str) {
                Some("network-accessible") => ExposureMode::NetworkAccessible,
                _ => ExposureMode::LocalOnly,
            },
            tailscale_serve_enabled: flag("tailscaleServeEnabled") == Some(true),
            tailscale_serve_port: port,
            update_channel: if configured {
                update_channel.unwrap_or("latest").to_owned()
            } else {
                defaults.update_channel
            },
            update_channel_configured_by_user: configured,
            wsl_backend_enabled: flag("wslBackendEnabled") == Some(true)
                || (flag("wslBackendEnabled").is_none()
                    && document.get("wslMode").and_then(Value::as_str) == Some("wsl")),
            wsl_distro: distro,
            wsl_only: flag("wslOnly") == Some(true),
            other,
        }
    }

    /// `toDesktopSettingsDocument`.
    pub fn to_document(&self) -> Value {
        let defaults = Self::default();
        let mut document = self.other.clone();
        let mut put = |key: &str, differs: bool, value: Value| {
            if differs {
                document.insert(key.into(), value);
            }
        };
        put("localEnvironmentEnabled", !self.local_environment_enabled, json!(false));
        if let Some(b) = self.main_window_bounds {
            put("mainWindowBounds", true, json!({ "x": b.x, "y": b.y, "width": b.width, "height": b.height }));
        }
        put("mainWindowMaximized", self.main_window_maximized, json!(true));
        put(
            "serverExposureMode",
            self.server_exposure_mode != defaults.server_exposure_mode,
            json!(self.server_exposure_mode.id()),
        );
        put("tailscaleServeEnabled", self.tailscale_serve_enabled, json!(true));
        put(
            "tailscaleServePort",
            self.tailscale_serve_port != DEFAULT_TAILSCALE_SERVE_PORT,
            json!(self.tailscale_serve_port),
        );
        put("updateChannel", self.update_channel != defaults.update_channel, json!(self.update_channel));
        put("updateChannelConfiguredByUser", self.update_channel_configured_by_user, json!(true));
        put("wslBackendEnabled", self.wsl_backend_enabled, json!(true));
        put("wslDistro", self.wsl_distro.is_some(), json!(self.wsl_distro));
        put("wslOnly", self.wsl_only, json!(true));
        Value::Object(document)
    }
}

/// `isValidDistroName` in `apps/desktop/src/wsl/wslPathParsing.ts`: `^\w(?:[\w \-.]*\w)?$`.
pub fn valid_distro_name(name: &str) -> bool {
    let word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    name.starts_with(word) && name.ends_with(word) && name.chars().all(|c| word(c) || matches!(c, ' ' | '-' | '.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn documents_round_trip_and_write_only_changed_values() {
        assert_eq!(DesktopSettings::default().to_document(), json!({}));
        let document = json!({
            "mainWindowBounds": { "x": -1200, "y": 40, "width": 1300, "height": 900 },
            "mainWindowMaximized": true,
            "serverExposureMode": "network-accessible",
            "tailscaleServePort": 8443,
            "linuxPasswordStore": "basic",
        });
        let settings = DesktopSettings::from_document(&document);
        assert_eq!(settings.main_window_bounds, Some(WindowRect { x: -1200, y: 40, width: 1300, height: 900 }));
        assert!(settings.main_window_maximized);
        assert_eq!(settings.server_exposure_mode, ExposureMode::NetworkAccessible);
        assert_eq!(settings.to_document(), document);
    }

    #[test]
    fn invalid_values_fall_back_like_the_desktop_app() {
        let settings = DesktopSettings::from_document(&json!({
            "mainWindowBounds": { "x": 0.5, "y": 0, "width": 1300, "height": 900 },
            "mainWindowMaximized": true,
            "tailscaleServePort": 70000,
            "wslDistro": "-bad",
            "wslMode": "wsl",
        }));
        assert_eq!(settings.main_window_bounds, None);
        // Maximized only with bounds to restore to.
        assert!(!settings.main_window_maximized);
        assert_eq!(settings.tailscale_serve_port, 443);
        assert_eq!(settings.wsl_distro, None);
        // The legacy `wslMode` still turns the WSL backend on.
        assert!(settings.wsl_backend_enabled);
        let small = json!({ "mainWindowBounds": { "x": 0, "y": 0, "width": 800, "height": 900 } });
        assert_eq!(DesktopSettings::from_document(&small).main_window_bounds, None);
    }

    #[test]
    fn the_update_channel_counts_only_when_the_user_chose_it() {
        let channel = |document: Value| DesktopSettings::from_document(&document).update_channel;
        assert_eq!(channel(json!({ "updateChannel": "nightly" })), "nightly");
        assert_eq!(channel(json!({ "updateChannel": "nightly", "updateChannelConfiguredByUser": false })), "latest");
        assert_eq!(channel(json!({ "updateChannel": "beta", "updateChannelConfiguredByUser": true })), "latest");
    }
}
