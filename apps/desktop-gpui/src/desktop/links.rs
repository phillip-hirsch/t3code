//! App links the OS hands the desktop app (`t3code://…`): `packages/shared/src/promptLinks.ts`
//! and `readPairingAppLink` in `packages/shared/src/remote.ts`.

use url::Url;

use crate::model::{ModelSelection, ProjectShell, Provider};

/// `PROMPT_LINK_MAX_PROMPT_LENGTH`.
const MAX_PROMPT_LENGTH: usize = 8_000;
/// `DEFAULT_HOSTED_APP_URL`.
const HOSTED_APP_ORIGIN: &str = "https://app.t3.codes";

/// What an app link opens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppLink {
    /// `/new?…`: the query is parsed by [`parse_new_thread_link`].
    New { query: String },
    /// `/<environment>/<thread>?…`, with an optional `prompt`.
    Thread { environment_id: String, thread_id: String, query: String },
    /// `t3code://pair?pairingUrl=…`: adds a remote backend.
    Pair { pairing_url: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkedPrompt {
    pub text: String,
    pub hidden_characters: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Repository {
    pub owner: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequest {
    pub repository: Repository,
    pub number: u64,
}

impl PullRequest {
    /// `promptLinkPullRequestUrl`.
    pub fn url(&self) -> String {
        format!("https://github.com/{}/{}/pull/{}", self.repository.owner, self.repository.name, self.number)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NewThreadLink {
    New { prompt: LinkedPrompt, repository: Option<Repository>, provider: Option<String>, model: Option<String> },
    PullRequest { pull_request: PullRequest, prompt: Option<LinkedPrompt> },
}

fn is_app_scheme(scheme: &str) -> bool {
    scheme == "t3code"
        || scheme
            .strip_prefix("t3code-")
            .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_lowercase()))
}

/// `promptLinkAppPath` and `readPairingAppLink` together, as `openAppLink` tries them.
pub fn parse_app_link(value: &str) -> Option<AppLink> {
    let url = Url::parse(value).ok()?;
    let query = url.query().map(|q| format!("?{q}")).unwrap_or_default();
    let thread = |path: &str| {
        let mut parts = path.trim_start_matches('/').trim_end_matches('/').split('/');
        match (parts.next(), parts.next(), parts.next()) {
            (Some(environment), Some(thread), None) if !environment.is_empty() && !thread.is_empty() => {
                Some(AppLink::Thread {
                    environment_id: environment.into(),
                    thread_id: thread.into(),
                    query: query.clone(),
                })
            }
            _ => None,
        }
    };
    let bare = url.path().is_empty() || url.path() == "/";
    if is_app_scheme(url.scheme()) {
        return match url.host_str() {
            Some("pair") if bare => url
                .query_pairs()
                .find(|(key, _)| key == "pairingUrl")
                .map(|(_, value)| value.trim().to_owned())
                .filter(|value| !value.is_empty())
                .map(|pairing_url| AppLink::Pair { pairing_url }),
            Some("new") if bare => Some(AppLink::New { query }),
            Some("threads") => thread(url.path()),
            _ => None,
        };
    }
    if url.origin().ascii_serialization() != HOSTED_APP_ORIGIN {
        return None;
    }
    if url.path() == "/new" {
        return Some(AppLink::New { query });
    }
    // The web app's own thread route is a link only with a prompt, so reloading a tab never asks.
    match url.path().strip_prefix("/threads/") {
        Some(rest) => thread(rest),
        None if url.query_pairs().any(|(key, _)| key == "prompt") => thread(url.path()),
        None => None,
    }
}

fn param(query: &str, key: &str) -> Option<String> {
    url::form_urlencoded::parse(query.trim_start_matches('?').as_bytes())
        .find(|(k, _)| k == key)
        .map(|(_, value)| value.into_owned())
}

fn optional_param(query: &str, key: &str) -> Option<String> {
    param(query, key).map(|value| value.trim().to_owned()).filter(|value| !value.is_empty())
}

/// The characters `HIDDEN_CHARACTERS` removes: models read them, people reviewing the prompt don't.
fn is_hidden(c: char) -> bool {
    matches!(c as u32,
        0x0000..=0x0008 | 0x000B | 0x000C | 0x000E..=0x001F | 0x007F..=0x009F | 0x00AD | 0x034F | 0x061C
        | 0x115F | 0x1160 | 0x17B4 | 0x17B5 | 0x180B..=0x180F | 0x200B | 0x200C | 0x200E | 0x200F
        | 0x202A..=0x202E | 0x2060..=0x206F | 0x3164 | 0xFE00..=0xFE0D | 0xFEFF | 0xFFA0 | 0xFFF0..=0xFFFB
        | 0x1BCA0..=0x1BCA3 | 0x1D173..=0x1D17A | 0xE0000..=0xE0FFF)
}

fn is_emoji_part(c: Option<char>) -> bool {
    c.is_some_and(|c| matches!(c as u32, 0x1F000..=0x1FAFF | 0x2190..=0x2BFF | 0xFE0F))
}

/// `sanitizeLinkedPrompt`: newlines normalized, hidden characters dropped and counted; a
/// zero-width joiner survives only between two emoji parts.
pub fn sanitize_prompt(raw: &str) -> LinkedPrompt {
    let normalized = raw.replace("\r\n", "\n").replace(['\r', '\u{2028}', '\u{2029}'], "\n");
    let mut hidden_characters = 0;
    let visible: Vec<char> = normalized
        .chars()
        .filter(|c| {
            let hidden = is_hidden(*c);
            hidden_characters += usize::from(hidden);
            !hidden
        })
        .collect();
    let mut text = String::with_capacity(visible.len());
    for (index, c) in visible.iter().enumerate() {
        if *c == '\u{200D}' {
            let kept = is_emoji_part(index.checked_sub(1).map(|i| visible[i]))
                && is_emoji_part(visible.get(index + 1).copied());
            if !kept {
                hidden_characters += 1;
                continue;
            }
        }
        text.push(*c);
    }
    LinkedPrompt { text: text.trim().to_owned(), hidden_characters }
}

fn repository_of(owner: &str, name: &str) -> Option<Repository> {
    let name = match name.len().checked_sub(4) {
        Some(cut) if name.is_char_boundary(cut) && name[cut..].eq_ignore_ascii_case(".git") => &name[..cut],
        _ => name,
    };
    let owner_ok = !owner.is_empty()
        && owner.len() <= 39
        && owner.starts_with(|c: char| c.is_ascii_alphanumeric())
        && owner.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    let name_ok = !name.is_empty()
        && name.len() <= 100
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        && !name.chars().all(|c| c == '.');
    (owner_ok && name_ok).then(|| Repository { owner: owner.into(), name: name.into() })
}

fn strip_github(value: &str) -> Option<&str> {
    let lower = value.to_ascii_lowercase();
    ["https://www.github.com/", "https://github.com/", "http://www.github.com/", "http://github.com/"]
        .into_iter()
        .find(|prefix| lower.starts_with(prefix))
        .map(|prefix| &value[prefix.len()..])
}

/// `parsePromptLinkRepository`: `owner/name` or a GitHub repository URL.
pub fn parse_repository(value: &str) -> Option<Repository> {
    let trimmed = value.trim();
    let path = match strip_github(trimmed) {
        Some(rest) => rest.strip_suffix('/').unwrap_or(rest),
        None if trimmed.chars().any(char::is_whitespace) => return None,
        None => trimmed,
    };
    let (owner, name) = path.split_once('/')?;
    if name.contains(['/', '?', '#']) || owner.contains(['?', '#']) {
        return None;
    }
    repository_of(owner, name)
}

/// `parsePromptLinkPullRequest`: a GitHub pull request URL, `owner/name/123` or `owner/name#123`.
fn parse_pull_request(value: &str) -> Option<PullRequest> {
    let trimmed = value.trim();
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let (owner, name, number) = match strip_github(trimmed) {
        Some(rest) => {
            let mut parts = rest.splitn(4, '/');
            let (owner, name, pull, tail) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
            // The number may be followed by `/files`, `?…` or `#…`.
            let number = &tail[..tail.find(['/', '?', '#']).unwrap_or(tail.len())];
            if !pull.eq_ignore_ascii_case("pull") || [owner, name].iter().any(|part| part.contains(['?', '#'])) {
                return None;
            }
            (owner, name, number)
        }
        None => {
            if trimmed.chars().any(char::is_whitespace) {
                return None;
            }
            let split = trimmed.rfind(['/', '#'])?;
            let (owner, name) = trimmed[..split].split_once('/')?;
            if [owner, name].iter().any(|part| part.contains(['/', '#'])) {
                return None;
            }
            (owner, name, &trimmed[split + 1..])
        }
    };
    if !digits(number) {
        return None;
    }
    let number: u64 = number.parse().ok().filter(|n| *n > 0 && *n <= 9_007_199_254_740_991)?;
    Some(PullRequest { repository: repository_of(owner, name)?, number })
}

/// `readPrompt`.
fn read_prompt(query: &str) -> Result<Option<LinkedPrompt>, String> {
    let Some(raw) = param(query, "prompt") else { return Ok(None) };
    // JavaScript counts UTF-16 code units.
    if raw.encode_utf16().count() > MAX_PROMPT_LENGTH {
        return Err("This prompt is longer than 8,000 characters, so T3 Code did not open it.".into());
    }
    let prompt = sanitize_prompt(&raw);
    if prompt.text.is_empty() && prompt.hidden_characters > 0 {
        return Err("This prompt had only hidden characters, so it may be malicious. T3 Code did not open it.".into());
    }
    Ok((!prompt.text.is_empty()).then_some(prompt))
}

/// `parseNewThreadLink`.
pub fn parse_new_thread_link(query: &str) -> Result<NewThreadLink, String> {
    let prompt = read_prompt(query)?;
    if let Some(pr) = param(query, "pr") {
        let pull_request =
            parse_pull_request(&pr).ok_or_else(|| "This link names a pull request T3 Code cannot read.".to_owned())?;
        return Ok(NewThreadLink::PullRequest { pull_request, prompt });
    }
    let prompt = prompt.ok_or_else(|| "This link has no prompt.".to_owned())?;
    let repository = match param(query, "repo") {
        Some(repo) => Some(
            parse_repository(&repo).ok_or_else(|| "This link names a repository T3 Code cannot read.".to_owned())?,
        ),
        None => None,
    };
    Ok(NewThreadLink::New {
        prompt,
        repository,
        provider: optional_param(query, "provider").map(|p| p.to_lowercase()),
        model: optional_param(query, "model"),
    })
}

/// `parseThreadLinkPrompt`.
pub fn parse_thread_link_prompt(query: &str) -> Result<Option<LinkedPrompt>, String> {
    read_prompt(query)
}

// A link's `owner/name` is GitHub's; git.cafe mirrors GitHub paths two-way, so its checkouts count.
const REPOSITORY_HOSTS: [&str; 3] = ["github.com", "git.cafe", "staging.git.cafe"];

/// `findPromptLinkProjects`: the repository's projects, in the order given (most recent first).
pub fn find_projects<'a>(projects: &[&'a ProjectShell], repository: &Repository) -> Vec<&'a ProjectShell> {
    let path = format!("{}/{}", repository.owner, repository.name).to_lowercase();
    projects
        .iter()
        .copied()
        .filter(|project| {
            let Some(identity) = &project.repository_identity else { return false };
            // `repositoryGroupingKeyOf`.
            let key = identity.origin.as_ref().map(|o| o.canonical_key.as_str()).or(identity.canonical_key.as_deref());
            key.is_some_and(|key| REPOSITORY_HOSTS.iter().any(|host| key.to_lowercase() == format!("{host}/{path}")))
        })
        .collect()
}

