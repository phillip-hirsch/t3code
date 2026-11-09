//! The Welcome page's rules (`apps/web/src/onboarding/*.logic.ts`): when a launch opens the
//! page, which agents are ready, the commands their setup terminal types, and which projects
//! from agent history it offers and picks. Providers and settings stay the wire's JSON.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::project_groups::normalize_path;

/// `RECENT_PROJECT_WINDOW_MS`.
const RECENT_WINDOW_MS: i64 = 30 * 24 * 60 * 60 * 1000;
/// `DEFAULT_SELECTION_MIN_THREADS`.
const DEFAULT_SELECTION_MIN_THREADS: u64 = 3;
/// `AgentSessionSource`, in its order, with the names the page shows.
pub const IMPORT_SOURCES: [(&str, &str); 3] = [("claudeAgent", "Claude Code"), ("codex", "Codex"), ("pi", "Pi")];
/// `IMPORT_SOURCES_LABEL`.
pub const IMPORT_SOURCES_LABEL: &str = "Claude Code, Codex, and Pi";

// ---------------------------------------------------------------------------
// First run (`firstRun.logic.ts`)

/// What a launch shows once the primary workspace is known (`resolveFirstRunDecision`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FirstRun {
    /// The app; `persist` stores the completion so a later launch skips the check.
    App {
        persist: bool,
    },
    Welcome,
}

/// Setup shows only on a fresh install: never completed, no linked computers, and an empty
/// workspace. This client's own server never creates a cwd bootstrap project, so any project or
/// thread is the user's. A non-empty workspace counts as setup done.
pub fn first_run(completed: bool, linked_environments: bool, projects: usize, threads: usize) -> FirstRun {
    if completed {
        FirstRun::App { persist: false }
    } else if linked_environments || projects > 0 || threads > 0 {
        FirstRun::App { persist: true }
    } else {
        FirstRun::Welcome
    }
}

// ---------------------------------------------------------------------------
// Agents (`welcomePage.logic.ts`, `providerReadiness.logic.ts`)

/// An agent's state for new threads on a computer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentState {
    Ready,
    SignIn,
    Attention,
    Disabled,
    Install,
    Checking,
}

impl AgentState {
    /// `FOUND_ORDER`: ready agents first, then ones that need a sign-in or a look.
    fn found_rank(self) -> Option<u8> {
        match self {
            Self::Ready => Some(0),
            Self::SignIn => Some(1),
            Self::Attention => Some(2),
            Self::Disabled | Self::Install | Self::Checking => None,
        }
    }
}

fn text<'a>(value: &'a Value, pointer: &str) -> Option<&'a str> {
    value.pointer(pointer).and_then(Value::as_str)
}

fn driver(provider: &Value) -> &str {
    text(provider, "/driver").unwrap_or_default()
}

pub fn instance_id(provider: &Value) -> &str {
    text(provider, "/instanceId").unwrap_or_default()
}

/// `isProviderAvailable`.
pub fn is_available(provider: &Value) -> bool {
    text(provider, "/availability") != Some("unavailable")
}

/// `getOnboardingProviderState`.
pub fn onboarding_state(provider: &Value) -> AgentState {
    let status = text(provider, "/status").unwrap_or_default();
    let installed = provider["installed"] == true;
    let auth = text(provider, "/auth/status").unwrap_or_default();
    if provider["enabled"] != true || status == "disabled" {
        AgentState::Disabled
    } else if !installed && status == "warning" && auth == "unknown" {
        AgentState::Checking
    } else if !installed {
        AgentState::Install
    } else if auth == "unauthenticated" {
        AgentState::SignIn
    } else if status == "ready" {
        AgentState::Ready
    } else {
        AgentState::Attention
    }
}

/// `isBrokenVersion`: a CLI the server knows is too old or otherwise broken; Settings updates it.
pub fn is_broken_version(provider: &Value) -> bool {
    provider["installed"] == true && text(provider, "/compatibilityAdvisory/status") == Some("broken")
}

/// `welcomeAgentState`. The server also lists agents only some projects turn on, lists an agent
/// it could not load as off, which Turn on cannot fix, and can pass a CLI it knows is broken.
pub fn agent_state(provider: &Value, settings: Option<&crate::settings::Settings>) -> AgentState {
    if !is_available(provider) {
        return AgentState::Attention;
    }
    if settings.is_some_and(|settings| {
        !crate::settings::scope::provider_instance_enabled(settings, None, instance_id(provider))
    }) {
        return AgentState::Disabled;
    }
    match onboarding_state(provider) {
        AgentState::Ready if is_broken_version(provider) => AgentState::Attention,
        // An ACP agent that is not prepared reports the same fields as one still being checked.
        AgentState::Checking if driver(provider) == "acpRegistry" => AgentState::Attention,
        state => state,
    }
}

/// A computer's allowed agents split into the ones on it and the ones the user can add.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentGroups {
    /// Some agent is still being checked, or the list has not arrived.
    pub checking: bool,
    pub ready_count: usize,
    /// Indices into the provider list: ready, then needing a sign-in, then a look; by name.
    pub found: Vec<usize>,
    /// Every other agent, by name. Agents still being checked are here, so a slow check never
    /// drops one.
    pub available: Vec<usize>,
}

