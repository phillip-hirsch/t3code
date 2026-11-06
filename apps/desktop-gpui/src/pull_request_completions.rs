//! What pull request composers complete at the caret: `@login`, `#123` and GitLab `!123`
//! (`packages/shared/src/pullRequestComposerTrigger.ts`). Offsets are byte offsets into the text.

use std::ops::Range;

use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Mention,
    Reference,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Trigger {
    pub kind: Kind,
    /// `@`, `#` or `!`.
    pub sigil: char,
    pub query: String,
    pub range: Range<usize>,
}

/// What a pull request's host can complete (`capabilities.mentions`, `referenceSigils`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Support {
    pub mentions: bool,
    pub reference_sigils: Vec<String>,
}

impl Support {
    /// `pullRequestCompletionScope`: `None` on hosts that complete nothing.
    pub fn any(self) -> Option<Self> {
        (self.mentions || !self.reference_sigils.is_empty()).then_some(self)
    }
}

fn is_login(token: &str) -> bool {
    let mut chars = token.chars();
    chars.next().is_none_or(|first| {
        (first.is_ascii_alphanumeric() || first == '_')
            && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
    })
}

/// `detectPullRequestComposerTrigger`: the token being typed at the caret, when the caret sits at
/// its end and the host completes its sigil.
pub fn detect(text: &str, cursor: usize, support: &Support) -> Option<Trigger> {
    let end = cursor.min(text.len());
    if !text.is_char_boundary(end) || text[end..].chars().next().is_some_and(|c| !c.is_whitespace()) {
        return None;
    }
    let start =
        text[..end].rfind(char::is_whitespace).map_or(0, |at| at + text[at..].chars().next().map_or(1, char::len_utf8));
    let token = &text[start..end];
    let sigil = token.chars().next()?;
    let query = &token[sigil.len_utf8()..];
    let trigger = |kind| Trigger { kind, sigil, query: query.to_owned(), range: start..end };
    match sigil {
        '@' if is_login(query) => support.mentions.then(|| trigger(Kind::Mention)),
        '#' | '!' if query.chars().all(|c| c.is_alphanumeric() || matches!(c, '_' | '-')) => {
            support.reference_sigils.iter().any(|s| s.starts_with(sigil)).then(|| trigger(Kind::Reference))
        }
        _ => None,
    }
}

/// `insertPullRequestCompletion`: the trigger replaced by `token`, and the caret past the space after it.
pub fn insert(text: &str, trigger: &Trigger, token: &str) -> (String, usize) {
    let followed_by_space = text[trigger.range.end..].starts_with([' ', '\t']);
    let replacement = if followed_by_space { token.to_owned() } else { format!("{token} ") };
    let next = format!("{}{replacement}{}", &text[..trigger.range.start], &text[trigger.range.end..]);
    (next, trigger.range.start + replacement.len() + usize::from(followed_by_space))
}

/// `pullRequestCompletionKey`: what a debounce delays, the sigil and its host search.
pub fn key(trigger: &Trigger) -> String {
    format!("{}{}", trigger.sigil, trigger.query.chars().take(100).collect::<String>())
}

/// `debouncedPullRequestCompletionSearch`: until a key for this sigil settles, the host is asked
/// for its unsearched list, which narrows locally, so a burst of typing sends one query.
pub fn debounced_search(sigil: char, settled: Option<&str>) -> Option<String> {
    settled.and_then(|key| key.strip_prefix(sigil)).filter(|query| !query.is_empty()).map(str::to_owned)
}

fn text(value: &Value, key: &str) -> String {
    value[key].as_str().unwrap_or_default().to_lowercase()
}

/// `narrowPullRequestMentions`.
pub fn narrow_mentions<'a>(users: &'a [Value], typed: &str, limit: usize) -> Vec<&'a Value> {
    let needle = typed.to_lowercase();
    users
        .iter()
        .filter(|u| text(u, "login").contains(&needle) || text(u, "name").contains(&needle))
        .take(limit)
        .collect()
}

/// `narrowPullRequestReferences`: the typed sigil's candidates, by number prefix or title.
pub fn narrow_references<'a>(candidates: &'a [Value], sigil: char, query: &str, limit: usize) -> Vec<&'a Value> {
    let needle = query.to_lowercase();
    candidates
        .iter()
        .filter(|c| {
            c["reference"].as_str().is_some_and(|r| r.starts_with(sigil))
                && (c["number"].as_u64().is_some_and(|n| n.to_string().starts_with(&needle))
                    || text(c, "title").contains(&needle))
        })
        .take(limit)
        .collect()
}

/// One suggestion: the token it inserts and the host's row (an actor or a reference candidate).
#[derive(Clone, Debug, PartialEq)]
pub struct Completion {
    pub token: String,
    pub item: Value,
}

