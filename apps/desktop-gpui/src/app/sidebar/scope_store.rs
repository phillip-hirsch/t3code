//! The sidebar's project filter, kept across launches like `uiStateStore.sidebarProjectScopeKeys`.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

fn path() -> Option<PathBuf> {
    Some(crate::settings::userdata_dir()?.join("ui-state.json"))
}

fn read_document(path: &Path) -> Value {
    std::fs::read(path).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or(Value::Null)
}

/// The saved keys, or the legacy single key older clients wrote; empty means every project.
fn read(path: &Path) -> Vec<String> {
    let document = read_document(path);
    let keys: Vec<String> = match document.get("sidebarProjectScopeKeys") {
        Some(Value::Array(keys)) => keys.iter().filter_map(Value::as_str).map(str::to_owned).collect(),
        _ => document.get("sidebarProjectScopeKey").and_then(Value::as_str).map(str::to_owned).into_iter().collect(),
    };
    let mut unique: Vec<String> = Vec::new();
    for key in keys {
        if !key.is_empty() && !unique.contains(&key) {
            unique.push(key);
        }
    }
    unique
}

fn write(path: &Path, keys: &[String]) -> std::io::Result<()> {
    let mut document = match read_document(path) {
        Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    document.insert("sidebarProjectScopeKeys".into(), json!(keys));
    // Older clients read only this field.
    let single = if let [key] = keys { json!(key) } else { Value::Null };
    document.insert("sidebarProjectScopeKey".into(), single);
    crate::settings::write_atomically(path, &Value::Object(document))
}

pub(super) fn load() -> Vec<String> {
    path().map(|path| read(&path)).unwrap_or_default()
}

pub(super) fn save(keys: &[String]) {
    let Some(path) = path() else { return };
    if let Err(error) = write(&path, keys) {
        log::warn!("sidebar project filter not saved: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_filter_round_trips_and_keeps_other_fields() {
        let dir = std::env::temp_dir().join(format!("t3-gpui-scope-store-{}", std::process::id()));
        let path = dir.join("ui-state.json");
        assert!(read(&path).is_empty(), "no file is every project");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, r#"{"sidebarProjectScopeKey": "legacy", "other": 1}"#).unwrap();
        assert_eq!(read(&path), ["legacy"]);
        write(&path, &["a".into(), "b".into()]).unwrap();
        assert_eq!(read(&path), ["a", "b"]);
        let document = read_document(&path);
        assert_eq!(document["other"], 1);
        assert_eq!(document["sidebarProjectScopeKey"], Value::Null);
        write(&path, &[]).unwrap();
        assert!(read(&path).is_empty());
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir(&dir).unwrap();
    }
}