/// `groupWelcomeAgents`.
pub fn group_agents(
    providers: Option<&[Value]>,
    settings: Option<&crate::settings::Settings>,
    allowed: impl Fn(&Value) -> bool,
) -> AgentGroups {
    let mut groups = AgentGroups { checking: providers.is_none(), ..AgentGroups::default() };
    let providers = providers.unwrap_or_default();
    let mut found = Vec::new();
    for (index, provider) in providers.iter().enumerate() {
        if !allowed(provider) {
            continue;
        }
        let state = agent_state(provider, settings);
        groups.ready_count += usize::from(state == AgentState::Ready);
        groups.checking |= state == AgentState::Checking;
        match state.found_rank() {
            Some(rank) => found.push((rank, index)),
            None => groups.available.push(index),
        }
    }
    let by_name = |a: usize, b: usize| compare_names(&agent_name(&providers[a]), &agent_name(&providers[b]));
    found.sort_by(|(rank_a, a), (rank_b, b)| rank_a.cmp(rank_b).then_with(|| by_name(*a, *b)));
    groups.available.sort_by(|a, b| by_name(*a, *b));
    groups.found = found.into_iter().map(|(_, index)| index).collect();
    groups
}

/// `localeCompare` for names: case folded first, so "codex" sits beside "Codex".
fn compare_names(a: &str, b: &str) -> Ordering {
    a.to_lowercase().cmp(&b.to_lowercase()).then_with(|| a.cmp(b))
}

/// `isManagedDriverAllowed`.
pub fn driver_allowed(policy: &Value, driver: &str) -> bool {
    policy
        .pointer("/limits/providers/allowedDrivers")
        .and_then(Value::as_array)
        .is_none_or(|allowed| allowed.iter().any(|d| d.as_str() == Some(driver)))
}

/// `PROVIDER_DISPLAY_NAMES`.
fn driver_display_name(driver: &str) -> Option<&'static str> {
    Some(match driver {
        "antigravity" => "Antigravity",
        "codex" => "Codex",
        "claudeAgent" => "Claude",
        "cursor" => "Cursor",
        "grok" => "Grok",
        "fx" => "fx",
        "kiro" => "Kiro",
        "muse" => "Muse Code",
        "acpRegistry" => "ACP Registry",
        "pi" => "Pi",
        "opencode" => "OpenCode",
        _ => return None,
    })
}

/// `humanizeSlug`: `claude_work` reads "Claude Work", `myAgent` "My Agent".
fn humanize_slug(slug: &str) -> String {
    let mut spaced = String::with_capacity(slug.len() + 4);
    let mut previous: Option<char> = None;
    for c in slug.chars() {
        if c.is_ascii_uppercase() && previous.is_some_and(|p| p.is_ascii_lowercase()) {
            spaced.push(' ');
        }
        if c == '_' || c == '-' {
            if !spaced.ends_with(' ') {
                spaced.push(' ');
            }
        } else {
            spaced.push(c);
        }
        previous = Some(c);
    }
    let mut out = String::with_capacity(spaced.len());
    let mut at_word_start = true;
    for c in spaced.trim().chars() {
        let word = c.is_alphanumeric() || c == '_';
        if word && at_word_start {
            out.extend(c.to_uppercase());
        } else {
            out.push(c);
        }
        at_word_start = !word;
    }
    out
}

/// `resolveProviderInstanceDisplayName`.
pub fn agent_name(provider: &Value) -> String {
    let driver = driver(provider);
    let kind_label = driver_display_name(driver).map_or_else(|| humanize_slug(driver), str::to_owned);
    let own = text(provider, "/displayName").map(str::trim).filter(|name| !name.is_empty());
    if let Some(name) = own.filter(|name| *name != kind_label) {
        return name.to_owned();
    }
    let instance = instance_id(provider);
    if instance != driver {
        let humanized = humanize_slug(instance);
        if !humanized.is_empty() {
            return humanized;
        }
    }
    own.map_or(kind_label, str::to_owned)
}

/// `turnedOnInstance`: older settings can also keep `enabled: false` inside `config`.
pub fn turned_on_instance(instance: &Value) -> Value {
    let mut next = instance.clone();
    if !next.is_object() {
        next = Value::Object(serde_json::Map::new());
    }
    next["enabled"] = true.into();
    if let Some(config) = next.get_mut("config").and_then(Value::as_object_mut)
        && config.get("enabled") == Some(&Value::Bool(false))
    {
        config.insert("enabled".into(), true.into());
    }
    next
}

/// The drivers whose row installs or signs in through the setup terminal.
pub fn is_installable(driver: &str) -> bool {
    matches!(driver, "codex" | "claudeAgent")
}

/// What "Use existing CLI" does next for Codex (`cliTerminalStep`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CliStep {
    /// Wait for the CLI's own check.
    Wait,
    /// Open its install or sign-in terminal.
    Open,
    Done,
}

