//! The reviewer menu's search past the first page of candidates
//! (`packages/client-runtime/src/pullRequestReviewers.ts`). Candidates stay the host's JSON rows.

use std::collections::HashSet;

use serde_json::Value;

fn key(candidate: &Value) -> String {
    format!("{}:{}", candidate["kind"].as_str().unwrap_or_default(), candidate["id"].as_str().unwrap_or_default())
}

fn field(candidate: &Value, name: &str) -> String {
    candidate[name].as_str().unwrap_or_default().to_lowercase()
}

/// `mergeReviewerCandidates`: loaded rows keep the request state a toggle updated, and the host's
/// matches follow, unnarrowed, since it matched them on its own terms (an email, say).
pub fn merge_candidates(loaded: &[Value], searched: &[Value], query: &str) -> Vec<Value> {
    let needle = query.trim().to_lowercase();
    let known: HashSet<String> = loaded.iter().map(key).collect();
    let host_matches: HashSet<String> = searched.iter().map(key).collect();
    loaded
        .iter()
        .filter(|candidate| {
            host_matches.contains(&key(candidate))
                || needle.is_empty()
                || field(candidate, "login").contains(&needle)
                || field(candidate, "name").contains(&needle)
        })
        .chain(searched.iter().filter(|candidate| !known.contains(&key(candidate))))
        .cloned()
        .collect()
}

/// `ReviewerSearchInput`: the first page as read, whether the host declared
/// `reviewers.searchCandidates`, and what is typed now and once the debounce settled.
pub struct SearchInput<'a> {
    pub loaded: Option<&'a Value>,
    pub searchable: bool,
    pub query: &'a str,
    pub debounced: &'a str,
}

fn truncated(list: Option<&Value>) -> bool {
    list.is_some_and(|list| list["truncated"].as_bool() == Some(true))
}

/// `hostReviewerQuery`: what to ask the host, or `None` when the loaded page already answers.
pub fn host_query(input: &SearchInput) -> Option<String> {
    let debounced = input.debounced.trim();
    (input.searchable && truncated(input.loaded) && !debounced.is_empty()).then(|| debounced.to_owned())
}

/// `ReviewerTruncation`: why the list may be missing people.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Truncation {
    Unsearchable,
    Searchable,
    SearchFailed,
    SearchTruncated,
}

impl Truncation {
    /// `TRUNCATED_LABELS`.
    pub fn label(self, error: Option<&str>) -> String {
        match self {
            Self::Unsearchable => {
                "This repository has more people with access than are listed here. Ask for the rest on the host.".into()
            }
            Self::Searchable => {
                "This repository has more people with access than are listed here. Search to find them.".into()
            }
            Self::SearchFailed => {
                format!("Only the people already listed are matched: the host search failed. {}", error.unwrap_or(""))
            }
            Self::SearchTruncated => "More people match than the host returned. Keep typing to narrow it down.".into(),
        }
    }
}

/// The host's answer for the query `host_query` names: `None` while it is in flight.
pub type SearchResult<'a> = Option<Result<&'a Value, &'a str>>;

pub struct View {
    pub candidates: Vec<Value>,
    /// Nothing matches yet and the host has not answered, which is not "nobody matches".
    pub search_pending: bool,
    pub search_error: Option<String>,
    pub truncation: Option<Truncation>,
}

