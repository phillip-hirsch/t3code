//! The command palette (`apps/web/src/components/CommandPalette*.tsx`): actions,
//! recent threads, search over threads, projects and settings, message hits,
//! "New thread in…", and the add-project flow (browse a folder, or pick one
//! with the native dialog, then `projects.mutate`; or clone a Git URL or a host's repository,
//! then `projectClone.start`), and New project (`projects.createNew`).
//!
//! Go to file and Search project contents open the child searches in `files/`.
//! Actions this client has no surface for yet (guest sharing, WSL folders) are left out, and the
//! theme list has no environment-published themes.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::base::FocusTrapElement as _;
use gpui_kit::component::button::Button;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, AppContext as _, ClickEvent, Context, Entity, FocusHandle, FontWeight, Hsla, InteractiveElement as _,
    IntoElement, KeyDownEvent, Keystroke, ParentElement as _, PathPromptOptions, ScrollHandle, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window, div, prelude::FluentBuilder as _, px,
    relative, svg,
};
use serde::Deserialize as _;
use serde_json::{Value, json};

use super::keymap;
use super::pages::Page;
use super::palette_actions::github_repository;
use super::project_clone::paths as clone_paths;
use super::source_control;
use super::{Target, Workspace, new_id, project_name};
use crate::inbox;
use crate::model::{ProjectShell, ScopedId};
use crate::search::{self, Field, Weighted};
use crate::theme::Tokens;

mod clone_flow;
mod new_project;

const RECENT_LIMIT: usize = 12;
const THREAD_LIMIT: usize = 20;
const MESSAGE_LIMIT: usize = 10;
const SECONDARY_LIMIT: usize = 3;
const MESSAGE_DEBOUNCE: Duration = Duration::from_millis(120);

/// One message hit from `orchestration.searchThreads`.
#[derive(Clone)]
pub(super) struct MessageMatch {
    /// The environment that answered: thread ids repeat across environments.
    pub(super) environment_id: SharedString,
    pub(super) thread_id: String,
    pub(super) user: bool,
    pub(super) snippet: String,
    pub(super) message_id: Option<String>,
    pub(super) at: Option<String>,
    /// 1 for whole words, 0.8 for a word start, less inside words; older servers omit it.
    quality: Option<f32>,
    /// How many of the thread's newest messages match; older servers omit it.
    hits: Option<u32>,
}

impl MessageMatch {
    fn from_json(environment_id: &SharedString, value: &Value) -> Option<Self> {
        #[expect(clippy::cast_possible_truncation, reason = "a match quality near 1")]
        let quality = value["quality"].as_f64().map(|q| q as f32);
        Some(Self {
            environment_id: environment_id.clone(),
            thread_id: value["threadId"].as_str()?.to_owned(),
            user: value["source"] == "user",
            snippet: value["snippet"].as_str()?.to_owned(),
            message_id: value["messageId"].as_str().map(str::to_owned),
            at: value["messageCreatedAt"].as_str().map(str::to_owned),
            quality,
            hits: value["hits"].as_u64().and_then(|hits| u32::try_from(hits).ok()),
        })
    }

    /// The hit's thread on the environment that answered.
    pub(super) fn thread_key(&self) -> ScopedId {
        ScopedId::new(self.environment_id.clone(), self.thread_id.as_str())
    }

    fn is_from(&self, key: &ScopedId) -> bool {
        self.environment_id == key.env && self.thread_id == key.id
    }

    pub(super) fn is_from_thread(&self, thread: &crate::model::ThreadShell) -> bool {
        self.environment_id == thread.environment_id && self.thread_id == thread.id
    }
}

/// How a search reaches environments whose shells leave older settled threads out
/// (`ThreadSearchScope`); others ignore it.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct SearchScope {
    /// The sidebar filter's projects by environment; `None` searches every project.
    pub(super) projects: Option<HashMap<SharedString, Vec<String>>>,
    /// The caller's names for environments, which its own ranking also matches.
    pub(super) labels: Option<HashMap<SharedString, SharedString>>,
}

/// `orchestration.searchThreads` for one environment, or `None` when it has nothing to search:
/// messages need a word of three or more letters, while older settled titles below `before`
/// (the environment's cut) match from two.
fn search_input(query: &str, before: Option<&str>, scope: &SearchScope, environment: &str) -> Option<Value> {
    let messages = search::has_long_token(query);
    let Some(before) = before else { return messages.then(|| json!({"query": query})) };
    if !messages && query.chars().count() < 2 {
        return None;
    }
    let mut input = json!({"query": query, "olderSettledBefore": before});
    if let Some(projects) = &scope.projects {
        input["projectIds"] = json!(projects.get(environment).map_or(&[][..], Vec::as_slice));
    }
    if let Some(label) = scope.labels.as_ref().and_then(|labels| labels.get(environment)) {
        input["environmentLabel"] = json!(label.as_ref());
    }
    if !messages {
        input["titlesOnly"] = true.into();
    }
    Some(input)
}