/// `cliTerminalStep`. Only the ChatGPT sign-in runtime reports `setup.canAuthenticate`.
pub fn cli_terminal_step(provider: &Value, settings: Option<&crate::settings::Settings>) -> CliStep {
    if provider.pointer("/setup/canAuthenticate") == Some(&Value::Bool(true)) {
        return CliStep::Wait;
    }
    let state = agent_state(provider, settings);
    if state == AgentState::Checking {
        return CliStep::Wait;
    }
    // A broken version is updated from its row, and an installed, signed-in CLI needs no terminal.
    if is_broken_version(provider)
        || (provider["installed"] == true && text(provider, "/auth/status") == Some("authenticated"))
    {
        return CliStep::Done;
    }
    // The terminal shows the command first, so it also helps a CLI whose check failed.
    let fixable = matches!(state, AgentState::Install | AgentState::SignIn)
        || (state == AgentState::Attention && is_available(provider));
    if fixable { CliStep::Open } else { CliStep::Done }
}

/// `codexNeedsSetup`: whether Codex's row opens its setup card, which offers ChatGPT sign-in or
/// the CLI. A signed-in or broken Codex gets its plain row action instead.
pub fn codex_needs_setup(provider: &Value, settings: Option<&crate::settings::Settings>) -> bool {
    if driver(provider) != "codex"
        || text(provider, "/auth/status") == Some("authenticated")
        || is_broken_version(provider)
    {
        return false;
    }
    match agent_state(provider, settings) {
        AgentState::Install | AgentState::SignIn => true,
        AgentState::Attention => is_available(provider),
        _ => false,
    }
}

/// The row action an install or sign-in opens the terminal for (`needsInstallOrSignIn`).
pub fn needs_install_or_sign_in(provider: &Value, settings: Option<&crate::settings::Settings>) -> bool {
    is_installable(driver(provider))
        && !is_broken_version(provider)
        && matches!(agent_state(provider, settings), AgentState::Install | AgentState::SignIn)
}

/// `NATIVE_INSTALL_COMMANDS`: installers that need no Node or npm and put the binary where
/// provider maintenance recognizes it.
pub fn install_command(driver: &str, platform_os: &str) -> Option<&'static str> {
    let windows = platform_os == "windows";
    Some(match (driver, windows) {
        ("claudeAgent", true) => "irm https://claude.ai/install.ps1 | iex",
        ("claudeAgent", false) => "curl -fsSL https://claude.ai/install.sh | bash",
        ("codex", true) => "irm https://chatgpt.com/codex/install.ps1 | iex",
        ("codex", false) => "curl -fsSL https://chatgpt.com/codex/install.sh | sh",
        _ => return None,
    })
}

/// `SAFE_SHELL_BINARY_PATTERN`.
fn is_safe_binary(path: &str) -> bool {
    !path.is_empty() && path.chars().all(|c| c.is_ascii_alphanumeric() || "_./:\\-".contains(c))
}

/// `quoteProviderBinary`: the binary as the platform's shell reads it.
fn quote_binary(path: &str, fallback: &str, platform_os: &str) -> String {
    if is_safe_binary(path) && (platform_os == "windows" || !path.contains('\\')) {
        return path.to_owned();
    }
    let posix_quote = |text: &str| format!("'{}'", text.replace('\'', r#"'"'"'"#));
    match platform_os {
        "windows" => format!("& '{}'", path.replace('\'', "''")),
        "darwin" | "linux" => match path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
            Some(rest) => format!("~/{}", posix_quote(rest)),
            None => posix_quote(path),
        },
        _ => fallback.to_owned(),
    }
}

/// `resolveOnboardingProviderLoginCommand`: the instance's binary, then its login subcommand.
pub fn login_command(provider: &Value, server_settings: &Value, platform_os: &str) -> String {
    let instance = &server_settings["providerInstances"][instance_id(provider)];
    let binary = |fallback: &str| {
        let path = text(instance, "/config/binaryPath").map(str::trim).filter(|path| !path.is_empty());
        quote_binary(path.unwrap_or(fallback), fallback, platform_os)
    };
    match driver(provider) {
        "claudeAgent" => format!("{} auth login", binary("claude")),
        "codex" => format!("{} login", binary("codex")),
        other => other.to_owned(),
    }
}

// ---------------------------------------------------------------------------
// Projects (`projectImport.logic.ts`, `welcomeState.ts`)

/// Whether a candidate is a git repository: unknown on servers that predate git scans, which
/// count as repositories.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Git {
    NotARepository,
    Repository { remote_key: Option<String>, repository: Option<String> },
}

/// `AgentSessionProjectCandidate` on one environment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub key: String,
    pub environment: String,
    pub path: String,
    pub title: String,
    /// The server's match to an existing project, which the client snapshot can lag behind.
    pub project_id: Option<String>,
    pub sources: Vec<String>,
    pub thread_count: u64,
    pub last_active_at: Option<String>,
    pub git: Git,
}

