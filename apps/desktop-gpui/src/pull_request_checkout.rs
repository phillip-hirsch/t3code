//! Checking a change request out into a draft by its reference (`PullRequestThreadDialog`):
//! the reference a URL, a checkout command or a number names (`parsePullRequestReference`), the
//! host's words for it, and the dialog's resolve-then-prepare state.

use std::collections::HashMap;

use serde::Deserialize;
use serde_json::Value;

use crate::pull_request_detail::CheckoutMode;
use crate::rpc::RpcError;

/// A source control host's name and its words for a change request
/// (`getSourceControlPresentation`'s `providerName` and `terminology`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostTerms {
    pub label: &'static str,
    pub singular: &'static str,
    pub short: &'static str,
}

/// The definition `kind` names, or the unknown one (`sourceControlClients.get`).
pub fn host_terms(kind: Option<&str>) -> HostTerms {
    let (label, singular, short) = match kind.unwrap_or_default() {
        "github" => ("GitHub", "pull request", "PR"),
        "gitlab" => ("GitLab", "merge request", "MR"),
        "bitbucket" => ("Bitbucket", "pull request", "PR"),
        "azure-devops" => ("Azure DevOps", "pull request", "PR"),
        "forgejo" => ("Forgejo", "pull request", "PR"),
        "gitcafe" => ("GitCafe", "pull request", "PR"),
        "cursor-origin" => ("Cursor Origin", "pull request", "PR"),
        _ => ("source control", "change request", "change request"),
    };
    HostTerms { label, singular, short }
}

/// CSS `capitalize`: each word's first letter upper case.
pub fn capitalize_words(text: &str) -> String {
    text.split(' ')
        .map(|word| {
            let mut chars = word.chars();
            chars.next().map_or_else(String::new, |first| first.to_uppercase().chain(chars).collect())
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// `words` (any case), each followed by whitespace, then the rest of `input` on one line
/// (`/^gh\s+pr\s+checkout\s+(.+)$/i`).
fn strip_command<'a>(input: &'a str, words: &[&str]) -> Option<&'a str> {
    let mut rest = input;
    for word in words {
        if !rest.get(..word.len())?.eq_ignore_ascii_case(word) {
            return None;
        }
        let after = &rest[word.len()..];
        let spaced = after.trim_start();
        if spaced.len() == after.len() {
            return None;
        }
        rest = spaced;
    }
    let one_line = !rest.contains(['\n', '\r', '\u{2028}', '\u{2029}']);
    (!rest.is_empty() && one_line).then_some(rest)
}

/// `checkoutCommandId`: `--id 12`, `-i 12`, `--id=12`, or the first argument that is no flag.
fn azure_checkout_id(args: &str) -> Option<&str> {
    let parts: Vec<&str> = args.split_whitespace().collect();
    for (index, part) in parts.iter().enumerate() {
        if matches!(*part, "--id" | "-i") {
            return parts.get(index + 1).copied();
        }
        if let Some(id) = part.strip_prefix("--id=") {
            return (!id.is_empty()).then_some(id);
        }
    }
    parts.into_iter().find(|part| !part.starts_with('-'))
}

/// Each host's `checkoutCommandArgument`: what its CLI's checkout command names.
fn checkout_command_argument(input: &str) -> Option<&str> {
    const COMMANDS: &[&[&str]] = &[
        &["gh", "pr", "checkout"],
        &["glab", "mr", "checkout"],
        &["tea", "pr", "checkout"],
        &["tea", "pulls", "checkout"],
        &["origin", "pr", "checkout"],
    ];
    COMMANDS
        .iter()
        .find_map(|words| strip_command(input, words).map(str::trim))
        .or_else(|| strip_command(input, &["az", "repos", "pr", "checkout"]).and_then(azure_checkout_id))
}

/// Cursor Origin's `https://cursor.com/codebase/{owner}/{repo}/pull/{n}`.
fn is_cursor_origin_reference(reference: &str) -> bool {
    let Ok(url) = url::Url::parse(reference) else { return false };
    let host = url.host_str().unwrap_or_default().to_lowercase();
    if !matches!(url.scheme(), "http" | "https") || !matches!(host.as_str(), "cursor.com" | "www.cursor.com") {
        return false;
    }
    let segments: Vec<&str> = url.path().trim_start_matches('/').split('/').collect();
    matches!(segments.as_slice(), ["codebase", owner, repo, "pull", number, ..]
        if !owner.is_empty() && !repo.is_empty() && !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()))
}