/// Server message search for a query over every connected environment, debounced
/// (`useThreadSearch`). Each environment's matches join as it answers, and the rows of its
/// best older settled matches merge into the shell so the caller's own ranking finds them.
#[derive(Default)]
pub(super) struct MessageSearch {
    pub(super) query: String,
    scope: SearchScope,
    pub(super) matches: Vec<MessageMatch>,
    /// Some environment has not answered yet.
    pub(super) pending: bool,
    /// Counts answers, so views can cache what they derive from `matches`.
    generation: u64,
    _task: Option<Task<()>>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum View {
    Root,
    NewThreadIn,
    /// "Add project" (`addProjectEntryGroups`): the machines to add on, or one machine's sources.
    AddProject,
    /// The sources of [`Palette::add_environment`].
    AddProjectSources,
    Browse,
    /// A clone source's repository field, then where to clone it ([`Palette::clone`]).
    Clone,
    /// New project's name, machine and repository option ([`Palette::new_project`]).
    NewProject,
    /// "Change theme" (`theme.select`).
    Themes,
    /// "Change appearance".
    Appearance,
}

#[derive(Clone)]
enum Run {
    /// A draft in this project, on its environment.
    NewThread(ScopedId),
    Open(View),
    Thread(ScopedId),
    /// A thread on this environment, which the shell may not list (an archived linked thread).
    ThreadIn(SharedString, String),
    /// A project group's members and the checkout to start a thread in when none has one.
    Project(Vec<ScopedId>, ScopedId),
    /// "Add project" (`openAddProjectFlow`).
    AddProject,
    /// An environment's sources (`startAddProjectSourceSelection`).
    AddProjectIn(SharedString),
    /// "Local folder": browse a folder on this environment (`startAddProjectBrowse`).
    BrowseIn(SharedString),
    /// "Git URL" (`None`) or a host's repository: clone on this environment (`startAddProjectClone`).
    CloneFrom(SharedString, Option<&'static str>),
    /// The root's "New project" (`openNewProjectFlow`).
    NewProject,
    /// A machine's "New project" source (`startNewProject`).
    NewProjectIn(SharedString),
    /// New project's machine.
    NewProjectMachine(SharedString),
    /// New project's private repository toggle.
    NewProjectPublish,
    /// New project's "Add existing project".
    NewProjectExisting,
    Settings(&'static str),
    /// One setting: its section, scrolled to its row.
    Setting(&'static crate::settings::SearchItem),
    /// Settings › Projects with this project's group picked.
    ProjectSettings(ScopedId),
    Page(Page),
    /// Browse into this directory (the query becomes it).
    BrowseTo(String),
    /// Find in the open thread (`chat.find`).
    Find,
    /// A message hit: opens its thread with find on the query, at the message.
    FindIn {
        thread: ScopedId,
        query: String,
        message_id: Option<String>,
    },
    /// "New thread without a project" (`chat.newWithoutProject`), on this environment.
    ScratchThread(SharedString),
    Triage,
    /// The composer's prompt as a link; `true` adds the repository and model.
    CopyPromptLink(bool),
    /// `thread.copyReference`.
    CopyThread,
    RestartSession,
    /// Pins (`true`) or unpins these projects, a group's members.
    PinProjects(Vec<ScopedId>, bool),
    Theme(crate::themes::CardId),
    Appearance(crate::settings::ThemePreference),
    /// `themeEditor.toggle`.
    ThemeEditor,
    /// Go to file or Search project contents, returning here on Esc.
    FileSearch(super::files::OverlayMode),
    /// `workspace.focusNext` (`true`) or `workspace.focusPrevious`, once the palette has closed.
    WorkspaceFocus(bool),
    /// "Link pull request to thread" for this thread.
    LinkPullRequest(ScopedId),
    /// "Show linked pull requests": the right panel's Linked pull requests surface.
    LinkedPullRequests,
    /// "Use T3 Code in another app (MCP)" for this environment, else the one Sharing & access picks.
    UseInAnotherApp(Option<SharedString>),
}

impl Run {
    /// Lands on a thread or a draft, now or once the server answers.
    fn opens_chat(&self) -> bool {
        matches!(
            self,
            Self::NewThread(_)
                | Self::Thread(_)
                | Self::Project(..)
                | Self::FindIn { .. }
                | Self::ScratchThread(_)
                | Self::Triage
        )
    }
}

#[derive(Clone)]
enum RowIcon {
    Lucide(IconName),
    Project(ScopedId),
    /// A source control host's mark.
    Host(&'static str),
}

#[derive(Clone)]
enum Description {
    Text(SharedString),
    /// "You:" / "Agent:" and the snippet in highlighted runs.
    Snippet(bool, Vec<(String, bool)>),
    /// `ProjectLocationDescription`: a remote machine's icon, where the project lives, its path,
    /// and whether it leads with "Current".
    Location(Option<IconName>, SharedString, SharedString, bool),
    Thread(ThreadSubtitle),
}

/// `ThreadCommandSubtitle`: the project with its favicon and machine, the checkout, the provider,
/// and whether it is the open thread.
#[derive(Clone)]
struct ThreadSubtitle {
    project: Option<(ScopedId, SharedString)>,
    location: SharedString,
    branch: Option<SharedString>,
    worktree: bool,
    provider: Option<(&'static str, Option<Hsla>)>,
    current: bool,
}

#[derive(Clone)]
struct Row {
    key: SharedString,
    icon: RowIcon,
    title: SharedString,
    /// Drawn after `title` at weight 600 ("New thread in **X**").
    strong: Option<SharedString>,
    /// `ThreadRowContext`: the project beside a thread's title when a snippet takes its line.
    context: Option<SharedString>,
    description: Option<Description>,
    time: Option<String>,
    shortcut: Option<SharedString>,
    /// The command `shortcut` shows (`shortcutCommand`), which runs this row inside the palette.
    command: Option<SharedString>,
    chevron: bool,
    /// `ProjectPinButton`: the group's project ids and whether it is pinned.
    pin: Option<(Vec<ScopedId>, bool)>,
    run: Run,
    /// Keywords that match besides the title (`searchTerms`).
    terms: &'static [&'static str],
    /// Search terms from the data: member names, paths and environments.
    data_terms: Vec<SharedString>,
    /// Ranking, while a query is typed.
    quality: f32,
    /// Shown dimmed, and neither highlighted nor run (`DisabledCommandPaletteResultRow`).
    disabled: bool,
    /// A clone source that is not set up: Setup Required, with how (`titleTrailingContent`).
    setup: Option<SharedString>,
    /// Listed after the other rows of its group (`secondary`).
    secondary: bool,
    /// The thread a thread result names, whose status slots it draws.
    thread: Option<ScopedId>,
    /// The chosen option of its group, marked with a check.
    checked: bool,
    /// A toggle row's checkbox and whether it is on.
    toggle: Option<bool>,
}

impl Row {
    fn new(key: impl Into<SharedString>, icon: RowIcon, title: impl Into<SharedString>, run: Run) -> Self {
        Self {
            key: key.into(),
            icon,
            title: title.into(),
            strong: None,
            context: None,
            description: None,
            time: None,
            shortcut: None,
            command: None,
            chevron: false,
            pin: None,
            run,
            terms: &[],
            data_terms: Vec::new(),
            quality: 0.,
            disabled: false,
            setup: None,
            secondary: false,
            thread: None,
            checked: false,
            toggle: None,
        }
    }
}

/// A thread result's status slots (`renderLeadingContent`, `renderTrailingContent`).
#[derive(Default)]
struct ThreadSlots {
    leading: Option<AnyElement>,
    trailing: Option<AnyElement>,
}

struct Group {
    label: &'static str,
    rows: Vec<Row>,
}

/// What a folder listing answers: the machine, the folder relative paths start from, and the
/// directory. A listing for another key is stale.
#[derive(Clone, Default, PartialEq, Eq)]
struct BrowseKey {
    environment: Option<SharedString>,
    cwd: Option<String>,
    directory: String,
}

/// The folder listing behind the browse view (`filesystem.browse`).
#[derive(Default)]
struct Browse {
    key: BrowseKey,
    parent: Option<String>,
    entries: Vec<(String, String)>,
    loading: bool,
    error: Option<String>,
    _task: Option<Task<()>>,
}

pub(super) struct Palette {
    input: Entity<InputState>,
    view: View,
    /// Parent views and their queries, for Back.
    stack: Vec<(View, String)>,
    /// The highlighted row; `None` paints none, but Enter runs the first.
    highlight: Option<usize>,
    /// The arrows moved the highlight: the next render scrolls it into view (`block: "nearest"`).
    reveal: bool,
    scroll: ScrollHandle,
    browse: Browse,
    messages: MessageSearch,
    placeholder: SharedString,
    /// The app is dark: the theme list marks the dark appearance's theme as current.
    dark: bool,
    /// The root view's Threads group, ranked once per query and shell change.
    threads: Option<ThreadRanking>,
    /// "In messages", in the order last shown.
    message_order: MessageOrder,
    /// The threads a pull request's count opened the palette with, listed while its query stays.
    linked: Option<LinkedSearch>,
    /// Project and "New thread in..." rows by group key, in the order the palette opened with:
    /// a pin from a row must not move rows under the pointer or the jump numbers.
    project_order: RefCell<Option<Vec<String>>>,
    new_thread_order: RefCell<Option<Vec<String>>>,
    /// The environment "Add project" adds on (`addProjectEnvironmentId`), once one is picked.
    add_environment: Option<SharedString>,
    /// The clone flow, once a clone source is picked (`addProjectCloneFlow`).
    clone: Option<CloneFlow>,
    /// The New project flow, while it is open (`newProjectFlow`).
    new_project: Option<new_project::NewProjectFlow>,
    /// A repository lookup or a clone start is on its way (`isRemoteProjectPending`).
    clone_pending: bool,
    _clone_task: Option<Task<()>>,
    /// `server.discoverSourceControl` for an add-project environment: clone sources' readiness.
    discovery: Option<(SharedString, Option<Value>)>,
    _discovery_task: Option<Task<()>>,
    /// Tab stays inside the palette while it is open (the web's modal dialog).
    trap: FocusHandle,
    _subscription: Subscription,
}

/// A thread that links a pull request (`PullRequestLinkedThreadsResult`'s threads).
#[derive(Clone, Debug)]
pub(super) struct LinkedThread {
    pub(super) id: String,
    pub(super) title: String,
    pub(super) archived: bool,
}

/// `CommandPaletteOpenIntent` "search" with `linkedThreads`: the query that opened the palette
/// and the threads to list for it in place of thread search.
struct LinkedSearch {
    query: String,
    environment: SharedString,
    threads: Vec<LinkedThread>,
}

impl LinkedSearch {
    /// `buildLinkedThreadActionItems`.
    fn rows(&self) -> Vec<Row> {
        self.threads
            .iter()
            .map(|thread| {
                let title = if thread.title.is_empty() { "Untitled thread" } else { thread.title.as_str() };
                let run = Run::ThreadIn(self.environment.clone(), thread.id.clone());
                let mut row =
                    Row::new(format!("thread-{}", thread.id), RowIcon::Lucide(IconName::MessageSquare), title, run);
                let description = if thread.archived { "Archived thread" } else { "Linked thread" };
                row.description = Some(Description::Text(description.into()));
                row.data_terms = vec![self.query.clone().into(), thread.title.clone().into()];
                row
            })
            .collect()
    }
}

/// The message rows' ranking for a query, answers and shell, and the order shown so far.
#[derive(Default)]
struct MessageOrder {
    key: (String, u64, u64),
    order: Option<ShownOrder>,
}

/// `ShownOrder`: message result keys in the order the palette last showed them for `query`.
#[derive(Debug, PartialEq)]
struct ShownOrder<K = ScopedId> {
    query: String,
    keys: Vec<K>,
    /// How many of `keys` are on screen; only those hold their places.
    shown: usize,
}

/// Rows past the limit a slower environment's best matches may take.
const LATE_ROOM: usize = 3;

/// `nextShownOrder`: shown rows keep their places when a slower environment answers, so the
/// pointer and the arrows stay on the same row; its best matches may take a few rows more.
fn next_shown_order<K: PartialEq + Clone>(
    previous: Option<ShownOrder<K>>,
    query: &str,
    ranked: Vec<K>,
    limit: usize,
) -> ShownOrder<K> {
    let Some(previous) = previous.filter(|p| p.query == query) else {
        let shown = limit.min(ranked.len());
        return ShownOrder { query: query.to_owned(), keys: ranked, shown };
    };
    let kept: Vec<K> = previous.keys[..previous.shown].iter().filter(|key| ranked.contains(key)).cloned().collect();
    let rest: Vec<K> = ranked.into_iter().filter(|key| !kept.contains(key)).collect();
    let arrived = rest.iter().filter(|key| !previous.keys.contains(key)).count();
    let shown = (kept.len() + rest.len()).min(limit + LATE_ROOM).min(limit.max(kept.len() + arrived.min(LATE_ROOM)));
    ShownOrder { query: previous.query, keys: [kept, rest].concat(), shown }
}

/// `useOrderWhileOpen`: `items` sorted into the held order by key, rows it has not seen after,
/// in their own order; those then hold their places too.
fn hold_order<T>(order: &RefCell<Option<Vec<String>>>, items: Vec<(String, T)>) -> Vec<(String, T)> {
    let mut order = order.borrow_mut();
    let held = order.get_or_insert_with(Vec::new);
    let rank = |key: &str| held.iter().position(|k| k == key);
    let mut ranked: Vec<(usize, (String, T))> =
        items.into_iter().enumerate().map(|(at, item)| (rank(&item.0).unwrap_or(held.len() + at), item)).collect();
    ranked.sort_by_key(|(rank, _)| *rank);
    let items: Vec<(String, T)> = ranked.into_iter().map(|(_, item)| item).collect();
    for (key, _) in &items {
        if !held.contains(key) {
            held.push(key.clone());
        }
    }
    items
}

/// `MESSAGE_PREFIX`: a match quality at or above it is a word match.
const MESSAGE_PREFIX: f32 = 0.8;

/// `rankMessageMatches`: word matches first; among them a pick for the query, then the closer
/// match, then many hits on a used thread, then the newer message.
fn rank_messages<'a>(
    entries: Vec<(&'a MessageMatch, search::ThreadUse)>,
    tokens: &[Vec<char>],
    now: i64,
) -> Vec<&'a MessageMatch> {
    #[expect(clippy::cast_precision_loss, reason = "a hit count")]
    let weight = |hits: u32| 1. + (1. + hits as f32).log2();
    let mut ranked: Vec<(&MessageMatch, f32, f64, f32)> = entries
        .into_iter()
        .map(|(found, used)| {
            // Older servers send no quality; their matches count as words.
            let quality = found.quality.unwrap_or(1.);
            let picked_at = used.picked_at(tokens, now).unwrap_or(0.);
            (found, quality, picked_at, weight(found.hits.unwrap_or(1)) * used.boost(now))
        })
        .collect();
    ranked.sort_by(|a, b| {
        (b.1 >= MESSAGE_PREFIX)
            .cmp(&(a.1 >= MESSAGE_PREFIX))
            .then(b.2.total_cmp(&a.2))
            .then(b.1.total_cmp(&a.1))
            .then(b.3.total_cmp(&a.3))
            .then_with(|| b.0.at.cmp(&a.0.at))
    });
    ranked.into_iter().map(|(found, ..)| found).collect()
}

/// Ranking every thread is the palette's one cost that grows with the data, so it runs
/// when its inputs change, not on each repaint, and only the listed rows are built.
struct ThreadRanking {
    key: RankingKey,
    /// The exact pass; with no query, the recent threads.
    strict: RankedThreads,
    /// The typo-tolerant pass, needed only when no exact thread match is strong.
    typos: Option<RankedThreads>,
}

#[derive(PartialEq)]
struct RankingKey {
    query: String,
    revision: u64,
    current: Option<ScopedId>,
    /// Rows show "5m ago" and rank by recency.
    minute: i64,
}

struct RankedThreads {
    /// The listed rows, with whether each matched strongly.
    rows: Vec<(Row, bool)>,
    /// Any thread matched strongly, listed or not.
    strong: bool,
}

/// `filterCommandPaletteGroups`: a leading `>` searches the actions alone, for the rest of the query.
fn actions_filter(query: &str) -> (bool, &str) {
    query.strip_prefix('>').map_or((false, query), |rest| (true, rest))
}

/// A submenu with no row for a typed query searches everything instead.
fn submenu_falls_back(scoped: &[Group], query: &str) -> bool {
    !query.trim().is_empty() && scoped.iter().all(|group| group.rows.is_empty())
}

/// `isFilesystemBrowseQuery`: the query reads as a path.
pub(super) fn is_browse_query(query: &str) -> bool {
    let bytes = query.as_bytes();
    ["./", "../", ".\\", "..\\", "/", "~/", "~\\"].iter().any(|p| query.starts_with(p))
        || (bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && matches!(bytes[2], b'\\' | b'/'))
}

/// The directory part (through the last separator) and the leaf after it.
pub(super) fn split_browse(query: &str) -> (&str, &str) {
    match query.rfind(['/', '\\']) {
        Some(at) => (&query[..=at], &query[at + 1..]),
        None => ("", query),
    }
}

/// `inferProjectTitleFromPath`: the last non-empty path segment.
pub(super) fn project_title_from_path(path: &str) -> String {
    path.split(['/', '\\']).rfind(|s| !s.is_empty()).unwrap_or(path).to_owned()
}

/// `filterFilesystemBrowseEntries`: prefix match on the leaf; dot folders only for a dot leaf.
impl Browse {
    /// The listing, when it answers `key`.
    fn of(&self, key: &BrowseKey) -> Option<&Self> {
        (self.key == *key).then_some(self)
    }
}

/// What Enter adds for `query`: the folder the listing resolved it to, else the typed path. Pass
/// only the listing of `query`'s own directory on the browse machine and cwd.
fn submit_path(browse: Option<&Browse>, query: &str) -> Option<String> {
    let leaf = split_browse(query).1;
    let resolved = browse.and_then(|browse| {
        if leaf.is_empty() {
            browse.parent.clone()
        } else {
            browse.entries.iter().find(|(name, _)| name == leaf).map(|(_, full)| full.clone())
        }
    });
    resolved.or_else(|| (!query.trim().is_empty()).then(|| query.trim().to_owned()))
}

/// `AddProjectCloneFlow`: where a clone happens and what it clones.
#[derive(Clone, Debug, PartialEq)]
struct CloneFlow {
    environment: SharedString,
    /// The host kind; `None` is a Git URL.
    source: Option<&'static str>,
    /// The resolved repository, once the flow moved on to its destination (`step: "confirm"`).
    confirm: Option<CloneTarget>,
}

#[derive(Clone, Debug, PartialEq)]
struct CloneTarget {
    /// What the user typed.
    input: String,
    /// The looked-up repository's `nameWithOwner` and web URL.
    repository: Option<(String, String)>,
    remote_url: String,
}

impl CloneFlow {
    /// `remoteProjectButtonLabel`.
    fn submit_label(&self) -> &'static str {
        if self.source.is_some() { "Lookup" } else { "Continue" }
    }
}

/// Whether the view browses folders for `query`: the browse view, a path typed at the root, or
/// the clone destination (`getFilesystemBrowsePath(…).isBrowsing`).
fn browses(palette: &Palette, query: &str) -> bool {
    match palette.view {
        View::Browse => true,
        View::Root => is_browse_query(query),
        View::Clone => palette.clone.as_ref().is_some_and(|flow| flow.confirm.is_some()) && is_browse_query(query),
        _ => false,
    }
}

pub(super) fn filter_entries<'a>(entries: &'a [(String, String)], leaf: &str) -> Vec<&'a (String, String)> {
    let lower = leaf.to_lowercase();
    entries
        .iter()
        .filter(|(name, _)| {
            name.to_lowercase().starts_with(&lower) && (leaf.starts_with('.') || !name.starts_with('.'))
        })
        .collect()
}

impl Workspace {
    /// Opens the palette in `view`; Ctrl+K on an open root palette closes it.
    pub(super) fn open_palette(&mut self, view: View, window: &mut Window, cx: &mut Context<Self>) {
        if view == View::Root && self.palette.as_ref().is_some_and(|p| p.stack.is_empty() && p.view == View::Root) {
            self.close_palette(window, cx);
            return;
        }
        let input = cx.new(|cx| InputState::new(window, cx));
        let subscription = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
            if matches!(event, InputEvent::Change) {
                this.palette_query_changed(window, cx);
            }
        });
        input.update(cx, |input, cx| input.focus(window, cx));
        self.palette = Some(Palette {
            input,
            view: View::Root,
            stack: Vec::new(),
            highlight: Some(0),
            reveal: false,
            scroll: ScrollHandle::new(),
            browse: Browse::default(),
            messages: MessageSearch::default(),
            placeholder: SharedString::default(),
            dark: crate::theme::tokens(cx).dark,
            threads: None,
            message_order: MessageOrder::default(),
            linked: None,
            project_order: RefCell::default(),
            new_thread_order: RefCell::default(),
            add_environment: None,
            clone: None,
            new_project: None,
            clone_pending: false,
            _clone_task: None,
            discovery: None,
            _discovery_task: None,
            trap: cx.focus_handle(),
            _subscription: subscription,
        });
        self.palette_query_changed(window, cx);
        if view == View::Root {
            // `autoHighlight="always"`: the root opens on its first row.
            if let Some(palette) = &mut self.palette {
                palette.highlight = Some(0);
            }
        } else {
            self.push_palette_view(view, window, cx);
        }
        cx.notify();
    }

    /// `openCommandPalette({ query, linkedThreads })`: a fresh palette searching `query`, which
    /// lists `linked`'s threads while the query stays as it opened.
    pub(super) fn open_palette_search(
        &mut self,
        query: String,
        environment: SharedString,
        linked: Vec<LinkedThread>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.palette = None;
        self.open_palette(View::Root, window, cx);
        if let Some(palette) = &mut self.palette {
            palette.linked = Some(LinkedSearch { query: query.clone(), environment, threads: linked });
        }
        self.set_palette_query(&query, window, cx);
    }

    /// Puts back the query the palette had before a child search opened.
    pub(super) fn restore_palette_query(&mut self, query: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.set_palette_query(query, window, cx);
    }

    pub(super) fn close_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette.take().is_some() {
            self.focus_composer(window, cx);
            cx.notify();
        }
    }

    fn palette_query(&self, cx: &gpui_kit::App) -> String {
        self.palette.as_ref().map(|p| p.input.read(cx).value().to_string()).unwrap_or_default()
    }

    fn set_palette_query(&mut self, query: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(palette) = &self.palette {
            let input = palette.input.clone();
            input.update(cx, |input, cx| input.set_value(query.to_owned(), window, cx));
        }
        self.palette_query_changed(window, cx);
    }

    /// Pushes a submenu; its query starts empty, or at the base folder when browsing.
    fn push_palette_view(&mut self, view: View, window: &mut Window, cx: &mut Context<Self>) {
        let query = self.palette_query(cx);
        let start = match view {
            // `getAddProjectInitialQueryForEnvironment`: the browse environment's base folder.
            View::Browse => {
                self.browse_environment().map_or_else(|| "~/".to_owned(), |env| self.add_project_base(&env))
            }
            _ => String::new(),
        };
        let Some(palette) = &mut self.palette else { return };
        palette.stack.push((palette.view, query));
        palette.view = view;
        palette.highlight = None;
        self.set_palette_query(&start, window, cx);
        if matches!(view, View::AddProject | View::AddProjectSources | View::NewProject) {
            self.load_palette_discovery(cx);
        }
    }

    /// The environment the add-project flow browses and adds on: the one picked, else the first
    /// that can add projects (`browseEnvironmentId`).
    fn browse_environment(&self) -> Option<SharedString> {
        let picked = self.palette.as_ref().and_then(|palette| palette.add_environment.clone());
        picked.or_else(|| self.add_project_environments().into_iter().next().map(|option| option.id))
    }

    /// The open project's folder when it lives on the browse environment, which relative paths
    /// start from (`currentProjectCwdForBrowse`).
    fn browse_cwd(&self) -> Option<String> {
        let environment = self.browse_environment()?;
        let project = self.current_project().filter(|project| project.environment_id == environment)?;
        Some(project.workspace_root.clone())
    }

    fn browse_key(&self, directory: &str) -> BrowseKey {
        BrowseKey { environment: self.browse_environment(), cwd: self.browse_cwd(), directory: directory.to_owned() }
    }

    /// The listing of `directory` on the current browse machine and cwd, once it was asked for.
    fn current_browse(&self, directory: &str) -> Option<&Browse> {
        self.palette.as_ref()?.browse.of(&self.browse_key(directory))
    }

    /// `relativePathNeedsActiveProject`.
    fn relative_needs_project(&self, query: &str) -> bool {
        is_explicit_relative(query.trim()) && self.browse_cwd().is_none()
    }

    /// `addProjectEnvironmentOptions`: the connected environments, this device's first, then by name.
    fn add_project_environments(&self) -> Vec<AddEnvironment> {
        let primary = self.primary_environment();
        let mut options: Vec<AddEnvironment> = self
            .sessions()
            .iter()
            .filter(|session| session.is_connected())
            .filter_map(|session| {
                let id = session.id.clone()?;
                let is_primary = primary == Some(&id);
                let info = &session.server_info;
                Some(AddEnvironment {
                    label: environment_option_label(is_primary, &id, info.environment_label.as_deref()),
                    machine: super::host_picker::machine_icon(info.machine_kind()),
                    primary: is_primary,
                    id,
                })
            })
            .collect();
        options.sort_by_cached_key(|option| (!option.primary, option.label.to_lowercase()));
        options
    }

    fn add_project_entry(&self) -> AddProjectEntry {
        let ids: Vec<SharedString> = self.add_project_environments().into_iter().map(|option| option.id).collect();
        add_project_entry(&ids)
    }

    /// `addProjectEnvironmentItems`: each environment, opening its sources.
    fn add_project_environment_rows(&self) -> Vec<Row> {
        self.add_project_environments()
            .into_iter()
            .map(|option| {
                let key = format!("add-project-environment:{}", option.id);
                let run = Run::AddProjectIn(option.id.clone());
                environment_row(option, key, run)
            })
            .collect()
    }

    /// `openAddProjectFlow`: no environment opens Connections, several ask which, one shows its sources.
    fn start_add_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.add_project_entry() {
            AddProjectEntry::Connect => {
                self.close_palette(window, cx);
                self.open_settings("connections", cx);
            }
            AddProjectEntry::Choose => self.push_palette_view(View::AddProject, window, cx),
            AddProjectEntry::Sources(environment) => self.open_add_project_sources(environment, window, cx),
        }
    }

    /// `startAddProjectSourceSelection`.
    fn open_add_project_sources(&mut self, environment: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(palette) = &mut self.palette {
            palette.add_environment = Some(environment);
        }
        self.push_palette_view(View::AddProjectSources, window, cx);
    }

    fn pop_palette_view(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(palette) = &mut self.palette else { return };
        let Some((view, query)) = palette.stack.pop() else { return };
        palette.view = view;
        palette.highlight = None;
        // `popView`: Back leaves the clone flow, and drops what it was waiting on.
        palette.clone = None;
        palette.clone_pending = false;
        palette._clone_task = None;
        self.leave_new_project();
        self.set_palette_query(&query, window, cx);
    }

    fn palette_query_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let query = self.palette_query(cx);
        let Some(palette) = &mut self.palette else { return };
        let browsing = browses(palette, &query);
        let placeholder: SharedString = match (&palette.clone, browsing, palette.stack.is_empty()) {
            _ if palette.view == View::NewProject => "Project name".into(),
            // `remoteProjectInputPlaceholder`.
            (Some(CloneFlow { source: None, confirm: None, .. }), ..) => "Enter Git clone URL".into(),
            (Some(CloneFlow { source: Some(kind), confirm: None, .. }), ..) => {
                let host = source_control::definition(kind);
                format!("Enter {} repository ({})", host.label, host.path_hint).into()
            }
            (_, true, true) => "Enter project path (e.g. ~/projects/my-app)".into(),
            (_, true, false) => "Enter path (e.g. ~/projects/my-app)".into(),
            (_, false, true) => "Search commands, projects, and threads...".into(),
            (_, false, false) => "Search...".into(),
        };
        if palette.placeholder != placeholder {
            palette.placeholder = placeholder.clone();
            let input = palette.input.clone();
            input.update(cx, |input, cx| input.set_placeholder(placeholder, window, cx));
        }
        let Some(palette) = &mut self.palette else { return };
        if palette.view == View::Browse && query.is_empty() {
            self.pop_palette_view(window, cx);
            return;
        }
        let Some(palette) = &mut self.palette else { return };
        // After a keystroke no row paints until the arrows move (`CommandPalette.tsx`).
        palette.highlight = None;
        if browsing {
            if !self.relative_needs_project(&query) {
                self.browse_folder(split_browse(&query).0.to_owned(), cx);
            }
        } else if palette.view == View::Root {
            let query = if actions_filter(&query).0 { String::new() } else { query };
            // The palette matches environment names when there are several; older rows must too.
            let labels = (self.sessions().len() > 1).then(|| {
                let ids = self.sessions().iter().filter_map(|session| session.id.clone());
                ids.map(|id| {
                    let label = self.location_label(&id);
                    (id, label)
                })
                .collect()
            });
            let scope = SearchScope { projects: None, labels };
            self.search_messages(query, scope, |this| this.palette.as_mut().map(|p| &mut p.messages), cx);
        }
        cx.notify();
    }

    /// Lists `directory` through `filesystem.browse`, once per machine, cwd and directory.
    fn browse_folder(&mut self, directory: String, cx: &mut Context<Self>) {
        let key = self.browse_key(&directory);
        let Some(client) = key.environment.as_ref().and_then(|environment| self.env_client(environment)) else {
            return;
        };
        let Some(palette) = &mut self.palette else { return };
        if palette.browse.key == key && (palette.browse.loading || palette.browse.error.is_none()) {
            return;
        }
        palette.browse = Browse { key: key.clone(), loading: true, ..Browse::default() };
        if directory.is_empty() {
            palette.browse.loading = false;
            return;
        }
        let mut input = json!({"partialPath": directory});
        if let Some(cwd) = &key.cwd {
            input["cwd"] = cwd.clone().into();
        }
        palette.browse._task = Some(cx.spawn(async move |this, cx| {
            let result = client.request("filesystem.browse", input).await;
            let _ = this.update(cx, |this, cx| {
                let Some(browse) = this.palette.as_mut().map(|p| &mut p.browse).filter(|b| b.key == key) else {
                    return;
                };
                browse.loading = false;
                match result {
                    Ok(value) => {
                        browse.parent = value["parentPath"].as_str().map(str::to_owned);
                        browse.entries = value["entries"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|e| Some((e["name"].as_str()?.to_owned(), e["fullPath"].as_str()?.to_owned())))
                            .collect();
                    }
                    Err(error) => browse.error = Some(error.to_string()),
                }
                cx.notify();
            });
        }));
    }

    /// Debounced `orchestration.searchThreads` into the search `slot` returns.
    pub(super) fn search_messages(
        &mut self,
        query: String,
        scope: SearchScope,
        slot: fn(&mut Workspace) -> Option<&mut MessageSearch>,
        cx: &mut Context<Self>,
    ) {
        let query: String = query.trim().chars().take(200).collect::<String>().trim().to_owned();
        // Every connected environment is searched, as `useThreadSearch` does over its ids. The
        // generation keeps a read made under an earlier snapshot from merging rows.
        let clients: Vec<(SharedString, crate::rpc::RpcClient, u64, Value)> = (self.sessions().iter())
            .filter_map(|session| {
                let id = session.id.clone()?;
                let before = self.shell.older_settled(&id).map(|cut| cut.before.as_str());
                let input = search_input(&query, before, &scope, &id)?;
                Some((id.clone(), session.client.clone()?, self.shell.generation(&id), input))
            })
            .collect();
        let Some(search) = slot(self) else { return };
        if search.query == query && search.scope == scope {
            return;
        }
        search.query = query.clone();
        search.scope = scope;
        search.matches.clear();
        search._task = None;
        search.pending = !clients.is_empty();
        if !search.pending {
            return;
        }
        search._task = Some(cx.spawn(async move |this, cx| {
            use futures::StreamExt as _;
            cx.background_executor().timer(MESSAGE_DEBOUNCE).await;
            let mut answers: futures::stream::FuturesUnordered<_> = clients
                .into_iter()
                .map(|(env, client, generation, input)| async move {
                    let answer = client.request("orchestration.searchThreads", input).await;
                    (env, generation, answer)
                })
                .collect();
            // Each environment's matches show as it answers; one that fails leaves the others'.
            while let Some((env, generation, answer)) = answers.next().await {
                let settled = answers.is_empty();
                let answer = answer.inspect_err(|error| log::warn!("orchestration.searchThreads: {error}")).ok();
                let matches: Vec<MessageMatch> = (answer.as_ref())
                    .and_then(|value| {
                        let matches = value["matches"].as_array()?;
                        Some(matches.iter().filter_map(|m| MessageMatch::from_json(&env, m)).collect())
                    })
                    .unwrap_or_default();
                let rows = answer.and_then(|value| crate::model::ThreadRows::deserialize(&value).ok());
                let updated = this.update(cx, |this, cx| {
                    if slot(this).is_none_or(|s| s.query != query) {
                        return;
                    }
                    // Older settled matches the shell left out join it, so their hits list too.
                    if let Some(rows) = rows {
                        this.shell.mount_rows(&env, generation, rows);
                    }
                    let Some(search) = slot(this) else { return };
                    search.pending = !settled;
                    search.matches.extend(matches);
                    search.generation += 1;
                    this.notify_sidebar(cx);
                    cx.notify();
                });
                if updated.is_err() {
                    return;
                }
            }
        }));
    }

    /// The project of the open thread or draft.
    /// The open thread's or draft's project, on the target's environment.
    pub(super) fn current_project(&self) -> Option<&ProjectShell> {
        let (env, id) = match self.target.as_ref()? {
            Target::Thread { env, id } => {
                (env, &self.shell.thread(&ScopedId::new(env.clone(), id.as_str()))?.project_id)
            }
            Target::Draft { env, project_id, .. } => (env, project_id),
        };
        self.shell.projects.iter().find(|p| &p.id == id && p.environment_id == *env)
    }

    /// The name of a project's group (`displayName`): the repository's name, not owner/repo.
    pub(super) fn group_name(&self, project: &ProjectShell) -> SharedString {
        let groups = self.project_groups();
        groups
            .iter()
            .find(|g| g.contains_project(project))
            .map_or_else(|| project_name(project), |g| g.display_name.clone().into())
    }

    /// "New thread in...": each project group by its name, with "No project" among them.
    fn new_thread_rows(&self) -> Vec<Row> {
        let groups = self.project_groups();
        let contextual = self.contextual_project();
        let pickable = crate::project_groups::picker(&groups, contextual);
        let entries: Vec<NoProjectEntry> = pickable
            .iter()
            .map(|g| NoProjectEntry {
                scratch: g.members[0].is_scratch,
                pinned: g.pinned,
                preferred: contextual.is_some_and(|p| g.contains_project(p)),
            })
            .collect();
        let mut rows: Vec<(String, Row)> = pickable
            .iter()
            .filter(|g| !g.members[0].is_scratch)
            .map(|g| {
                let target = g.target_for(contextual);
                // `runProject`: the contextual checkout when it is one of the group's, else the target.
                let start = contextual.filter(|p| g.contains_project(p)).unwrap_or(target);
                let run = Run::NewThread(ScopedId::of_project(start));
                let mut row = self.project_row(format!("new-in-{}", target.id), g, target, run);
                if let Some(Description::Location(.., current)) = &mut row.description {
                    *current = self.current_project().is_some_and(|p| g.contains_project(p));
                }
                // `buildNewThreadInProjectItems`: every member's name and path, and the machine.
                row.data_terms.extend(
                    g.members
                        .iter()
                        .flat_map(|m| [m.project.title.clone().into(), m.project.workspace_root.clone().into()]),
                );
                row.data_terms.push(self.location_label(&target.environment_id));
                (g.key.clone(), row)
            })
            .collect();
        if let Some(environment) = self.scratch_environment() {
            let mut row = Row::new(
                NO_PROJECT_KEY,
                RowIcon::Lucide(IconName::MessageSquareDashed),
                "No project",
                Run::ScratchThread(environment),
            );
            row.terms = &["no project", "without project", "none"];
            self.bind_shortcut(&mut row, "chat.newWithoutProject");
            rows.insert(no_project_index(&entries), (NO_PROJECT_KEY.to_owned(), row));
        }
        let Some(palette) = &self.palette else { return Vec::new() };
        let rows = hold_order(&palette.new_thread_order, rows);
        // Jump shortcuts follow the rows as shown, so a pin cannot move a number to another row.
        let mut jump = 0;
        rows.into_iter()
            .map(|(key, mut row)| {
                if key != NO_PROJECT_KEY {
                    jump += 1;
                    if jump <= 9 {
                        self.bind_shortcut(&mut row, &format!("thread.jump.{jump}"));
                    }
                }
                row
            })
            .collect()
    }

    /// The row shows `command`'s shortcut, which runs it while the palette is open.
    fn bind_shortcut(&self, row: &mut Row, command: &str) {
        row.shortcut = self.shortcut(command);
        row.command = Some(SharedString::from(command.to_owned()));
    }

    /// `resolveThreadActionProjectRef`: the open thread's or draft's project, else the most recent.
    fn contextual_project(&self) -> Option<&ProjectShell> {
        self.current_project().or_else(|| inbox::most_recent_project(&self.shell.projects, &self.shell.threads))
    }

    /// `buildProjectActionItems`: a project group's row at its `target` checkout, matched by its
    /// names and path.
    fn project_row(&self, key: String, group: &crate::project_groups::ProjectGroup, p: &ProjectShell, run: Run) -> Row {
        let mut row = Row::new(key, RowIcon::Project(ScopedId::of_project(p)), group.display_name.clone(), run);
        let machine = self.location_machine(&p.environment_id);
        let location = self.location_label(&p.environment_id);
        row.description = Some(Description::Location(machine, location, p.workspace_root.clone().into(), false));
        row.data_terms = vec![p.title.clone().into(), p.workspace_root.clone().into()];
        // `projectPin`: only where this connection may pin every member.
        let members = group.member_refs();
        row.pin = self.can_pin_projects(&members).then_some((members, group.pinned));
        row
    }

    /// Projects as root search results (`buildProjectSearchItems`): a grouped project matches on
    /// every member's name, path and machine, and lists the machines it lives on.
    fn project_search_rows(&self) -> Vec<Row> {
        let groups = self.project_groups();
        let contextual = self.contextual_project();
        let rows = crate::project_groups::picker(&groups, contextual)
            .into_iter()
            .map(|g| {
                let p = g.target_for(contextual);
                let run = Run::Project(g.member_refs(), ScopedId::of_project(p));
                let mut row = self.project_row(format!("project-{}", p.id), g, p, run);
                let mut labels: Vec<SharedString> = Vec::new();
                for member in &g.members {
                    let label = self.location_label(&member.project.environment_id);
                    row.data_terms.extend([
                        member.project.title.clone().into(),
                        member.project.workspace_root.clone().into(),
                        label.clone(),
                    ]);
                    if !labels.contains(&label) {
                        labels.push(label);
                    }
                }
                if g.members.len() > 1 {
                    row.description = Some(Description::Text(labels.join(" · ").into()));
                }
                (g.key.clone(), row)
            })
            .collect();
        let Some(palette) = &self.palette else { return Vec::new() };
        hold_order(&palette.project_order, rows).into_iter().map(|(_, row)| row).collect()
    }

    /// The live shortcut label of `command` (`shortcutCommand`), as tooltips show it.
    fn shortcut(&self, command: &str) -> Option<SharedString> {
        self.shortcut_label(command).map(Into::into)
    }

    /// Actions in `CommandPalette.tsx` order.
    fn action_rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        let current = self.contextual_project();
        let lucide = |key: &'static str, icon: IconName, title: &'static str, run: Run| {
            Row::new(key, RowIcon::Lucide(icon), title, run)
        };
        if let Some(project) = current {
            let mut row = lucide(
                "new-thread-here",
                IconName::SquarePen,
                "New thread in",
                Run::NewThread(ScopedId::of_project(project)),
            );
            row.strong = Some(self.group_name(project));
            row.terms = &["new thread", "chat", "create", "draft"];
            self.bind_shortcut(&mut row, "chat.newLocal");
            rows.push(row);
        }
        if !self.shell.projects.is_empty() {
            let mut row =
                lucide("new-thread-in", IconName::SquarePen, "New thread in...", Run::Open(View::NewThreadIn));
            row.chevron = true;
            row.terms = &["new thread", "project", "pick", "choose", "select"];
            rows.push(row);
        }
        if let Some(environment) = self.scratch_environment() {
            let mut row = lucide(
                "new-thread-without-project",
                IconName::MessageSquareDashed,
                "New thread without a project",
                Run::ScratchThread(environment),
            );
            row.terms = &["new thread", "no project", "without project", "none", "chat"];
            self.bind_shortcut(&mut row, "chat.newWithoutProject");
            rows.push(row);
        }
        if self.server_info().capability("triage") {
            let mut row = lucide("triage", IconName::Bug, "Triage an issue", Run::Triage);
            row.description =
                Some(Description::Text("An agent investigates a T3 Code problem and helps file an issue".into()));
            row.terms = &["triage", "bug", "issue", "report", "broken", "debug", "help", "support"];
            rows.push(row);
        }
        let thread = self.target.as_ref().and_then(Target::thread_key).and_then(|key| self.shell.thread(&key));
        if thread.is_some() && self.thread_find_available() {
            let mut row = lucide("find-current-thread", IconName::TextSearch, "Find in current thread", Run::Find);
            row.terms = &["find", "search", "messages", "plans"];
            row.shortcut = self.shortcut("chat.find");
            rows.push(row);
        }
        if self.target.is_some() && !self.composer_empty {
            let mut row = lucide("copy-prompt-link", IconName::Link, "Copy prompt link", Run::CopyPromptLink(false));
            row.terms = &["copy", "share", "prompt", "link"];
            rows.push(row);
            if self.current_project().and_then(|p| p.repository_identity.as_ref()).and_then(github_repository).is_some()
            {
                let mut row = lucide(
                    "copy-prompt-link-with-context",
                    IconName::Link,
                    "Copy prompt link with repo and model",
                    Run::CopyPromptLink(true),
                );
                row.terms = &["copy", "share", "prompt", "link", "repository", "model"];
                rows.push(row);
            }
        }
        if let Some(target) = self.reference_copy_target() {
            use super::palette_actions::CopyTarget;
            let (title, description, terms): (_, _, &'static [&'static str]) = match target {
                CopyTarget::PullRequest(url) => ("Copy PR link", url, &["copy", "pull request", "link", "reference"]),
                CopyTarget::Thread { title, .. } => {
                    ("Copy thread", title, &["copy", "thread", "thread id", "reference"])
                }
            };
            let mut row = lucide("copy-thread-reference", IconName::Link, title, Run::CopyThread);
            row.description = Some(Description::Text(description.into()));
            row.terms = terms;
            row.shortcut = self.shortcut("thread.copyReference");
            rows.push(row);
        }
        if let Some(thread) = thread {
            if self.pull_request_linking(&ScopedId::of_thread(thread)) {
                let link = Run::LinkPullRequest(ScopedId::of_thread(thread));
                let mut row = lucide("link-pull-request", IconName::Link2, "Link pull request to thread", link);
                row.terms = &["link", "pull request", "pr", "attach", "stack"];
                rows.push(row);
                let mut row = lucide(
                    "open-thread-pull-requests",
                    IconName::Link2,
                    "Show linked pull requests",
                    Run::LinkedPullRequests,
                );
                row.terms = &["pull requests", "linked", "stack", "prs"];
                row.disabled = !self.thread_links_pull_requests();
                rows.push(row);
            }
            let mut row =
                lucide("restart-agent-session", IconName::RotateCcw, "Restart agent session", Run::RestartSession);
            row.terms = &["restart", "reset", "reload", "agent", "session", "skills", "plugins", "mcp"];
            rows.push(row);
        }
        let mut row = lucide(
            "open-file-picker",
            IconName::FileSearch,
            "Go to file",
            Run::FileSearch(super::files::OverlayMode::Files),
        );
        row.terms = &["go to file", "open file", "file picker", "find file", "quick open"];
        row.shortcut = self.shortcut("filePicker.toggle");
        rows.push(row);
        let mut row = lucide(
            "search-project-contents",
            IconName::TextSearch,
            "Search project contents",
            Run::FileSearch(super::files::OverlayMode::Content),
        );
        row.terms = &["search project", "find in files", "grep", "content search", "text search"];
        row.shortcut = self.shortcut("projectSearch.toggle");
        rows.push(row);
        rows.extend(self.new_project_row());
        rows.push(add_project_row());
        let mut row = lucide("change-theme", IconName::Palette, "Change theme", Run::Open(View::Themes));
        row.chevron = true;
        row.terms = &["change theme", "appearance", "colors", "palette"];
        row.shortcut = self.shortcut("theme.select");
        rows.push(row);
        let mut row = lucide("change-appearance", IconName::Monitor, "Change appearance", Run::Open(View::Appearance));
        row.chevron = true;
        row.terms = &["change appearance", "light", "dark", "system", "mode", "toggle"];
        row.shortcut = self.shortcut("appearance.cycle");
        rows.push(row);
        let mut row = lucide("theme-editor", IconName::Palette, "Toggle theme editor", Run::ThemeEditor);
        row.terms = &["theme", "appearance", "colors", "palette", "customize"];
        row.shortcut = self.shortcut("themeEditor.toggle");
        rows.push(row);
        let mut row =
            lucide("pull-requests", IconName::GitPullRequestArrow, "Open pull requests", Run::Page(Page::PullRequests));
        row.terms = &["pull requests", "prs", "pr", "review", "merge", "branch"];
        rows.push(row);
        let environment = self.target.as_ref().map(|target| target.environment().clone());
        let mut row = lucide(
            "use-in-another-app",
            IconName::Plug,
            "Use T3 Code in another app (MCP)",
            Run::UseInAnotherApp(environment),
        );
        row.terms = &[
            "mcp",
            "connector",
            "another app",
            "poke",
            "claude",
            "chatgpt",
            "grok",
            "muse",
            "instinct",
            "openclaw",
            "hermes",
        ];
        rows.push(row);
        let mut usage = lucide("usage", IconName::ChartNoAxesColumn, "Open usage", Run::Page(Page::Usage));
        usage.terms = &["usage", "use", "tokens", "cost", "spend", "limits", "stats", "analytics"];
        usage.shortcut = self.shortcut("usage.open");
        rows.push(usage);
        for (forward, key, title, command) in [
            (true, "workspace-focus:1", "Focus Next Workspace Area", "workspace.focusNext"),
            (false, "workspace-focus:-1", "Focus Previous Workspace Area", "workspace.focusPrevious"),
        ] {
            let mut row = lucide(key, IconName::Monitor, title, Run::WorkspaceFocus(forward));
            row.terms = &["accessibility", "navigation", "focus", "F6"];
            row.shortcut = self.shortcut(command);
            rows.push(row);
        }
        let mut row = lucide("settings", IconName::Settings, "Open settings", Run::Settings("general"));
        row.terms = &["settings", "preferences", "configuration", "keybindings"];
        rows.push(row);
        let groups = self.project_groups();
        let pinnable = |g: &&crate::project_groups::ProjectGroup| self.can_pin_projects(&g.member_refs());
        if let Some(group) =
            self.current_project().and_then(|p| groups.iter().find(|g| g.contains_project(p))).filter(pinnable)
        {
            let (title, icon) =
                if group.pinned { ("Unpin project", IconName::PinOff) } else { ("Pin project", IconName::Pin) };
            let mut row = lucide("project-pin", icon, title, Run::PinProjects(group.member_refs(), !group.pinned));
            row.description = Some(Description::Text(group.display_name.clone().into()));
            row.terms = &["pin", "unpin", "project", "favorite"];
            rows.push(row);
        }
        // `contextualProjectGroup`: the open project's group, else the first.
        let current_group =
            self.current_project().and_then(|p| Some((groups.iter().find(|g| g.contains_project(p))?, p)));
        let contextual = current_group.or_else(|| groups.first().map(|g| (g, g.representative())));
        if let Some((group, project)) = contextual {
            let run = Run::ProjectSettings(ScopedId::of_project(project));
            let mut row = lucide("project-settings", IconName::Folder, "Project settings", run);
            row.description = Some(Description::Text(group.display_name.clone().into()));
            row.terms = &[
                "project",
                "settings",
                "name",
                "icon",
                "scripts",
                "model",
                "workspace",
                "grouping",
                "checkout",
                "remove",
                "t3.json",
            ];
            rows.push(row);
        }
        rows
    }

    /// "Change theme" (`useCommandPaletteThemeItems`): the standard card, the built-in themes,
    /// the saved ones, then the theme environment's published ones.
    fn theme_rows(&self, dark: bool) -> Vec<Row> {
        let current = self.client_settings.theme_choice().owner(dark);
        let library = &self.client_settings.themes;
        let saved = library.custom().iter().chain(library.published_cards());
        let cards = crate::themes::built_in().iter().chain(saved).map(|theme| {
            let only = theme.single_appearance().map(|dark| if dark { "For dark mode" } else { "For light mode" });
            (Some(theme.id.clone()), theme.label.clone(), only)
        });
        std::iter::once((None, "T3 Code".to_owned(), None))
            .chain(cards)
            .map(|(card, label, only)| {
                let key = format!("theme:{}", card.as_deref().unwrap_or("standard"));
                let mut row = Row::new(key, RowIcon::Lucide(IconName::Palette), label, Run::Theme(card.clone()));
                row.description = only.map(|text| Description::Text(text.into()));
                row.terms = &["theme", "appearance"];
                row.time = (current == card).then(|| "Current".to_owned());
                row
            })
            .collect()
    }

    fn appearance_rows(&self) -> Vec<Row> {
        use crate::settings::ThemePreference as Mode;
        let mode = self.client_settings.theme;
        let options = [
            (Mode::System, "System", IconName::Monitor),
            (Mode::Light, "Light", IconName::Sun),
            (Mode::Dark, "Dark", IconName::Moon),
        ];
        options
            .into_iter()
            .map(|(option, label, icon)| {
                let key = format!("appearance:{}", option.id());
                let mut row = Row::new(key, RowIcon::Lucide(icon), label, Run::Appearance(option));
                row.terms = &["appearance", "mode"];
                row.time = (mode == option).then(|| "Current".to_owned());
                row
            })
            .collect()
    }

    fn thread_row(&self, thread: &crate::model::ThreadShell, now: i64) -> Row {
        let mut row = Row::new(
            format!("thread-{}", thread.id),
            RowIcon::Lucide(IconName::MessageSquare),
            thread.title.clone(),
            Run::Thread(ScopedId::of_thread(thread)),
        );
        row.thread = Some(ScopedId::of_thread(thread));
        let project = self.shell.project(&ScopedId::thread_project(thread));
        let project = project.map(|p| p.title.trim()).filter(|title| !title.is_empty());
        // `runtime?.providerInstanceId ?? modelSelection.instanceId`.
        let instance = thread
            .provider_instance_id
            .as_deref()
            .or_else(|| thread.model_selection.as_ref().map(|m| m.instance_id.as_str()));
        let driver = instance.and_then(|i| self.env_info(&thread.environment_id)?.provider(i)).map(|p| &p.driver);
        row.description = Some(Description::Thread(ThreadSubtitle {
            project: project.map(|title| (ScopedId::thread_project(thread), title.to_owned().into())),
            location: self.location_label(&thread.environment_id),
            branch: thread.branch.as_deref().map(str::trim).filter(|b| !b.is_empty()).map(|b| b.to_owned().into()),
            worktree: thread.worktree_path.as_deref().is_some_and(|path| !path.trim().is_empty()),
            provider: driver.and_then(|driver| crate::assets::provider_icon(driver)),
            current: self.route_key() == Some(thread.thread_ref()),
        }));
        row.time = crate::timestamp::parse_ms(thread.latest_user_message_at.as_deref().unwrap_or(&thread.updated_at))
            .map(|at| ago(now - at));
        row
    }

    /// `ThreadRowContext`: the project, and the machine when there are several, for a row whose
    /// second line shows a message snippet instead.
    fn thread_context(&self, thread: &crate::model::ThreadShell) -> Option<SharedString> {
        let project = self.shell.project(&ScopedId::thread_project(thread)).map(|p| p.title.trim());
        let environment = (self.sessions().len() > 1).then(|| self.location_label(&thread.environment_id));
        let parts: Vec<&str> = project.into_iter().chain(environment.as_deref()).filter(|p| !p.is_empty()).collect();
        (!parts.is_empty()).then(|| parts.join(" · ").into())
    }

    fn thread_use(&self, thread: &crate::model::ThreadShell) -> search::ThreadUse {
        search::ThreadUse::of(thread, self.local_opened_at(thread))
    }

    /// Threads the palette lists.
    fn palette_threads(&self) -> impl Iterator<Item = &crate::model::ThreadShell> {
        self.shell.threads.iter().filter(|t| listed_in_palette(t))
    }

    fn palette_groups(&self, query: &str) -> Vec<Group> {
        let Some(palette) = &self.palette else { return Vec::new() };
        if browses(palette, query) {
            return self.browse_groups(query);
        }
        let (actions_only, search_query) = actions_filter(query);
        let tokens = search::fold_query(search_query);
        // A `>` query matches no submenu row, so it searches the root's actions.
        let scope = |rows| if actions_only { Vec::new() } else { rank_rows(rows, &tokens) };
        let scoped = match palette.view {
            View::NewThreadIn => {
                let mut rows = self.new_thread_rows();
                // Every project archived and no "No project" home leaves only adding one.
                if rows.is_empty() {
                    rows.push(add_project_row());
                }
                vec![Group { label: "Projects", rows: scope(rows) }]
            }
            View::Themes => vec![Group { label: "Change theme", rows: scope(self.theme_rows(palette.dark)) }],
            View::Appearance => vec![Group { label: "Change appearance", rows: scope(self.appearance_rows()) }],
            View::AddProject => match self.add_project_entry() {
                AddProjectEntry::Connect => vec![Group { label: "", rows: scope(vec![connect_environment_row()]) }],
                AddProjectEntry::Choose => {
                    vec![Group { label: "Environments", rows: scope(self.add_project_environment_rows()) }]
                }
                AddProjectEntry::Sources(environment) => {
                    vec![Group { label: "Sources", rows: scope(self.add_project_source_rows(environment)) }]
                }
            },
            View::AddProjectSources => {
                let rows =
                    palette.add_environment.clone().map(|env| self.add_project_source_rows(env)).unwrap_or_default();
                vec![Group { label: "Sources", rows: scope(rows) }]
            }
            // The repository field lists nothing; the destination lists folders while it is a path.
            View::Clone => return Vec::new(),
            // The query is the name, so the flow's rows are never filtered.
            View::NewProject => return self.new_project_groups(query),
            View::Root | View::Browse => return self.search_groups(query, true),
        };
        if submenu_falls_back(&scoped, query) {
            // Submenus search no messages (`threadSearchQuery` is empty there).
            return self.search_groups(query, false);
        }
        scoped
    }

    /// The root view's results for `query`: everything, or only the actions after a `>`.
    fn search_groups(&self, query: &str, messages: bool) -> Vec<Group> {
        let Some(palette) = &self.palette else { return Vec::new() };
        let now = super::sidebar::now_ms();
        let (actions_only, search_query) = actions_filter(query);
        let tokens = search::fold_query(search_query);
        if actions_only {
            return vec![Group { label: "Actions", rows: rank_rows(self.action_rows(), &tokens) }];
        }
        let Some(ranking) = &palette.threads else { return Vec::new() };
        if tokens.is_empty() {
            let recent = ranking.strict.rows.iter().map(|(row, _)| row.clone()).collect();
            return vec![
                Group { label: "Actions", rows: self.action_rows() },
                Group { label: "Recent Threads", rows: recent },
            ];
        }
        let action_rows = self.action_rows();
        let submenus = action_rows
            .iter()
            .filter_map(|parent| {
                let children = match parent.run {
                    Run::Open(View::NewThreadIn) => self.new_thread_rows(),
                    Run::Open(View::Themes) => self.theme_rows(palette.dark),
                    Run::Open(View::Appearance) => self.appearance_rows(),
                    _ => return None,
                };
                Some((parent, children))
            })
            .collect();
        let suggestions = nested_rows(&action_rows, submenus, &tokens);
        let project_rows = self.project_search_rows();
        let mut settings = setting_rows();
        settings.retain(|row| !matches!(row.run, Run::Setting(item) if !self.setting_offered(item)));
        let linked = palette.linked.as_ref().filter(|linked| linked.query == query).map(LinkedSearch::rows);
        let rank = |typos: bool| {
            let actions = score_rows(action_rows.clone(), &tokens, typos);
            let suggestions = score_rows(suggestions.clone(), &tokens, typos);
            let threads = match (typos, &ranking.typos, &linked) {
                (_, _, Some(rows)) => score_rows(rows.clone(), &tokens, typos),
                (true, Some(ranked), None) => ranked.rows.clone(),
                _ => ranking.strict.rows.clone(),
            };
            let projects = score_rows(project_rows.clone(), &tokens, typos);
            let settings = score_rows(settings.clone(), &tokens, typos);
            [
                ("Actions", actions),
                ("Suggestions", suggestions),
                ("Threads", threads),
                ("Projects", projects),
                ("Settings", settings),
            ]
        };
        let mut ranked = rank(false);
        // Linked threads stand in for thread search, so its strong match does not count.
        let strong_thread = ranking.strict.strong && linked.is_none();
        if !strong_thread && !ranked.iter().any(|(_, rows)| rows.iter().any(|(_, strong)| *strong)) {
            ranked = rank(true);
        }
        let threads_matched = !ranked[2].1.is_empty();
        let mut groups: Vec<(u8, usize, Group)> = ranked
            .into_iter()
            .enumerate()
            .filter(|(_, (_, rows))| !rows.is_empty())
            .map(|(order, (label, rows))| {
                let limit = match label {
                    "Threads" => THREAD_LIMIT,
                    "Suggestions" => SECONDARY_LIMIT,
                    _ if threads_matched => SECONDARY_LIMIT,
                    _ => usize::MAX,
                };
                let mut rows: Vec<Row> = rows.into_iter().map(|(row, _)| row).collect();
                // Secondary rows stay below the rest; the sort is stable, so score order holds.
                rows.sort_by_key(|row| row.secondary);
                rows.truncate(limit);
                let best = rows
                    .iter()
                    .map(|r| search::class(r.quality.min(if label == "Threads" { 1.49 } else { f32::MAX })))
                    .max()
                    .unwrap_or(0);
                (best, order, Group { label, rows })
            })
            .collect();
        groups.sort_by_key(|(best, order, _)| (std::cmp::Reverse(*best), *order));
        let mut groups: Vec<Group> = groups.into_iter().map(|(_, _, g)| g).collect();
        let shown = palette.message_order.order.as_ref().filter(|_| messages && linked.is_none());
        let shown = shown.map_or(&[][..], |order| &order.keys[..order.shown]);
        let message_rows: Vec<Row> = shown
            .iter()
            .filter_map(|key| palette.messages.matches.iter().find(|m| m.is_from(key)))
            .filter_map(|m| {
                let thread = self.palette_threads().find(|t| m.is_from_thread(t))?;
                let mut row = self.thread_row(thread, now);
                let at = m.at.as_deref().unwrap_or_default();
                row.key = format!("message-{}-{}-{at}", m.environment_id, m.thread_id).into();
                row.run = Run::FindIn {
                    thread: ScopedId::of_thread(thread),
                    query: super::find::query_for_message(&m.snippet, query),
                    message_id: m.message_id.clone(),
                };
                row.context = self.thread_context(thread);
                let ranges = search::token_ranges(&m.snippet, query);
                row.description = Some(Description::Snippet(m.user, search::split_ranges(&m.snippet, &ranges)));
                row.time = m.at.as_deref().and_then(crate::timestamp::parse_ms).map(|at| ago(now - at));
                Some(row)
            })
            .collect();
        if !message_rows.is_empty() {
            groups.push(Group { label: "In messages", rows: message_rows });
        }
        groups
    }

    /// What the palette derives from its query and the shell, before it builds rows.
    fn prepare_palette(&mut self, query: &str) {
        self.rank_palette_threads(query);
        self.order_palette_messages();
    }

    /// Ranks the message hits of threads no title match lists (`withoutTitleMatches`), keeping
    /// the rows already shown in their places.
    fn order_palette_messages(&mut self) {
        let Some(palette) = &self.palette else { return };
        let search = &palette.messages;
        let key = (search.query.clone(), search.generation, self.shell.revision);
        if palette.message_order.key == key {
            return;
        }
        let tokens = search::fold_query(&search.query);
        let now = super::sidebar::now_ms();
        let entries = search
            .matches
            .iter()
            .filter_map(|found| {
                let thread = self.palette_threads().find(|t| found.is_from_thread(t))?;
                let titled = self.match_thread_fields(thread, &tokens, false).is_some();
                (!titled).then(|| (found, self.thread_use(thread)))
            })
            .collect();
        let ranked = rank_messages(entries, &tokens, now).into_iter().map(MessageMatch::thread_key).collect();
        let Some(palette) = &mut self.palette else { return };
        let order = next_shown_order(palette.message_order.order.take(), &key.0, ranked, MESSAGE_LIMIT);
        palette.message_order = MessageOrder { key, order: Some(order) };
    }

    /// `scoreThreadSearch` over the thread's title, pull requests, branch and project.
    fn match_thread_fields(
        &self,
        thread: &crate::model::ThreadShell,
        tokens: &[Vec<char>],
        typos: bool,
    ) -> Option<search::Match> {
        let pull_requests = crate::thread_pull_requests::search_entries(thread);
        // Environment names match only when there are several to tell apart.
        let environment = (self.sessions().len() > 1).then(|| self.location_label(&thread.environment_id));
        let subject = search::ThreadSubject {
            id: &thread.id,
            title: &thread.title,
            branch: thread.branch.as_deref(),
            project: self.shell.project(&ScopedId::thread_project(thread)).map(|p| p.title.as_str()),
            environment: environment.as_deref(),
            pull_requests: &pull_requests,
        };
        search::match_thread(tokens, &subject, typos)
    }

    /// `projectEnvironmentLocationById`'s label: "Local" for this machine's server, else the
    /// environment's name.
    pub(super) fn location_label(&self, environment_id: &str) -> SharedString {
        if self.primary_environment().is_some_and(|primary| primary == environment_id) {
            return "Local".into();
        }
        self.env_info(environment_id)
            .and_then(|info| info.environment_label.clone())
            .map_or_else(|| "Remote".into(), SharedString::from)
    }

    /// `EnvironmentMachineIcon` before a remote location; this machine's server shows none.
    fn location_machine(&self, environment_id: &str) -> Option<IconName> {
        if self.primary_environment().is_some_and(|primary| primary == environment_id) {
            return None;
        }
        Some(super::host_picker::machine_icon(self.env_info(environment_id).map_or("server", |i| i.machine_kind())))
    }

    /// A thread result's leading status, and its trailing pull request number, running terminal
    /// (`ThreadRowTrailingStatus`) and remote machine.
    fn palette_thread_slots(
        &self,
        row: &Row,
        query: &str,
        requests: &inbox::ChildRequests,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> ThreadSlots {
        let Some(thread) = row.thread.as_ref().and_then(|id| self.palette_threads().find(|t| id.is_thread(t))) else {
            return ThreadSlots::default();
        };
        let leading = self.render_thread_leading_status(thread, requests, cx);
        let number = self.render_thread_search_number(thread, query, cx);
        let key = format!("{}-{}", thread.environment_id, thread.id);
        let terminal = (!self.terminals.running(&thread.id).is_empty()).then(|| {
            let color = super::pages::terminal_running_color(tokens);
            div()
                .id(SharedString::from(format!("palette-terminal-{key}")))
                .flex_none()
                .flex()
                .child(Icon::new(IconName::Terminal).size(px(12.)).text_color(color))
                .tooltip(|window, cx| {
                    gpui_kit::component::tooltip::Tooltip::new("Terminal process running").build(window, cx)
                })
        });
        let remote = self.location_machine(&thread.environment_id).map(|machine| {
            let label = self.location_label(&thread.environment_id);
            div()
                .id(SharedString::from(format!("palette-remote-{key}")))
                .flex_none()
                .flex()
                .child(Icon::new(machine).size(px(12.)).text_color(tokens.muted_foreground.opacity(0.6)))
                .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(label.clone()).build(window, cx))
        });
        let status = (terminal.is_some() || remote.is_some())
            .then(|| h_flex().flex_none().gap(px(6.)).children(terminal).children(remote));
        let trailing = (number.is_some() || status.is_some())
            .then(|| h_flex().flex_none().gap(px(8.)).children(number).children(status).into_any_element());
        ThreadSlots { leading, trailing }
    }

    /// Ranks the root view's threads for `query` unless the last ranking still holds.
    fn rank_palette_threads(&mut self, query: &str) {
        let Some(palette) = &self.palette else { return };
        if browses(palette, query) || actions_filter(query).0 {
            return;
        }
        let now = super::sidebar::now_ms();
        let key = RankingKey {
            query: query.to_owned(),
            revision: self.shell.revision,
            current: self.target.as_ref().and_then(Target::thread_key),
            minute: now / 60_000,
        };
        if palette.threads.as_ref().is_some_and(|ranking| ranking.key == key) {
            return;
        }
        let tokens = search::fold_query(query);
        let (strict, typos) = if tokens.is_empty() {
            let mut recent: Vec<_> =
                self.palette_threads().filter(|t| !key.current.as_ref().is_some_and(|c| c.is_thread(t))).collect();
            // Recent means used recently: written in or opened, whichever is later.
            recent.sort_by_cached_key(|t| std::cmp::Reverse(self.thread_use(t).recent()));
            let rows = recent.into_iter().take(RECENT_LIMIT).map(|t| (self.thread_row(t, now), false)).collect();
            (RankedThreads { rows, strong: false }, None)
        } else {
            let strict = self.score_threads(&tokens, false, now);
            let typos = (!strict.strong).then(|| self.score_threads(&tokens, true, now));
            (strict, typos)
        };
        if let Some(palette) = &mut self.palette {
            palette.threads = Some(ThreadRanking { key, strict, typos });
        }
    }

    /// Scores every listed thread, then builds rows for the first [`THREAD_LIMIT`] only.
    fn score_threads(&self, tokens: &[Vec<char>], typos: bool, now: i64) -> RankedThreads {
        let current = self.route_key();
        let mut scored: Vec<(&crate::model::ThreadShell, search::Match, f32)> = self
            .palette_threads()
            .filter_map(|thread| {
                let found = self.match_thread_fields(thread, tokens, typos)?;
                let rank = if current == Some(thread.thread_ref()) {
                    -1.
                } else {
                    found.quality * self.thread_use(thread).boost(now)
                };
                Some((thread, found, rank))
            })
            .collect();
        scored.sort_by(|a, b| {
            (b.2 >= 0.).cmp(&(a.2 >= 0.)).then_with(|| search::compare_ranked((&a.1, a.2), (&b.1, b.2)))
        });
        let strong = scored.iter().any(|(_, found, _)| found.strong);
        let rows = scored
            .into_iter()
            .take(THREAD_LIMIT)
            .map(|(thread, found, _)| {
                let mut row = self.thread_row(thread, now);
                row.quality = found.quality;
                (row, found.strong)
            })
            .collect();
        RankedThreads { rows, strong }
    }

    fn browse_groups(&self, query: &str) -> Vec<Group> {
        // A relative path lists nothing until there is a project to start from.
        if self.palette.is_none() || self.relative_needs_project(query) {
            return Vec::new();
        }
        let (directory, leaf) = split_browse(query);
        // A clone's own folder stays at the end of its destination while the user browses.
        let pinned = self.pinned_clone_folder();
        let case_sensitive = !self.browse_on_windows();
        let filter = pinned.as_deref().map_or(leaf, |pinned| clone_paths::pinned_filter(leaf, pinned, case_sensitive));
        let mut rows = Vec::new();
        if filter.is_empty()
            && let Some(parent) = up_path(directory)
        {
            let up = match pinned.as_deref() {
                Some(pinned) => clone_paths::clone_destination_path(&parent, pinned),
                None => parent,
            };
            rows.push(Row::new("browse-up", RowIcon::Lucide(IconName::CornerLeftUp), "..", Run::BrowseTo(up)));
        }
        if let Some(browse) = self.current_browse(directory) {
            for (name, _) in filter_entries(&browse.entries, filter) {
                let to = match pinned.as_deref() {
                    Some(pinned) => clone_paths::clone_destination_browse_path(directory, name, pinned, case_sensitive),
                    None => browse_into(directory, name),
                };
                rows.push(Row::new(
                    format!("dir-{name}"),
                    RowIcon::Lucide(IconName::Folder),
                    name.clone(),
                    Run::BrowseTo(to),
                ));
            }
        }
        let label = if pinned.is_some() { "Select where to clone" } else { "Directories" };
        vec![Group { label, rows }]
    }

    /// `pinnedCloneDirectoryName`: the clone's own folder at its destination step.
    fn pinned_clone_folder(&self) -> Option<String> {
        let flow = self.palette.as_ref()?.clone.as_ref()?;
        let target = flow.confirm.as_ref()?;
        let repository = target.repository.as_ref().map_or(target.remote_url.as_str(), |(name, _)| name.as_str());
        let name =
            clone_paths::clone_directory_leaf_name(&target.remote_url, self.ghq_layout(&flow.environment), repository);
        (!name.is_empty()).then_some(name)
    }

    /// `ghqCloneLayout` on the environment.
    fn ghq_layout(&self, environment: &str) -> bool {
        self.env_info(environment).is_some_and(|info| info.settings.get("ghqCloneLayout").as_bool() == Some(true))
    }

    /// The browse environment runs Windows, whose folder names match without case.
    fn browse_on_windows(&self) -> bool {
        let environment = self.browse_environment();
        let os = environment.as_deref().and_then(|env| self.env_info(env)).and_then(|info| info.platform_os.as_deref());
        os.is_some_and(is_windows_platform)
    }

    /// `getAddProjectInitialQueryForEnvironment`: the environment's base folder to browse from.
    fn add_project_base(&self, environment: &str) -> String {
        let info = self.env_info(environment);
        let base = info.and_then(|info| info.settings.get("addProjectBaseDirectory").as_str());
        clone_paths::add_project_initial_query(base)
    }

    /// Whether Enter can add the typed path and whether that creates its folder
    /// (`canSubmitBrowsePath`, `willCreateProjectPath`).
    fn browse_submit(&self, query: &str) -> BrowseSubmit {
        let connected = self.browse_environment().is_some_and(|environment| self.env_client(&environment).is_some());
        let Some(palette) = &self.palette else { return BrowseSubmit::default() };
        let cloning = palette.clone.as_ref().filter(|flow| flow.confirm.is_some());
        let can_clone = cloning.is_none_or(|flow| self.can_clone_on(&flow.environment) && !palette.clone_pending);
        let can_submit = connected && !self.relative_needs_project(query) && can_clone;
        let (directory, leaf) = split_browse(query);
        let browse = self.current_browse(directory);
        let settled = browse.is_some_and(|browse| !browse.loading);
        let case_sensitive = cloning.is_none() || !self.browse_on_windows();
        // A folder that could not be read is one Enter creates.
        let found = browse.is_some_and(|browse| {
            browse.error.is_none()
                && if leaf.is_empty() {
                    browse.parent.is_some()
                } else {
                    browse.entries.iter().any(|(name, _)| clone_paths::names_match(name, leaf, case_sensitive))
                }
        });
        let will_create = can_submit && settled && !query.trim().is_empty() && palette.highlight.is_none() && !found;
        BrowseSubmit { can_submit, will_create }
    }

    /// Tab while browsing (`handleKeyDown`): enters the highlighted folder or "..", else completes
    /// the typed name as far as the folders it matches agree. Returns whether it took the key.
    pub(super) fn palette_tab(&mut self, keystroke: &Keystroke, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if keystroke.key != "tab" || keystroke.modifiers.modified() {
            return false;
        }
        let Some(palette) = &self.palette else { return false };
        let query = palette.input.read(cx).value().to_string();
        if !browses(palette, &query) || self.relative_needs_project(&query) {
            return false;
        }
        let highlighted = palette.highlight.and_then(|index| {
            self.browse_groups(&query).into_iter().flat_map(|group| group.rows).nth(index).map(|row| row.run)
        });
        let (directory, leaf) = split_browse(&query);
        let next = match (highlighted, self.current_browse(directory)) {
            (Some(Run::BrowseTo(path)), _) => Some(path),
            (_, None) => None,
            (_, Some(browse)) => {
                let pinned = self.pinned_clone_folder();
                let case_sensitive = !self.browse_on_windows();
                let filter =
                    pinned.as_deref().map_or(leaf, |pinned| clone_paths::pinned_filter(leaf, pinned, case_sensitive));
                let names: Vec<&str> =
                    filter_entries(&browse.entries, filter).into_iter().map(|(name, _)| name.as_str()).collect();
                // A clone's destination only enters whole folders, keeping its own folder last.
                tab_completion(&names, filter).and_then(|(name, complete)| match (complete, pinned.as_deref()) {
                    (true, Some(pinned)) => {
                        Some(clone_paths::clone_destination_browse_path(directory, &name, pinned, case_sensitive))
                    }
                    (true, None) => Some(browse_into(directory, &name)),
                    (false, Some(_)) => None,
                    (false, None) => Some(format!("{directory}{name}")),
                })
            }
        };
        if let Some(next) = next {
            self.set_palette_query(&next, window, cx);
        }
        true
    }

    /// The path Enter adds while browsing (`CommandPalette.tsx` submit).
    fn browse_submit_path(&self, query: &str) -> Option<String> {
        self.palette.as_ref()?;
        submit_path(self.current_browse(split_browse(query).0), query)
    }

    /// Adds the typed path as a project on the browse environment.
    fn submit_browse_path(&mut self, query: &str, window: &mut Window, cx: &mut Context<Self>) {
        let cloning =
            self.palette.as_ref().and_then(|palette| palette.clone.as_ref()).is_some_and(|flow| flow.confirm.is_some());
        match (self.browse_environment(), self.browse_submit_path(query)) {
            (Some(_), Some(path)) if cloning => self.submit_clone_destination(&path, window, cx),
            (Some(environment), Some(path)) => self.add_project(environment, &path, window, cx),
            _ => {}
        }
    }

    /// The command `keystroke` runs from the palette's search field (`resolveShortcutCommand`).
    fn palette_command(&self, keystroke: &Keystroke) -> Option<String> {
        let context = keymap::ShortcutContext { editable_focus: true, ..keymap::ShortcutContext::default() };
        keymap::resolve(&self.keymap, keystroke, &context).map(str::to_owned)
    }

    fn run_palette_row(&mut self, run: Run, window: &mut Window, cx: &mut Context<Self>) {
        match run {
            Run::Open(view) => return self.push_palette_view(view, window, cx),
            Run::BrowseTo(path) => return self.set_palette_query(&path, window, cx),
            Run::FileSearch(mode) => {
                let query = self.palette_query(cx);
                return self.open_file_overlay(mode, Some(query), window, cx);
            }
            Run::AddProject => return self.start_add_project(window, cx),
            Run::AddProjectIn(environment) => return self.open_add_project_sources(environment, window, cx),
            Run::BrowseIn(environment) => {
                if let Some(palette) = &mut self.palette {
                    palette.add_environment = Some(environment);
                }
                return self.push_palette_view(View::Browse, window, cx);
            }
            Run::CloneFrom(environment, source) => {
                return self.start_add_project_clone(environment, source, window, cx);
            }
            Run::NewProject => return self.open_new_project_flow(window, cx),
            Run::NewProjectIn(environment) => {
                return self.start_new_project(environment.clone(), Some(environment), window, cx);
            }
            Run::NewProjectMachine(environment) => return self.switch_new_project_environment(environment, cx),
            Run::NewProjectPublish => return self.toggle_new_project_publish(cx),
            Run::NewProjectExisting => return self.show_existing_project_sources(window, cx),
            _ => {}
        }
        self.close_palette(window, cx);
        if run.opens_chat() {
            // Settings covers the chat; going to a thread leaves it, as the web's route change does.
            self.settings = None;
        }
        match run {
            Run::NewThread(project) => self.start_draft_in(project.env, project.id, window, cx),
            Run::Thread(thread) => self.select_thread_in(thread.env, thread.id, window, cx),
            Run::ThreadIn(environment, thread_id) => self.select_thread_in(environment, thread_id, window, cx),
            Run::Project(members, project) => self.open_project_group(&members, project, window, cx),
            Run::Settings(section) => self.open_settings(section, cx),
            Run::Setting(item) => self.open_settings_search_result(item, window, cx),
            Run::ProjectSettings(project_id) => self.open_project_settings(&project_id, cx),
            Run::Page(page) => self.open_page(page, cx),
            Run::Find => self.open_thread_find_or_explain(window, cx),
            Run::ScratchThread(environment) => self.start_scratch_thread(environment, None, window, cx),
            Run::Triage => self.start_triage(window, cx),
            Run::CopyPromptLink(with_context) => self.copy_prompt_link(with_context, window, cx),
            Run::CopyThread => self.copy_thread_reference(window, cx),
            Run::RestartSession => self.restart_agent_session(window, cx),
            Run::PinProjects(ids, pinned) => self.set_projects_pinned(&ids, pinned, cx),
            Run::Theme(card) => {
                let mut choice = self.client_settings.theme_choice();
                choice.use_card(card);
                self.set_theme_choice(choice, window, cx);
            }
            Run::Appearance(mode) => {
                let choice = crate::themes::ThemeChoice { mode, ..self.client_settings.theme_choice() };
                self.set_theme_choice(choice, window, cx);
            }
            Run::ThemeEditor => self.toggle_theme_editor(window, cx),
            Run::WorkspaceFocus(forward) => self.move_workspace_focus(forward, window, cx),
            Run::LinkPullRequest(thread) => self.open_link_pull_request(thread, window, cx),
            Run::LinkedPullRequests => self.open_surface(super::right_panel::Surface::PullRequests, window, cx),
            Run::UseInAnotherApp(environment) => self.open_use_in_another_app(environment.as_deref(), window, cx),
            Run::FindIn { thread, query, message_id } => {
                self.select_thread_in(thread.env, thread.id, window, cx);
                self.open_thread_find_on(&query, message_id, window, cx);
            }
            Run::Open(_)
            | Run::BrowseTo(_)
            | Run::FileSearch(_)
            | Run::AddProject
            | Run::AddProjectIn(_)
            | Run::BrowseIn(_)
            | Run::CloneFrom(..)
            | Run::NewProject
            | Run::NewProjectIn(_)
            | Run::NewProjectMachine(_)
            | Run::NewProjectPublish
            | Run::NewProjectExisting => {}
        }
    }

    /// `openProjectFromSearch`: the group's latest thread by the sidebar's order, settled or not,
    /// else a new draft in `project`.
    fn open_project_group(
        &mut self,
        members: &[ScopedId],
        project: ScopedId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let sort_order = self.thread_sort_order();
        match latest_thread(&self.shell.threads, members, &sort_order).map(ScopedId::of_thread) {
            Some(thread) => self.select_thread_in(thread.env, thread.id, window, cx),
            None => self.start_draft_in(project.env, project.id, window, cx),
        }
    }

    /// `sidebarThreadSortOrder`.
    fn thread_sort_order(&self) -> String {
        self.client_settings.values.get("sidebarThreadSortOrder").as_str().unwrap_or_default().to_owned()
    }

    /// `handleAddProjectForEnvironment`: resolve `raw` against the open project, reuse a project
    /// already there (unarchiving it), else `projects.mutate` a new one; then open it.
    pub(super) fn add_project(
        &mut self,
        environment: SharedString,
        raw: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let raw = raw.trim();
        let info = self.env_info(&environment);
        let platform = info.and_then(|info| info.platform_os.as_deref());
        let cwd = self
            .current_project()
            .filter(|project| project.environment_id == environment)
            .map(|project| project.workspace_root.clone());
        let failure = match self.env_client(&environment) {
            None => Err(format!(
                "Environment unavailable: {} is not connected.",
                info.and_then(|info| info.environment_label.as_deref()).unwrap_or("The selected environment")
            )),
            Some(_) if is_windows_absolute(raw) && platform.is_some_and(|os| !is_windows_platform(os)) => {
                Err("Failed to add project: Windows-style paths are only supported on Windows.".to_owned())
            }
            Some(_) if is_explicit_relative(raw) && cwd.is_none() => {
                Err("Failed to add project: Relative paths require an active project.".to_owned())
            }
            Some(client) => Ok(client),
        };
        let client = match failure {
            Ok(client) => client,
            Err(message) => {
                self.error = Some(message.into());
                cx.notify();
                return;
            }
        };
        let path = resolve_project_path(raw, cwd.as_deref());
        if path.is_empty() {
            return;
        }
        self.close_palette(window, cx);
        self.settings = None;
        let key = crate::project_groups::normalize_path(&path);
        let existing = self.shell.projects.iter().find(|p| {
            p.environment_id == environment && crate::project_groups::normalize_path(&p.workspace_root) == key
        });
        if let Some(existing) = existing {
            let project = ScopedId::of_project(existing);
            if existing.archived_at.is_some() {
                let mutation = json!({"type": "project.update", "commandId": new_id(), "projectId": project.id, "archived": false});
                self.mutate_project(&project.env, mutation, cx);
            }
            // A settled latest thread is left for a new draft.
            let sort_order = self.thread_sort_order();
            let latest = latest_thread(&self.shell.threads, std::slice::from_ref(&project), &sort_order)
                .filter(|thread| thread.settled_override.as_deref() != Some("settled"))
                .map(ScopedId::of_thread);
            match latest {
                Some(thread) => self.select_thread_in(thread.env, thread.id, window, cx),
                None => self.start_draft_in(project.env, project.id, window, cx),
            }
            return;
        }
        let project_id = new_id();
        let mutation = json!({
            "type": "project.create",
            "commandId": new_id(),
            "projectId": project_id,
            "title": project_title_from_path(&path),
            "workspaceRoot": path,
            "createWorkspaceRootIfMissing": true,
            "defaultModelSelection": Value::Null,
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("projects.mutate", mutation).await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                // The shell may not list the new project yet.
                Ok(_) => this.start_draft_in(environment, project_id, window, cx),
                Err(error) => {
                    this.error = Some(format!("Failed to add project: {error}").into());
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// `projects.mutate` on the project's environment.
    pub(super) fn mutate_project(&mut self, environment: &str, mutation: Value, cx: &mut Context<Self>) {
        let Some(client) = self.env_client(environment) else { return };
        cx.spawn(async move |this, cx| {
            if let Err(error) = client.request("projects.mutate", mutation).await {
                let _ = this.update(cx, |this, cx| {
                    this.error = Some(error.to_string().into());
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// The native folder dialog in place of the web's desktop bridge `pickFolder`.
    fn pick_project_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(environment) = self.browse_environment() else { return };
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Add project".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else { return };
            let Some(path) = paths.into_iter().next().map(|p: PathBuf| p.to_string_lossy().into_owned()) else {
                return;
            };
            let _ = this.update_in(cx, |this, window, cx| this.add_project(environment, &path, window, cx));
        })
        .detach();
    }

    /// The native folder dialog picks on this machine, so it is offered while browsing the
    /// primary environment with its server here (`canOpenProjectFromFileManager`).
    fn browse_is_local(&self) -> bool {
        let Some(environment) = self.browse_environment() else { return false };
        let local = |base: &str| base.contains("://127.0.0.1") || base.contains("://localhost");
        self.primary_environment() == Some(&environment)
            && self.env_endpoint(&environment).is_some_and(|endpoint| local(&endpoint.http_base))
    }

    fn palette_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(query) = self.palette.as_ref().map(|p| p.input.read(cx).value().to_string()) else { return };
        self.prepare_palette(&query);
        let Some(palette) = &self.palette else { return };
        let browsing = browses(palette, &query);
        let groups = self.palette_groups(&query);
        // A disabled row is not an item: the arrows, Enter and the pointer pass over it.
        let rows: Vec<&Row> = groups.iter().flat_map(|g| g.rows.iter()).filter(|r| !r.disabled).collect();
        let modifiers = event.keystroke.modifiers;
        let highlight = palette.highlight;
        // `thread.pin` pins the highlighted project row (`CommandPalette.tsx` handleKeyDown).
        if self.is_pin_shortcut(&event.keystroke)
            && let Some((ids, pinned)) =
                marked_row(highlight, browsing, rows.len()).and_then(|i| rows.get(i)).and_then(|r| r.pin.clone())
        {
            self.set_projects_pinned(&ids, !pinned, cx);
            cx.stop_propagation();
            return;
        }
        // The app's own handlers leave these to the palette while it is open.
        if let Some(command) = self.palette_command(&event.keystroke) {
            let shown: Vec<&Row> = groups.iter().flat_map(|g| g.rows.iter()).collect();
            let shortcut = match palette_shortcut(&shown, &command) {
                // Copying the reference closes the palette first; with none to copy it does nothing.
                PaletteShortcut::Pass if command == "thread.copyReference" => match self.reference_copy_target() {
                    Some(_) => PaletteShortcut::Run(Run::CopyThread),
                    None => PaletteShortcut::Swallow,
                },
                shortcut => shortcut,
            };
            match shortcut {
                PaletteShortcut::Pass => {}
                PaletteShortcut::Swallow => {
                    cx.stop_propagation();
                    return;
                }
                PaletteShortcut::Run(run) => {
                    self.run_palette_row(run, window, cx);
                    cx.stop_propagation();
                    return;
                }
            }
        }
        let primary_modifier = if keymap::MAC {
            modifiers.platform && !modifiers.control
        } else {
            modifiers.control && !modifiers.platform
        };
        let repository_step = self.palette.as_ref().is_some_and(|palette| {
            palette.view == View::Clone && palette.clone.as_ref().is_some_and(|flow| flow.confirm.is_none())
        });
        let new_project = self.palette.as_ref().is_some_and(|palette| palette.view == View::NewProject);
        match event.keystroke.key.as_str() {
            "escape" => self.close_palette(window, cx),
            "enter" if repository_step => self.submit_clone_repository(window, cx),
            // Enter creates unless a row is highlighted; Mod+Enter creates regardless.
            "enter" if new_project && (highlight.is_none() || primary_modifier) => self.submit_new_project(window, cx),
            "enter" if new_project => {
                if let Some(run) = highlight.and_then(|i| rows.get(i)).map(|row| row.run.clone()) {
                    self.run_palette_row(run, window, cx);
                }
            }
            "up" | "down" if !rows.is_empty() => {
                let last = rows.len() - 1;
                let next = match (event.keystroke.key.as_str(), highlight) {
                    ("down", None) => 0,
                    ("down", Some(i)) => {
                        if i >= last {
                            0
                        } else {
                            i + 1
                        }
                    }
                    (_, None | Some(0)) => last,
                    (_, Some(i)) => i - 1,
                };
                if let Some(palette) = &mut self.palette {
                    palette.highlight = Some(next);
                    palette.reveal = true;
                }
                cx.notify();
            }
            // Enter adds the typed path unless a row is highlighted; Mod+Enter adds it regardless.
            "enter"
                if browsing && self.browse_submit(&query).can_submit && (highlight.is_none() || primary_modifier) =>
            {
                self.submit_browse_path(&query, window, cx);
            }
            "enter" => {
                let row = match highlight {
                    Some(i) => rows.get(i).map(|r| r.run.clone()),
                    None if browsing => None,
                    None => rows.first().map(|r| r.run.clone()),
                };
                if let Some(run) = row {
                    self.run_palette_row(run, window, cx);
                }
            }
            "backspace" if query.is_empty() && !self.palette.as_ref().is_some_and(|p| p.stack.is_empty()) => {
                self.pop_palette_view(window, cx);
            }
            _ => return,
        }
        cx.stop_propagation();
    }

    pub(super) fn render_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let query = self.palette.as_ref()?.input.read(cx).value().to_string();
        self.prepare_palette(&query);
        let reveal = std::mem::take(&mut self.palette.as_mut()?.reveal);
        let palette = self.palette.as_ref()?;
        let tokens = *crate::theme::tokens(cx);
        let view = palette.view;
        let browsing = browses(palette, &query);
        let in_submenu = !palette.stack.is_empty();
        // Only `highlightedItemValue` paints; Enter still runs the first row when it is `None`.
        let highlight = palette.highlight;
        let groups = self.palette_groups(&query);
        let items = groups.iter().flat_map(|g| g.rows.iter()).filter(|r| !r.disabled).count();
        let marked = marked_row(highlight, browsing, items);
        let input = palette.input.clone();
        let trap = palette.trap.clone();
        let fg = tokens.foreground;
        let muted = tokens.muted_foreground;
        let lead_icon = if in_submenu {
            div()
                .id("palette-back")
                .cursor_pointer()
                .on_click(cx.listener(|this, _, window, cx| this.pop_palette_view(window, cx)))
                .child(
                    Icon::new(if browsing { IconName::FolderPlus } else { IconName::ArrowLeft })
                        .size(px(16.))
                        .text_color(muted.opacity(0.8)),
                )
                .into_any_element()
        } else {
            Icon::new(IconName::Search).size(px(16.)).text_color(muted.opacity(0.8)).into_any_element()
        };
        let submit = browsing.then(|| self.browse_submit(&query));
        let flow = palette.clone.as_ref().filter(|_| view == View::Clone);
        let repository_step = flow.filter(|flow| flow.confirm.is_none());
        let cloning_to = flow.and_then(|flow| flow.confirm.as_ref().map(|target| (flow, target)));
        let pending = palette.clone_pending;
        let new_project = view == View::NewProject;
        let highlighted_run = highlight
            .and_then(|i| groups.iter().flat_map(|g| g.rows.iter()).filter(|r| !r.disabled).nth(i))
            .map(|r| &r.run);
        // `CommandPaletteSubmitButton`: the trailing Add / Create & Add button while browsing, the
        // clone flow's Continue / Lookup and Clone / Create & Clone, and New project's Create.
        let accessory = if new_project {
            self.render_new_project_submit(&query, highlighted_run, &tokens, cx).map(|button| div().child(button))
        } else if let Some(flow) = repository_step {
            let label = if pending { "Working" } else { flow.submit_label() };
            let can_submit = !pending && !query.trim().is_empty() && self.env_client(&flow.environment).is_some();
            let button = Button::new("palette-clone-submit")
                .outline()
                .xsmall()
                .tab_stop(false)
                .label(label)
                .child(super::model_picker::kbd("Enter", &tokens))
                .disabled(!can_submit)
                .tooltip(format!("{} (Enter)", flow.submit_label()))
                .on_click(cx.listener(|this, _, window, cx| this.submit_clone_repository(window, cx)));
            Some(div().absolute().right(px(2.)).top_0().bottom_0().flex().items_center().child(button))
        } else {
            submit.map(|submit| {
                let (label, shortcut) = browse_submit_labels(submit.will_create, highlight.is_some(), keymap::MAC);
                let tooltip = match cloning_to {
                    Some((flow, _)) if !self.can_clone_on(&flow.environment) => {
                        "This connection needs permission to write source control and add projects.".to_owned()
                    }
                    Some(_) => format!("{} ({shortcut})", clone_submit_label(label)),
                    None => format!("{label} ({shortcut})"),
                };
                let label = match cloning_to {
                    Some(_) if pending => "Cloning",
                    Some(_) => clone_submit_label(label),
                    None => label,
                };
                let query = query.clone();
                let button = Button::new("palette-browse-submit")
                    .outline()
                    .xsmall()
                    .tab_stop(false)
                    .label(label)
                    .child(super::model_picker::kbd(&shortcut, &tokens))
                    .disabled(!submit.can_submit)
                    .tooltip(tooltip)
                    .on_click(cx.listener(move |this, _, window, cx| this.submit_browse_path(&query, window, cx)));
                div().absolute().right(px(2.)).top_0().bottom_0().flex().items_center().child(button)
            })
        };
        // `browseInputEndPaddingClass`: room for the button's widest label.
        let input_end = match submit {
            _ if new_project && highlight.is_some() => px(152.),
            _ if repository_step.is_some() || new_project => px(128.),
            Some(submit) if submit.will_create => px(144.),
            Some(_) if highlight.is_some() => px(112.),
            Some(_) => px(88.),
            None => px(0.),
        };
        // `RemoteRepositorySummary`: what the destination step clones.
        let new_project_summary = self.render_new_project_summary(&query, &tokens);
        let summary = cloning_to.map(|(flow, target)| {
            let icon = match flow.source {
                Some(kind) => source_control::definition(kind).mark(16., muted),
                None => Icon::new(IconName::Link).size(px(16.)).text_color(muted).into_any_element(),
            };
            let (title, description) = match &target.repository {
                Some((name, url)) => (name.clone(), url.clone()),
                None => (target.input.clone(), target.remote_url.clone()),
            };
            v_flex()
                .flex_none()
                .p(px(8.))
                .pb_0()
                .child(
                    div()
                        .px(px(8.))
                        .py(px(6.))
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(muted)
                        .child("Repository"),
                )
                .child(
                    h_flex()
                        .min_h(px(32.))
                        .items_center()
                        .gap(px(8.))
                        .px(px(8.))
                        .py(px(6.))
                        .child(div().flex_none().self_start().h(px(20.)).pt(px(2.)).child(icon))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(div().truncate().text_sm().text_color(fg).child(title))
                                .child(div().truncate().text_xs().text_color(muted.opacity(0.85)).child(description)),
                        ),
                )
        });
        let input_row = h_flex().flex_none().h(px(46.)).px(px(8.)).py(px(6.)).child(
            h_flex()
                .size_full()
                .relative()
                .child(div().absolute().left(px(11.)).child(lead_icon))
                .child(
                    div()
                        .flex_1()
                        .pl(px(32.))
                        .pr(input_end)
                        .child(Input::new(&input).appearance(false).text_sm().h(px(34.))),
                )
                .children(accessory),
        );
        let mut index = 0usize;
        // The list's direct children, so the scroll handle can reveal the highlighted row by position.
        let mut list: Vec<AnyElement> = Vec::new();
        let total_rows: usize = groups.iter().map(|g| g.rows.len()).sum();
        let requests = inbox::child_requests(&self.shell.threads);
        for (gi, group) in groups.iter().enumerate() {
            if group.rows.is_empty() {
                continue;
            }
            if !group.label.is_empty() {
                list.push(
                    div()
                        .px(px(8.))
                        .py(px(6.))
                        .when(gi > 0, |el| el.pt(px(12.)))
                        .text_xs()
                        .line_height(px(16.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(muted)
                        .child(group.label)
                        .into_any_element(),
                );
            }
            for row in &group.rows {
                if row.disabled {
                    list.push(self.render_disabled_palette_row(row, &tokens, cx));
                    continue;
                }
                let slots = self.palette_thread_slots(row, &query, &requests, &tokens, cx);
                let highlighted = highlight == Some(index);
                if highlighted && reveal {
                    palette.scroll.scroll_to_item(list.len());
                }
                let mark = RowMark { highlighted, marked: marked == Some(index) };
                list.push(self.render_palette_row(row, index, mark, slots, &tokens, cx));
                index += 1;
            }
        }
        let status: Option<&str> = if browsing {
            let browse = &palette.browse;
            if browse.loading {
                Some("Loading folders...")
            } else if browse.error.is_some() {
                Some("This connection cannot browse host folders.")
            } else {
                None
            }
        } else {
            None
        };
        let empty = (total_rows == 0).then(|| {
            let text = if let Some(flow) = repository_step {
                if flow.source.is_none() {
                    "Enter a Git clone URL and press Enter to continue."
                } else {
                    "Enter a repository path and press Enter to look it up."
                }
            } else if cloning_to.is_some() {
                "Choose a destination path and press Enter to clone."
            } else if browsing && self.relative_needs_project(&query) {
                "Relative paths require an active project."
            } else if submit.is_some_and(|submit| submit.will_create) {
                "Press Enter to create this folder and add it as a project."
            } else if !browsing && view == View::Root && palette.messages.pending {
                "Searching thread messages…"
            } else if actions_filter(&query).0 {
                "No matching actions."
            } else {
                "No matching commands, projects, or threads."
            };
            div().py(px(40.)).text_center().text_sm().text_color(muted).child(text)
        });
        let kbd = |label: SharedString| {
            div()
                .h(px(20.))
                .min_w(px(20.))
                .px(px(4.))
                .rounded(px(4.))
                .flex()
                .items_center()
                .justify_center()
                .bg(fg.opacity(0.08))
                .text_color(fg)
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .child(label)
        };
        let hint = |keys: Vec<SharedString>, label: &'static str| {
            h_flex().gap(px(4.)).children(keys.into_iter().map(kbd)).child(label)
        };
        // While Enter adds the typed path, the input's button says so instead.
        let adds = submit.is_some_and(|submit| submit.can_submit) && highlight.is_none();
        let file_manager = browsing && self.browse_is_local();
        let footer = h_flex()
            .flex_none()
            .justify_between()
            .px(px(16.))
            .py(px(10.))
            .border_t_1()
            .border_color(fg.opacity(0.07))
            .bg(fg.opacity(0.025))
            .text_sm()
            .font_weight(FontWeight::MEDIUM)
            .text_color(muted)
            .child(
                h_flex()
                    .gap(px(12.))
                    .child(hint(vec!["↑".into(), "↓".into()], "Navigate"))
                    .when(!adds, |el| {
                        let label = self
                            .new_project_footer_label(highlighted_run)
                            .unwrap_or_else(|| repository_step.map_or("Select", CloneFlow::submit_label));
                        el.child(hint(vec!["Enter".into()], label))
                    })
                    .when(in_submenu, |el| el.child(hint(vec!["Backspace".into()], "Back")))
                    .child(hint(vec!["Esc".into()], "Close")),
            )
            .when(file_manager, |el| {
                el.child(
                    div()
                        .id("palette-file-manager")
                        .px(px(8.))
                        .py(px(2.))
                        .rounded(px(6.))
                        .cursor_pointer()
                        .text_xs()
                        .hover(|s| s.bg(fg.opacity(0.06)).text_color(fg))
                        .on_click(cx.listener(|this, _, window, cx| this.pick_project_folder(window, cx)))
                        .child("Open in File Explorer"),
                )
            })
            .when(!browsing && palette.messages.pending, |el| el.child(div().text_xs().child("Searching messages…")));
        let height = window.viewport_size().height;
        Some(
            div()
                .id("palette-backdrop")
                .absolute()
                .inset_0()
                .occlude()
                .bg(crate::theme::dialog_backdrop(crate::theme::tokens(cx)))
                .flex()
                .flex_col()
                .items_center()
                .px(px(16.))
                .pt(height * 0.1)
                .on_mouse_down(
                    gpui_kit::MouseButton::Left,
                    cx.listener(|this, _, window, cx| this.close_palette(window, cx)),
                )
                .child(
                    v_flex()
                        .id("palette")
                        .key_context(super::keymap::COMBOBOX_CONTEXT)
                        .w_full()
                        .max_w(px(576.))
                        // `max-h-105` on the popup, inside the viewport's 10vh top and bottom insets.
                        .max_h(px(420.).min(height * 0.8))
                        .overflow_hidden()
                        .rounded(px(16.))
                        .border_1()
                        .border_color(fg.opacity(0.08))
                        // `dialog-glass`: the background at the glass opacity over the dimmed,
                        // blurred page, which reads as the background itself.
                        .bg(tokens.background)
                        .shadow_2xl()
                        .on_mouse_down(gpui_kit::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .capture_key_down(
                            cx.listener(|this, event: &KeyDownEvent, window, cx| this.palette_key(event, window, cx)),
                        )
                        .child(input_row)
                        .children(new_project_summary)
                        .children(summary)
                        .children(
                            status.map(|text| div().px(px(16.)).py(px(12.)).text_sm().text_color(muted).child(text)),
                        )
                        .child(
                            // `max-h-[min(28rem,70vh)]` on the list alone.
                            v_flex()
                                .relative()
                                .flex_1()
                                .min_h_0()
                                .max_h((height * 0.7).min(px(448.)))
                                .child(
                                    v_flex()
                                        .id("palette-list")
                                        .track_scroll(&palette.scroll)
                                        .flex_1()
                                        .min_h_0()
                                        .p(px(8.))
                                        .overflow_y_scroll()
                                        .children(list)
                                        .children(empty),
                                )
                                .vertical_scrollbar(&palette.scroll),
                        )
                        .child(footer)
                        .focus_trap("palette-trap", &trap),
                )
                .into_any_element(),
        )
    }

    fn palette_row_icon(&self, row: &Row, tokens: &Tokens) -> AnyElement {
        match &row.icon {
            RowIcon::Lucide(name) => {
                Icon::new(*name).size(px(16.)).text_color(tokens.muted_foreground).into_any_element()
            }
            RowIcon::Project(project) => match self.shell.project(project) {
                Some(project) => super::sidebar::project_icon(project, tokens, false, 16., self.favicon(project)),
                None => div().size(px(16.)).into_any_element(),
            },
            RowIcon::Host(kind) => source_control::definition(kind).mark(16., tokens.muted_foreground),
        }
    }

    /// `ThreadCommandSubtitle`'s line, its parts split by muted dots.
    fn render_thread_subtitle(&self, subtitle: &ThreadSubtitle, tokens: &Tokens) -> AnyElement {
        let muted = tokens.muted_foreground;
        let dot = || div().flex_none().text_color(muted.opacity(0.5)).child("·");
        let mut parts: Vec<AnyElement> = Vec::new();
        if let Some((project, title)) = &subtitle.project {
            let project = self.shell.project(project);
            let favicon = project.map(|p| super::sidebar::project_icon(p, tokens, false, 12., self.favicon(p)));
            parts.push(
                h_flex()
                    .min_w_0()
                    .gap(px(4.))
                    .children(favicon)
                    .child(div().min_w_0().truncate().child(title.clone()))
                    .child(dot())
                    .child(div().flex_none().child(subtitle.location.clone()))
                    .into_any_element(),
            );
        }
        if let Some(branch) = &subtitle.branch {
            let icon = if subtitle.worktree { IconName::FolderGit2 } else { IconName::Folder };
            parts.push(
                h_flex()
                    .min_w_0()
                    .gap(px(4.))
                    .child(Icon::new(icon).size(px(12.)).flex_none().text_color(muted.opacity(0.7)))
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis_middle()
                            .child(branch.clone()),
                    )
                    .into_any_element(),
            );
        }
        if let Some((path, tint)) = subtitle.provider {
            let color = tint.unwrap_or(tokens.foreground);
            parts.push(svg().path(path).size(px(12.)).flex_none().text_color(color).opacity(0.7).into_any_element());
        }
        if subtitle.current {
            parts.push(div().flex_none().child("Current thread").into_any_element());
        }
        let mut line = h_flex().min_w_0().max_w_full().gap(px(4.));
        for (index, part) in parts.into_iter().enumerate() {
            if index > 0 {
                line = line.child(dot());
            }
            line = line.child(part);
        }
        line.into_any_element()
    }

    /// `DisabledCommandPaletteResultRow`: the icon, title and description at 64% opacity, and a
    /// clone source's Setup Required, which opens Source control settings.
    fn render_disabled_palette_row(&self, row: &Row, tokens: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let setup = row.setup.clone().map(|hint| {
            let warning = tokens.warning;
            let ring = tokens.ring;
            div()
                .id(SharedString::from(format!("{}-setup", row.key)))
                .flex_none()
                .ml_auto()
                .h(px(20.))
                .px(px(5.))
                .flex()
                .items_center()
                .rounded(px(4.))
                .border_1()
                .border_color(warning.opacity(0.32))
                .bg(warning.opacity(0.08))
                .text_size(px(11.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(tokens.warning_foreground)
                .cursor_pointer()
                .tab_index(0)
                .focus_visible(move |style| style.border_color(ring))
                .hover(move |s| s.border_color(warning.opacity(0.4)).bg(warning.opacity(0.16)))
                .child("Setup Required")
                .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(hint.clone()).build(window, cx))
                .on_mouse_down(gpui_kit::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.close_palette(window, cx);
                    this.open_settings("source-control", cx);
                }))
        });
        let description = match &row.description {
            Some(Description::Text(text)) => Some(text.clone()),
            _ => None,
        };
        h_flex()
            .id(row.key.clone())
            .items_center()
            .gap(px(8.))
            .px(px(8.))
            .py(px(6.))
            .opacity(0.64)
            .child(div().flex_none().self_start().h(px(20.)).pt(px(2.)).child(self.palette_row_icon(row, tokens)))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .truncate()
                            .text_sm()
                            .line_height(px(20.))
                            .text_color(tokens.foreground)
                            .child(row.title.clone()),
                    )
                    .children(description.map(|text| {
                        div()
                            .truncate()
                            .text_xs()
                            .line_height(px(16.))
                            .text_color(tokens.muted_foreground.opacity(0.7))
                            .child(text)
                    })),
            )
            .children(setup)
            .into_any_element()
    }

    fn render_palette_row(
        &self,
        row: &Row,
        index: usize,
        RowMark { highlighted, marked }: RowMark,
        slots: ThreadSlots,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let muted = tokens.muted_foreground;
        let icon = self.palette_row_icon(row, tokens);
        let description: Option<AnyElement> = row.description.as_ref().map(|d| match d {
            Description::Text(text) => div().truncate().child(text.clone()).into_any_element(),
            Description::Location(machine, label, path, current) => {
                let dot = || div().flex_none().text_color(muted.opacity(0.5)).child("·");
                h_flex()
                    .min_w_0()
                    .gap(px(4.))
                    .when(*current, |el| el.child(div().flex_none().child("Current")).child(dot()))
                    .children(
                        machine.map(|icon| Icon::new(icon).size(px(12.)).flex_none().text_color(muted.opacity(0.7))),
                    )
                    .child(div().min_w_0().truncate().child(label.clone()))
                    .child(dot())
                    .child(div().min_w_0().flex_1().truncate().child(path.clone()))
                    .into_any_element()
            }
            Description::Snippet(user, parts) => h_flex()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .child(
                    div()
                        .flex_none()
                        .mr(px(4.))
                        .text_color(if *user { tokens.info_foreground } else { tokens.success_foreground })
                        .child(if *user { "You:" } else { "Agent:" }),
                )
                .children(parts.iter().map(|(text, bold)| {
                    div()
                        .flex_none()
                        .when(*bold, |el| el.font_weight(FontWeight::SEMIBOLD).text_color(tokens.foreground))
                        .child(text.clone())
                }))
                .into_any_element(),
            Description::Thread(subtitle) => self.render_thread_subtitle(subtitle, tokens),
        });
        let run = row.run.clone();
        let pin_shortcut = self.shortcut_label("thread.pin");
        // `CommandItem`: centred, with the icon held to the title line (`CommandPaletteTitleIcon`)
        // and the pin, time, shortcut and chevron beside the text, in that order.
        h_flex()
            .id(row.key.clone())
            .items_center()
            .gap(px(8.))
            .px(px(8.))
            .py(px(6.))
            .rounded(px(4.))
            .cursor_pointer()
            .when(highlighted, |el| el.bg(tokens.foreground.opacity(0.04)))
            // The pointer highlights a row by moving, not by a row appearing under it.
            .on_mouse_move(cx.listener(move |this, _: &gpui_kit::MouseMoveEvent, _, cx| {
                if let Some(palette) = &mut this.palette
                    && palette.highlight != Some(index)
                {
                    palette.highlight = Some(index);
                    cx.notify();
                }
            }))
            .on_click(
                cx.listener(move |this, _: &ClickEvent, window, cx| this.run_palette_row(run.clone(), window, cx)),
            )
            .child(div().flex_none().self_start().h(px(20.)).pt(px(2.)).child(icon))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        h_flex().h(px(20.)).gap(px(8.)).child(
                            h_flex()
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_sm()
                                .line_height(px(20.))
                                .text_color(tokens.foreground)
                                .children(slots.leading.map(|leading| div().flex_none().mr(px(6.)).child(leading)))
                                .child(
                                    div()
                                        .when(row.strong.is_none(), |el| el.min_w_0().truncate())
                                        .when(row.strong.is_none() && row.context.is_none(), gpui_kit::Styled::flex_1)
                                        .child(row.title.clone()),
                                )
                                .children(row.context.clone().map(|context| {
                                    div()
                                        .flex_none()
                                        .max_w(relative(0.45))
                                        .ml(px(6.))
                                        .truncate()
                                        .text_xs()
                                        .text_color(muted.opacity(0.7))
                                        .child(context)
                                }))
                                .children(row.strong.clone().map(|strong| {
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .ml(px(4.))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(strong)
                                })),
                        ),
                    )
                    .children(
                        description
                            .map(|d| div().text_xs().line_height(px(16.)).text_color(muted.opacity(0.7)).child(d)),
                    ),
            )
            .children(slots.trailing)
            .children(row.pin.clone().filter(|(_, pinned)| *pinned || marked).map(|(ids, pinned)| {
                // `ProjectPinButton`: shown while pinned or highlighted; a pinned one offers to unpin on hover.
                let group: SharedString = format!("pin-{}", row.key).into();
                let action = format!("{} {}", if pinned { "Unpin" } else { "Pin" }, row.title);
                let title: SharedString = match &pin_shortcut {
                    Some(shortcut) => format!("{action} ({shortcut})").into(),
                    None => action.into(),
                };
                let glyph = |name: IconName| Icon::new(name).size(px(14.)).text_color(muted);
                div()
                    .id(group.clone())
                    .group(group.clone())
                    .flex_none()
                    .size(px(24.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.))
                    .cursor_pointer()
                    .hover(|s| s.bg(tokens.foreground.opacity(0.04)))
                    .tooltip(move |window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new(title.clone()).build(window, cx)
                    })
                    .child(
                        div()
                            .when(pinned, |el| el.group_hover(group.clone(), gpui_kit::Styled::invisible))
                            .child(glyph(IconName::Pin)),
                    )
                    .when(pinned, |el| {
                        el.child(
                            div()
                                .absolute()
                                .invisible()
                                .group_hover(group, gpui_kit::Styled::visible)
                                .child(glyph(IconName::PinOff)),
                        )
                    })
                    .on_mouse_down(gpui_kit::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        // The palette stays open, as the web's pin button leaves it.
                        cx.stop_propagation();
                        this.set_projects_pinned(&ids, !pinned, cx);
                    }))
            }))
            .children(row.time.clone().map(|time| {
                div().flex_none().min_w(px(48.)).text_right().text_xs().text_color(muted.opacity(0.7)).child(time)
            }))
            .children(row.shortcut.clone().map(|shortcut| {
                div()
                    .flex_none()
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(tokens.sidebar.secondary_label)
                    .child(shortcut)
            }))
            .when(row.checked, |el| {
                el.child(
                    div().flex_none().child(Icon::new(IconName::Check).size(px(16.)).text_color(muted.opacity(0.7))),
                )
            })
            .children(row.toggle.map(|on| {
                let picker =
                    matches!(row.run, Run::NewProjectPublish).then(|| self.render_publish_host_picker(tokens, cx));
                h_flex().flex_none().gap(px(8.)).children(picker.flatten()).child(toggle_box(on, tokens))
            }))
            .when(row.chevron, |el| {
                el.child(
                    div()
                        .flex_none()
                        .mr(px(-2.))
                        .child(Icon::new(IconName::ChevronRight).size(px(16.)).text_color(muted.opacity(0.7))),
                )
            })
            .into_any_element()
    }
}