/// `reviewerCandidateView`.
pub fn candidate_view(input: &SearchInput, search: SearchResult) -> View {
    let query = input.query.trim();
    let host_query = host_query(input);
    // Only once the debounce has caught up, or rows matching the previous text would linger.
    let searched = host_query.as_deref() == Some(query);
    let empty = Vec::new();
    let loaded = input.loaded.and_then(|list| list["candidates"].as_array()).unwrap_or(&empty);
    let found = match search {
        Some(Ok(list)) if searched => list["candidates"].as_array().unwrap_or(&empty),
        _ => &empty,
    };
    let candidates = merge_candidates(loaded, found, query);
    let searching = input.searchable && truncated(input.loaded) && !query.is_empty();
    let search_error = match search {
        Some(Err(error)) if searched => Some(error.to_owned()),
        _ => None,
    };
    let truncation = if !truncated(input.loaded) {
        None
    } else if !input.searchable {
        Some(Truncation::Unsearchable)
    } else if query.is_empty() {
        Some(Truncation::Searchable)
    } else if !searched {
        None
    } else if search_error.is_some() {
        Some(Truncation::SearchFailed)
    } else if matches!(search, Some(Ok(list)) if truncated(Some(list))) {
        Some(Truncation::SearchTruncated)
    } else {
        None
    };
    View {
        search_pending: candidates.is_empty() && searching && (!searched || search.is_none()),
        candidates,
        search_error,
        truncation,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn reviewer(login: &str) -> Value {
        json!({"login": login, "name": null, "avatarUrl": null, "id": login, "kind": "user", "isRequested": false})
    }

    fn logins(candidates: &[Value]) -> Vec<&str> {
        candidates.iter().map(|c| c["login"].as_str().unwrap()).collect()
    }

    #[test]
    fn merging_narrows_the_loaded_page_and_appends_what_only_the_host_found() {
        let mut grace = reviewer("grace");
        grace["name"] = json!("Grace Hopper");
        assert_eq!(logins(&merge_candidates(&[reviewer("ada"), grace, reviewer("linus")], &[], "HOP")), ["grace"]);
        let merged =
            merge_candidates(&[reviewer("ada"), reviewer("adam")], &[reviewer("adam"), reviewer("adalbert")], "ada");
        assert_eq!(logins(&merged), ["ada", "adam", "adalbert"]);
    }

    #[test]
    fn merging_keeps_the_loaded_row_a_request_updated_even_on_a_host_only_match() {
        let mut ada = reviewer("ada");
        ada["isRequested"] = json!(true);
        let loaded = [ada];
        assert_eq!(merge_candidates(&loaded, &[reviewer("ada")], "ada"), loaded);
        assert_eq!(merge_candidates(&loaded, &[reviewer("ada")], "ada@example.com"), loaded);
        let mut team = reviewer("core");
        team["kind"] = json!("team");
        let kinds: Vec<Value> =
            merge_candidates(&[team], &[reviewer("core")], "core").iter().map(|c| c["kind"].clone()).collect();
        assert_eq!(kinds, [json!("team"), json!("user")], "a team is not the person sharing its id");
    }

    #[test]
    fn the_view_follows_the_host_search() {
        type Case<'a> = (
            &'a str,
            bool,
            &'a str,
            &'a str,
            SearchResult<'a>,
            Option<&'a str>,
            Vec<&'a str>,
            bool,
            Option<Truncation>,
        );
        let first_page = json!({"candidates": [reviewer("ada"), reviewer("grace")], "truncated": true});
        let zed = json!({"candidates": [reviewer("zed")], "truncated": false});
        let zelda = json!({"candidates": [reviewer("zelda")], "truncated": false});
        let cases: [Case; 5] = [
            (
                "an older server narrows the page itself",
                false,
                "zed",
                "zed",
                None,
                None,
                vec![],
                false,
                Some(Truncation::Unsearchable),
            ),
            (
                "a searchable host is asked once the debounce catches up",
                true,
                "zed",
                "zed",
                Some(Ok(&zed)),
                Some("zed"),
                vec!["zed"],
                false,
                None,
            ),
            (
                "results for text the debounce has not reached are not a no-match",
                true,
                "zed",
                "ze",
                Some(Ok(&zelda)),
                Some("ze"),
                vec![],
                true,
                None,
            ),
            (
                "a failed search falls back to the loaded page",
                true,
                "gr",
                "gr",
                Some(Err("rate limited")),
                Some("gr"),
                vec!["grace"],
                false,
                Some(Truncation::SearchFailed),
            ),
            (
                "an empty query lists the page and offers search",
                true,
                "",
                "",
                None,
                None,
                vec!["ada", "grace"],
                false,
                Some(Truncation::Searchable),
            ),
        ];
        for (name, searchable, query, debounced, search, host, expected, pending, truncation) in cases {
            let input = SearchInput { loaded: Some(&first_page), searchable, query, debounced };
            let view = candidate_view(&input, search);
            assert_eq!(host_query(&input).as_deref(), host, "{name}");
            assert_eq!(logins(&view.candidates), expected, "{name}");
            assert_eq!(view.search_pending, pending, "{name}");
            assert_eq!(view.truncation, truncation, "{name}");
        }
    }
}