/// `onboardingProjectKey`.
pub fn project_key(environment: &str, path: &str) -> String {
    serde_json::json!([environment, path]).to_string()
}

/// One environment's `AgentSessionScanResult`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Scan {
    pub candidates: Vec<Candidate>,
    pub truncated: bool,
}

impl Scan {
    /// Reads a scan reply; malformed candidates are skipped.
    pub fn parse(environment: &str, reply: &Value) -> Self {
        let candidates = reply["candidates"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|candidate| {
                let path = text(candidate, "/path")?.to_owned();
                let git = match candidate.get("git") {
                    Some(Value::Null) => Git::NotARepository,
                    git => Git::Repository {
                        remote_key: git.and_then(|g| text(g, "/remoteKey")).map(str::to_owned),
                        repository: git.and_then(|g| text(g, "/repository")).map(str::to_owned),
                    },
                };
                Some(Candidate {
                    key: project_key(environment, &path),
                    environment: environment.to_owned(),
                    title: text(candidate, "/title").unwrap_or(&path).to_owned(),
                    project_id: text(candidate, "/projectId").map(str::to_owned),
                    sources: candidate["sources"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect(),
                    thread_count: candidate["threadCount"].as_u64().unwrap_or(0),
                    last_active_at: text(candidate, "/lastActiveAt").map(str::to_owned),
                    path,
                    git,
                })
            })
            .collect();
        Self { candidates, truncated: reply["truncated"] == true }
    }
}

/// `partitionOnboardingProjects`'s `recent`: git repositories active in the last 30 days with at
/// least three conversations.
pub fn is_recent(candidate: &Candidate, now_ms: i64) -> bool {
    candidate.git != Git::NotARepository
        && candidate.thread_count >= DEFAULT_SELECTION_MIN_THREADS
        && candidate
            .last_active_at
            .as_deref()
            .and_then(crate::timestamp::parse_ms)
            .is_some_and(|at| at >= now_ms - RECENT_WINDOW_MS && at <= now_ms)
}

/// `isNewOnboardingProject`: not a project on its environment yet, by the server's match or a
/// listed project with the same root.
pub struct ProjectRoots(HashSet<(String, String)>);

impl ProjectRoots {
    pub fn new<'a>(projects: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
        Self(projects.into_iter().map(|(env, root)| (env.to_owned(), normalize_path(root))).collect())
    }

    pub fn is_new(&self, candidate: &Candidate) -> bool {
        if candidate.project_id.is_some() {
            return false;
        }
        let path = normalize_path(&candidate.path);
        path.is_empty() || !self.0.contains(&(candidate.environment.clone(), path))
    }
}

/// `WelcomeImportSelection`: a computer's project choices, stored until setup finishes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportSelection {
    /// Projects listed when the selection changed; projects found later get the defaults.
    pub known_keys: Vec<String>,
    pub keys: Vec<String>,
    /// Unchecking the main box also leaves out projects found later, until it is checked again.
    pub opted_out: bool,
}

/// The checked candidates (`useProjectImport`'s `selected`): the stored picks among projects
/// listed when they were made, and the recent new projects among the rest.
pub fn selected<'a>(
    candidates: &'a [Candidate],
    recent: &HashSet<&str>,
    selection: Option<&ImportSelection>,
) -> Vec<&'a Candidate> {
    let Some(selection) = selection else {
        return candidates.iter().filter(|c| recent.contains(c.key.as_str())).collect();
    };
    if selection.opted_out {
        return Vec::new();
    }
    let known: HashSet<&str> = selection.known_keys.iter().map(String::as_str).collect();
    let picked: HashSet<&str> = selection.keys.iter().map(String::as_str).collect();
    candidates
        .iter()
        .filter(|c| {
            if known.contains(c.key.as_str()) {
                picked.contains(c.key.as_str())
            } else {
                recent.contains(c.key.as_str())
            }
        })
        .collect()
}

/// What checking the main box picks: recent new projects, else any new ones, else everything.
pub fn default_keys(candidates: &[Candidate], recent: &[&Candidate], roots: &ProjectRoots) -> Vec<String> {
    if !recent.is_empty() {
        return recent.iter().map(|c| c.key.clone()).collect();
    }
    let fresh: Vec<String> = candidates.iter().filter(|c| roots.is_new(c)).map(|c| c.key.clone()).collect();
    if fresh.is_empty() { candidates.iter().map(|c| c.key.clone()).collect() } else { fresh }
}

/// `OnboardingProjectGroup`: clones of one remote, or one repository without a remote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectGroup<'a> {
    pub key: String,
    pub label: String,
    pub candidates: Vec<&'a Candidate>,
    pub thread_count: u64,
    pub last_active_at: Option<String>,
}

