//! Settings: the environment's `ServerSettings` (`packages/contracts/src/settings.ts`),
//! this device's client settings (Electron's `client-settings.json`), and the scope layer
//! that resolves both for the Settings modal (`scope`).
//!
//! Both documents are kept as JSON objects layered over the contract's defaults, so every
//! known key reads a value, unknown keys survive, and one bad field never drops the rest.

pub mod scope;

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::Deserialize;
use serde_json::{Map, Value};

/// Data generated from the TypeScript contracts by `tools/settings_data.test.ts`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Contract {
    /// `DEFAULT_SERVER_SETTINGS`, encoded as the wire carries it.
    pub server_defaults: Map<String, Value>,
    /// `DEFAULT_CLIENT_SETTINGS`.
    pub client_defaults: Map<String, Value>,
    /// `PROJECT_SCOPED_SERVER_SETTING_KEYS`, in resolution order.
    pub project_scoped_keys: Vec<String>,
    /// Project-scoped keys whose override may store null (`isNullableProjectSettingsOverride`).
    pub nullable_override_keys: Vec<String>,
    /// `PROJECT_FILE_BACKED_SETTINGS[key].builtIn`.
    pub file_backed_built_ins: Map<String, Value>,
    /// `SETTINGS_SEARCH_ITEMS`, with `to` as the section id.
    pub search: Vec<SearchItem>,
}

/// One searchable setting (`SettingsSearchItem` in `settingsSearch.ts`).
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SearchItem {
    pub id: String,
    pub title: String,
    /// The section id, e.g. "general".
    pub to: String,
    pub target_id: Option<String>,
    pub search_terms: Vec<String>,
    pub secondary: bool,
    pub mac_only: bool,
    pub windows_only: bool,
    pub secrets_feature_only: bool,
    pub cloud_only: bool,
    pub local_backend_management_only: bool,
    pub wsl_available_only: bool,
    pub managed_tunnel_only: bool,
    pub peer_links_only: bool,
    pub mac_provider_settings_only: bool,
}

pub fn contract() -> &'static Contract {
    static CONTRACT: OnceLock<Contract> = OnceLock::new();
    CONTRACT.get_or_init(|| serde_json::from_str(include_str!("contract.json")).expect("settings contract.json"))
}

impl Contract {
    /// Optional keys such as `defaultThreadEnvMode` have no default but are still the server's.
    pub fn is_server_key(&self, key: &str) -> bool {
        self.server_defaults.contains_key(key) || self.is_project_scoped(key)
    }

    /// A key only this device stores. Keys both documents know are the server's, as in
    /// `planScopedSettingsPatch`.
    pub fn is_client_key(&self, key: &str) -> bool {
        self.client_defaults.contains_key(key) && !self.is_server_key(key)
    }

    pub fn is_project_scoped(&self, key: &str) -> bool {
        self.project_scoped_keys.iter().any(|k| k == key)
    }

    /// The built-in value of a key, server or client (`DEFAULT_UNIFIED_SETTINGS`).
    pub fn default_value(&self, key: &str) -> &Value {
        self.server_defaults.get(key).or_else(|| self.client_defaults.get(key)).unwrap_or(&Value::Null)
    }

    /// The search item with this id (`searchableSetting`).
    pub fn search_item(&self, id: &str) -> Option<&SearchItem> {
        self.search.iter().find(|item| item.id == id)
    }
}

/// A settings document: the contract's defaults with the stored values over them.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings(Value);

impl Settings {
    /// `stored`'s fields over `defaults`; a non-object `stored` leaves the defaults.
    pub fn layered(defaults: &Map<String, Value>, stored: &Value) -> Self {
        let mut values = defaults.clone();
        if let Some(stored) = stored.as_object() {
            values.extend(stored.iter().map(|(key, value)| (key.clone(), value.clone())));
        }
        Self(Value::Object(values))
    }

    pub fn server_defaults() -> Self {
        Self(Value::Object(contract().server_defaults.clone()))
    }

    pub fn get(&self, key: &str) -> &Value {
        &self.0[key]
    }

    pub fn bool(&self, key: &str) -> bool {
        self.get(key).as_bool().unwrap_or(false)
    }

    pub fn set(&mut self, key: impl Into<String>, value: Value) {
        self.map_mut().insert(key.into(), value);
    }

    /// Shallow merge, the way the server applies a `ServerSettingsPatch` at the top level.
    pub fn apply(&mut self, patch: &Map<String, Value>) {
        let map = self.map_mut();
        for (key, value) in patch {
            map.insert(key.clone(), value.clone());
        }
    }

    pub fn as_map(&self) -> &Map<String, Value> {
        self.0.as_object().expect("settings are an object")
    }