/// `buildThreadActionItems`: unarchived and not a subagent or side chat; a fork is listed.
fn listed_in_palette(thread: &crate::model::ThreadShell) -> bool {
    thread.is_listed() && thread.archived_at.is_none() && thread.deleted_at.is_none()
}

/// `formatRelativeTimeLabel`: "just now", "5m ago", "3h ago", "2d ago".
fn ago(elapsed_ms: i64) -> String {
    match inbox::format_relative(elapsed_ms) {
        now if now == "now" => "just now".into(),
        label => format!("{label} ago"),
    }
}

/// The folder above `directory` (which ends with a separator), if any.
fn up_path(directory: &str) -> Option<String> {
    let trimmed = directory.trim_end_matches(['/', '\\']);
    let at = trimmed.rfind(['/', '\\'])?;
    let parent = &trimmed[..=at];
    (!parent.is_empty() && trimmed != "~").then(|| parent.to_owned())
}

/// Scores rows on their title (plus the bold part); unmatched rows drop out.
fn score_rows(rows: Vec<Row>, tokens: &[Vec<char>], typos: bool) -> Vec<(Row, bool)> {
    let mut scored: Vec<(Row, bool)> = rows
        .into_iter()
        .filter_map(|mut row| {
            let title = row_name(&row);
            // The title names the item; its terms are keywords, the best one counts.
            let found = std::iter::once(title.as_str())
                .chain(row.terms.iter().copied())
                .chain(row.data_terms.iter().map(SharedString::as_ref))
                .filter_map(|text| search::match_fields(tokens, &[Weighted::new(&Field::new(text), 1.)], typos))
                .max_by(|a, b| a.quality.total_cmp(&b.quality))?;
            row.quality = found.quality;
            Some((row, found.strong))
        })
        .collect();
    scored.sort_by(|a, b| {
        let class = |r: &Row| search::class(r.quality);
        class(&b.0)
            .min(2)
            .cmp(&class(&a.0).min(2))
            .then(class(&b.0).cmp(&class(&a.0)))
            .then(b.0.quality.total_cmp(&a.0.quality))
    });
    scored
}