/// `pullRequestCompletions`: the host's `pullRequests.filterOptions` answer narrowed to what is typed.
pub fn completions(trigger: &Trigger, options: Option<&Value>, limit: usize) -> Vec<Completion> {
    let empty = Vec::new();
    let list = |key: &str| options.and_then(|o| o[key].as_array()).unwrap_or(&empty);
    match trigger.kind {
        Kind::Mention => narrow_mentions(list("authors"), &trigger.query, limit)
            .into_iter()
            .map(|actor| Completion {
                token: format!("@{}", actor["login"].as_str().unwrap_or_default()),
                item: actor.clone(),
            })
            .collect(),
        Kind::Reference => narrow_references(list("references"), trigger.sigil, &trigger.query, limit)
            .into_iter()
            .map(|c| Completion { token: c["reference"].as_str().unwrap_or_default().to_owned(), item: c.clone() })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn gitlab() -> Support {
        Support { mentions: true, reference_sigils: vec!["#".into(), "!".into()] }
    }

    #[test]
    fn finds_the_token_at_the_caret() {
        for (text, start, sigil, query) in [
            ("@", 0, '@', ""),
            ("cc @ju", 3, '@', "ju"),
            ("line one\n@first.last", 9, '@', "first.last"),
            ("@jane-doe more", 0, '@', "jane-doe"),
            ("fixes #", 6, '#', ""),
            ("see #12", 4, '#', "12"),
            ("after !5", 6, '!', "5"),
            ("#crash", 0, '#', "crash"),
        ] {
            let cursor = start + 1 + query.len();
            let kind = if sigil == '@' { Kind::Mention } else { Kind::Reference };
            assert_eq!(
                detect(text, cursor, &gitlab()),
                Some(Trigger { kind, sigil, query: query.into(), range: start..cursor }),
                "{text:?}"
            );
        }
    }

    #[test]
    fn ignores_what_is_not_a_trigger_or_what_the_host_cannot_complete() {
        for (text, cursor) in [
            ("mail me at jane@example.com", 27),
            ("@jane done", 10),
            ("@-jane", 6),
            ("`@decorator`", 11),
            ("@jane", 3),
            ("C#", 2),
            ("![image", 7),
            ("## Heading", 2),
            ("é", 2),
            ("日本", 6),
            ("hi 👋", 7),
        ] {
            assert_eq!(detect(text, cursor, &gitlab()), None, "{text:?}");
        }
        let hosts = [
            ("@ju", Support::default()),
            ("#12", Support { mentions: true, reference_sigils: vec![] }),
            ("!important", Support { mentions: true, reference_sigils: vec!["#".into()] }),
        ];
        for (text, support) in hosts {
            assert_eq!(detect(text, text.len(), &support), None, "{text:?}");
        }
        assert_eq!(Support::default().any(), None);
    }

    #[test]
    fn inserting_replaces_the_token_and_moves_past_one_space() {
        for suffix in ["", "\nnext line", "\r\nnext line"] {
            let text = format!("thanks @ju{suffix}");
            let trigger = detect(&text, "thanks @ju".len(), &gitlab()).unwrap();
            assert_eq!(
                insert(&text, &trigger, "@julius"),
                (format!("thanks @julius {suffix}"), "thanks @julius ".len())
            );
        }
        let text = "#1 please look";
        let trigger = detect(text, 2, &gitlab()).unwrap();
        let (next, cursor) = insert(text, &trigger, "#123");
        assert_eq!((next.as_str(), cursor), ("#123 please look", 5));
        assert_eq!(detect(&next, cursor, &gitlab()), None);
    }

    #[test]
    fn the_settled_key_names_the_host_search() {
        assert_eq!(debounced_search('#', Some("#crash")).as_deref(), Some("crash"));
        assert_eq!(debounced_search('@', Some("@")), None);
        assert_eq!(debounced_search('!', Some("#12")), None);
        assert_eq!(debounced_search('#', None), None);
    }

    #[test]
    fn narrowing_matches_logins_names_numbers_and_titles() {
        let users = [
            json!({"login": "TonybynMp4", "name": null}),
            json!({"login": "16g", "name": "Tom"}),
            json!({"login": "theo", "name": "Theo"}),
        ];
        let logins: Vec<&str> = narrow_mentions(&users, "to", 5).iter().map(|u| u["login"].as_str().unwrap()).collect();
        assert_eq!(logins, ["TonybynMp4", "16g"]);
        assert_eq!(narrow_mentions(&users, "", 2).len(), 2);
        let candidates = [
            json!({"reference": "#12", "number": 12, "title": "Crash on start"}),
            json!({"reference": "!12", "number": 12, "title": "Fix the crash"}),
            json!({"reference": "#3", "number": 3, "title": "Issue 12 follow-up"}),
            json!({"reference": "#120", "number": 120, "title": "Docs"}),
        ];
        let narrowed = |sigil, query| -> Vec<String> {
            narrow_references(&candidates, sigil, query, 5)
                .iter()
                .map(|c| c["reference"].as_str().unwrap().into())
                .collect()
        };
        assert_eq!(narrowed('#', "12"), ["#12", "#3", "#120"]);
        assert_eq!(narrowed('!', "CRASH"), ["!12"]);
        assert_eq!(narrowed('#', ""), ["#12", "#3", "#120"]);
    }
}