/// `resolvePromptLinkModelSelection`: the first ready (else warning) enabled instance of the
/// provider, with the named model or its default. `exact` is false when the model was not found.
pub fn resolve_model(providers: &[Provider], provider: &str, model: Option<&str>) -> Option<(ModelSelection, bool)> {
    let driver = if provider == "claude" { "claudeagent" } else { provider };
    let usable: Vec<&Provider> = providers
        .iter()
        .filter(|p| p.driver.to_lowercase() == driver && p.enabled && p.availability.as_deref() != Some("unavailable"))
        .collect();
    let instance =
        usable.iter().find(|p| p.status == "ready").or_else(|| usable.iter().find(|p| p.status == "warning"))?;
    // `resolveSelectableModel` by slug, then display name.
    let requested = model.map(str::trim).filter(|m| !m.is_empty()).and_then(|wanted| {
        let models = &instance.models;
        models
            .iter()
            .find(|m| m.slug == wanted)
            .or_else(|| models.iter().find(|m| m.name.as_deref().is_some_and(|n| n.eq_ignore_ascii_case(wanted))))
            .map(|m| m.slug.clone())
    });
    let slug = requested.clone().or_else(|| {
        let models = &instance.models;
        models.iter().find(|m| m.is_default == Some(true)).or(models.first()).map(|m| m.slug.clone())
    })?;
    let selection = ModelSelection { instance_id: instance.instance_id.clone(), model: slug, options: None };
    Some((selection, model.is_none() || requested.is_some()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(owner: &str, name: &str) -> Repository {
        Repository { owner: owner.into(), name: name.into() }
    }

    #[test]
    fn app_links_resolve_like_prompt_link_app_path() {
        assert_eq!(parse_app_link("t3code://new?prompt=hi"), Some(AppLink::New { query: "?prompt=hi".into() }));
        assert_eq!(parse_app_link("t3code-dev://new/"), Some(AppLink::New { query: String::new() }));
        assert_eq!(
            parse_app_link("t3code://threads/env-1/thread-2?prompt=x"),
            Some(AppLink::Thread {
                environment_id: "env-1".into(),
                thread_id: "thread-2".into(),
                query: "?prompt=x".into()
            })
        );
        assert_eq!(parse_app_link("t3code://threads/env-1"), None);
        assert_eq!(parse_app_link("t3code://new/extra"), None);
        assert_eq!(parse_app_link("t3codex://new"), None);
        // Schemes are case-insensitive, as `new URL` lowercases them.
        assert!(parse_app_link("t3code-DEV://new").is_some());
        assert_eq!(
            parse_app_link("t3code://pair?pairingUrl=%20https%3A%2F%2Fhost%2Fpair%23t%3Dx%20"),
            Some(AppLink::Pair { pairing_url: "https://host/pair#t=x".into() })
        );
        assert_eq!(parse_app_link("t3code://pair?pairingUrl="), None);
        // Hosted app URLs.
        assert_eq!(
            parse_app_link("https://app.t3.codes/new?prompt=a"),
            Some(AppLink::New { query: "?prompt=a".into() })
        );
        assert!(matches!(parse_app_link("https://app.t3.codes/threads/e/t"), Some(AppLink::Thread { .. })));
        assert!(matches!(parse_app_link("https://app.t3.codes/e/t?prompt=a"), Some(AppLink::Thread { .. })));
        assert_eq!(parse_app_link("https://app.t3.codes/e/t"), None);
        assert_eq!(parse_app_link("https://example.com/new?prompt=a"), None);
        assert_eq!(parse_app_link("not a url"), None);
    }

    #[test]
    fn prompts_lose_hidden_characters_and_count_them() {
        assert_eq!(sanitize_prompt(" a\r\nb\u{200B}c "), LinkedPrompt { text: "a\nbc".into(), hidden_characters: 1 });
        // A joiner inside an emoji sequence stays; elsewhere it is hidden.
        let family = "\u{1F468}\u{200D}\u{1F469}";
        assert_eq!(sanitize_prompt(family).text, family);
        assert_eq!(sanitize_prompt("a\u{200D}b"), LinkedPrompt { text: "ab".into(), hidden_characters: 1 });
        assert_eq!(sanitize_prompt("x\u{E0041}"), LinkedPrompt { text: "x".into(), hidden_characters: 1 });
    }

    #[test]
    fn new_thread_links_parse_like_the_web() {
        assert_eq!(
            parse_new_thread_link(
                "?prompt=Fix+it&repo=https://github.com/pingdotgg/t3code.git/&provider=Claude&model=%20opus%20"
            ),
            Ok(NewThreadLink::New {
                prompt: LinkedPrompt { text: "Fix it".into(), hidden_characters: 0 },
                repository: Some(repo("pingdotgg", "t3code")),
                provider: Some("claude".into()),
                model: Some("opus".into()),
            })
        );
        assert_eq!(parse_new_thread_link("?repo=a/b"), Err("This link has no prompt.".into()));
        assert_eq!(
            parse_new_thread_link("?prompt=x&repo=a/b/c"),
            Err("This link names a repository T3 Code cannot read.".into())
        );
        assert_eq!(
            parse_new_thread_link("?prompt=%E2%80%8B"),
            Err("This prompt had only hidden characters, so it may be malicious. T3 Code did not open it.".into())
        );
        let long = format!("?prompt={}", "a".repeat(8_001));
        assert!(parse_new_thread_link(&long).unwrap_err().contains("8,000"));
        for (pr, number) in [
            ("https://github.com/a/b/pull/12/files", 12),
            ("a/b#7", 7),
            ("a/b/9", 9),
            ("HTTPS://www.GitHub.com/a/b/pull/3?x", 3),
        ] {
            assert_eq!(
                parse_new_thread_link(&format!(
                    "?pr={}",
                    url::form_urlencoded::byte_serialize(pr.as_bytes()).collect::<String>()
                )),
                Ok(NewThreadLink::PullRequest {
                    pull_request: PullRequest { repository: repo("a", "b"), number },
                    prompt: None
                }),
                "{pr}"
            );
        }
        assert!(parse_new_thread_link("?pr=a/b#0").is_err());
        assert!(parse_new_thread_link("?pr=https://github.com/a/b/issues/3").is_err());
    }

    #[test]
    fn links_pick_projects_by_repository_and_models_by_provider() {
        let project = |id: &str, key: &str| -> ProjectShell {
            serde_json::from_value(serde_json::json!({
                "id": id, "title": id, "workspaceRoot": "/w",
                "repositoryIdentity": { "canonicalKey": key },
            }))
            .unwrap()
        };
        let (a, b, c) = (
            project("a", "github.com/Owner/Repo"),
            project("b", "git.cafe/owner/repo"),
            project("c", "github.com/o/x"),
        );
        let found: Vec<&str> =
            find_projects(&[&b, &c, &a], &repo("owner", "repo")).iter().map(|p| p.id.as_str()).collect();
        assert_eq!(found, ["b", "a"]);

        let providers: Vec<Provider> = serde_json::from_value(serde_json::json!([
            { "instanceId": "c1", "driver": "claudeAgent", "enabled": true, "status": "error",
              "models": [{ "slug": "m0" }] },
            { "instanceId": "c2", "driver": "claudeAgent", "enabled": true, "status": "warning",
              "models": [{ "slug": "m1" }, { "slug": "m2", "name": "Opus", "isDefault": true }] },
        ]))
        .unwrap();
        let pick = |model| resolve_model(&providers, "claude", model).map(|(s, exact)| (s.instance_id, s.model, exact));
        assert_eq!(pick(Some("opus")), Some(("c2".into(), "m2".into(), true)));
        assert_eq!(pick(Some("m1")), Some(("c2".into(), "m1".into(), true)));
        assert_eq!(pick(Some("gpt")), Some(("c2".into(), "m2".into(), false)));
        assert_eq!(pick(None), Some(("c2".into(), "m2".into(), true)));
        assert_eq!(resolve_model(&providers, "codex", None), None);
    }

    #[test]
    fn repositories_follow_github_name_rules() {
        assert_eq!(parse_repository("owner/name"), Some(repo("owner", "name")));
        assert_eq!(parse_repository("-owner/name"), None);
        assert_eq!(parse_repository("owner/.."), None);
        assert_eq!(parse_repository("owner/na me"), None);
        assert_eq!(parse_repository("https://github.com/o/n/tree/main"), None);
    }
}