/// `appendBrowsePathSegment`: the folder `name` inside `directory`, with its separator.
fn browse_into(directory: &str, name: &str) -> String {
    let separator = if directory.ends_with('\\') { "\\" } else { "/" };
    format!("{directory}{name}{separator}")
}

/// `getFilesystemBrowseTabCompletion`: the one folder matching `leaf` (complete), or the longest
/// prefix the matching folders share, ignoring case, when it adds to `leaf`.
fn tab_completion(names: &[&str], leaf: &str) -> Option<(String, bool)> {
    let (first, rest) = names.split_first()?;
    if rest.is_empty() {
        return Some(((*first).to_owned(), true));
    }
    let mut prefix: Vec<char> = first.chars().collect();
    for name in rest {
        let shared = prefix.iter().zip(name.chars()).take_while(|(a, b)| a.to_lowercase().eq(b.to_lowercase())).count();
        prefix.truncate(shared);
    }
    let name: String = prefix.into_iter().collect();
    let lower = name.to_lowercase();
    let extends = lower.chars().count() > leaf.to_lowercase().chars().count();
    (extends && names.iter().all(|n| n.to_lowercase().starts_with(&lower))).then_some((name, false))
}

/// Whether Enter can add the typed path, and whether adding it creates its folder.
#[derive(Clone, Copy, Default)]
struct BrowseSubmit {
    can_submit: bool,
    will_create: bool,
}

