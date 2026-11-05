//! Device-local settings (`ClientSettingsSchema` in `packages/contracts/src/settings.ts`).
//! The web keeps them in localStorage and Electron in `<stateDir>/client-settings.json`;
//! this client writes the same JSON under its own home. Keys it doesn't know round-trip.

use std::path::PathBuf;

use serde_json::{Map, Value, json};

pub struct ClientSettings {
    path: Option<PathBuf>,
    values: Map<String, Value>,
}

impl ClientSettings {
    /// Reads `<T3CODE_HOME, else ~/.t3-gpui>/userdata/client-settings.json`, the home
    /// `backend::LaunchConfig` uses. A missing or unreadable file starts from the defaults.
    pub fn load() -> Self {
        let home = std::env::var_os("T3CODE_HOME").map(PathBuf::from).or_else(|| {
            std::env::var_os("USERPROFILE")
                .or_else(|| std::env::var_os("HOME"))
                .map(|h| PathBuf::from(h).join(".t3-gpui"))
        });
        let path = home.map(|home| home.join("userdata").join("client-settings.json"));
        let values = path
            .as_ref()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice::<Map<String, Value>>(&bytes).ok())
            .unwrap_or_default();
        Self { path, values }
    }

    /// The stored value, else the schema default.
    pub fn get(&self, key: &str) -> Value {
        self.values.get(key).cloned().unwrap_or_else(|| default(key))
    }

    pub fn is_default(&self, key: &str) -> bool {
        self.get(key) == default(key)
    }

    /// `persistClientSettingsPatch`: merges the patch and writes the whole snapshot.
    pub fn patch(&mut self, patch: Map<String, Value>) {
        self.values.extend(patch);
        let Some(path) = &self.path else { return };
        let write = || -> std::io::Result<()> {
            std::fs::create_dir_all(path.parent().expect("settings file has a parent"))?;
            let temporary = path.with_extension("json.tmp");
            std::fs::write(&temporary, serde_json::to_vec_pretty(&self.values)?)?;
            std::fs::rename(&temporary, path)
        };
        if let Err(error) = write() {
            log::warn!("could not save {}: {error}", path.display());
        }
    }
}

/// The schema's decoding defaults for the keys this client reads.
pub fn default(key: &str) -> Value {
    match key {
        "composerRichTextEnabled"
        | "showSkillsInSlashMenu"
        | "diffIgnoreWhitespace"
        | "diffFilesCollapsed"
        | "pullRequestLinksOpenInApp"
        | "snapShotIncludeAccessibility"
        | "snapShotPlaySound"
        | "snapShotFlash"
        | "snapShotAnimations" => json!(true),
        "proactivePanelsEnabled" | "snapShotEnabled" => json!(false),
        "sendShortcut" => json!("enter"),
        "followUpBehavior" => json!("steer"),
        "diffLayout" => json!("stacked"),
        "diffColorScheme" => json!("red-green"),
        _ => Value::Null,
    }
}
