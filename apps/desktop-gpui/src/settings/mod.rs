//! Settings: the environment's `ServerSettings` (`packages/contracts/src/settings.ts`),
//! this device's client settings (Electron's `client-settings.json`), and the scope layer
//! that resolves both for the Settings modal (`scope`).
//!
//! Both documents are kept as JSON objects layered over the contract's defaults, so every
//! known key reads a value, unknown keys survive, and one bad field never drops the rest.

pub mod scope;

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::themes::{Library, ThemeChoice, ThemeHalves};

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

/// A settings document: the contract's defaults with the stored values over them. Clones
/// share the document; a write copies it only while another clone still holds it.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings(Arc<Value>);

impl Settings {
    /// `stored`'s fields over `defaults`; a non-object `stored` leaves the defaults.
    pub fn layered(defaults: &Map<String, Value>, stored: &Value) -> Self {
        let mut values = defaults.clone();
        if let Some(stored) = stored.as_object() {
            values.extend(stored.iter().map(|(key, value)| (key.clone(), value.clone())));
        }
        Self(Arc::new(Value::Object(values)))
    }

    pub fn server_defaults() -> Self {
        Self(Arc::new(Value::Object(contract().server_defaults.clone())))
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
        Arc::make_mut(&mut self.0).as_object_mut().expect("settings are an object")
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

/// Keys in `client-settings.json` for what the web keeps in local storage: the appearance
/// mode (`t3code:theme-appearance-mode`), the theme in use (`t3code:theme`) and the
/// per-appearance mix (`t3code:theme-halves:v1`).
const THEME_KEY: &str = "theme";
const THEME_ID_KEY: &str = "themeId";
const THEME_HALVES_KEY: &str = "themeHalves";
/// `t3code:themes:v1`: the saved themes.
const CUSTOM_THEMES_KEY: &str = "customThemes";
/// `t3code:typography-advanced`: prompt, code and terminal fonts set separately.
pub const TYPOGRAPHY_ADVANCED_KEY: &str = "typographyAdvanced";

/// The `ThemeLibraryStorageError` messages, naming this client's storage.
const LIBRARY_READ_ERROR: &str = "Failed to read the theme library from client-settings.json.";
const LIBRARY_WRITE_ERROR: &str = "Failed to write the theme library to client-settings.json.";

/// The saved theme records as stored (`storedThemes`), so records this build can't read survive.
#[derive(Debug)]
enum StoredThemes {
    Ready(Vec<Value>),
    /// `customThemes` isn't a list: written back as found, and the library takes no changes.
    Malformed(Value),
    /// The file exists but couldn't be read or parsed: nothing is written over it.
    Unreadable,
}

/// This device's settings, persisted like the desktop app's `client-settings.json`.
pub struct ClientSettings {
    pub values: Settings,
    pub theme: ThemePreference,
    pub theme_id: Option<String>,
    pub theme_halves: ThemeHalves,
    pub themes: Library,
    stored_themes: StoredThemes,
    path: Option<PathBuf>,
    /// The file exists but couldn't be read or decoded. Nothing is written over it, and the
    /// app shows a recovery screen until a retry reads it (`FirstRunRecovery`).
    pub read_failed: bool,
}

impl ClientSettings {
    /// Reads `<T3 home>/userdata/client-settings.json`; a missing file is the defaults.
    pub fn load() -> Self {
        Self::load_from(client_settings_path())
    }

    fn load_from(path: Option<PathBuf>) -> Self {
        match path.as_deref().map_or(Ok(Value::Null), read_stored) {
            Ok(stored) => Self::from_stored(&stored, path),
            Err(()) => {
                let mut settings = Self::from_stored(&Value::Null, path);
                settings.stored_themes = StoredThemes::Unreadable;
                settings.read_failed = true;
                settings
            }
        }
    }

    fn from_stored(stored: &Value, path: Option<PathBuf>) -> Self {
        let text = |value: &Value| value.as_str().map(str::to_owned);
        let theme = ThemePreference::parse(stored.get(THEME_KEY).and_then(Value::as_str).unwrap_or_default());
        let halves = &stored[THEME_HALVES_KEY];
        let stored_themes = match &stored[CUSTOM_THEMES_KEY] {
            Value::Null => StoredThemes::Ready(Vec::new()),
            Value::Array(records) => StoredThemes::Ready(records.clone()),
            other => StoredThemes::Malformed(other.clone()),
        };
        Self {
            values: Settings::layered(&contract().client_defaults, stored),
            theme,
            theme_id: text(&stored[THEME_ID_KEY]),
            theme_halves: ThemeHalves { light: text(&halves["light"]), dark: text(&halves["dark"]) },
            themes: Library::new(crate::theme_file::parse_stored_list(&stored[CUSTOM_THEMES_KEY])),
            stored_themes,
            path,
            read_failed: false,
        }
    }

    pub fn theme_choice(&self) -> ThemeChoice {
        ThemeChoice {
            mode: self.theme,
            base: self.theme_id.clone(),
            halves: self.theme_halves.clone(),
            library: self.themes.clone(),
        }
    }

    /// Stores the saved themes and the pick that may name them, or changes nothing when the
    /// library can't be read or written (`ThemeLibraryStorageError`).
    pub fn set_themes(&mut self, themes: Library, choice: ThemeChoice) -> Result<(), String> {
        if !matches!(self.stored_themes, StoredThemes::Ready(_)) {
            return Err(LIBRARY_READ_ERROR.to_owned());
        }
        let previous_themes = std::mem::replace(&mut self.themes, themes);
        let previous_choice = (self.theme, self.theme_id.take(), std::mem::take(&mut self.theme_halves));
        (self.theme, self.theme_id, self.theme_halves) = (choice.mode, choice.base, choice.halves);
        if let Err(error) = self.save() {
            log::warn!("could not save the theme library: {error}");
            self.themes = previous_themes;
            (self.theme, self.theme_id, self.theme_halves) = previous_choice;
            return Err(LIBRARY_WRITE_ERROR.to_owned());
        }
        Ok(())
    }

    /// Stores a theme library pick (mode, theme and mix together).
    pub fn set_theme_choice(&mut self, choice: ThemeChoice) {
        (self.theme, self.theme_id, self.theme_halves) = (choice.mode, choice.base, choice.halves);
        self.save_or_warn();
    }

    pub fn apply(&mut self, patch: &Map<String, Value>) {
        self.values.apply(patch);
        self.save_or_warn();
    }

    /// [`Self::apply`], keeping the old values when the file can't be written.
    pub fn try_apply(&mut self, patch: &Map<String, Value>) -> std::io::Result<()> {
        let previous = self.values.clone();
        self.values.apply(patch);
        let saved = self.save();
        if saved.is_err() {
            self.values = previous;
        }
        saved
    }

    fn document(&self) -> Value {
        let mut document = self.values.as_map().clone();
        document.insert(THEME_KEY.into(), self.theme.id().into());
        document.insert(THEME_ID_KEY.into(), self.theme_id.clone().into());
        let halves = &self.theme_halves;
        document.insert(THEME_HALVES_KEY.into(), serde_json::json!({ "light": halves.light, "dark": halves.dark }));
        let themes = match &self.stored_themes {
            StoredThemes::Ready(records) => Value::Array(merge_stored_themes(records, self.themes.custom())),
            StoredThemes::Malformed(value) => value.clone(),
            StoredThemes::Unreadable => Value::Null,
        };
        document.insert(CUSTOM_THEMES_KEY.into(), themes);
        Value::Object(document)
    }

    /// Writes a temporary file and renames it over the old one, as the desktop app does.
    fn save(&mut self) -> std::io::Result<()> {
        if self.read_failed || matches!(self.stored_themes, StoredThemes::Unreadable) {
            return Err(std::io::Error::other("the file on disk could not be read"));
        }
        let Some(path) = &self.path else { return Ok(()) };
        let document = self.document();
        write_atomically(path, &document)?;
        if let (StoredThemes::Ready(records), Value::Array(written)) =
            (&mut self.stored_themes, &document[CUSTOM_THEMES_KEY])
        {
            records.clone_from(written);
        }
        Ok(())
    }

    fn save_or_warn(&mut self) {
        if let Err(error) = self.save() {
            log::warn!("could not save the client settings: {error}");
        }
    }
}

/// The records to store for `themes` (`saveCustomThemes` after an install, update or removal):
/// an unchanged theme keeps its record as stored, an edited one is rewritten in place, a removed
/// one goes, records this build can't read stay, and new themes go last.
fn merge_stored_themes(records: &[Value], themes: &[crate::themes::ThemeDefinition]) -> Vec<Value> {
    let (mut written, mut edited) = (std::collections::HashSet::new(), std::collections::HashSet::new());
    let mut merged = Vec::with_capacity(records.len().max(themes.len()));
    for record in records {
        let Some(parsed) = crate::theme_file::parse_stored(record) else {
            merged.push(record.clone());
            continue;
        };
        let Some(theme) = themes.iter().find(|theme| theme.id == parsed.id) else { continue };
        let id = theme.id.as_str();
        if written.insert(id) {
            if parsed == *theme {
                merged.push(record.clone());
            } else {
                edited.insert(id);
                merged.push(crate::theme_file::stored(theme));
            }
        } else if !edited.contains(id) {
            merged.push(record.clone());
        }
    }
    merged.extend(themes.iter().filter(|theme| !written.contains(theme.id.as_str())).map(crate::theme_file::stored));
    merged
}

/// `DesktopClientSettings.get`: the stored settings (the document's `settings`, or the document
/// itself), `Null` when the file doesn't exist; a file that can't be read or isn't a JSON object fails.
fn read_stored(path: &Path) -> Result<Value, ()> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Value::Null),
        Err(error) => {
            log::warn!("could not read {}: {error}", path.display());
            return Err(());
        }
    };
    let stored = match serde_json::from_str::<Value>(&raw) {
        Ok(Value::Object(mut document)) => document.remove("settings").unwrap_or(Value::Object(document)),
        Ok(other) => other,
        Err(error) => {
            log::warn!("could not decode {}: {error}", path.display());
            return Err(());
        }
    };
    stored.is_object().then_some(stored).ok_or(())
}