/// `submitActionLabel` and `addShortcutLabel`: a highlighted folder leaves Enter to it, so adding
/// takes the primary modifier.
fn browse_submit_labels(will_create: bool, highlighted: bool, mac: bool) -> (&'static str, String) {
    let label = if will_create { "Create & Add" } else { "Add" };
    let shortcut =
        if highlighted { format!("{} Enter", if mac { "\u{2318}" } else { "Ctrl" }) } else { "Enter".to_owned() };
    (label, shortcut)
}

/// The clone destination's words for [`browse_submit_labels`]' label (`submitActionLabel`).
fn clone_submit_label(label: &str) -> &'static str {
    if label == "Create & Add" { "Create & Clone" } else { "Clone" }
}

/// What a shortcut pressed in the palette does (`handleKeyDown`).
#[derive(Clone)]
enum PaletteShortcut {
    /// Not the palette's: it goes on to the search field.
    Pass,
    /// Taken, doing nothing.
    Swallow,
    Run(Run),
}

/// A jump or new-thread shortcut runs the shown row bound to it; a jump with none is still taken.
fn palette_shortcut(rows: &[&Row], command: &str) -> PaletteShortcut {
    let jump = command.starts_with("thread.jump.");
    if !jump && !matches!(command, "chat.new" | "chat.newLocal" | "chat.newWithoutProject") {
        return PaletteShortcut::Pass;
    }
    match rows.iter().find(|row| row.command.as_deref() == Some(command)) {
        Some(row) if row.disabled => PaletteShortcut::Swallow,
        Some(row) => PaletteShortcut::Run(row.run.clone()),
        None if jump => PaletteShortcut::Swallow,
        None => PaletteShortcut::Pass,
    }
}

