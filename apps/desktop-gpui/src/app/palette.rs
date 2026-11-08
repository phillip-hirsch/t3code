//! The command palette (`apps/web/src/components/CommandPalette*.tsx`): actions,
//! recent threads, search over threads, projects and settings, message hits,
//! "New thread in…", and the add-project flow (browse a folder, or pick one
//! with the native dialog, then `projects.mutate`).
//!
//! Go to file and Search project contents open the child searches in `files/`.
//! Actions this client has no surface for yet (guest sharing, PR linking, new project, clone, WSL folders,
//! the theme editor, workspace focus) are left out.

use std::path::PathBuf;
use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{Icon, h_flex, v_flex};
use gpui_kit::{
    AnyElement, AppContext as _, ClickEvent, Context, Entity, FontWeight, InteractiveElement as _, IntoElement,
    KeyDownEvent, ParentElement as _, PathPromptOptions, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, Task, Window, div, prelude::FluentBuilder as _, px,
};
use serde_json::{Value, json};

use super::pages::Page;
use super::palette_actions::github_repository;
use super::{Target, Workspace, new_id, project_name};
use crate::inbox;
use crate::model::ProjectShell;
use crate::search::{self, Field, Weighted};
use crate::theme::Tokens;

const RECENT_LIMIT: usize = 12;
const THREAD_LIMIT: usize = 20;
const MESSAGE_LIMIT: usize = 10;
const SECONDARY_LIMIT: usize = 3;
const MESSAGE_DEBOUNCE: Duration = Duration::from_millis(120);

/// One message hit from `orchestration.searchThreads`.
#[derive(Clone)]
pub(super) struct MessageMatch {
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
    fn from_json(value: &Value) -> Option<Self> {
        #[expect(clippy::cast_possible_truncation, reason = "a match quality near 1")]
        let quality = value["quality"].as_f64().map(|q| q as f32);
        Some(Self {
            thread_id: value["threadId"].as_str()?.to_owned(),
            user: value["source"] == "user",
            snippet: value["snippet"].as_str()?.to_owned(),
            message_id: value["messageId"].as_str().map(str::to_owned),
            at: value["messageCreatedAt"].as_str().map(str::to_owned),
            quality,
            hits: value["hits"].as_u64().and_then(|hits| u32::try_from(hits).ok()),
        })
    }
}

/// Server message search for a query over every connected environment, debounced
/// (`useThreadSearch`). Each environment's matches join as it answers.
#[derive(Default)]
pub(super) struct MessageSearch {
    pub(super) query: String,
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
    AddProject,
    Browse,
    /// "Change theme" (`theme.select`).
    Themes,
    /// "Change appearance".
    Appearance,
}

