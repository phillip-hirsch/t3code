//! The command palette (`apps/web/src/components/CommandPalette*.tsx`): actions,
//! recent threads, search over threads, projects and settings, message hits,
//! "New thread in…", and the add-project flow (browse a folder, or pick one
//! with the native dialog, then `projects.mutate`).
//!
//! Actions this client has no surface for (file picker, content search,
//! themes, guest sharing, PR linking, triage, scratch threads, clone) are left out.

use std::path::PathBuf;
use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{Icon, h_flex, v_flex};
use gpui_kit::{
    AnyElement, AppContext as _, ClickEvent, Context, Entity, FontWeight, InteractiveElement as _, IntoElement,
    KeyDownEvent, ParentElement as _, PathPromptOptions, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, Task, Window, div, prelude::FluentBuilder as _, px,
};
use serde_json::{Value, json};

use super::pages::Page;
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
    pub(super) at: Option<String>,
}

/// Server message search for a query, debounced (`useThreadSearch`).
#[derive(Default)]
pub(super) struct MessageSearch {
    pub(super) query: String,
    pub(super) matches: Vec<MessageMatch>,
    pub(super) pending: bool,
    _task: Option<Task<()>>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum View {
    Root,
    NewThreadIn,
    AddProject,
    Browse,
}

#[derive(Clone)]
enum Run {
    NewThread(String),
    Open(View),
    Thread(String),
    Project(String),
    Settings(&'static str),
    Page(Page),
    /// Browse into this directory (the query becomes it).
    BrowseTo(String),
}

enum RowIcon {
    Lucide(IconName),
    Project(String),
}

enum Description {
    Text(SharedString),
    /// "You:" / "Agent:" and the snippet in highlighted runs.
    Snippet(bool, Vec<(String, bool)>),
}

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
    run: Run,
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
            run,
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
    _subscription: Subscription,
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
            _subscription: subscription,
        });
        self.palette_query_changed(window, cx);
        if view != View::Root {
            self.push_palette_view(view, window, cx);
        }
        cx.notify();
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
        palette.highlight = (view != View::Browse).then_some(0);
        let start = match view {
            View::Browse => {
                let base = self
                    .server_info
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
        palette.highlight = Some(0);
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
        let client = self.client();
        let Some(search) = slot(self) else { return };
        let query: String = query.trim().chars().take(200).collect::<String>().trim().to_owned();
        if search.query == query {
            return;
        }
        search.query = query.clone();
        search.matches.clear();
        search._task = None;
        search.pending = search::has_long_token(&query) && client.is_some();
        let Some(client) = client.filter(|_| search.pending) else { return };
        search._task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(MESSAGE_DEBOUNCE).await;
            let result = client.request("orchestration.searchThreads", json!({"query": query})).await;
            let _ = this.update(cx, |this, cx| {
                let Some(search) = slot(this).filter(|s| s.query == query) else { return };
                search.pending = false;
                search.matches = result
                    .ok()
                    .and_then(|value| value["matches"].as_array().cloned())
                    .into_iter()
                    .flatten()
                    .filter_map(|m| {
                        Some(MessageMatch {
                            thread_id: m["threadId"].as_str()?.to_owned(),
                            user: m["source"] == "user",
                            snippet: m["snippet"].as_str()?.to_owned(),
                            at: m["messageCreatedAt"].as_str().map(str::to_owned),
                        })
                    })
                    .collect();
                this.notify_sidebar(cx);
                cx.notify();
            });
        }));
    }

    /// The project of the open thread or draft.
    fn current_project(&self) -> Option<&ProjectShell> {
        let id = match &self.target {
            Some(Target::Thread(id)) => self.shell.threads.iter().find(|t| &t.id == id).map(|t| &t.project_id),
            Some(Target::Draft { project_id }) => Some(project_id),
            None => None,
        }?;
        self.shell.projects.iter().find(|p| &p.id == id)
    }

    fn new_thread_rows(&self) -> Vec<Row> {
        self.pickable_projects()
            .into_iter()
            .enumerate()
            .map(|(i, p)| {
                let mut row = Row::new(
                    format!("new-in-{}", p.id),
                    RowIcon::Project(p.id.clone()),
                    project_name(p),
                    Run::NewThread(p.id.clone()),
                );
                row.description = Some(Description::Text(format!("Local · {}", p.workspace_root).into()));
                row.shortcut = (i < 9).then(|| format!("Ctrl+{}", i + 1).into());
                row
            })
            .collect()
    }

    fn action_rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        let current =
            self.current_project().or_else(|| inbox::most_recent_project(&self.shell.projects, &self.shell.threads));
        if let Some(project) = current {
            let mut row = Row::new(
                "new-thread-here",
                RowIcon::Lucide(IconName::SquarePen),
                "New thread in",
                Run::NewThread(project.id.clone()),
            );
            row.strong = Some(project_name(project));
            row.shortcut = Some("Ctrl+Shift+N".into());
            rows.push(row);
        }
        if !self.shell.projects.is_empty() {
            let mut row = Row::new(
                "new-thread-in",
                RowIcon::Lucide(IconName::SquarePen),
                "New thread in...",
                Run::Open(View::NewThreadIn),
            );
            row.chevron = true;
            rows.push(row);
        }
        rows.push(Row::new(
            "add-project",
            RowIcon::Lucide(IconName::FolderPlus),
            "Add project",
            Run::Open(View::AddProject),
        ));
        rows.push(Row::new(
            "pull-requests",
            RowIcon::Lucide(IconName::GitPullRequestArrow),
            "Open pull requests",
            Run::Page(Page::PullRequests),
        ));
        let mut usage =
            Row::new("usage", RowIcon::Lucide(IconName::ChartNoAxesColumn), "Open usage", Run::Page(Page::Usage));
        usage.shortcut = Some("Ctrl+U".into());
        rows.push(usage);
        rows.push(Row::new("settings", RowIcon::Lucide(IconName::Settings), "Open settings", Run::Settings("general")));
        if let Some(project) = current {
            let mut row = Row::new(
                "project-settings",
                RowIcon::Lucide(IconName::Folder),
                "Project settings",
                Run::Settings("projects"),
            );
            row.description = Some(Description::Text(project_name(project)));
            rows.push(row);
        }
        rows
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
        if self.target.as_ref() == Some(&Target::Thread(thread.id.clone())) {
            parts.push("Current thread".into());
        }
        row.description = Some(Description::Text(parts.join(" · ").into()));
        row.time = inbox::parse_ms(thread.latest_user_message_at.as_deref().unwrap_or(&thread.updated_at))
            .map(|at| ago(now - at));
        row
    }

    /// Threads the palette lists: visible, unarchived, not subagents or side chats.
    fn palette_threads(&self) -> impl Iterator<Item = &crate::model::ThreadShell> {
        self.shell.threads.iter().filter(|t| {
            t.is_listed() && t.archived_at.is_none() && t.deleted_at.is_none() && t.lineage.parent_thread_id.is_none()
        })
    }

    fn palette_groups(&self, query: &str) -> Vec<Group> {
        let Some(palette) = &self.palette else { return Vec::new() };
        let now = super::sidebar::now_ms();
        if palette.view == View::Browse || (palette.view == View::Root && is_browse_query(query)) {
            return self.browse_groups(query);
        }
        let tokens = search::fold_query(query);
        match palette.view {
            View::NewThreadIn => {
                let rows = self.new_thread_rows();
                return vec![Group { label: "Projects", rows: rank_rows(rows, &tokens) }];
            }
            View::AddProject => {
                let mut row = Row::new(
                    "local-folder",
                    RowIcon::Lucide(IconName::FolderPlus),
                    "Local folder",
                    Run::Open(View::Browse),
                );
                row.description = Some(Description::Text("Browse a folder on disk".into()));
                return vec![Group { label: "", rows: rank_rows(vec![row], &tokens) }];
            }
            _ => {}
        }
        if tokens.is_empty() {
            let mut recent: Vec<_> = self
                .palette_threads()
                .filter(|t| self.target.as_ref() != Some(&Target::Thread(t.id.clone())))
                .collect();
            recent.sort_by_key(|t| std::cmp::Reverse(used_at(t)));
            let recent = recent.into_iter().take(RECENT_LIMIT).map(|t| self.thread_row(t, now)).collect();
            return vec![
                Group { label: "Actions", rows: self.action_rows() },
                Group { label: "Recent Threads", rows: recent },
            ];
        }
        let projects = self.pickable_projects();
        let stem: Vec<char> = "new thread in".chars().collect();
        let names_stem = stem.starts_with(&tokens.join(&' ')[..]);
        let suggestions: Vec<Row> = if names_stem {
            Vec::new()
        } else {
            self.new_thread_rows()
                .into_iter()
                .map(|mut row| {
                    row.title = format!("New thread in {}", row.title).into();
                    row
                })
                .collect()
        };
        let threads: Vec<(Row, &crate::model::ThreadShell)> =
            self.palette_threads().map(|t| (self.thread_row(t, now), t)).collect();
        let project_rows: Vec<Row> = projects
            .iter()
            .map(|p| {
                let mut row = Row::new(
                    format!("project-{}", p.id),
                    RowIcon::Project(p.id.clone()),
                    project_name(p),
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
        let rank = |typos: bool| {
            let actions = score_rows(self.action_rows(), &tokens, typos);
            let suggestions = score_rows(suggestions_clone(&suggestions), &tokens, typos);
            let threads = self.score_threads(threads_clone(&threads), &tokens, typos, now);
            let projects = score_rows(rows_clone(&project_rows), &tokens, typos);
            let settings = score_rows(rows_clone(&settings), &tokens, typos);
            [
                ("Actions", actions),
                ("Suggestions", suggestions),
                ("Threads", threads),
                ("Projects", projects),
                ("Settings", settings),
            ]
        };
        let mut ranked = rank(false);
        if !ranked.iter().any(|(_, rows)| rows.iter().any(|(_, strong)| *strong)) {
            ranked = rank(true);
        }
        let threads_matched = ranked[2].1.iter().any(|_| true);
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
        let listed: Vec<String> = groups
            .iter()
            .flat_map(|g| g.rows.iter())
            .filter_map(|r| match &r.run {
                Run::Thread(id) => Some(id.clone()),
                _ => None,
            })
            .collect();
        let messages: Vec<Row> = palette
            .messages
            .matches
            .iter()
            .filter(|m| !listed.contains(&m.thread_id))
            .filter_map(|m| {
                let thread = self.palette_threads().find(|t| t.id == m.thread_id)?;
                let mut row = self.thread_row(thread, now);
                row.key = format!("message-{}-{}", m.thread_id, m.at.as_deref().unwrap_or_default()).into();
                let ranges = search::token_ranges(&m.snippet, query);
                row.description = Some(Description::Snippet(m.user, search::split_ranges(&m.snippet, &ranges)));
                row.time = m.at.as_deref().and_then(inbox::parse_ms).map(|at| ago(now - at));
                Some(row)
            })
            .take(MESSAGE_LIMIT)
            .collect();
        if !messages.is_empty() {
            groups.push(Group { label: "In messages", rows: messages });
        }
        groups
    }

    fn score_threads(
        &self,
        rows: Vec<(Row, &crate::model::ThreadShell)>,
        tokens: &[Vec<char>],
        typos: bool,
        now: i64,
    ) -> Vec<(Row, bool)> {
        let mut scored: Vec<(Row, bool, f32)> = rows
            .into_iter()
            .filter_map(|(mut row, thread)| {
                let title = Field::new(&thread.title);
                let branch = thread
                    .branch
                    .as_deref()
                    .map(|b| b.trim_start_matches("t3code/").trim_start_matches("t3/"))
                    .unwrap_or_default();
                let branch = Field::new(branch);
                let project = Field::new(
                    self.shell.projects.iter().find(|p| p.id == thread.project_id).map_or("", |p| p.title.as_str()),
                );
                let mut title_field = Weighted::new(&title, 1.);
                title_field.mid_word = true;
                let mut project_field = Weighted::new(&project, 0.25);
                project_field.context = true;
                let found =
                    search::match_fields(tokens, &[title_field, Weighted::new(&branch, 0.6), project_field], typos)?;
                let age = now - used_at(thread);
                row.quality = found.quality;
                let current = self.target.as_ref() == Some(&Target::Thread(thread.id.clone()));
                Some((row, found.strong, if current { -1. } else { found.quality * search::usage_boost(age) }))
            })
            .collect();
        scored.sort_by(|a, b| {
            let class = |r: &Row| search::class(r.quality);
            (b.2 >= 0.)
                .cmp(&(a.2 >= 0.))
                .then(class(&b.0).min(2).cmp(&class(&a.0).min(2)))
                .then(class(&b.0).cmp(&class(&a.0)))
                .then(b.2.total_cmp(&a.2))
        });
        scored.into_iter().map(|(row, strong, _)| (row, strong)).collect()
    }

    fn browse_groups(&self, query: &str) -> Vec<Group> {
        let Some(palette) = &self.palette else { return Vec::new() };
        let (directory, leaf) = split_browse(query);
        let browse = &palette.browse;
        let mut rows = Vec::new();
        if leaf.is_empty() {
            if let Some(parent) = up_path(directory) {
                rows.push(Row::new("browse-up", RowIcon::Lucide(IconName::CornerLeftUp), "..", Run::BrowseTo(parent)));
            }
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
            _ => {}
        }
        self.close_palette(window, cx);
        match run {
            Run::NewThread(project_id) => self.start_draft(project_id, window, cx),
            Run::Thread(thread_id) => self.select_thread(thread_id, window, cx),
            Run::Project(project_id) => self.open_project(&project_id, window, cx),
            Run::Settings(section) => self.open_settings(section, cx),
            Run::Page(page) => self.open_page(page, cx),
            Run::Open(_) | Run::BrowseTo(_) => {}
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
        let Some(client) = self.client() else { return };
        self.close_palette(window, cx);
        if let Some(existing) = self.shell.projects.iter().find(|p| same_path(&p.workspace_root, &path)) {
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
                Ok(_) => this.start_draft(project_id, window, cx),
                Err(error) => {
                    this.error = Some(format!("Failed to add project: {error}").into());
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(super) fn mutate_project(&mut self, mutation: Value, cx: &mut Context<Self>) {
        let Some(client) = self.client() else { return };
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
        let Some(palette) = &self.palette else { return };
        let query = palette.input.read(cx).value().to_string();
        let view = palette.view;
        let browsing = view == View::Browse || (view == View::Root && is_browse_query(&query));
        let groups = self.palette_groups(&query);
        let rows: Vec<&Row> = groups.iter().flat_map(|g| g.rows.iter()).collect();
        let modifiers = event.keystroke.modifiers;
        let highlight = palette.highlight;
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
                    (_, None) | (_, Some(0)) => last,
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
                self.pop_palette_view(window, cx)
            }
            key if modifiers.control
                && view == View::NewThreadIn
                && key.len() == 1
                && key.as_bytes()[0].is_ascii_digit() =>
            {
                let label = format!("Ctrl+{key}");
                if let Some(run) =
                    rows.iter().find(|r| r.shortcut.as_deref() == Some(label.as_str())).map(|r| r.run.clone())
                {
                    self.run_palette_row(run, window, cx);
                }
            }
            _ => return,
        }
        cx.stop_propagation();
    }

    pub(super) fn render_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let palette = self.palette.as_ref()?;
        let tokens = *crate::theme::tokens(cx);
        let query = palette.input.read(cx).value().to_string();
        let view = palette.view;
        let browsing = view == View::Browse || (view == View::Root && is_browse_query(&query));
        let in_submenu = !palette.stack.is_empty();
        let highlight = palette.highlight;
        let groups = self.palette_groups(&query);
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
            h_flex().size_full().relative().child(div().absolute().left(px(11.)).child(lead_icon)).child(
                div().flex_1().pl(px(32.)).child(Input::new(&input).appearance(false).text_size(px(14.)).h(px(34.))),
            ),
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
                        .text_size(px(12.))
                        .line_height(px(16.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(muted)
                        .child(group.label),
                );
            }
            for row in &group.rows {
                let highlighted = highlight == Some(index);
                list = list.child(self.render_palette_row(row, index, highlighted, &tokens, cx));
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
            let text = if !browsing && palette.messages.pending {
                "Searching thread messages…"
            } else if browsing {
                "Press Enter to add this folder as a project."
            } else if query.trim_start().starts_with('>') {
                "No matching actions."
            } else {
                "No matching commands, projects, or threads."
            };
            div().py(px(40.)).text_center().text_size(px(14.)).text_color(muted).child(text)
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
                .text_size(px(12.))
                .font_weight(FontWeight::MEDIUM)
                .child(label)
        };
        let hint = |keys: Vec<SharedString>, label: &'static str| {
            h_flex().gap(px(4.)).children(keys.into_iter().map(kbd)).child(label)
        };
        let footer_action = browsing.then_some(if highlight.is_some() { "Select" } else { "Add" });
        let file_manager = browsing && self.server_is_local();
        let footer = h_flex()
            .flex_none()
            .justify_between()
            .px(px(16.))
            .py(px(10.))
            .border_t_1()
            .border_color(fg.opacity(0.07))
            .bg(fg.opacity(0.025))
            .text_size(px(14.))
            .font_weight(FontWeight::MEDIUM)
            .text_color(muted)
            .child(
                h_flex()
                    .gap(px(12.))
                    .child(hint(vec!["↑".into(), "↓".into()], "Navigate"))
                    .children(footer_action.map(|label| hint(vec!["Enter".into()], label)))
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
                        .text_size(px(12.))
                        .hover(|s| s.bg(fg.opacity(0.06)).text_color(fg))
                        .on_click(cx.listener(|this, _, window, cx| this.pick_project_folder(window, cx)))
                        .child("Open in File Explorer"),
                )
            })
            .when(!browsing && palette.messages.pending, |el| {
                el.child(div().text_size(px(12.)).child("Searching messages…"))
            });
        let height = window.viewport_size().height;
        Some(
            div()
                .id("palette-backdrop")
                .absolute()
                .inset_0()
                .occlude()
                .bg(gpui_kit::black().opacity(0.64))
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
                        .w_full()
                        .max_w(px(576.))
                        .max_h(px(420.))
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
                            status.map(|text| {
                                div().px(px(16.)).py(px(12.)).text_size(px(14.)).text_color(muted).child(text)
                            }),
                        )
                        .child(
                            div().id("palette-list").flex_1().min_h_0().overflow_y_scroll().child(list).children(empty),
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
        highlighted: bool,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let muted = tokens.muted_foreground;
        let icon: AnyElement = match &row.icon {
            RowIcon::Lucide(name) => Icon::new(name.clone()).size(px(16.)).text_color(muted).into_any_element(),
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
        h_flex()
            .id(row.key.clone())
            .items_start()
            .gap(px(8.))
            .px(px(8.))
            .py(px(6.))
            .rounded(px(4.))
            .cursor_pointer()
            .when(highlighted, |el| el.bg(tokens.foreground.opacity(0.04)))
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                if *hovered {
                    if let Some(palette) = &mut this.palette {
                        if palette.highlight != Some(index) {
                            palette.highlight = Some(index);
                            cx.notify();
                        }
                    }
                }
            }))
            .on_click(
                cx.listener(move |this, _: &ClickEvent, window, cx| this.run_palette_row(run.clone(), window, cx)),
            )
            .child(div().flex_none().h(px(20.)).pt(px(2.)).child(icon))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        h_flex()
                            .h(px(20.))
                            .gap(px(8.))
                            .child(
                                h_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_size(px(14.))
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
                            )
                            .children(row.time.clone().map(|time| {
                                div()
                                    .flex_none()
                                    .min_w(px(48.))
                                    .text_right()
                                    .text_size(px(12.))
                                    .text_color(muted.opacity(0.7))
                                    .child(time)
                            }))
                            .children(row.shortcut.clone().map(|shortcut| {
                                div()
                                    .flex_none()
                                    .ml_auto()
                                    .text_size(px(12.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(tokens.sidebar.secondary_label)
                                    .child(shortcut)
                            }))
                            .when(row.chevron, |el| {
                                el.child(div().ml_auto().mr(px(-2.)).child(
                                    Icon::new(IconName::ChevronRight).size(px(16.)).text_color(muted.opacity(0.7)),
                                ))
                            }),
                    )
                    .children(description.map(|d| {
                        div().text_size(px(12.)).line_height(px(16.)).text_color(muted.opacity(0.7)).child(d)
                    })),
            )
            .into_any_element()
    }
}