/// `isExplicitRelativePath`.
fn is_explicit_relative(path: &str) -> bool {
    matches!(path, "." | "..") || ["./", "../", ".\\", "..\\"].iter().any(|prefix| path.starts_with(prefix))
}

/// `isWindowsAbsolutePath`: a drive or UNC path.
fn is_windows_absolute(path: &str) -> bool {
    path.starts_with("\\\\") || crate::project_groups::is_drive_path(path)
}

/// `isWindowsPlatform`.
fn is_windows_platform(os: &str) -> bool {
    os.get(..3).is_some_and(|start| start.eq_ignore_ascii_case("win"))
}

/// An absolute path's root, separator and segments (`splitAbsolutePath`).
fn split_absolute(path: &str) -> Option<(String, &'static str, Vec<&str>)> {
    fn segments<'p>(rest: &'p str, separators: &[char]) -> Vec<&'p str> {
        rest.split(separators).filter(|segment| !segment.is_empty()).collect()
    }
    if crate::project_groups::is_drive_path(path) {
        let rest = path.get(3..).unwrap_or_default();
        return Some((format!("{}\\", &path[..2]), "\\", segments(rest, &['\\', '/'])));
    }
    if path.starts_with("\\\\") {
        let mut parts = segments(path, &['\\', '/']).into_iter();
        let (server, share) = (parts.next()?, parts.next()?);
        return Some((format!("\\\\{server}\\{share}\\"), "\\", parts.collect()));
    }
    let rest = path.strip_prefix('/')?;
    Some(("/".to_owned(), "/", segments(rest, &['/'])))
}

