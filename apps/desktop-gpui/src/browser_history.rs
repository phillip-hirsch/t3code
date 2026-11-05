//! Pages opened in the browser panel, per project (`apps/web/src/browserHistoryStore.ts`),
//! for the empty tab's "Recently used". Kept on this device in `browser-history.json`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::mpsc::{Sender, channel};

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const MAX_ENTRIES_PER_PROJECT: usize = 50;
const MAX_PROJECTS: usize = 20;
const MAX_URL_LENGTH: usize = 2048;
const MAX_TITLE_LENGTH: usize = 512;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub url: String,
    /// Milliseconds since the epoch.
    pub last_visited_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserHistory {
    by_project_key: HashMap<String, Vec<Entry>>,
}

/// `normalizeHistoryUrl`: an http(s) URL without credentials, short enough to keep.
pub fn normalize(raw: &str) -> Option<String> {
    let mut url = url::Url::parse(raw.trim()).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let _ = url.set_username("");
    let _ = url.set_password(None);
    let href = url.to_string();
    (href.len() <= MAX_URL_LENGTH).then_some(href)
}

fn is_loopback(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "[::1]" | "::1" | "0.0.0.0") || host.ends_with(".localhost")
}

/// `visitLookupKey`: loopback hosts are one "local" host, so ports keep their history.
fn visit_key(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(mut parsed) => {
            if parsed.host_str().is_some_and(is_loopback) {
                let _ = parsed.set_host(Some("local"));
            }
            parsed.to_string()
        }
        Err(_) => url.to_owned(),
    }
}

/// `titleLookupKey`: the visit key without a trailing slash on a path.
fn title_key(url: &str) -> String {
    let key = visit_key(url);
    match url::Url::parse(&key) {
        Ok(mut parsed) if parsed.path() != "/" && parsed.path().ends_with('/') => {
            let path = parsed.path().trim_end_matches('/').to_owned();
            parsed.set_path(&path);
            parsed.to_string()
        }
        _ => key,
    }
}

fn stable_local(url: &str) -> bool {
    url::Url::parse(url).ok().and_then(|u| u.host_str().map(is_loopback)).unwrap_or(false)
}

impl BrowserHistory {
    fn path() -> Option<PathBuf> {
        Some(crate::settings::userdata_dir()?.join("browser-history.json"))
    }