/// `parsePullRequestReference`: a change request URL as given, a checkout command's argument,
/// or the digits of `123` / `#123`; `None` for anything else.
pub fn parse_reference(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    let normalized = checkout_command_argument(trimmed).unwrap_or(trimmed);
    if normalized.is_empty() {
        return None;
    }
    if crate::pull_request_detail::parse_change_request(normalized).is_some() || is_cursor_origin_reference(normalized)
    {
        return Some(normalized.to_owned());
    }
    let digits = normalized.strip_prefix('#').unwrap_or(normalized);
    (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())).then(|| digits.to_owned())
}

/// `GitResolvedPullRequest`.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedPullRequest {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub base_branch: String,
    pub head_branch: String,
    pub state: String,
}

impl ResolvedPullRequest {
    /// A `git.resolvePullRequest` answer.
    pub fn of_answer(value: &Value) -> Option<Self> {
        Self::deserialize(&value["pullRequest"]).ok()
    }
}

/// A `git.preparePullRequestThread` answer: where the checkout is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prepared {
    pub branch: String,
    pub worktree_path: Option<String>,
    /// False when local commits or edits kept a reused worktree off the change request's head.
    pub on_head: bool,
}

impl Prepared {
    pub fn of_answer(value: &Value) -> Option<Self> {
        let branch = value["branch"].as_str().filter(|b| !b.trim().is_empty())?.to_owned();
        Some(Self {
            branch,
            worktree_path: value["worktreePath"].as_str().map(str::to_owned),
            on_head: value["isOnPullRequestHead"].as_bool() != Some(false),
        })
    }
}

/// The `message` of the git workflow's errors (`GitManagerServiceError`), else the error's own
/// `message` or `detail`; `None` when it carries neither.
pub fn git_error_message(error: &RpcError) -> Option<String> {
    let RpcError::Failure(value) = error else { return Some(error.to_string()) };
    let field = |key: &str| value.get(key).and_then(Value::as_str).unwrap_or_default();
    let message = match value.get("_tag").and_then(Value::as_str) {
        Some("GitCommandError") => {
            let reason = value.get("reason").and_then(Value::as_str).map_or_else(String::new, |r| format!(" ({r})"));
            format!("Git command failed in {} ({}): {}{reason}", field("operation"), field("cwd"), field("detail"))
        }
        Some("GitManagerError") => format!("Git manager failed in {}: {}", field("operation"), field("detail")),
        Some("GitPullRequestMaterializationError") => format!(
            "Failed to materialize pull request #{} branch {} as {}.",
            value.get("pullRequestNumber").and_then(Value::as_u64).unwrap_or_default(),
            field("headBranch"),
            field("localBranch")
        ),
        Some("SourceControlProviderError") => format!(
            "Source control provider {} failed in {}: {}",
            field("provider"),
            field("operation"),
            field("detail")
        ),
        Some("TextGenerationError") => format!("Text generation failed in {}: {}", field("operation"), field("detail")),
        _ => return [field("message"), field("detail")].into_iter().find(|m| !m.is_empty()).map(str::to_owned),
    };
    Some(message)
}

/// What a checkout may do on the draft's environment: in place with `source-control:write`
/// (`isAllowed`), in a worktree with `orchestration:operate` too (`canOperateThread`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Access {
    pub write: bool,
    pub operate: bool,
}

impl Access {
    pub fn allows(self, mode: CheckoutMode) -> bool {
        match mode {
            CheckoutMode::Local => self.write,
            CheckoutMode::Worktree => self.write && self.operate,
        }
    }
}

/// The dialog's state: the typed reference, the one its debounce settled on, the host's
/// answers by reference, and the checkout in flight.
#[derive(Debug, Default)]
pub struct CheckoutState {
    reference: String,
    settled: String,
    dirty: bool,
    resolved: HashMap<String, ResolvedPullRequest>,
    /// The settled reference being read.
    reading: Option<String>,
    /// The last failed read, by reference.
    failure: Option<(String, String)>,
    preparing: Option<CheckoutMode>,
    prepare_error: Option<String>,
}