pub(crate) fn write_atomically(path: &Path, document: &Value) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let temp = path.with_extension(format!("{}.tmp", std::process::id()));
    std::fs::write(&temp, format!("{}\n", serde_json::to_string_pretty(document)?))?;
    std::fs::rename(&temp, path)
}

/// The T3 home: `T3CODE_HOME`, else `~/.t3-gpui`, apart from the Electron app's `~/.t3` until
/// this client reaches parity, so both can run at once without fighting over the state directory.
pub(crate) fn t3_home() -> Option<PathBuf> {
    std::env::var_os("T3CODE_HOME").map(PathBuf::from).or_else(|| {
        std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(|home| PathBuf::from(home).join(".t3-gpui"))
    })
}

/// `<T3 home>/userdata`.
pub(crate) fn userdata_dir() -> Option<PathBuf> {
    Some(t3_home()?.join("userdata"))
}

fn client_settings_path() -> Option<PathBuf> {
    Some(userdata_dir()?.join("client-settings.json"))
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
    fn an_unreadable_settings_file_fails_and_is_never_written_over() {
        let dir = std::env::temp_dir().join(format!("t3-gpui-client-settings-read-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("client-settings.json");
        assert_eq!(read_stored(&path), Ok(Value::Null));
        for (raw, stored) in [
            (r#"{"settings": {"timestampFormat": "24-hour"}}"#, Ok(json!({"timestampFormat": "24-hour"}))),
            (r#"{"timestampFormat": "12-hour"}"#, Ok(json!({"timestampFormat": "12-hour"}))),
            ("{not json", Err(())),
            ("[]", Err(())),
            (r#"{"settings": 1}"#, Err(())),
        ] {
            std::fs::write(&path, raw).unwrap();
            assert_eq!(read_stored(&path), stored, "{raw}");
        }
        let mut settings = ClientSettings::from_stored(&Value::Null, Some(path.clone()));
        settings.read_failed = true;
        settings.apply(json!({"timestampFormat": "24-hour"}).as_object().unwrap());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), r#"{"settings": 1}"#);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn client_settings_round_trip_through_the_electron_document() {
        let dir = std::env::temp_dir().join(format!("t3-gpui-client-settings-{}", std::process::id()));
        let path = dir.join("client-settings.json");
        let mut settings = ClientSettings::from_stored(&json!({}), Some(path.clone()));
        settings.apply(json!({"timestampFormat": "24-hour"}).as_object().unwrap());
        let mut choice = settings.theme_choice();
        choice.mode = ThemePreference::Dark;
        choice.assign_half(false, Some("grove".into()));
        settings.set_theme_choice(choice.clone());
        let aurora = crate::theme_file::parse_theme_file(
            &json!({"version": 1, "name": "Aurora", "appearance": "dark", "colors": {"canvas": "#101014"}}),
        )
        .unwrap();
        let themes = settings.themes.install(aurora).unwrap();
        let choice = ThemeChoice { base: Some("aurora".into()), library: themes.clone(), ..choice };
        settings.set_themes(themes, choice.clone()).unwrap();
        let raw: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let reloaded = ClientSettings::from_stored(&raw, None);
        assert_eq!(reloaded.values.get("timestampFormat"), "24-hour");
        assert_eq!(reloaded.theme, ThemePreference::Dark);
        assert_eq!(reloaded.theme_choice(), choice);
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn theme(name: &str) -> crate::themes::ThemeDefinition {
        crate::theme_file::parse_theme_file(
            &json!({"version": 1, "name": name, "appearance": "dark", "colors": {"canvas": "#101014"}}),
        )
        .unwrap()
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("t3-gpui-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("client-settings.json")
    }

    #[test]
    fn saving_themes_keeps_records_this_build_cannot_read() {
        let path = scratch("theme-records");
        let mut kept = crate::theme_file::stored(&theme("Kept"));
        kept["futureField"] = json!(1);
        let unknown = json!({"id": "from-a-newer-build", "label": "Newer", "appearance": "dim"});
        let removed = crate::theme_file::stored(&theme("Removed"));
        let document = json!({"customThemes": [unknown, kept, removed]});
        std::fs::write(&path, document.to_string()).unwrap();
        let mut settings = ClientSettings::load_from(Some(path.clone()));
        let themes = settings.themes.remove(&["removed".to_owned()]).install(theme("Added")).unwrap();
        let choice = ThemeChoice { library: themes.clone(), ..settings.theme_choice() };
        settings.set_themes(themes, choice).unwrap();
        let raw: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let ids: Vec<&str> = raw["customThemes"].as_array().unwrap().iter().filter_map(|r| r["id"].as_str()).collect();
        assert_eq!(ids, ["from-a-newer-build", "kept", "added"]);
        assert_eq!(raw["customThemes"][1]["futureField"], 1, "an unchanged record stays as stored");
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn an_unreadable_theme_library_takes_no_changes_and_is_not_overwritten() {
        let path = scratch("theme-unreadable");
        for stored in ["{not json", r#"{"customThemes": {"kept": true}}"#] {
            std::fs::write(&path, stored).unwrap();
            let mut settings = ClientSettings::load_from(Some(path.clone()));
            let themes = settings.themes.install(theme("Aurora")).unwrap();
            let choice = ThemeChoice { library: themes.clone(), ..settings.theme_choice() };
            assert!(settings.set_themes(themes, choice).is_err());
            assert!(settings.themes.custom().is_empty(), "nothing changes in memory");
            settings.apply(json!({"timestampFormat": "24-hour"}).as_object().unwrap());
            let raw = std::fs::read_to_string(&path).unwrap();
            let library = serde_json::from_str::<Value>(&raw).map(|doc| doc["customThemes"].clone());
            assert!(library.is_err() || library.unwrap() == json!({"kept": true}), "the stored library survives");
        }
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn a_failed_theme_save_reports_it_and_changes_nothing() {
        let blocker = scratch("theme-write-fails");
        std::fs::write(&blocker, "").unwrap();
        let mut settings = ClientSettings::from_stored(&json!({}), Some(blocker.join("client-settings.json")));
        let themes = settings.themes.install(theme("Aurora")).unwrap();
        let choice = ThemeChoice { base: Some("aurora".into()), library: themes.clone(), ..settings.theme_choice() };
        assert!(settings.set_themes(themes, choice).is_err());
        assert!(settings.themes.custom().is_empty());
        assert_eq!(settings.theme_id, None);
        std::fs::remove_dir_all(blocker.parent().unwrap()).unwrap();
    }
}