/// `resolveProjectPathForDispatch`: an explicit relative path joins `cwd`; any path loses its
/// trailing separators.
fn resolve_project_path(value: &str, cwd: Option<&str>) -> String {
    use crate::project_groups::trim_trailing_separators;
    let value = value.trim();
    let base = cwd.filter(|_| is_explicit_relative(value)).map(|cwd| trim_trailing_separators(cwd.trim()));
    let Some((root, separator, mut segments)) = base.as_deref().and_then(split_absolute) else {
        return trim_trailing_separators(value);
    };
    for segment in value.split(['/', '\\']) {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            segment => segments.push(segment),
        }
    }
    trim_trailing_separators(&format!("{root}{}", segments.join(separator)))
}

/// `sortThreads(..)[0]`: the newest unarchived thread of `projects` (on their environments) by
/// the sidebar's thread order, settled ones included; a tie goes to the larger id.
fn latest_thread<'a>(
    threads: &'a [crate::model::ThreadShell],
    projects: &[ScopedId],
    sort_order: &str,
) -> Option<&'a crate::model::ThreadShell> {
    let parse = crate::timestamp::parse_ms;
    let sorted_at = |t: &crate::model::ThreadShell| match sort_order {
        "created_at" => parse(&t.created_at).or_else(|| parse(&t.updated_at)),
        _ => t
            .latest_user_message_at
            .as_deref()
            .and_then(parse)
            .or_else(|| parse(&t.updated_at))
            .or_else(|| parse(&t.created_at)),
    };
    threads
        .iter()
        .filter(|t| listed_in_palette(t) && projects.iter().any(|p| p.env == t.environment_id && p.id == t.project_id))
        .max_by(|a, b| sorted_at(a).cmp(&sorted_at(b)).then_with(|| a.id.cmp(&b.id)))
}

/// The "Add project" action (`addProjectAction`).
fn add_project_row() -> Row {
    let mut row = Row::new("add-project", RowIcon::Lucide(IconName::FolderPlus), "Add project", Run::AddProject);
    row.terms = &["add project", "folder", "directory", "browse", "environment"];
    row
}

/// The "Add project" entry when no environment can add one.
fn connect_environment_row() -> Row {
    let run = Run::Settings("connections");
    let mut row = Row::new("add-project-connect", RowIcon::Lucide(IconName::Settings), "Connect an environment", run);
    row.description = Some(Description::Text("No connected environment can add a project".into()));
    row.terms = &["connect", "environment", "connection", "settings"];
    row
}

/// `buildEnvironmentItem`: an environment's row, which runs `run`.
fn environment_row(option: AddEnvironment, key: String, run: Run) -> Row {
    let mut row = Row::new(key, RowIcon::Lucide(option.machine), option.label.clone(), run);
    let description = if option.primary { "This device".into() } else { option.id.clone() };
    row.description = Some(Description::Text(description));
    row.data_terms = vec![option.label.into(), option.id];
    if option.primary {
        row.terms = &["this device"];
    }
    row
}

/// A toggle row's checkbox; the row takes the clicks and keys, so the box is no tab stop.
fn toggle_box(on: bool, t: &Tokens) -> AnyElement {
    div()
        .size(px(16.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.))
        .border_1()
        .map(|el| if on { el.bg(t.primary).border_color(t.primary) } else { el.bg(t.background).border_color(t.input) })
        .when(on, |el| el.child(Icon::new(IconName::Check).size(px(12.)).text_color(t.primary_foreground)))
        .into_any_element()
}

/// The "Local folder" source of `environment`.
fn local_folder_row(environment: SharedString) -> Row {
    let key = format!("add-project-local:{environment}");
    let mut row = Row::new(key, RowIcon::Lucide(IconName::FolderPlus), "Local folder", Run::BrowseIn(environment));
    row.description = Some(Description::Text("Browse a folder on disk".into()));
    row.terms = &["local", "folder", "directory", "browse"];
    row
}

/// An environment a project can be added on (`AddProjectEnvironmentOption`).
struct AddEnvironment {
    id: SharedString,
    label: String,
    primary: bool,
    machine: IconName,
}

/// `AddProjectEntry`: where "Add project" starts.
#[derive(Debug, PartialEq)]
enum AddProjectEntry {
    /// No connected environment: Connections.
    Connect,
    /// Several environments to choose from.
    Choose,
    /// The one environment's sources.
    Sources(SharedString),
}

/// `resolveAddProjectEntry` over the connected environments, in their listed order.
fn add_project_entry(environments: &[SharedString]) -> AddProjectEntry {
    match environments {
        [] => AddProjectEntry::Connect,
        [only] => AddProjectEntry::Sources(only.clone()),
        _ => AddProjectEntry::Choose,
    }
}

/// `resolveEnvironmentOptionLabel`: this device keeps its own name unless it is a generic
/// "Local", else reads "This device"; another environment its name, else its id.
pub(super) fn environment_option_label(primary: bool, id: &str, label: Option<&str>) -> String {
    let label = label.map(str::trim).filter(|label| !label.is_empty());
    if primary {
        let generic = |label: &&str| matches!(label.to_lowercase().as_str(), "local" | "local environment");
        return label.filter(|label| !generic(label)).unwrap_or("This device").to_owned();
    }
    label.unwrap_or(id).to_owned()
}

/// The "No project" row's key, which holds its place among the projects.
const NO_PROJECT_KEY: &str = "new-thread-no-project";

/// A pickable project group, as `newThreadNoProjectIndex` sees it.
struct NoProjectEntry {
    scratch: bool,
    pinned: bool,
    /// It holds the open thread's or draft's project.
    preferred: bool,
}

/// `newThreadNoProjectIndex`: "No project" keeps the scratch group's place when it is pinned
/// or current, else it follows the pinned projects.
fn no_project_index(entries: &[NoProjectEntry]) -> usize {
    let projects = entries.iter().filter(|e| !e.scratch).count();
    if let Some(at) = entries.iter().position(|e| e.scratch && (e.pinned || e.preferred)) {
        return entries[..at].iter().filter(|e| !e.scratch).count();
    }
    (entries.iter().filter(|e| !e.scratch && e.pinned).count() + 1).min(projects)
}

/// `settingsSearchItems`: each setting by its title, section and aliases, opening its row.
fn setting_rows() -> Vec<Row> {
    super::pages::palette_settings()
        .map(|(item, section)| {
            let title = SharedString::new_static(item.title.as_str());
            let mut row = Row::new(
                format!("setting:{}", item.id),
                RowIcon::Lucide(IconName::Settings),
                title,
                Run::Setting(item),
            );
            row.description = Some(Description::Text(format!("Settings · {section}").into()));
            row.data_terms = std::iter::once(section)
                .chain(item.search_terms.iter().map(String::as_str))
                .map(SharedString::new_static)
                .collect();
            row.secondary = item.secondary;
            row
        })
        .collect()
}

/// A row's name as a query reads it: the title and the bold part after it.
fn row_name(row: &Row) -> String {
    match &row.strong {
        Some(strong) => format!("{} {strong}", row.title),
        None => row.title.to_string(),
    }
}

/// `buildNestedSearchItems`: every option of each submenu as one root row ("New thread in t3code",
/// "Change theme: Dracula"), matched by that combined title alone. A query that only names the
/// submenu wants the submenu itself, and a combined title an action already has is left out.
fn nested_rows(actions: &[Row], submenus: Vec<(&Row, Vec<Row>)>, tokens: &[Vec<char>]) -> Vec<Row> {
    let query = tokens.join(&' ');
    let top_level: Vec<String> = actions.iter().map(row_name).collect();
    let mut rows = Vec::new();
    for (parent, children) in submenus {
        let stem = parent.title.trim_end_matches("...").trim_end_matches('…');
        if search::fold_query(stem).join(&' ').starts_with(&query[..]) {
            continue;
        }
        let prefix = if stem == parent.title.as_ref() { format!("{stem}: ") } else { format!("{} ", stem.trim_end()) };
        for mut child in children {
            let title = format!("{prefix}{}", row_name(&child));
            if top_level.contains(&title) {
                continue;
            }
            child.title = title.into();
            child.strong = None;
            child.terms = &[];
            child.data_terms = Vec::new();
            rows.push(child);
        }
    }
    rows
}

/// Submenu rows filtered by the query, in their own order when it is empty.
fn rank_rows(rows: Vec<Row>, tokens: &[Vec<char>]) -> Vec<Row> {
    if tokens.is_empty() {
        return rows;
    }
    let mut ranked = score_rows(rows.clone(), tokens, false);
    if ranked.is_empty() {
        ranked = score_rows(rows, tokens, true);
    }
    ranked.into_iter().map(|(row, _)| row).collect()
}

/// How a row shows the pointer or arrows on it.
#[derive(Clone, Copy)]
struct RowMark {
    /// `highlightedItemValue`: the painted highlight.
    highlighted: bool,
    /// Base UI's `data-highlighted`, which shows an unpinned project's pin.
    marked: bool,
}