/// `groupOnboardingProjects`: repositories newest first (then by label), and the folders that
/// are not git repositories.
pub fn group_projects(candidates: &[Candidate]) -> (Vec<ProjectGroup<'_>>, Vec<&Candidate>) {
    let mut groups: Vec<ProjectGroup<'_>> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut other = Vec::new();
    for candidate in candidates {
        let Git::Repository { remote_key, repository } = &candidate.git else {
            other.push(candidate);
            continue;
        };
        let key = remote_key.as_ref().map_or_else(|| format!("path:{}", candidate.path), |r| format!("remote:{r}"));
        match index.get(&key) {
            Some(&at) => {
                let group = &mut groups[at];
                group.candidates.push(candidate);
                group.thread_count += candidate.thread_count;
                group.last_active_at = latest(group.last_active_at.take(), candidate.last_active_at.clone());
            }
            None => {
                index.insert(key.clone(), groups.len());
                groups.push(ProjectGroup {
                    key,
                    label: repository.clone().unwrap_or_else(|| candidate.title.clone()),
                    candidates: vec![candidate],
                    thread_count: candidate.thread_count,
                    last_active_at: candidate.last_active_at.clone(),
                });
            }
        }
    }
    groups.sort_by(|a, b| match (&a.last_active_at, &b.last_active_at) {
        (x, y) if x == y => compare_names(&a.label, &b.label),
        (None, _) => Ordering::Greater,
        (_, None) => Ordering::Less,
        (Some(x), Some(y)) => y.cmp(x),
    });
    (groups, other)
}

fn latest(left: Option<String>, right: Option<String>) -> Option<String> {
    match (left, right) {
        (Some(l), Some(r)) => Some(if l > r { l } else { r }),
        (l, r) => l.or(r),
    }
}

/// `splitOnboardingProjectPath`: the last segment and the parent it sits in, separator kept.
pub fn split_project_path(path: &str) -> (&str, &str) {
    let is_separator = |c: char| c == '/' || c == '\\';
    let bytes = path.as_bytes();
    let drive_root =
        bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && path[2..].chars().all(is_separator);
    if drive_root || is_unc_share_root(path) {
        return (path, "");
    }
    let trimmed = path.trim_end_matches(is_separator);
    if trimmed.is_empty() {
        return (path, "");
    }
    match trimmed.rfind(is_separator) {
        Some(at) => (&trimmed[at + 1..], &trimmed[..=at]),
        None => (trimmed, ""),
    }
}

/// `\\server\share`, with optional trailing separators.
fn is_unc_share_root(path: &str) -> bool {
    let is_separator = |c: char| c == '/' || c == '\\';
    let Some(rest) = path.strip_prefix(['/', '\\']).and_then(|p| p.strip_prefix(['/', '\\'])) else {
        return false;
    };
    let mut parts = rest.trim_end_matches(is_separator).split(is_separator);
    matches!((parts.next(), parts.next(), parts.next()), (Some(server), Some(share), None) if !server.is_empty() && !share.is_empty())
}

/// `resolveOnboardingLandingProject`: the first picked project that brought history, else the
/// first that finished.
pub fn landing_project<'a, T>(
    selection: &[String],
    with_history: &'a HashMap<String, T>,
    completed: &'a HashMap<String, T>,
) -> Option<&'a T> {
    selection
        .iter()
        .find_map(|key| with_history.get(key))
        .or_else(|| selection.iter().find_map(|key| completed.get(key)))
}

fn threads(count: u64) -> String {
    format!("{count} {}", if count == 1 { "thread" } else { "threads" })
}

/// The warning after an import that left some projects unfinished.
pub fn import_warning(imported: u64, skipped: u64) -> String {
    match (imported, skipped) {
        (0, 0) => "Could not import thread history.".to_owned(),
        (0, 1) => "1 thread could not be imported.".to_owned(),
        (0, skipped) => format!("{skipped} threads could not be imported."),
        (imported, 0) => format!("Imported {}. Some thread history could not be imported.", threads(imported)),
        (imported, skipped) => format!("Imported {}. {} could not be imported.", threads(imported), threads(skipped)),
    }
}

/// The page's main button.
pub fn start_label(importing: bool, selected: usize, new: usize) -> String {
    let projects = |n: usize| format!("{n} {}", if n == 1 { "project" } else { "projects" });
    match (importing, selected, new) {
        (true, 0, _) => "Starting…".to_owned(),
        (true, ..) => "Importing…".to_owned(),
        (false, _, new) if new > 0 => format!("Import {} and start", projects(new)),
        (false, 0, _) => "Start".to_owned(),
        (false, ..) => "Import threads and start".to_owned(),
    }
}