    pub fn load() -> Self {
        Self::path()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Writes the history off the UI thread, atomically and in order: one writer thread
    /// saves the newest snapshot. A failed write only loses recent visits.
    pub fn save_in_background(&self) {
        static WRITER: OnceLock<Option<Sender<(PathBuf, Value)>>> = OnceLock::new();
        let (Some(path), Ok(document)) = (Self::path(), serde_json::to_value(self)) else { return };
        let writer = WRITER.get_or_init(|| {
            let (sender, receiver) = channel::<(PathBuf, Value)>();
            let spawned = std::thread::Builder::new().name("browser-history".into()).spawn(move || {
                while let Ok(mut latest) = receiver.recv() {
                    while let Ok(newer) = receiver.try_recv() {
                        latest = newer;
                    }
                    if let Err(error) = crate::settings::write_atomically(&latest.0, &latest.1) {
                        log::warn!("browser history not saved: {error}");
                    }
                }
            });
            spawned
                .inspect_err(|error| log::warn!("browser history writer did not start: {error}"))
                .ok()
                .map(|_| sender)
        });
        if let Some(sender) = writer {
            let _ = sender.send((path, document));
        }
    }

    pub fn recent(&self, project_key: &str, limit: usize) -> &[Entry] {
        self.by_project_key.get(project_key).map_or(&[], |entries| &entries[..entries.len().min(limit)])
    }

    /// `upsertHistoryEntry`: the visit moves to the front, keeping a loopback URL over another host's.
    pub fn record_visit(&mut self, project_key: &str, url: &str, at: i64) -> bool {
        let Some(url) = normalize(url) else { return false };
        let entries = self.by_project_key.entry(project_key.to_owned()).or_default();
        let key = visit_key(&url);
        let existing = entries.iter().position(|e| visit_key(&e.url) == key).map(|i| entries.remove(i));
        let entry = match existing {
            Some(mut existing) => {
                if !stable_local(&existing.url) && stable_local(&url) {
                    existing.url = url;
                }
                existing.last_visited_at = at;
                existing
            }
            None => Entry { url, last_visited_at: at, title: None },
        };
        entries.insert(0, entry);
        entries.truncate(MAX_ENTRIES_PER_PROJECT);
        self.evict();
        true
    }

    /// `setTitleForUrl`: names an entry already in the history.
    pub fn set_title(&mut self, project_key: &str, url: &str, title: &str) -> bool {
        let title = title.trim();
        let Some(entries) = self.by_project_key.get_mut(project_key).filter(|_| !title.is_empty()) else {
            return false;
        };
        let key = title_key(url);
        let title: String = title.chars().take(MAX_TITLE_LENGTH).collect();
        let mut changed = false;
        for entry in entries.iter_mut().filter(|e| title_key(&e.url) == key) {
            if entry.title.as_deref() != Some(title.as_str()) {
                entry.title = Some(title.clone());
                changed = true;
            }
        }
        changed
    }

    pub fn remove(&mut self, project_key: &str, url: &str) {
        if let Some(entries) = self.by_project_key.get_mut(project_key) {
            let key = visit_key(url);
            entries.retain(|e| visit_key(&e.url) != key);
        }
    }

    /// `evictExcessProjects`: the projects visited most recently stay.
    fn evict(&mut self) {
        if self.by_project_key.len() <= MAX_PROJECTS {
            return;
        }
        let mut keys: Vec<(String, i64)> = self
            .by_project_key
            .iter()
            .map(|(key, entries)| (key.clone(), entries.first().map_or(0, |e| e.last_visited_at)))
            .collect();
        keys.sort_by_key(|(_, at)| std::cmp::Reverse(*at));
        for (key, _) in keys.into_iter().skip(MAX_PROJECTS) {
            self.by_project_key.remove(&key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visits_move_to_the_front_and_loopback_hosts_share_an_entry() {
        let mut history = BrowserHistory::default();
        assert!(history.record_visit("p", "http://localhost:3000/", 1));
        assert!(history.record_visit("p", "https://example.com/docs", 2));
        assert!(history.record_visit("p", "http://127.0.0.1:3000/", 3));
        let urls: Vec<&str> = history.recent("p", 8).iter().map(|e| e.url.as_str()).collect();
        assert_eq!(urls, ["http://localhost:3000/", "https://example.com/docs"]);
        assert!(!history.record_visit("p", "file:///etc/passwd", 4));
        assert_eq!(normalize("https://user:pw@example.com/a").as_deref(), Some("https://example.com/a"));
    }

    #[test]
    fn titles_name_existing_entries_and_removal_drops_them() {
        let mut history = BrowserHistory::default();
        history.record_visit("p", "https://example.com/docs/", 1);
        assert!(!history.set_title("q", "https://example.com/docs", "Docs"));
        assert!(history.set_title("p", "https://example.com/docs", "Docs"));
        assert_eq!(history.recent("p", 1)[0].title.as_deref(), Some("Docs"));
        history.remove("p", "https://example.com/docs/");
        assert!(history.recent("p", 8).is_empty());
    }

    #[test]
    fn only_the_most_recent_projects_are_kept() {
        let mut history = BrowserHistory::default();
        for i in 0..=MAX_PROJECTS {
            history.record_visit(&format!("p{i}"), "https://example.com/", i as i64);
        }
        assert!(history.recent("p0", 1).is_empty());
        assert_eq!(history.recent(&format!("p{MAX_PROJECTS}"), 1).len(), 1);
    }
}
