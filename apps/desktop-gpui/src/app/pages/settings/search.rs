//! Settings search (`searchSettings` in `apps/web/src/components/settings/settingsSearch.ts`).

use std::sync::LazyLock;

use crate::settings::{SearchItem, contract};

/// `normalizeSearchText`: lowercase, accents dropped, whitespace collapsed.
fn normalize(text: &str) -> String {
    let folded: String = text.chars().map(fold_accent).flat_map(char::to_lowercase).collect();
    folded.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The base letter of a precomposed Latin letter, which `NFKD` plus dropping marks yields.
fn fold_accent(c: char) -> char {
    const FROM: &str = "àáâãäåèéêëìíîïñòóôõöùúûüýÿçÀÁÂÃÄÅÈÉÊËÌÍÎÏÑÒÓÔÕÖÙÚÛÜÝÇ";
    const TO: &str = "aaaaaaeeeeiiiinooooouuuuyycAAAAAAEEEEIIIINOOOOOUUUUYC";
    FROM.chars().zip(TO.chars()).find_map(|(from, to)| (from == c).then_some(to)).unwrap_or(c)
}

/// An item's searchable text, normalized once: its title, then every field it matches on.
pub(super) struct Indexed {
    title: String,
    fields: Vec<String>,
}

impl Indexed {
    pub(super) fn new(item: &SearchItem, section_label: &str) -> Self {
        let title = normalize(&item.title);
        let mut fields = vec![title.clone(), normalize(section_label)];
        fields.extend(item.search_terms.iter().map(|term| normalize(term)));
        Self { title, fields }
    }
}

/// The shipped items this client offers, with their normalized text.
pub(super) fn shipped() -> &'static [(&'static SearchItem, Indexed)] {
    static INDEX: LazyLock<Vec<(&'static SearchItem, Indexed)>> = LazyLock::new(|| {
        let label = |to: &str| super::section_label(to).unwrap_or_default();
        contract()
            .search
            .iter()
            .filter(|item| available(item))
            .map(|item| (item, Indexed::new(item, label(&item.to))))
            .collect()
    });
    &INDEX
}

/// Whether the item exists in this client: a desktop app on this OS, with only the
/// environment-dependent items it can offer.
pub(super) fn available(item: &SearchItem) -> bool {
    (!item.mac_only || cfg!(target_os = "macos"))
        && (!item.windows_only || cfg!(target_os = "windows"))
        // Features the web gates on a hosted config, a local backend it manages, WSL, tunnels,
        // peer links or a Mac environment are not offered by this client yet.
        && (!item.secrets_feature_only || super::secrets_enabled())
        && !item.cloud_only
        && !item.local_backend_management_only
        && !item.wsl_available_only
        && !item.managed_tunnel_only
        && !item.peer_links_only
        && !item.mac_provider_settings_only
        // Browser rows for surfaces this client doesn't have (see `browser.rs`).
        && !matches!(
            item.id.as_str(),
            "browser-auto-show-floating-preview" | "browser-recording-key-presses" | "browser-recording-mouse-presses"
        )
}

/// Matches every query token somewhere in the title, section label or search terms, and
/// ranks whole-title, title-prefix and title matches first; secondary items go last.
pub(super) fn search<'a>(query: &str, items: &'a [(&'a SearchItem, Indexed)]) -> Vec<&'a SearchItem> {
    let query = normalize(query);
    if query.is_empty() {
        return Vec::new();
    }
    let tokens: Vec<&str> = query.split(' ').collect();
    let mut ranked: Vec<(&SearchItem, usize, u8)> = items
        .iter()
        .enumerate()
        .filter_map(|(index, (item, Indexed { title, fields }))| {
            if !tokens.iter().all(|token| fields.iter().any(|field| field.contains(token))) {
                return None;
            }
            let rank = if *title == query {
                5
            } else if title.starts_with(&query) {
                4
            } else if title.contains(&query) {
                3
            } else if tokens.iter().all(|token| title.contains(token)) {
                2
            } else {
                u8::from(fields.iter().any(|field| field.contains(&query)))
            };
            Some((*item, index, rank))
        })
        .collect();
    ranked.sort_by(|a, b| a.0.secondary.cmp(&b.0.secondary).then(b.2.cmp(&a.2)).then(a.1.cmp(&b.1)));
    ranked.into_iter().map(|(item, ..)| item).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, title: &str, to: &str, terms: &[&str], secondary: bool) -> SearchItem {
        SearchItem {
            id: id.into(),
            title: title.into(),
            to: to.into(),
            search_terms: terms.iter().map(|t| (*t).into()).collect(),
            secondary,
            ..Default::default()
        }
    }

    fn ids(query: &str, items: &[SearchItem]) -> Vec<String> {
        let label = |to: &str| if to == "general" { "General" } else { "Appearance" };
        let indexed: Vec<_> = items.iter().map(|item| (item, Indexed::new(item, label(&item.to)))).collect();
        search(query, &indexed).into_iter().map(|i| i.id.clone()).collect()
    }

    #[test]
    fn ranks_exact_then_prefix_then_title_then_terms() {
        let items = [
            item("terms", "Delete now", "general", &["theme cleanup"], false),
            item("contains", "Custom theme", "general", &[], false),
            item("prefix", "Theme editor", "general", &[], false),
            item("exact", "Theme", "general", &[], false),
        ];
        assert_eq!(ids("theme", &items), ["exact", "prefix", "contains", "terms"]);
    }

    #[test]
    fn every_token_must_match_some_field_and_secondary_items_go_last() {
        let items = [
            item("shortcut", "Toggle sidebar", "general", &["ctrl+b"], true),
            item("row", "Sidebar width", "general", &[], false),
            item("section", "Contrast", "appearance", &[], false),
        ];
        assert_eq!(ids("sidebar", &items), ["row", "shortcut"]);
        assert_eq!(ids("appearance contrast", &items), ["section"]);
        assert!(ids("contrast missing", &items).is_empty());
        assert!(ids("   ", &items).is_empty());
    }

    #[test]
    fn matching_ignores_case_accents_and_spacing() {
        let items = [item("cafe", "Café   Mode", "general", &[], false)];
        assert_eq!(ids("CAFE mode", &items), ["cafe"]);
    }

    #[test]
    fn the_shipped_index_finds_its_settings() {
        let results = search("delete confirmation", shipped());
        assert_eq!(results.first().map(|i| i.id.as_str()), Some("delete-confirmation"));
    }
}