impl CheckoutState {
    /// The dialog opens on `initial`, already settled, so it reads at once.
    pub fn new(initial: Option<&str>) -> Self {
        let reference = initial.unwrap_or_default().to_owned();
        Self { settled: reference.clone(), reference, ..Self::default() }
    }

    /// The input changed; the debounce settles it later.
    pub fn edit(&mut self, value: String) {
        self.dirty = true;
        self.reference = value;
    }

    /// The debounce settled on the current input: the reference to read, unless it is known or
    /// already being read.
    pub fn settle(&mut self) -> Option<String> {
        self.settled.clone_from(&self.reference);
        let reference = parse_reference(&self.settled)?;
        if self.resolved.contains_key(&reference) || self.reading.as_ref() == Some(&reference) {
            return None;
        }
        self.reading = Some(reference.clone());
        self.failure = None;
        Some(reference)
    }

    /// The host answered for `reference`.
    pub fn landed(&mut self, reference: String, answer: Result<ResolvedPullRequest, String>) {
        if self.reading.as_ref() == Some(&reference) {
            self.reading = None;
        }
        match answer {
            Ok(pull_request) => {
                self.resolved.insert(reference, pull_request);
            }
            Err(message) => self.failure = Some((reference, message)),
        }
    }

    pub fn parsed(&self) -> Option<String> {
        parse_reference(&self.reference)
    }

    /// The change request the input names, once read (`resolvedPullRequest`).
    pub fn resolved(&self) -> Option<&ResolvedPullRequest> {
        self.resolved.get(&self.parsed()?)
    }

    /// `isResolving`: a reference without its answer, still settling or being read.
    pub fn is_resolving(&self) -> bool {
        let Some(parsed) = self.parsed() else { return false };
        let settled = parse_reference(&self.settled);
        self.resolved().is_none()
            && (self.reference != self.settled
                || settled.as_ref() != Some(&parsed)
                || self.reading.as_ref() == Some(&parsed))
    }

    pub fn preparing(&self) -> Option<CheckoutMode> {
        self.preparing
    }

    /// The message under the input: what the input lacks once edited, the checkout's failure,
    /// else the settled reference's failed read while nothing is resolved.
    pub fn error(&self, singular: &str) -> Option<String> {
        if self.dirty {
            if self.reference.trim().is_empty() {
                return Some(format!("Paste a {singular} URL, checkout command, or enter 123 / #123."));
            }
            if self.parsed().is_none() {
                return Some(format!("Use a {singular} URL, checkout command, 123, or #123."));
            }
        }
        if let Some(error) = &self.prepare_error {
            return Some(error.clone());
        }
        let settled = parse_reference(&self.settled);
        let failure = self.failure.as_ref().filter(|(reference, _)| Some(reference) == settled.as_ref());
        failure.filter(|_| self.resolved().is_none()).map(|(_, message)| message.clone())
    }

    /// Whether `mode`'s button can be pressed.
    pub fn can_confirm(&self, mode: CheckoutMode, access: Access) -> bool {
        access.allows(mode) && self.resolved().is_some() && !self.is_resolving() && self.preparing.is_none()
    }

    /// `handleConfirm`: the reference to check out in `mode`; an unparsable input only shows
    /// why.
    pub fn start(&mut self, mode: CheckoutMode, access: Access) -> Option<String> {
        if !access.allows(mode) || self.preparing.is_some() {
            return None;
        }
        let Some(reference) = self.parsed() else {
            self.dirty = true;
            return None;
        };
        self.resolved()?;
        self.prepare_error = None;
        self.preparing = Some(mode);
        Some(reference)
    }