#[derive(Clone)]
enum Run {
    NewThread(String),
    Open(View),
    Thread(String),
    Project(String),
    Settings(&'static str),
    /// Settings › Projects with this project's group picked.
    ProjectSettings(String),
    Page(Page),
    /// Browse into this directory (the query becomes it).
    BrowseTo(String),
    /// Find in the open thread (`chat.find`).
    Find,
    /// A message hit: opens its thread with find on the query, at the message.
    FindIn {
        thread_id: String,
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
    PinProjects(Vec<String>, bool),
    Theme(crate::themes::CardId),
    Appearance(crate::settings::ThemePreference),
    /// `themeEditor.toggle`.
    ThemeEditor,
    /// Go to file or Search project contents, returning here on Esc.
    FileSearch(super::files::OverlayMode),
}

#[derive(Clone)]
enum RowIcon {
    Lucide(IconName),
    Project(String),
}

#[derive(Clone)]
enum Description {
    Text(SharedString),
    /// "You:" / "Agent:" and the snippet in highlighted runs.
    Snippet(bool, Vec<(String, bool)>),
}

#[derive(Clone)]
struct Row {
    key: SharedString,
    icon: RowIcon,
    title: SharedString,
    /// Drawn after `title` at weight 600 ("New thread in **X**").
    strong: Option<SharedString>,
    description: Option<Description>,
    time: Option<String>,
    shortcut: Option<SharedString>,
    chevron: bool,
    /// `ProjectPinButton`: the group's project ids and whether it is pinned.
    pin: Option<(Vec<String>, bool)>,
    run: Run,
    /// Keywords that match besides the title (`searchTerms`).
    terms: &'static [&'static str],
    /// Ranking, while a query is typed.
    quality: f32,
}

impl Row {
    fn new(key: impl Into<SharedString>, icon: RowIcon, title: impl Into<SharedString>, run: Run) -> Self {
        Self {
            key: key.into(),
            icon,
            title: title.into(),
            strong: None,
            description: None,
            time: None,
            shortcut: None,
            chevron: false,
            pin: None,
            run,
            terms: &[],
            quality: 0.,
        }
    }
}

struct Group {
    label: &'static str,
    rows: Vec<Row>,
}

/// The folder listing behind the browse view (`filesystem.browse`).
#[derive(Default)]
struct Browse {
    directory: String,
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
    browse: Browse,
    messages: MessageSearch,
    placeholder: &'static str,
    /// The app is dark: the theme list marks the dark appearance's theme as current.
    dark: bool,
    /// The root view's Threads group, ranked once per query and shell change.
    threads: Option<ThreadRanking>,
    /// "In messages", in the order last shown.
    message_order: MessageOrder,
    _subscription: Subscription,
}

/// The message rows' ranking for a query, answers and shell, and the order shown so far.
#[derive(Default)]
struct MessageOrder {
    key: (String, u64, u64),
    order: Option<ShownOrder>,
}

/// `ShownOrder`: message result keys in the order the palette last showed them for `query`.
#[derive(Debug, PartialEq)]
struct ShownOrder {
    query: String,
    keys: Vec<String>,
    /// How many of `keys` are on screen; only those hold their places.
    shown: usize,
}

/// Rows past the limit a slower environment's best matches may take.
const LATE_ROOM: usize = 3;

/// `nextShownOrder`: shown rows keep their places when a slower environment answers, so the
/// pointer and the arrows stay on the same row; its best matches may take a few rows more.
fn next_shown_order(previous: Option<ShownOrder>, query: &str, ranked: Vec<String>, limit: usize) -> ShownOrder {
    let Some(previous) = previous.filter(|p| p.query == query) else {
        let shown = limit.min(ranked.len());
        return ShownOrder { query: query.to_owned(), keys: ranked, shown };
    };
    let kept: Vec<String> =
        previous.keys[..previous.shown].iter().filter(|key| ranked.contains(key)).cloned().collect();
    let rest: Vec<String> = ranked.into_iter().filter(|key| !kept.contains(key)).collect();
    let arrived = rest.iter().filter(|key| !previous.keys.contains(key)).count();
    let shown = (kept.len() + rest.len()).min(limit + LATE_ROOM).min(limit.max(kept.len() + arrived.min(LATE_ROOM)));
    ShownOrder { query: previous.query, keys: [kept, rest].concat(), shown }
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
    current: Option<String>,
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

/// Paths compare without a trailing separator and, for drive paths, without case.
pub(super) fn same_path(a: &str, b: &str) -> bool {
    let norm = |p: &str| {
        let p = p.trim_end_matches(['/', '\\']).replace('\\', "/");
        if p.as_bytes().get(1) == Some(&b':') { p.to_lowercase() } else { p }
    };
    norm(a) == norm(b)
}

/// `filterFilesystemBrowseEntries`: prefix match on the leaf; dot folders only for a dot leaf.
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
            browse: Browse::default(),
            messages: MessageSearch::default(),
            placeholder: "",
            dark: crate::theme::tokens(cx).dark,
            threads: None,
            message_order: MessageOrder::default(),
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
        let Some(palette) = &mut self.palette else { return };
        palette.stack.push((palette.view, query));
        palette.view = view;
        palette.highlight = None;
        let start = match view {
            View::Browse => {
                let base = self
                    .server_info()
                    .settings
                    .get("addProjectBaseDirectory")
                    .as_str()
                    .unwrap_or_default()
                    .trim_end_matches(['/', '\\']);
                if base.is_empty() { "~/".to_owned() } else { format!("{base}/") }
            }
            _ => String::new(),
        };
        self.set_palette_query(&start, window, cx);
    }

    fn pop_palette_view(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(palette) = &mut self.palette else { return };
        let Some((view, query)) = palette.stack.pop() else { return };
        palette.view = view;
        palette.highlight = None;
        self.set_palette_query(&query, window, cx);
    }