/// An import row's age: "now", "5m", "3h", "2d".
pub fn age_label(last_active_at: Option<&str>, now_ms: i64) -> String {
    let Some(at) = last_active_at.and_then(crate::timestamp::parse_ms) else { return String::new() };
    let label = crate::timestamp::relative_label(now_ms - at);
    match label.strip_suffix(" ago") {
        Some(value) => value.to_owned(),
        None => "now".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn provider(instance: &str, driver: &str, status: &str, installed: bool, auth: &str) -> Value {
        json!({"instanceId": instance, "driver": driver, "enabled": true, "status": status, "installed": installed, "auth": {"status": auth}})
    }

    fn settings(value: Value) -> crate::settings::Settings {
        crate::settings::Settings::layered(&crate::settings::contract().server_defaults, &value)
    }

    #[test]
    fn only_a_fresh_install_opens_setup() {
        assert_eq!(first_run(false, false, 0, 0), FirstRun::Welcome);
        assert_eq!(first_run(true, false, 0, 0), FirstRun::App { persist: false });
        assert_eq!(first_run(false, false, 1, 0), FirstRun::App { persist: true });
        assert_eq!(first_run(false, false, 0, 2), FirstRun::App { persist: true });
        assert_eq!(first_run(false, true, 0, 0), FirstRun::App { persist: true }, "a linked computer is setup done");
    }

    #[test]
    fn agent_states_follow_the_server_and_settings() {
        let ready = provider("claudeAgent", "claudeAgent", "ready", true, "authenticated");
        assert_eq!(agent_state(&ready, None), AgentState::Ready);
        let mut broken = ready.clone();
        broken["compatibilityAdvisory"] = json!({"status": "broken"});
        assert_eq!(agent_state(&broken, None), AgentState::Attention, "a broken CLI is never ready");
        let checking = provider("acp", "acpRegistry", "warning", false, "unknown");
        assert_eq!(agent_state(&checking, None), AgentState::Attention, "an unprepared ACP agent needs a look");
        assert_eq!(agent_state(&provider("codex", "codex", "warning", false, "unknown"), None), AgentState::Checking);
        let mut unavailable = ready;
        unavailable["availability"] = json!("unavailable");
        assert_eq!(agent_state(&unavailable, None), AgentState::Attention);
        // Pi is off unless configured, even while the server still lists it.
        let pi = provider("pi", "pi", "ready", true, "authenticated");
        assert_eq!(agent_state(&pi, Some(&settings(json!({})))), AgentState::Disabled);
        let on = settings(json!({"providerInstances": {"pi": {"driver": "pi", "enabled": true}}}));
        assert_eq!(agent_state(&pi, Some(&on)), AgentState::Ready);
    }

    #[test]
    fn groups_ready_agents_first_then_by_name() {
        let providers = [
            provider("codex", "codex", "ready", true, "unauthenticated"),
            provider("cursor", "cursor", "ready", false, "unknown"),
            provider("zed", "acpRegistry", "error", true, "authenticated"),
            provider("claudeAgent", "claudeAgent", "ready", true, "authenticated"),
            provider("grok", "grok", "warning", false, "unknown"),
        ];
        let groups = group_agents(Some(&providers), None, |p| instance_id(p) != "cursor");
        assert_eq!(groups.found, [3, 0, 2], "ready, then sign-in, then attention");
        assert_eq!(groups.available, [4], "disallowed agents are left out; checking ones go to add");
        assert!(groups.checking);
        assert_eq!(groups.ready_count, 1);
        assert!(group_agents(None, None, |_| true).checking, "no list yet is still checking");
    }

    #[test]
    fn agent_names_prefer_the_instance_label() {
        assert_eq!(agent_name(&json!({"instanceId": "claudeAgent", "driver": "claudeAgent"})), "Claude");
        assert_eq!(agent_name(&json!({"instanceId": "codex_work", "driver": "codex"})), "Codex Work");
        assert_eq!(agent_name(&json!({"instanceId": "codex", "driver": "codex", "displayName": " Mine "})), "Mine");
        assert_eq!(agent_name(&json!({"instanceId": "myAgent", "driver": "someAgent"})), "My Agent");
    }

    #[test]
    fn turning_on_also_clears_a_config_off() {
        let next = turned_on_instance(&json!({"driver": "pi", "enabled": false, "config": {"enabled": false, "x": 1}}));
        assert_eq!(next, json!({"driver": "pi", "enabled": true, "config": {"enabled": true, "x": 1}}));
        assert_eq!(turned_on_instance(&json!({"driver": "pi"})), json!({"driver": "pi", "enabled": true}));
    }

    #[test]
    fn codex_setup_and_its_cli_step() {
        let signed_out = provider("codex", "codex", "ready", true, "unauthenticated");
        assert!(codex_needs_setup(&signed_out, None));
        assert_eq!(cli_terminal_step(&signed_out, None), CliStep::Open);
        let signed_in = provider("codex", "codex", "ready", true, "authenticated");
        assert!(!codex_needs_setup(&signed_in, None));
        assert_eq!(cli_terminal_step(&signed_in, None), CliStep::Done);
        let mut managed = signed_out.clone();
        managed["setup"] = json!({"canAuthenticate": true});
        assert_eq!(cli_terminal_step(&managed, None), CliStep::Wait, "the ChatGPT runtime is still swapping out");
        let checking = provider("codex", "codex", "warning", false, "unknown");
        assert_eq!(cli_terminal_step(&checking, None), CliStep::Wait);
        let mut broken = signed_out;
        broken["compatibilityAdvisory"] = json!({"status": "broken"});
        assert!(!codex_needs_setup(&broken, None));
        assert_eq!(cli_terminal_step(&broken, None), CliStep::Done);
    }

    #[test]
    fn setup_commands_quote_custom_binaries_per_shell() {
        assert_eq!(install_command("codex", "windows"), Some("irm https://chatgpt.com/codex/install.ps1 | iex"));
        assert_eq!(install_command("claudeAgent", "darwin"), Some("curl -fsSL https://claude.ai/install.sh | bash"));
        assert_eq!(install_command("pi", "linux"), None);
        let claude = provider("claudeAgent", "claudeAgent", "ready", true, "unauthenticated");
        let with_path = |path: &str| json!({"providerInstances": {"claudeAgent": {"config": {"binaryPath": path}}}});
        assert_eq!(login_command(&claude, &json!({}), "linux"), "claude auth login");
        assert_eq!(login_command(&claude, &with_path("~/my bin/claude"), "darwin"), "~/'my bin/claude' auth login");
        assert_eq!(login_command(&claude, &with_path("it's"), "linux"), r#"'it'"'"'s' auth login"#);
        assert_eq!(
            login_command(&claude, &with_path(r"C:\Program Files\claude.exe"), "windows"),
            r"& 'C:\Program Files\claude.exe' auth login"
        );
        assert_eq!(
            login_command(&claude, &with_path(r"C:\bin\claude.exe"), "windows"),
            r"C:\bin\claude.exe auth login"
        );
        assert_eq!(
            login_command(&claude, &with_path(r"a\b"), "linux"),
            r"'a\b' auth login",
            "a backslash is literal in sh"
        );
        let codex = provider("codex", "codex", "ready", true, "unauthenticated");
        assert_eq!(login_command(&codex, &json!({}), "windows"), "codex login");
    }

    /// A scan candidate; `git: None` leaves the field out, as servers before git scans do.
    fn candidate(path: &str, threads: u64, at: Option<&str>, git: Option<Value>) -> Value {
        let mut c =
            json!({"path": path, "title": path, "sources": ["codex"], "threadCount": threads, "lastActiveAt": at});
        if let Some(git) = git {
            c["git"] = git;
        }
        c
    }

    fn repo(remote: Option<&str>, name: Option<&str>) -> Value {
        json!({"remoteKey": remote, "repository": name})
    }

    #[test]
    fn scan_reads_git_as_unknown_none_or_a_repository() {
        let reply = json!({"candidates": [
            {"path": "/a", "title": "a", "sources": [], "threadCount": 1, "lastActiveAt": null},
            {"path": "/b", "title": "b", "sources": [], "threadCount": 1, "lastActiveAt": null, "git": null},
            {"path": "/c", "title": "c", "sources": ["pi"], "threadCount": 1, "lastActiveAt": null, "git": {"remoteKey": "gh/x", "repository": "x/y"}},
            {"title": "missing path"},
        ], "truncated": true});
        let scan = Scan::parse("e", &reply);
        assert!(scan.truncated);
        let git: Vec<&Git> = scan.candidates.iter().map(|c| &c.git).collect();
        assert_eq!(git.len(), 3, "a candidate without a path is skipped");
        assert_eq!(git[0], &Git::Repository { remote_key: None, repository: None }, "older servers count as git");
        assert_eq!(git[1], &Git::NotARepository);
        assert_eq!(git[2], &Git::Repository { remote_key: Some("gh/x".into()), repository: Some("x/y".into()) });
        assert_eq!(scan.candidates[0].key, r#"["e","/a"]"#);
    }

    #[test]
    fn selection_starts_on_recent_new_projects_and_keeps_later_choices() {
        let now = crate::timestamp::parse_ms("2026-11-09T00:00:00Z").expect("valid time");
        let reply = json!({"candidates": [
            candidate("/recent", 3, Some("2026-11-01T00:00:00Z"), Some(repo(None, None))),
            candidate("/few", 2, Some("2026-11-01T00:00:00Z"), Some(repo(None, None))),
            candidate("/old", 9, Some("2026-01-01T00:00:00Z"), Some(repo(None, None))),
            candidate("/plain", 9, Some("2026-11-01T00:00:00Z"), Some(Value::Null)),
            candidate("/known", 9, Some("2026-11-01T00:00:00Z"), Some(repo(None, None))),
        ]});
        let scan = Scan::parse("e", &reply);
        let roots = ProjectRoots::new([("e", "/known/")]);
        let recent: Vec<&Candidate> = scan.candidates.iter().filter(|c| is_recent(c, now) && roots.is_new(c)).collect();
        let recent_keys: HashSet<&str> = recent.iter().map(|c| c.key.as_str()).collect();
        let paths = |list: Vec<&Candidate>| list.into_iter().map(|c| c.path.clone()).collect::<Vec<_>>();
        assert_eq!(paths(selected(&scan.candidates, &recent_keys, None)), ["/recent"]);

        // A project found after the choice gets the defaults; known ones keep the choice.
        let choice = ImportSelection {
            known_keys: scan.candidates[1..].iter().map(|c| c.key.clone()).collect(),
            keys: vec![scan.candidates[2].key.clone()],
            opted_out: false,
        };
        assert_eq!(paths(selected(&scan.candidates, &recent_keys, Some(&choice))), ["/recent", "/old"]);
        let out = ImportSelection { opted_out: true, ..choice };
        assert!(selected(&scan.candidates, &recent_keys, Some(&out)).is_empty(), "opting out drops later finds too");

        let defaults = default_keys(&scan.candidates, &recent, &roots);
        assert_eq!(defaults, [scan.candidates[0].key.clone()]);
        // Without recent projects the box picks every new one, then everything.
        assert_eq!(default_keys(&scan.candidates, &[], &roots).len(), 4);
        let all_known = ProjectRoots::new(scan.candidates.iter().map(|c| ("e", c.path.as_str())));
        assert_eq!(default_keys(&scan.candidates, &[], &all_known).len(), 5);
    }

    #[test]
    fn a_server_match_or_a_listed_root_is_not_new() {
        let scan = Scan::parse(
            "e",
            &json!({"candidates": [
                {"path": "C:/Repo", "title": "r", "sources": [], "threadCount": 1, "lastActiveAt": null},
                {"path": "/m", "title": "m", "projectId": "p", "sources": [], "threadCount": 1, "lastActiveAt": null},
            ]}),
        );
        let roots = ProjectRoots::new([("e", r"c:\repo"), ("other", "/m")]);
        assert!(!roots.is_new(&scan.candidates[0]), "Windows roots compare case- and slash-blind");
        assert!(!roots.is_new(&scan.candidates[1]));
        assert!(ProjectRoots::new([("f", r"c:\repo")]).is_new(&scan.candidates[0]), "another computer's project");
    }

    #[test]
    fn groups_clones_by_remote_newest_first() {
        let reply = json!({"candidates": [
            candidate("/a1", 1, Some("2026-01-01T00:00:00Z"), Some(repo(Some("gh/a"), Some("o/a")))),
            candidate("/b", 2, None, Some(repo(None, None))),
            candidate("/a2", 4, Some("2026-03-01T00:00:00Z"), Some(repo(Some("gh/a"), Some("o/a")))),
            candidate("/plain", 1, None, Some(Value::Null)),
        ]});
        let scan = Scan::parse("e", &reply);
        let (groups, other) = group_projects(&scan.candidates);
        assert_eq!(groups.iter().map(|g| g.label.as_str()).collect::<Vec<_>>(), ["o/a", "/b"]);
        assert_eq!(groups[0].thread_count, 5);
        assert_eq!(groups[0].last_active_at.as_deref(), Some("2026-03-01T00:00:00Z"));
        assert_eq!(groups[0].candidates.len(), 2);
        assert_eq!(other.iter().map(|c| c.path.as_str()).collect::<Vec<_>>(), ["/plain"]);
    }

    #[test]
    fn splits_paths_at_their_last_separator() {
        assert_eq!(split_project_path("/home/me/repo/"), ("repo", "/home/me/"));
        assert_eq!(split_project_path(r"C:\work\app"), ("app", r"C:\work\"));
        assert_eq!(split_project_path(r"C:\"), (r"C:\", ""));
        assert_eq!(split_project_path(r"\\server\share\"), (r"\\server\share\", ""));
        assert_eq!(split_project_path("repo"), ("repo", ""));
        assert_eq!(split_project_path("/"), ("/", ""));
    }

    #[test]
    fn lands_in_a_project_with_history_before_a_finished_one() {
        let keys = vec!["a".to_owned(), "b".to_owned()];
        let history = HashMap::from([("b".to_owned(), 2)]);
        let completed = HashMap::from([("a".to_owned(), 1), ("b".to_owned(), 2)]);
        assert_eq!(landing_project(&keys, &history, &completed), Some(&2));
        assert_eq!(landing_project(&keys, &HashMap::new(), &completed), Some(&1));
        assert_eq!(landing_project::<i32>(&keys, &HashMap::new(), &HashMap::new()), None);
    }

    #[test]
    fn import_messages_count_threads() {
        assert_eq!(import_warning(0, 0), "Could not import thread history.");
        assert_eq!(import_warning(0, 1), "1 thread could not be imported.");
        assert_eq!(import_warning(0, 3), "3 threads could not be imported.");
        assert_eq!(import_warning(1, 0), "Imported 1 thread. Some thread history could not be imported.");
        assert_eq!(import_warning(2, 1), "Imported 2 threads. 1 thread could not be imported.");
        assert_eq!(start_label(false, 2, 1), "Import 1 project and start");
        assert_eq!(start_label(false, 2, 0), "Import threads and start");
        assert_eq!(start_label(false, 0, 0), "Start");
        assert_eq!(start_label(true, 0, 0), "Starting…");
        assert_eq!(start_label(true, 1, 1), "Importing…");
        let now = crate::timestamp::parse_ms("2026-11-09T12:00:00Z").expect("valid time");
        assert_eq!(age_label(Some("2026-11-09T11:59:30Z"), now), "now");
        assert_eq!(age_label(Some("2026-11-07T12:00:00Z"), now), "2d");
        assert_eq!(age_label(None, now), "");
    }
}