/// When a thread was last used: its latest user-authored message.
fn used_at(thread: &crate::model::ThreadShell) -> i64 {
    thread
        .latest_user_authored_message_at
        .clone()
        .flatten()
        .or_else(|| thread.latest_user_message_at.clone())
        .and_then(|at| inbox::parse_ms(&at))
        .or_else(|| inbox::parse_ms(&thread.updated_at))
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

fn rows_clone(rows: &[Row]) -> Vec<Row> {
    rows.iter().map(row_clone).collect()
}

fn suggestions_clone(rows: &[Row]) -> Vec<Row> {
    rows_clone(rows)
}

fn threads_clone<'a>(rows: &[(Row, &'a crate::model::ThreadShell)]) -> Vec<(Row, &'a crate::model::ThreadShell)> {
    rows.iter().map(|(row, thread)| (row_clone(row), *thread)).collect()
}

fn row_clone(row: &Row) -> Row {
    Row {
        key: row.key.clone(),
        icon: match &row.icon {
            RowIcon::Lucide(name) => RowIcon::Lucide(name.clone()),
            RowIcon::Project(id) => RowIcon::Project(id.clone()),
        },
        title: row.title.clone(),
        strong: row.strong.clone(),
        description: row.description.as_ref().map(|d| match d {
            Description::Text(t) => Description::Text(t.clone()),
            Description::Snippet(user, parts) => Description::Snippet(*user, parts.clone()),
        }),
        time: row.time.clone(),
        shortcut: row.shortcut.clone(),
        chevron: row.chevron,
        run: row.run.clone(),
        quality: row.quality,
    }
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
            let field = Field::new(&title);
            let found = search::match_fields(tokens, &[Weighted::new(&field, 1.)], typos)?;
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

/// Submenu rows filtered by the query, in their own order when it is empty.
fn rank_rows(rows: Vec<Row>, tokens: &[Vec<char>]) -> Vec<Row> {
    if tokens.is_empty() {
        return rows;
    }
    let mut ranked = score_rows(rows_clone(&rows), tokens, false);
    if ranked.is_empty() {
        ranked = score_rows(rows, tokens, true);
    }
    ranked.into_iter().map(|(row, _)| row).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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