    fn palette_query_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let query = self.palette_query(cx);
        let Some(palette) = &mut self.palette else { return };
        let browsing = palette.view == View::Browse || (palette.view == View::Root && is_browse_query(&query));
        let placeholder = match (browsing, palette.stack.is_empty()) {
            (true, true) => "Enter project path (e.g. ~/projects/my-app)",
            (true, false) => "Enter path (e.g. ~/projects/my-app)",
            (false, true) => "Search commands, projects, and threads...",
            (false, false) => "Search...",
        };
        if palette.placeholder != placeholder {
            palette.placeholder = placeholder;
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
            self.browse_folder(split_browse(&query).0.to_owned(), cx);
        } else if palette.view == View::Root {
            let query = if actions_filter(&query).0 { String::new() } else { query };
            self.search_messages(query, |this| this.palette.as_mut().map(|p| &mut p.messages), cx);
        }
        cx.notify();
    }

    /// Lists `directory` through `filesystem.browse`, once per directory.
    fn browse_folder(&mut self, directory: String, cx: &mut Context<Self>) {
        let cwd = self.current_project().map(|p| p.workspace_root.clone());
        let Some(client) = self.client() else { return };
        let Some(palette) = &mut self.palette else { return };
        if palette.browse.directory == directory && (palette.browse.loading || palette.browse.error.is_none()) {
            return;
        }
        palette.browse = Browse { directory: directory.clone(), loading: true, ..Browse::default() };
        if directory.is_empty() {
            palette.browse.loading = false;
            return;
        }
        let mut input = json!({"partialPath": directory});
        if let Some(cwd) = cwd {
            input["cwd"] = cwd.into();
        }
        palette.browse._task = Some(cx.spawn(async move |this, cx| {
            let result = client.request("filesystem.browse", input).await;
            let _ = this.update(cx, |this, cx| {
                let Some(browse) = this.palette.as_mut().map(|p| &mut p.browse).filter(|b| b.directory == directory)
                else {
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
        slot: fn(&mut Workspace) -> Option<&mut MessageSearch>,
        cx: &mut Context<Self>,
    ) {
        // Every connected environment is searched, as `useThreadSearch` does over its ids.
        let clients: Vec<crate::rpc::RpcClient> =
            self.sessions().iter().filter_map(|session| session.client.clone()).collect();
        let Some(search) = slot(self) else { return };
        let query: String = query.trim().chars().take(200).collect::<String>().trim().to_owned();
        if search.query == query {
            return;
        }
        search.query = query.clone();
        search.matches.clear();
        search._task = None;
        search.pending = search::has_long_token(&query) && !clients.is_empty();
        if !search.pending {
            return;
        }
        search._task = Some(cx.spawn(async move |this, cx| {
            use futures::StreamExt as _;
            cx.background_executor().timer(MESSAGE_DEBOUNCE).await;
            let mut answers: futures::stream::FuturesUnordered<_> = clients
                .iter()
                .map(|client| client.request("orchestration.searchThreads", json!({"query": query})))
                .collect();
            // Each environment's matches show as it answers; one that fails leaves the others'.
            while let Some(answer) = answers.next().await {
                let settled = answers.is_empty();
                let matches: Vec<MessageMatch> = answer
                    .inspect_err(|error| log::warn!("orchestration.searchThreads: {error}"))
                    .ok()
                    .and_then(|value| {
                        value["matches"].as_array().map(|m| m.iter().filter_map(MessageMatch::from_json).collect())
                    })
                    .unwrap_or_default();
                let updated = this.update(cx, |this, cx| {
                    let Some(search) = slot(this).filter(|s| s.query == query) else { return };
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
    pub(super) fn current_project(&self) -> Option<&ProjectShell> {
        let id = match &self.target {
            Some(Target::Thread { id, .. }) => self.shell.threads.iter().find(|t| &t.id == id).map(|t| &t.project_id),
            Some(Target::Draft { project_id, .. }) => Some(project_id),
            None => None,
        }?;
        self.shell.projects.iter().find(|p| &p.id == id)
    }

    /// The name of a project's group (`displayName`): the repository's name, not owner/repo.
    pub(super) fn group_name(&self, project: &ProjectShell) -> SharedString {
        let groups = self.project_groups();
        groups
            .iter()
            .find(|g| g.project_ids.contains(&project.id))
            .map_or_else(|| project_name(project), |g| g.display_name.clone().into())
    }

    /// "New thread in...": each project group by its name, with "No project" among them.
    fn new_thread_rows(&self) -> Vec<Row> {
        let groups = self.project_groups();
        let current = self.current_project().map(|p| p.id.clone());
        let pickable = crate::project_groups::picker(&groups, current.as_deref());
        let entries: Vec<NoProjectEntry> = pickable
            .iter()
            .map(|g| NoProjectEntry {
                scratch: g.members[0].is_scratch,
                pinned: g.pinned,
                preferred: current.as_ref().is_some_and(|id| g.project_ids.contains(id)),
            })
            .collect();
        let mut rows: Vec<Row> = pickable
            .iter()
            .filter(|g| !g.members[0].is_scratch)
            .enumerate()
            .map(|(i, g)| {
                let p = g.representative();
                let mut row = Row::new(
                    format!("new-in-{}", p.id),
                    RowIcon::Project(p.id.clone()),
                    g.display_name.clone(),
                    Run::NewThread(p.id.clone()),
                );
                row.description = Some(Description::Text(format!("Local · {}", p.workspace_root).into()));
                row.shortcut = (i < 9).then(|| self.shortcut(&format!("thread.jump.{}", i + 1))).flatten();
                row.pin = Some((g.project_ids.clone(), g.pinned));
                row
            })
            .collect();
        if let Some(environment) = self.scratch_environment() {
            let mut row = Row::new(
                "new-thread-no-project",
                RowIcon::Lucide(IconName::MessageSquareDashed),
                "No project",
                Run::ScratchThread(environment),
            );
            row.terms = &["no project", "without project", "none"];
            row.shortcut = self.shortcut("chat.newWithoutProject");
            rows.insert(no_project_index(&entries), row);
        }
        rows
    }

    /// The live shortcut label of `command` (`shortcutCommand`), as tooltips show it.
    fn shortcut(&self, command: &str) -> Option<SharedString> {
        self.shortcut_label(command).map(Into::into)
    }

    /// Actions in `CommandPalette.tsx` order.
    fn action_rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        let current =
            self.current_project().or_else(|| inbox::most_recent_project(&self.shell.projects, &self.shell.threads));
        let lucide = |key: &'static str, icon: IconName, title: &'static str, run: Run| {
            Row::new(key, RowIcon::Lucide(icon), title, run)
        };
        if let Some(project) = current {
            let mut row =
                lucide("new-thread-here", IconName::SquarePen, "New thread in", Run::NewThread(project.id.clone()));
            row.strong = Some(self.group_name(project));
            row.terms = &["new thread", "chat", "create", "draft"];
            row.shortcut = self.shortcut("chat.newLocal");
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
            row.shortcut = self.shortcut("chat.newWithoutProject");
            rows.push(row);
        }
        if self.server_info().capability("triage") {
            let mut row = lucide("triage", IconName::Bug, "Triage an issue", Run::Triage);
            row.description =
                Some(Description::Text("An agent investigates a T3 Code problem and helps file an issue".into()));
            row.terms = &["triage", "bug", "issue", "report", "broken", "debug", "help", "support"];
            rows.push(row);
        }
        let thread = match &self.target {
            Some(Target::Thread { id, .. }) => self.shell.threads.iter().find(|t| &t.id == id),
            _ => None,
        };
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
        if let Some(thread) = thread {
            let mut row = lucide("copy-thread-reference", IconName::Link, "Copy thread", Run::CopyThread);
            row.description = Some(Description::Text(thread.title.clone().into()));
            row.terms = &["copy", "thread", "thread id", "reference"];
            row.shortcut = self.shortcut("thread.copyReference");
            rows.push(row);
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
        let mut row = lucide("add-project", IconName::FolderPlus, "Add project", Run::Open(View::AddProject));
        row.terms = &["add project", "folder", "directory", "browse", "environment"];
        rows.push(row);
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
        let mut usage = lucide("usage", IconName::ChartNoAxesColumn, "Open usage", Run::Page(Page::Usage));
        usage.terms = &["usage", "use", "tokens", "cost", "spend", "limits", "stats", "analytics"];
        usage.shortcut = self.shortcut("usage.open");
        rows.push(usage);
        let mut row = lucide("settings", IconName::Settings, "Open settings", Run::Settings("general"));
        row.terms = &["settings", "preferences", "configuration", "keybindings"];
        rows.push(row);
        let groups = self.project_groups();
        if let Some(group) = self.current_project().and_then(|p| groups.iter().find(|g| g.contains(&p.id))) {
            let (title, icon) =
                if group.pinned { ("Unpin project", IconName::PinOff) } else { ("Pin project", IconName::Pin) };
            let ids = group.members.iter().map(|m| m.project.id.clone()).collect();
            let mut row = lucide("project-pin", icon, title, Run::PinProjects(ids, !group.pinned));
            row.description = Some(Description::Text(group.display_name.clone().into()));
            row.terms = &["pin", "unpin", "project", "favorite"];
            rows.push(row);
        }
        // `contextualProjectGroup`: the open project's group, else the first.
        let current_group = self.current_project().and_then(|p| Some((groups.iter().find(|g| g.contains(&p.id))?, p)));
        let contextual = current_group.or_else(|| groups.first().map(|g| (g, g.representative())));
        if let Some((group, project)) = contextual {
            let run = Run::ProjectSettings(project.id.clone());
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
    /// then the saved ones.
    fn theme_rows(&self, dark: bool) -> Vec<Row> {
        let current = self.client_settings.theme_choice().owner(dark);
        let saved = self.client_settings.themes.custom().iter();
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
            RowIcon::Project(thread.project_id.clone()),
            thread.title.clone(),
            Run::Thread(thread.id.clone()),
        );
        let project = self.shell.projects.iter().find(|p| p.id == thread.project_id);
        let mut parts: Vec<String> = project.map(|p| p.title.clone()).into_iter().collect();
        parts.extend(thread.branch.clone());
        if self.open_thread_id() == Some(&*thread.id) {
            parts.push("Current thread".into());
        }
        row.description = Some(Description::Text(parts.join(" · ").into()));
        row.time = crate::timestamp::parse_ms(thread.latest_user_message_at.as_deref().unwrap_or(&thread.updated_at))
            .map(|at| ago(now - at));
        row
    }

    fn thread_use(&self, thread: &crate::model::ThreadShell) -> search::ThreadUse {
        search::ThreadUse::of(thread, self.local_opened_at(&thread.id))
    }

    /// Threads the palette lists.
    fn palette_threads(&self) -> impl Iterator<Item = &crate::model::ThreadShell> {
        self.shell.threads.iter().filter(|t| listed_in_palette(t))
    }

    fn palette_groups(&self, query: &str) -> Vec<Group> {
        let Some(palette) = &self.palette else { return Vec::new() };
        if palette.view == View::Browse || (palette.view == View::Root && is_browse_query(query)) {
            return self.browse_groups(query);
        }
        let (actions_only, search_query) = actions_filter(query);
        let tokens = search::fold_query(search_query);
        // A `>` query matches no submenu row, so it searches the root's actions.
        let scope = |rows| if actions_only { Vec::new() } else { rank_rows(rows, &tokens) };
        let scoped = match palette.view {
            View::NewThreadIn => vec![Group { label: "Projects", rows: scope(self.new_thread_rows()) }],
            View::Themes => vec![Group { label: "Change theme", rows: scope(self.theme_rows(palette.dark)) }],
            View::Appearance => vec![Group { label: "Change appearance", rows: scope(self.appearance_rows()) }],
            View::AddProject => {
                let mut row = Row::new(
                    "local-folder",
                    RowIcon::Lucide(IconName::FolderPlus),
                    "Local folder",
                    Run::Open(View::Browse),
                );
                row.description = Some(Description::Text("Browse a folder on disk".into()));
                vec![Group { label: "", rows: scope(vec![row]) }]
            }
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
        let current = self.current_project().map(|p| p.id.clone());
        let projects = self.pickable_projects(current.as_deref());
        let stem: Vec<char> = "new thread in".chars().collect();
        let names_stem = stem.starts_with(&tokens.join(&' ')[..]);
        let suggestions: Vec<Row> = if names_stem {
            Vec::new()
        } else {
            self.new_thread_rows()
                .into_iter()
                .filter(|row| matches!(row.run, Run::NewThread(_)))
                .map(|mut row| {
                    row.title = format!("New thread in {}", row.title).into();
                    row
                })
                .collect()
        };
        let project_rows: Vec<Row> = projects
            .iter()
            .map(|p| {
                let mut row = Row::new(
                    format!("project-{}", p.id),
                    RowIcon::Project(p.id.clone()),
                    self.group_name(p),
                    Run::Project(p.id.clone()),
                );
                row.description = Some(Description::Text(format!("Local · {}", p.workspace_root).into()));
                row
            })
            .collect();
        let settings: Vec<Row> = super::pages::settings_sections()
            .map(|(group, id, label)| {
                let mut row =
                    Row::new(format!("settings-{id}"), RowIcon::Lucide(IconName::Settings), label, Run::Settings(id));
                row.description = Some(Description::Text(match group {
                    Some(group) => format!("Settings · {group}").into(),
                    None => "Settings".into(),
                }));
                row
            })
            .collect();
        let action_rows = self.action_rows();
        let rank = |typos: bool| {
            let actions = score_rows(action_rows.clone(), &tokens, typos);
            let suggestions = score_rows(suggestions.clone(), &tokens, typos);
            let threads = match (typos, &ranking.typos) {
                (true, Some(ranked)) => ranked.rows.clone(),
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
        if !ranking.strict.strong && !ranked.iter().any(|(_, rows)| rows.iter().any(|(_, strong)| *strong)) {
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
                let rows: Vec<Row> = rows.into_iter().map(|(row, _)| row).take(limit).collect();
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
        let shown = palette.message_order.order.as_ref().filter(|_| messages);
        let shown = shown.map_or(&[][..], |order| &order.keys[..order.shown]);
        let message_rows: Vec<Row> = shown
            .iter()
            .filter_map(|thread_id| palette.messages.matches.iter().find(|m| &m.thread_id == thread_id))
            .filter_map(|m| {
                let thread = self.palette_threads().find(|t| t.id == m.thread_id)?;
                let mut row = self.thread_row(thread, now);
                row.key = format!("message-{}-{}", m.thread_id, m.at.as_deref().unwrap_or_default()).into();
                row.run = Run::FindIn {
                    thread_id: m.thread_id.clone(),
                    query: super::find::query_for_message(&m.snippet, query),
                    message_id: m.message_id.clone(),
                };
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
                let thread = self.palette_threads().find(|t| t.id == found.thread_id)?;
                let titled = self.match_thread_fields(thread, &tokens, false).is_some();
                (!titled).then(|| (found, self.thread_use(thread)))
            })
            .collect();
        let ranked = rank_messages(entries, &tokens, now).into_iter().map(|m| m.thread_id.clone()).collect();
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
        let subject = search::ThreadSubject {
            id: &thread.id,
            title: &thread.title,
            branch: thread.branch.as_deref(),
            project: self.shell.projects.iter().find(|p| p.id == thread.project_id).map(|p| p.title.as_str()),
            pull_requests: &pull_requests,
        };
        search::match_thread(tokens, &subject, typos)
    }

    /// Ranks the root view's threads for `query` unless the last ranking still holds.
    fn rank_palette_threads(&mut self, query: &str) {
        let Some(palette) = &self.palette else { return };
        let browsing = palette.view == View::Browse || (palette.view == View::Root && is_browse_query(query));
        if browsing || actions_filter(query).0 {
            return;
        }
        let now = super::sidebar::now_ms();
        let key = RankingKey {
            query: query.to_owned(),
            revision: self.shell.revision,
            current: self.route_thread().map(str::to_owned),
            minute: now / 60_000,
        };
        if palette.threads.as_ref().is_some_and(|ranking| ranking.key == key) {
            return;
        }
        let tokens = search::fold_query(query);
        let (strict, typos) = if tokens.is_empty() {
            let mut recent: Vec<_> =
                self.palette_threads().filter(|t| key.current.as_deref() != Some(t.id.as_str())).collect();
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
        let current = self.route_thread();
        let mut scored: Vec<(&crate::model::ThreadShell, f32, bool, f32)> = self
            .palette_threads()
            .filter_map(|thread| {
                let found = self.match_thread_fields(thread, tokens, typos)?;
                let rank = if current == Some(thread.id.as_str()) {
                    -1.
                } else {
                    found.quality * self.thread_use(thread).boost(now)
                };
                Some((thread, found.quality, found.strong, rank))
            })
            .collect();
        scored.sort_by(|a, b| {
            let (class_a, class_b) = (search::class(a.1), search::class(b.1));
            (b.3 >= 0.)
                .cmp(&(a.3 >= 0.))
                .then(class_b.min(2).cmp(&class_a.min(2)))
                .then(class_b.cmp(&class_a))
                .then(b.3.total_cmp(&a.3))
        });
        let strong = scored.iter().any(|(_, _, strong, _)| *strong);
        let rows = scored
            .into_iter()
            .take(THREAD_LIMIT)
            .map(|(thread, quality, strong, _)| {
                let mut row = self.thread_row(thread, now);
                row.quality = quality;
                (row, strong)
            })
            .collect();
        RankedThreads { rows, strong }
    }

    fn browse_groups(&self, query: &str) -> Vec<Group> {
        let Some(palette) = &self.palette else { return Vec::new() };
        let (directory, leaf) = split_browse(query);
        let browse = &palette.browse;
        let mut rows = Vec::new();
        if leaf.is_empty()
            && let Some(parent) = up_path(directory)
        {
            rows.push(Row::new("browse-up", RowIcon::Lucide(IconName::CornerLeftUp), "..", Run::BrowseTo(parent)));
        }
        if browse.directory == directory {
            for (name, _) in filter_entries(&browse.entries, leaf) {
                let sep = if directory.ends_with('\\') { "\\" } else { "/" };
                rows.push(Row::new(
                    format!("dir-{name}"),
                    RowIcon::Lucide(IconName::Folder),
                    name.clone(),
                    Run::BrowseTo(format!("{directory}{name}{sep}")),
                ));
            }
        }
        vec![Group { label: "Directories", rows }]
    }

    /// The path Enter adds while browsing (`CommandPalette.tsx` submit).
    fn browse_submit_path(&self, query: &str) -> Option<String> {
        let palette = self.palette.as_ref()?;
        let (directory, leaf) = split_browse(query);
        if leaf.is_empty() {
            return palette
                .browse
                .parent
                .clone()
                .filter(|_| palette.browse.directory == directory)
                .or_else(|| (!query.trim().is_empty()).then(|| query.trim().to_owned()));
        }
        palette
            .browse
            .entries
            .iter()
            .find(|(name, _)| name == leaf)
            .map(|(_, full)| full.clone())
            .or_else(|| Some(query.trim().to_owned()))
    }

    fn run_palette_row(&mut self, run: Run, window: &mut Window, cx: &mut Context<Self>) {
        match run {
            Run::Open(view) => return self.push_palette_view(view, window, cx),
            Run::BrowseTo(path) => return self.set_palette_query(&path, window, cx),
            Run::FileSearch(mode) => {
                let query = self.palette_query(cx);
                return self.open_file_overlay(mode, Some(query), window, cx);
            }
            _ => {}
        }
        self.close_palette(window, cx);
        match run {
            Run::NewThread(project_id) => self.start_draft(project_id, window, cx),
            Run::Thread(thread_id) => self.select_thread(thread_id, window, cx),
            Run::Project(project_id) => self.open_project(&project_id, window, cx),
            Run::Settings(section) => self.open_settings(section, cx),
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
            Run::FindIn { thread_id, query, message_id } => {
                self.select_thread(thread_id, window, cx);
                self.open_thread_find_on(&query, message_id, window, cx);
            }
            Run::Open(_) | Run::BrowseTo(_) | Run::FileSearch(_) => {}
        }
    }

    /// A project's latest unsettled thread, else a new draft in it.
    fn open_project(&mut self, project_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let latest = self
            .shell
            .threads
            .iter()
            .filter(|t| {
                t.project_id == project_id
                    && t.is_listed()
                    && t.archived_at.is_none()
                    && t.settled_override.as_deref() != Some("settled")
            })
            .max_by_key(|t| used_at(t))
            .map(|t| t.id.clone());
        match latest {
            Some(thread_id) => self.select_thread(thread_id, window, cx),
            None => self.start_draft(project_id.to_owned(), window, cx),
        }
    }

    /// `handleAddProjectForEnvironment`: reuse a project already at `path`
    /// (unarchiving it), else `projects.mutate` a new one; then open it.
    pub(super) fn add_project(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(environment) = self.active_environment().cloned() else { return };
        let Some(client) = self.env_client(&environment) else { return };
        self.close_palette(window, cx);
        let existing =
            self.shell.projects.iter().find(|p| p.environment_id == environment && same_path(&p.workspace_root, &path));
        if let Some(existing) = existing {
            let project_id = existing.id.clone();
            if existing.archived_at.is_some() {
                let mutation = json!({"type": "project.update", "commandId": new_id(), "projectId": project_id, "archived": false});
                self.mutate_project(mutation, cx);
            }
            self.open_project(&project_id, window, cx);
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

    pub(super) fn mutate_project(&mut self, mutation: Value, cx: &mut Context<Self>) {
        let Some(client) = self.command_client(&mutation) else { return };
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
            let _ = this.update_in(cx, |this, window, cx| this.add_project(path, window, cx));
        })
        .detach();
    }

    /// The server runs on this machine, so its paths are this machine's.
    fn server_is_local(&self) -> bool {
        self.http_base().is_some_and(|base| base.contains("://127.0.0.1") || base.contains("://localhost"))
    }

    fn palette_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(query) = self.palette.as_ref().map(|p| p.input.read(cx).value().to_string()) else { return };
        self.prepare_palette(&query);
        let Some(palette) = &self.palette else { return };
        let view = palette.view;
        let browsing = view == View::Browse || (view == View::Root && is_browse_query(&query));
        let groups = self.palette_groups(&query);
        let rows: Vec<&Row> = groups.iter().flat_map(|g| g.rows.iter()).collect();
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
        match event.keystroke.key.as_str() {
            "escape" => self.close_palette(window, cx),
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
                }
                cx.notify();
            }
            "enter" => {
                let row = match highlight {
                    Some(i) => rows.get(i).map(|r| r.run.clone()),
                    None if browsing => None,
                    None => rows.first().map(|r| r.run.clone()),
                };
                match row {
                    Some(Run::BrowseTo(_)) if modifiers.control => {
                        if let Some(path) = self.browse_submit_path(&query) {
                            self.add_project(path, window, cx);
                        }
                    }
                    Some(run) => self.run_palette_row(run, window, cx),
                    None if browsing => {
                        if let Some(path) = self.browse_submit_path(&query) {
                            self.add_project(path, window, cx);
                        }
                    }
                    None => {}
                }
            }
            "backspace" if query.is_empty() && !self.palette.as_ref().is_some_and(|p| p.stack.is_empty()) => {
                self.pop_palette_view(window, cx);
            }
            key if modifiers.secondary(cx)
                && view == View::NewThreadIn
                && key.len() == 1
                && key.as_bytes()[0].is_ascii_digit() =>
            {
                let label = self.shortcut(&format!("thread.jump.{key}"));
                if let Some(run) = rows.iter().find(|r| label.is_some() && r.shortcut == label).map(|r| r.run.clone()) {
                    self.run_palette_row(run, window, cx);
                }
            }
            _ => return,
        }
        cx.stop_propagation();
    }

    pub(super) fn render_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let query = self.palette.as_ref()?.input.read(cx).value().to_string();
        self.prepare_palette(&query);
        let palette = self.palette.as_ref()?;
        let tokens = *crate::theme::tokens(cx);
        let view = palette.view;
        let browsing = view == View::Browse || (view == View::Root && is_browse_query(&query));
        let in_submenu = !palette.stack.is_empty();
        // Only `highlightedItemValue` paints; Enter still runs the first row when it is `None`.
        let highlight = palette.highlight;
        let groups = self.palette_groups(&query);
        let marked = marked_row(highlight, browsing, groups.iter().map(|g| g.rows.len()).sum());
        let input = palette.input.clone();
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
        let input_row = h_flex().flex_none().h(px(46.)).px(px(8.)).py(px(6.)).child(
            h_flex()
                .size_full()
                .relative()
                .child(div().absolute().left(px(11.)).child(lead_icon))
                .child(div().flex_1().pl(px(32.)).child(Input::new(&input).appearance(false).text_sm().h(px(34.)))),
        );
        let mut index = 0usize;
        let mut list = v_flex().p(px(8.));
        let total_rows: usize = groups.iter().map(|g| g.rows.len()).sum();
        for (gi, group) in groups.iter().enumerate() {
            if group.rows.is_empty() {
                continue;
            }
            if !group.label.is_empty() {
                list = list.child(
                    div()
                        .px(px(8.))
                        .py(px(6.))
                        .when(gi > 0, |el| el.pt(px(12.)))
                        .text_xs()
                        .line_height(px(16.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(muted)
                        .child(group.label),
                );
            }
            for row in &group.rows {
                let highlighted = highlight == Some(index);
                let mark = RowMark { highlighted, marked: marked == Some(index) };
                list = list.child(self.render_palette_row(row, index, mark, &tokens, cx));
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
            let text = if !browsing && view == View::Root && palette.messages.pending {
                "Searching thread messages…"
            } else if browsing {
                "Press Enter to add this folder as a project."
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
        // While browsing with nothing highlighted, Enter adds the typed path.
        let footer_action = if browsing && highlight.is_none() { "Add" } else { "Select" };
        let file_manager = browsing && self.server_is_local();
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
                    .child(hint(vec!["Enter".into()], footer_action))
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
                        .bg(tokens.popover)
                        .shadow_2xl()
                        .on_mouse_down(gpui_kit::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .capture_key_down(
                            cx.listener(|this, event: &KeyDownEvent, window, cx| this.palette_key(event, window, cx)),
                        )
                        .child(input_row)
                        .children(
                            status.map(|text| div().px(px(16.)).py(px(12.)).text_sm().text_color(muted).child(text)),
                        )
                        .child(
                            // `max-h-[min(28rem,70vh)]` on the list alone.
                            div()
                                .id("palette-list")
                                .flex_1()
                                .min_h_0()
                                .max_h((height * 0.7).min(px(448.)))
                                .overflow_y_scrollbar()
                                .child(list)
                                .children(empty),
                        )
                        .child(footer),
                )
                .into_any_element(),
        )
    }

    fn render_palette_row(
        &self,
        row: &Row,
        index: usize,
        RowMark { highlighted, marked }: RowMark,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let muted = tokens.muted_foreground;
        let icon: AnyElement = match &row.icon {
            RowIcon::Lucide(name) => Icon::new(*name).size(px(16.)).text_color(muted).into_any_element(),
            RowIcon::Project(project_id) => match self.shell.projects.iter().find(|p| &p.id == project_id) {
                Some(project) => super::sidebar::project_icon(project, tokens, false, 16., self.favicon(project)),
                None => div().size(px(16.)).into_any_element(),
            },
        };
        let description: Option<AnyElement> = row.description.as_ref().map(|d| match d {
            Description::Text(text) => div().truncate().child(text.clone()).into_any_element(),
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
                                .child(
                                    div()
                                        .when(row.strong.is_none(), |el| el.flex_1().min_w_0().truncate())
                                        .child(row.title.clone()),
                                )
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

/// When a thread was last used: its latest user-authored message.
fn used_at(thread: &crate::model::ThreadShell) -> i64 {
    thread
        .latest_user_authored_message_at
        .as_ref()
        .and_then(Option::as_deref)
        .or(thread.latest_user_message_at.as_deref())
        .and_then(crate::timestamp::parse_ms)
        .or_else(|| crate::timestamp::parse_ms(&thread.updated_at))
        .unwrap_or(0)
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
            let title = match &row.strong {
                Some(strong) => format!("{} {strong}", row.title),
                None => row.title.to_string(),
            };
            // The title names the item; its terms are keywords, the best one counts.
            let found = std::iter::once(title.as_str())
                .chain(row.terms.iter().copied())
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
        assert!(same_path("H:\\Dev\\App\\", "h:/dev/app"));
        assert!(!same_path("/home/App", "/home/app"));
        assert_eq!(ago(30_000), "just now");
        assert_eq!(ago(5 * 60_000), "5m ago");
    }
}