    fn map_mut(&mut self) -> &mut Map<String, Value> {
        self.0.as_object_mut().expect("settings are an object")
    }
}

/// Raw JSON reads (`settings["key"][..]`) for sections not on the widgets yet.
impl std::ops::Deref for Settings {
    type Target = Value;
    fn deref(&self) -> &Value {
        &self.0
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self::server_defaults()
    }
}

/// The web's theme preference (`useTheme`); Electron keeps it in local storage.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ThemePreference {
    #[default]
    System,
    Light,
    Dark,
}

impl ThemePreference {
    pub fn id(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    pub fn parse(id: &str) -> Self {
        match id {
            "light" => Self::Light,
            "dark" => Self::Dark,
            _ => Self::System,
        }
    }
}

/// Key in `client-settings.json` holding the theme, which this client has no local storage for.
const THEME_KEY: &str = "theme";

/// This device's settings, persisted like the desktop app's `client-settings.json`.
pub struct ClientSettings {
    pub values: Settings,
    pub theme: ThemePreference,
    path: Option<PathBuf>,
}

impl ClientSettings {
    /// Reads `<T3 home>/userdata/client-settings.json`; a missing or unreadable file is the defaults.
    pub fn load() -> Self {
        let path = client_settings_path();
        let stored = path
            .as_deref()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            .map(|document| match document.get("settings") {
                Some(settings) => settings.clone(),
                None => document,
            })
            .unwrap_or_default();
        Self::from_stored(&stored, path)
    }

    fn from_stored(stored: &Value, path: Option<PathBuf>) -> Self {
        let theme = ThemePreference::parse(stored.get(THEME_KEY).and_then(Value::as_str).unwrap_or_default());
        Self { values: Settings::layered(&contract().client_defaults, stored), theme, path }
    }

    pub fn apply(&mut self, patch: &Map<String, Value>) {
        self.values.apply(patch);
        self.save();
    }

    pub fn set_theme(&mut self, theme: ThemePreference) {
        self.theme = theme;
        self.save();
    }

    fn document(&self) -> Value {
        let mut document = self.values.as_map().clone();
        document.insert(THEME_KEY.into(), self.theme.id().into());
        Value::Object(document)
    }

    /// Writes a temporary file and renames it over the old one, as the desktop app does.
    fn save(&self) {
        let Some(path) = &self.path else { return };
        if let Err(error) = write_atomically(path, &self.document()) {
            log::warn!("could not save {}: {error}", path.display());
        }
    }
}

fn write_atomically(path: &Path, document: &Value) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let temp = path.with_extension(format!("{}.tmp", std::process::id()));
    std::fs::write(&temp, format!("{}\n", serde_json::to_string_pretty(document)?))?;
    std::fs::rename(&temp, path)
}

/// The T3 home rule of `backend::LaunchConfig::from_env`: `T3CODE_HOME`, else `~/.t3-gpui`.
fn client_settings_path() -> Option<PathBuf> {
    let home = std::env::var_os("T3CODE_HOME").map(PathBuf::from).or_else(|| {
        std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(|home| PathBuf::from(home).join(".t3-gpui"))
    })?;
    Some(home.join("userdata").join("client-settings.json"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn stored_fields_layer_over_defaults_and_unknown_fields_survive() {
        let settings = Settings::layered(&contract().server_defaults, &json!({"confirmQuit": "x", "futureKey": 1}));
        assert_eq!(settings.get("futureKey"), &json!(1));
        assert_eq!(settings.get("defaultRuntimeMode"), contract().default_value("defaultRuntimeMode"));
        assert_eq!(Settings::layered(&contract().server_defaults, &json!(null)), Settings::server_defaults());
    }

    #[test]
    fn keys_both_documents_know_belong_to_the_server() {
        let contract = contract();
        assert!(contract.is_client_key("confirmThreadDelete"));
        assert!(!contract.is_client_key("favorites") && contract.is_server_key("favorites"));
        assert!(contract.is_project_scoped("defaultThreadEnvMode"));
        assert_eq!(contract.search_item("delete-confirmation").map(|i| i.title.as_str()), Some("Delete confirmation"));
    }

    #[test]
    fn client_settings_round_trip_through_the_electron_document() {
        let dir = std::env::temp_dir().join(format!("t3-gpui-client-settings-{}", std::process::id()));
        let path = dir.join("client-settings.json");
        let mut settings = ClientSettings::from_stored(&json!({}), Some(path.clone()));
        settings.apply(json!({"timestampFormat": "24-hour"}).as_object().unwrap());
        settings.set_theme(ThemePreference::Dark);
        let raw: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let reloaded = ClientSettings::from_stored(&raw, None);
        assert_eq!(reloaded.values.get("timestampFormat"), "24-hour");
        assert_eq!(reloaded.theme, ThemePreference::Dark);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