/// The row Base UI highlights: the arrowed or hovered one, else the first (`autoHighlight="always"`
/// outside browsing) even while no highlight paints.
fn marked_row(highlight: Option<usize>, browsing: bool, rows: usize) -> Option<usize> {
    highlight.or((!browsing && rows > 0).then_some(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thread(fields: Value) -> crate::model::ThreadShell {
        let mut value = json!({"id": "t1", "projectId": "p1", "title": "Fix it", "status": "idle", "updatedAt": "2026-01-01T00:00:00Z"});
        if let (Some(base), Value::Object(fields)) = (value.as_object_mut(), fields) {
            base.extend(fields);
        }
        serde_json::from_value(value).expect("a thread shell")
    }

    #[test]
    fn a_search_asks_environments_with_a_cut_for_their_older_settled_matches() {
        const CUT: &str = "2024-02-01T00:00:00.000Z";
        let open = SearchScope::default();
        assert_eq!(search_input("fix bug", None, &open, "a"), Some(json!({"query": "fix bug"})));
        assert_eq!(search_input("fx", None, &open, "a"), None, "messages need a word of three letters");
        assert_eq!(search_input("f", Some(CUT), &open, "a"), None);
        // Older settled titles match from two letters, without the message search.
        assert_eq!(
            search_input("fx", Some(CUT), &open, "a"),
            Some(json!({"query": "fx", "olderSettledBefore": CUT, "titlesOnly": true}))
        );
        let scoped = SearchScope {
            projects: Some(HashMap::from([("a".into(), vec!["p1".to_owned()])])),
            labels: Some(HashMap::from([("a".into(), "Laptop".into())])),
        };
        assert_eq!(
            search_input("fix bug", Some(CUT), &scoped, "a"),
            Some(
                json!({"query": "fix bug", "olderSettledBefore": CUT, "projectIds": ["p1"], "environmentLabel": "Laptop"})
            )
        );
        // The filter keeps no project there, so no older rows come back from it.
        assert_eq!(search_input("fix bug", Some(CUT), &scoped, "b").unwrap()["projectIds"], json!([]));
    }

    #[test]
    fn project_rows_hold_their_opening_order_while_new_ones_join_at_the_end() {
        let order = RefCell::default();
        let keys = |items: Vec<(String, ())>| items.into_iter().map(|(key, ())| key).collect::<Vec<_>>();
        let items = |keys: &[&str]| keys.iter().map(|key| ((*key).to_owned(), ())).collect::<Vec<_>>();
        assert_eq!(keys(hold_order(&order, items(&["a", "b", "c"]))), ["a", "b", "c"]);
        assert_eq!(keys(hold_order(&order, items(&["c", "a", "b"]))), ["a", "b", "c"], "a pin moves nothing");
        assert_eq!(keys(hold_order(&order, items(&["d", "c", "a"]))), ["a", "c", "d"], "a new row joins at the end");
        assert_eq!(keys(hold_order(&order, items(&["e", "d", "b", "a"]))), ["a", "b", "d", "e"]);
    }

    #[test]
    fn a_pull_request_link_lists_its_linked_threads_archived_ones_too() {
        let query = "https://github.com/pingdotgg/t3code/pull/22889";
        let thread = |id: &str, title: &str, archived| LinkedThread { id: id.into(), title: title.into(), archived };
        let linked = LinkedSearch {
            query: query.into(),
            environment: "env".into(),
            threads: vec![thread("t1", "Speed up diffs", false), thread("t2", "", true)],
        };
        let rows = score_rows(linked.rows(), &search::fold_query(query), false);
        let shown: Vec<(&str, Option<&str>)> = rows
            .iter()
            .map(|(row, _)| {
                let description = match &row.description {
                    Some(Description::Text(text)) => Some(text.as_ref()),
                    _ => None,
                };
                (row.title.as_ref(), description)
            })
            .collect();
        assert_eq!(shown, [("Speed up diffs", Some("Linked thread")), ("Untitled thread", Some("Archived thread"))]);
        assert!(matches!(&rows[1].0.run, Run::ThreadIn(env, id) if env == "env" && id == "t2"));
    }

    #[test]
    fn submenu_options_join_the_root_search_under_their_combined_titles() {
        let row = |title: &str, run: Run| {
            Row::new(title.to_owned(), RowIcon::Lucide(IconName::Palette), title.to_owned(), run)
        };
        let mut here = row("New thread in", Run::NewThread(ScopedId::new("env", "p1")));
        here.strong = Some("t3code".into());
        let new_in = row("New thread in...", Run::Open(View::NewThreadIn));
        let themes = row("Change theme", Run::Open(View::Themes));
        let actions = vec![here, new_in.clone(), themes.clone()];
        let mut project = row("t3code", Run::NewThread(ScopedId::new("env", "p1")));
        project.data_terms = vec![r"H:\Developer\t3code".into()];
        let children = || {
            vec![
                (
                    &new_in,
                    vec![
                        project.clone(),
                        row("docs", Run::NewThread(ScopedId::new("env", "p2"))),
                        row("No project", Run::Triage),
                    ],
                ),
                (&themes, vec![row("Dracula", Run::Theme(Some("dracula".into())))]),
            ]
        };
        let titles = |query: &str| -> Vec<String> {
            nested_rows(&actions, children(), &search::fold_query(query)).iter().map(|r| r.title.to_string()).collect()
        };
        // The open project's option is the "New thread in t3code" action already.
        assert_eq!(titles("drac"), ["New thread in docs", "New thread in No project", "Change theme: Dracula"]);
        assert_eq!(titles("new thread"), ["Change theme: Dracula"], "a query naming the submenu wants the submenu");
        let suggestions = nested_rows(&actions, children(), &search::fold_query("x"));
        assert!(suggestions.iter().all(|r| r.terms.is_empty() && r.data_terms.is_empty()), "only the combined title");
    }

    #[test]
    fn settings_search_lists_each_setting_and_opens_its_row() {
        let found = score_rows(setting_rows(), &search::fold_query("delete confirmation"), false);
        let (row, _) = found.first().expect("a setting");
        assert!(matches!(row.run, Run::Setting(item) if item.id == "delete-confirmation"));
        assert!(matches!(&row.description, Some(Description::Text(text)) if text.starts_with("Settings · ")));
    }

    #[test]
    fn project_rows_match_member_paths_and_machines() {
        let mut row = Row::new("p", RowIcon::Lucide(IconName::Folder), "t3code", Run::Open(View::Browse));
        row.data_terms = vec!["H:\\Developer\\t3code".into(), "Studio Mac".into()];
        let finds = |query: &str| !score_rows(vec![row.clone()], &search::fold_query(query), false).is_empty();
        assert!(finds("developer") && finds("studio"));
        assert!(!finds("sidebar"));
    }

    #[test]
    fn shown_message_rows_hold_their_places_and_leave_room_for_a_slower_environment() {
        let keys = |prefix: &str, n: usize| (0..n).map(|i| format!("{prefix}{i}")).collect::<Vec<_>>();
        let fast = keys("a", 12);
        let first = next_shown_order(None, "rust", fast.clone(), 10);
        assert_eq!(first.keys[..first.shown], fast[..10]);
        let slow = keys("b", 4);
        let second = next_shown_order(Some(first), "rust", [slow.clone(), fast.clone()].concat(), 10);
        assert_eq!(second.keys[..second.shown], [&fast[..10], &slow[..3]].concat(), "its best few come next");
        let again = next_shown_order(
            Some(ShownOrder { query: second.query.clone(), keys: second.keys.clone(), shown: second.shown }),
            "rust",
            [slow.clone(), fast.clone()].concat(),
            10,
        );
        assert_eq!(again, second, "nothing new moves nothing");
        let third = next_shown_order(Some(second), "rust", [vec!["c0".to_owned()], slow, fast].concat(), 10);
        assert_eq!(third.shown, 13, "a third environment gets no more room past the limit");
        assert_eq!(next_shown_order(Some(third), "rusty", vec!["b0".into()], 10).keys, ["b0"]);
    }

    #[test]
    #[expect(clippy::cast_precision_loss, reason = "test epoch ms")]
    fn message_hits_rank_by_word_match_then_pick_then_quality_hits_and_use() {
        const DAY: i64 = 86_400_000;
        let now = 100 * DAY;
        let hit = |thread_id: &str, quality: f32, hits: u32| MessageMatch {
            environment_id: "env".into(),
            thread_id: thread_id.into(),
            user: true,
            snippet: String::new(),
            message_id: None,
            at: None,
            quality: Some(quality),
            hits: Some(hits),
        };
        let used = |active: i64| search::ThreadUse { activity_at: Some(now - active), opens: None };
        let picked = search::ThreadUse {
            activity_at: Some(now - 30 * DAY),
            opens: Some(crate::model::ThreadOpens {
                opened_at: (now - DAY) as f64,
                score: 1.,
                picks: vec![crate::model::ThreadPick { query: "needle".into(), at: (now - DAY) as f64 }],
            }),
        };
        let hits = [hit("a-word", 1., 1), hit("b-prefix", 0.8, 1), hit("b-recent", 1., 1), hit("b-picked", 0.8, 1)];
        let entries =
            vec![(&hits[0], used(30 * DAY)), (&hits[1], used(30 * DAY)), (&hits[2], used(0)), (&hits[3], picked)];
        let ranked = |entries| -> Vec<String> {
            rank_messages(entries, &search::fold_query("needle"), now)
                .into_iter()
                .map(|m| m.thread_id.clone())
                .collect()
        };
        assert_eq!(ranked(entries), ["b-picked", "b-recent", "a-word", "b-prefix"]);
        let (mention, about) = (hit("mention", 1., 1), hit("about", 1., 21));
        assert_eq!(ranked(vec![(&mention, used(3 * DAY)), (&about, used(48 * DAY))]), ["about", "mention"]);
        let inside = hit("inside", 0.3, 40);
        assert_eq!(ranked(vec![(&inside, used(0)), (&about, used(48 * DAY))]), ["about", "inside"]);
    }

    #[test]
    fn forks_are_listed_but_subagents_side_chats_and_archived_threads_are_not() {
        let lineage = |lineage: Value| thread(json!({"lineage": lineage}));
        assert!(listed_in_palette(&lineage(json!({"parentThreadId": "t0", "relationshipToParent": "fork"}))));
        assert!(!listed_in_palette(&lineage(json!({"parentThreadId": "t0", "relationshipToParent": "subagent"}))));
        assert!(!listed_in_palette(&lineage(json!({"parentThreadId": "t0", "sideChat": true}))));
        assert!(!listed_in_palette(&thread(json!({"archivedAt": "2026-01-02T00:00:00Z"}))));
    }

    #[test]
    fn the_first_row_stays_highlighted_for_the_pin_until_another_is() {
        assert_eq!(marked_row(None, false, 3), Some(0), "a fresh view marks its first row");
        assert_eq!(marked_row(Some(2), false, 3), Some(2));
        assert_eq!(marked_row(None, true, 3), None, "browsing highlights nothing on its own");
        assert_eq!(marked_row(None, false, 0), None);
    }

    #[test]
    fn no_project_follows_the_pinned_projects_unless_scratch_is_pinned_or_current() {
        let entry = |scratch, pinned, preferred| NoProjectEntry { scratch, pinned, preferred };
        let projects = |pinned: usize, total: usize| (0..total).map(move |i| entry(false, i < pinned, false));
        assert_eq!(no_project_index(&projects(0, 3).collect::<Vec<_>>()), 1);
        assert_eq!(no_project_index(&projects(2, 3).collect::<Vec<_>>()), 3);
        assert_eq!(no_project_index(&projects(3, 3).collect::<Vec<_>>()), 3);
        assert_eq!(no_project_index(&[]), 0);
        let with_scratch = |pinned, preferred| {
            let mut entries: Vec<_> = projects(1, 3).collect();
            entries.insert(2, entry(true, pinned, preferred));
            entries
        };
        assert_eq!(no_project_index(&with_scratch(false, false)), 2);
        assert_eq!(no_project_index(&with_scratch(true, false)), 2);
        let mut first: Vec<_> = projects(1, 3).collect();
        first.insert(0, entry(true, true, false));
        assert_eq!(no_project_index(&first), 0);
        let mut current: Vec<_> = projects(0, 3).collect();
        current.push(entry(true, false, true));
        assert_eq!(no_project_index(&current), 3);
    }

    #[test]
    fn a_leading_angle_searches_actions_and_an_empty_submenu_searches_everything() {
        assert_eq!(actions_filter(">new"), (true, "new"));
        assert_eq!(actions_filter(">"), (true, ""));
        assert_eq!(actions_filter(" >new"), (false, " >new"), "only a leading `>` counts");
        let group = |rows: Vec<Row>| vec![Group { label: "Projects", rows }];
        let row = Row::new("p", RowIcon::Lucide(IconName::Folder), "t3code", Run::Open(View::Browse));
        assert!(submenu_falls_back(&group(Vec::new()), "settings"));
        assert!(!submenu_falls_back(&group(vec![row]), "t3"));
        assert!(!submenu_falls_back(&group(Vec::new()), "  "), "a blank query keeps the submenu");
    }

    #[test]
    fn browse_queries_split_into_folder_and_leaf() {
        assert!(is_browse_query("~/code") && is_browse_query("C:\\Users") && is_browse_query("./x"));
        assert!(!is_browse_query("fix sidebar") && !is_browse_query("~code"));
        assert_eq!(split_browse("~/code/t3"), ("~/code/", "t3"));
        assert_eq!(split_browse("C:\\dev\\"), ("C:\\dev\\", ""));
        assert_eq!(up_path("~/code/t3/").as_deref(), Some("~/code/"));
        assert_eq!(up_path("~/"), None);
        assert_eq!(up_path("C:\\dev\\").as_deref(), Some("C:\\"));
    }

    #[test]
    fn browse_entries_hide_dot_folders_unless_asked() {
        let entries: Vec<(String, String)> =
            [".git", "apps", "Assets"].iter().map(|n| (n.to_string(), format!("/r/{n}"))).collect();
        let names = |leaf| filter_entries(&entries, leaf).into_iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>();
        assert_eq!(names(""), ["apps", "Assets"]);
        assert_eq!(names("a"), ["apps", "Assets"]);
        assert_eq!(names("."), [".git"]);
    }

    #[test]
    fn project_paths_name_and_compare_like_the_web() {
        assert_eq!(project_title_from_path("H:\\Developer\\t3code\\"), "t3code");
        assert_eq!(project_title_from_path("/home/me/app"), "app");
        let norm = crate::project_groups::normalize_path;
        assert_eq!(norm("H:\\Dev\\App\\"), norm("h:/dev/app"));
        assert_ne!(norm("/home/App"), norm("/home/app"));
        assert_eq!(ago(30_000), "just now");
        assert_eq!(ago(5 * 60_000), "5m ago");
    }

    #[test]
    fn relative_project_paths_resolve_against_the_open_project() {
        assert!(is_explicit_relative("..") && is_explicit_relative(".\\src") && !is_explicit_relative(".git"));
        assert_eq!(resolve_project_path("../api", Some("/home/me/app")), "/home/me/api");
        assert_eq!(resolve_project_path("./web/", Some("/home/me/app/")), "/home/me/app/web");
        assert_eq!(resolve_project_path(".\\docs\\..\\site", Some("H:\\Dev\\app")), "H:\\Dev\\app\\site");
        assert_eq!(resolve_project_path("../..", Some("C:\\dev")), "C:\\");
        assert_eq!(resolve_project_path("./x", Some("\\\\box\\share\\repo")), "\\\\box\\share\\repo\\x");
        assert_eq!(
            resolve_project_path(" ~/code/app/ ", None),
            "~/code/app",
            "absolute paths only lose the trailing slash"
        );
        assert_eq!(resolve_project_path("./x", None), "./x");
        assert!(is_windows_absolute("D:\\x") && is_windows_absolute("\\\\box\\s") && !is_windows_absolute("/d"));
        assert!(is_windows_platform("windows") && is_windows_platform("Win32") && !is_windows_platform("darwin"));
    }

    #[test]
    fn tab_completes_one_folder_or_the_prefix_the_matches_share() {
        assert_eq!(tab_completion(&["apps"], "a"), Some(("apps".into(), true)));
        assert_eq!(tab_completion(&["packages", "Pages"], "p"), Some(("pa".into(), false)), "case is ignored");
        assert_eq!(tab_completion(&["docs", "dist"], "d"), None, "nothing to add");
        assert_eq!(tab_completion(&[], "x"), None);
        assert_eq!(browse_into("C:\\dev\\", "app"), "C:\\dev\\app\\");
        assert_eq!(browse_into("~/code/", "app"), "~/code/app/");
    }

    #[test]
    fn a_folder_listing_resolves_paths_only_for_its_own_machine_cwd_and_directory() {
        let key = |environment: &str, cwd: Option<&str>| BrowseKey {
            environment: Some(environment.to_owned().into()),
            cwd: cwd.map(str::to_owned),
            directory: "~/".into(),
        };
        let browse = Browse {
            key: key("a", None),
            parent: Some("/home/a".into()),
            entries: vec![("code".into(), "/home/a/code".into())],
            ..Browse::default()
        };
        assert_eq!(submit_path(browse.of(&key("a", None)), "~/code").as_deref(), Some("/home/a/code"));
        assert_eq!(submit_path(browse.of(&key("a", None)), "~/").as_deref(), Some("/home/a"));
        // Machine B's add never takes machine A's folders, nor a listing from another cwd.
        assert_eq!(submit_path(browse.of(&key("b", None)), "~/code").as_deref(), Some("~/code"));
        assert_eq!(submit_path(browse.of(&key("b", None)), "~/").as_deref(), Some("~/"));
        assert_eq!(submit_path(browse.of(&key("a", Some("/repo"))), "~/code").as_deref(), Some("~/code"));
    }

    #[test]
    fn the_browse_button_says_whether_adding_creates_the_folder() {
        assert_eq!(browse_submit_labels(true, false, false), ("Create & Add", "Enter".to_owned()));
        assert_eq!(browse_submit_labels(false, true, false), ("Add", "Ctrl Enter".to_owned()));
        assert_eq!(browse_submit_labels(false, true, true), ("Add", "\u{2318} Enter".to_owned()));
    }

    #[test]
    fn new_thread_and_jump_shortcuts_run_the_row_they_label() {
        let row = |key: &str, command: &str, run: Run| {
            let mut row = Row::new(key.to_owned(), RowIcon::Lucide(IconName::SquarePen), key.to_owned(), run);
            row.command = Some(command.to_owned().into());
            row
        };
        let here = row("here", "chat.newLocal", Run::NewThread(ScopedId::new("env", "p1")));
        let jump = row("p2", "thread.jump.2", Run::NewThread(ScopedId::new("env", "p2")));
        let rows = [&here, &jump];
        assert!(
            matches!(palette_shortcut(&rows, "chat.newLocal"), PaletteShortcut::Run(Run::NewThread(p)) if p.id == "p1")
        );
        assert!(
            matches!(palette_shortcut(&rows, "thread.jump.2"), PaletteShortcut::Run(Run::NewThread(p)) if p.id == "p2")
        );
        assert!(matches!(palette_shortcut(&rows, "thread.jump.3"), PaletteShortcut::Swallow), "a jump never leaves");
        assert!(matches!(palette_shortcut(&rows, "chat.new"), PaletteShortcut::Pass));
        assert!(matches!(palette_shortcut(&rows, "sidebar.toggle"), PaletteShortcut::Pass));
    }

    #[test]
    fn add_project_starts_from_connections_one_machine_or_a_choice() {
        let ids = |ids: &[&str]| ids.iter().map(|id| SharedString::from((*id).to_owned())).collect::<Vec<_>>();
        assert_eq!(add_project_entry(&[]), AddProjectEntry::Connect);
        assert_eq!(add_project_entry(&ids(&["local"])), AddProjectEntry::Sources("local".into()));
        assert_eq!(add_project_entry(&ids(&["local", "box"])), AddProjectEntry::Choose);
        assert_eq!(environment_option_label(true, "env-1", Some("Local")), "This device");
        assert_eq!(environment_option_label(true, "env-1", Some("Studio")), "Studio");
        assert_eq!(environment_option_label(false, "env-2", Some(" ")), "env-2");
    }

    #[test]
    fn opening_a_project_lands_on_its_groups_latest_thread_settled_or_not() {
        let at = |id: &str, project: &str, at: &str, extra: Value| {
            let mut fields = json!({"id": id, "projectId": project, "latestUserMessageAt": at, "createdAt": at});
            if let (Some(base), Value::Object(extra)) = (fields.as_object_mut(), extra) {
                base.extend(extra);
            }
            crate::model::ThreadShell { environment_id: "env".into(), ..thread(fields) }
        };
        let threads = [
            at("old", "p1", "2026-01-01T00:00:00Z", json!({})),
            at("other-checkout", "p2", "2026-01-03T00:00:00Z", json!({"settledOverride": "settled"})),
            at("archived", "p1", "2026-01-05T00:00:00Z", json!({"archivedAt": "2026-01-06T00:00:00Z"})),
            at("elsewhere", "p3", "2026-01-09T00:00:00Z", json!({})),
        ];
        let group = [ScopedId::new("env", "p1"), ScopedId::new("env", "p2")];
        let latest = |order: &str| latest_thread(&threads, &group, order).map(|t| t.id.as_str());
        assert_eq!(latest("updated_at"), Some("other-checkout"));
        assert_eq!(latest_thread(&threads, &group[..1], "updated_at").map(|t| t.id.as_str()), Some("old"));
        assert_eq!(latest_thread(&threads, &[ScopedId::new("env", "p4")], "updated_at").map(|t| t.id.as_str()), None);
        // `p1` on another machine is another project.
        assert_eq!(latest_thread(&threads, &[ScopedId::new("copy", "p1")], "updated_at").map(|t| t.id.as_str()), None);
    }
}