    /// The checkout ended; `error` says why it failed.
    pub fn finished(&mut self, error: Option<String>) {
        self.preparing = None;
        self.prepare_error = error;
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_reference_is_a_url_a_checkout_command_or_a_number() {
        let parse = parse_reference;
        let url = "https://github.com/acme/app/pull/42";
        assert_eq!(parse(&format!("  {url} ")).as_deref(), Some(url));
        assert_eq!(
            parse("https://gitlab.com/g/p/-/merge_requests/7").as_deref(),
            Some("https://gitlab.com/g/p/-/merge_requests/7")
        );
        assert_eq!(parse("#42").as_deref(), Some("42"));
        assert_eq!(parse("42").as_deref(), Some("42"));
        assert_eq!(parse("gh pr checkout 42").as_deref(), Some("42"));
        assert_eq!(parse("GH  PR checkout   #42").as_deref(), Some("42"), "the command is any case and spacing");
        assert_eq!(parse(&format!("gh pr checkout {url}")).as_deref(), Some(url));
        assert_eq!(parse("glab mr checkout 7").as_deref(), Some("7"));
        assert_eq!(parse("tea pulls checkout 3").as_deref(), Some("3"));
        assert_eq!(parse("tea pr checkout 3").as_deref(), Some("3"));
        assert_eq!(parse("origin pr checkout 5").as_deref(), Some("5"));
        assert_eq!(parse("az repos pr checkout --id 9").as_deref(), Some("9"));
        assert_eq!(parse("az repos pr checkout --id=9").as_deref(), Some("9"));
        assert_eq!(parse("az repos pr checkout --remote origin 9"), None, "the first non-flag is the id");
        assert_eq!(parse("az repos pr checkout 9 --remote x").as_deref(), Some("9"));
        assert_eq!(
            parse("https://cursor.com/codebase/acme/app/pull/4").as_deref(),
            Some("https://cursor.com/codebase/acme/app/pull/4")
        );
        for input in
            ["", "   ", "gh pr checkout", "ghpr checkout 1", "feature/branch", "12a", "#", "gh pr checkout 1 --force"]
        {
            assert_eq!(parse(input), None, "{input:?}");
        }
        assert_eq!(parse("gh pr checkout 1\n2"), None, "a command stays on one line");
    }

    #[test]
    fn git_errors_read_as_their_messages() {
        let failure = |value: Value| git_error_message(&RpcError::Failure(value));
        assert_eq!(
            failure(json!({"_tag": "GitCommandError", "operation": "checkout", "cwd": "/r", "detail": "no ref", "reason": "ref_not_found"})).as_deref(),
            Some("Git command failed in checkout (/r): no ref (ref_not_found)")
        );
        assert_eq!(
            failure(json!({"_tag": "GitManagerError", "operation": "resolve", "cwd": "/r", "detail": "gh missing"}))
                .as_deref(),
            Some("Git manager failed in resolve: gh missing")
        );
        assert_eq!(
            failure(json!({"_tag": "GitPullRequestMaterializationError", "pullRequestNumber": 4, "headBranch": "f", "localBranch": "pr-4"})).as_deref(),
            Some("Failed to materialize pull request #4 branch f as pr-4.")
        );
        assert_eq!(
            failure(
                json!({"_tag": "EnvironmentAuthorizationError", "message": "This connection cannot change threads."})
            )
            .as_deref(),
            Some("This connection cannot change threads.")
        );
        assert_eq!(failure(json!({"_tag": "Other"})), None);
    }

    fn pull_request(number: u64) -> ResolvedPullRequest {
        ResolvedPullRequest::of_answer(&json!({"pullRequest": {
            "number": number, "title": "Add", "url": format!("https://github.com/a/b/pull/{number}"),
            "baseBranch": "main", "headBranch": "feature", "state": "open",
        }}))
        .expect("a resolved pull request")
    }

    const ALL: Access = Access { write: true, operate: true };

    #[test]
    fn a_reference_resolves_once_settled_and_its_answer_is_kept() {
        let mut state = CheckoutState::new(None);
        assert!(!state.is_resolving() && state.error("pull request").is_none(), "nothing typed yet says nothing");
        state.edit("#12".into());
        assert!(state.is_resolving(), "settling");
        assert_eq!(state.settle().as_deref(), Some("12"));
        assert_eq!(state.settle(), None, "already being read");
        assert!(state.is_resolving() && !state.can_confirm(CheckoutMode::Worktree, ALL));
        state.landed("12".into(), Ok(pull_request(12)));
        assert!(!state.is_resolving());
        assert_eq!(state.resolved().map(|p| p.number), Some(12));
        assert!(state.can_confirm(CheckoutMode::Worktree, ALL));
        // Typing on: the next reference settles and reads; going back shows the kept answer at once.
        state.edit("13".into());
        assert!(state.resolved().is_none() && state.is_resolving());
        state.edit("12".into());
        assert_eq!(state.resolved().map(|p| p.number), Some(12));
        assert!(!state.is_resolving());
        assert_eq!(state.settle(), None, "a known reference is not read again");
    }

    #[test]
    fn a_late_answer_for_an_older_reference_does_not_end_the_current_read() {
        let mut state = CheckoutState::new(Some("12"));
        assert_eq!(state.settle().as_deref(), Some("12"), "an opening reference reads at once");
        state.edit("13".into());
        assert_eq!(state.settle().as_deref(), Some("13"));
        state.landed("12".into(), Err("gone".into()));
        assert!(state.is_resolving(), "13 is still being read");
        assert_eq!(state.error("pull request"), None, "12's failure is not 13's");
        state.landed("13".into(), Err("Git manager failed in resolve: not found".into()));
        assert!(!state.is_resolving());
        assert_eq!(state.error("pull request").as_deref(), Some("Git manager failed in resolve: not found"));
    }

    #[test]
    fn the_input_says_what_it_lacks_once_edited() {
        let mut state = CheckoutState::new(None);
        state.edit("  ".into());
        assert_eq!(
            state.error("merge request").as_deref(),
            Some("Paste a merge request URL, checkout command, or enter 123 / #123.")
        );
        state.edit("main".into());
        assert_eq!(
            state.error("pull request").as_deref(),
            Some("Use a pull request URL, checkout command, 123, or #123.")
        );
        assert!(!state.is_resolving(), "nothing to resolve");
        let mut untouched = CheckoutState::new(None);
        assert_eq!(untouched.start(CheckoutMode::Worktree, ALL), None);
        assert!(untouched.error("pull request").is_some(), "Enter on an empty input shows why");
    }

    #[test]
    fn a_checkout_needs_its_grants_and_runs_once() {
        let mut state = CheckoutState::new(Some("12"));
        state.settle();
        state.landed("12".into(), Ok(pull_request(12)));
        let write_only = Access { write: true, operate: false };
        assert!(state.can_confirm(CheckoutMode::Local, write_only));
        assert!(!state.can_confirm(CheckoutMode::Worktree, write_only), "a worktree also needs operate");
        assert_eq!(state.start(CheckoutMode::Worktree, write_only), None);
        assert!(!state.can_confirm(CheckoutMode::Local, Access::default()));
        assert_eq!(state.start(CheckoutMode::Local, write_only).as_deref(), Some("12"));
        assert_eq!(state.preparing(), Some(CheckoutMode::Local));
        assert!(!state.can_confirm(CheckoutMode::Local, write_only), "pending");
        assert_eq!(state.start(CheckoutMode::Local, write_only), None, "one at a time");
        state.finished(Some("Git manager failed in prepare: dirty".into()));
        assert_eq!(state.preparing(), None);
        assert_eq!(state.error("pull request").as_deref(), Some("Git manager failed in prepare: dirty"));
        assert_eq!(state.start(CheckoutMode::Local, write_only).as_deref(), Some("12"), "it can try again");
        assert_eq!(state.error("pull request"), None, "a retry clears the failure");
    }

    #[test]
    fn a_prepared_checkout_says_where_it_is() {
        let prepared = Prepared::of_answer(&json!({"branch": "feature", "worktreePath": "/w", "pullRequest": {}}));
        assert_eq!(
            prepared,
            Some(Prepared { branch: "feature".into(), worktree_path: Some("/w".into()), on_head: true })
        );
        let off =
            Prepared::of_answer(&json!({"branch": "feature", "worktreePath": null, "isOnPullRequestHead": false}));
        assert_eq!(off.map(|p| (p.worktree_path, p.on_head)), Some((None, false)));
        assert_eq!(capitalize_words("merge request"), "Merge Request");
        assert_eq!(host_terms(Some("gitlab")).short, "MR");
        assert_eq!(host_terms(None).singular, "change request");
    }
}
