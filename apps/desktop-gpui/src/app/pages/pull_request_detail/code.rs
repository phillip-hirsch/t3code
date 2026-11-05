//! The Code tab (`PullRequestCodeTab`, `PullRequestReviewAnnotation`): the change's diff in the
//! right panel's viewer, paged as the host serves it, with review conversations under their lines.
//! The diff is read over HTTP (`POST /api/pull-requests/diff`), as the web reads it.

use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::diff::{
    Diff, DiffAnnotation, DiffFile, DiffLinePosition, DiffMode, DiffParseOptions, DiffSide, DiffState,
};

use super::*;
use crate::backend::Endpoint;
use crate::pull_request_detail::{ReviewThread, ThreadComment};
use crate::pull_request_diff::{self as diff_logic, FilePatch, Side};

const DIFF_TIMEOUT: Duration = Duration::from_secs(120);

/// One page of the host's diff.
struct Slice {
    cursor: Option<String>,
    patch: String,
    truncated: bool,
    next_cursor: Option<String>,
    omitted: Vec<(String, u64, u64)>,
}

pub(in crate::app) struct CodeTab {
    diff: Entity<DiffState>,
    /// A commit to read alone, or the whole change.
    commit: Option<String>,
    slices: Vec<Slice>,
    loading: bool,
    error: Option<String>,
    _load: Option<Task<()>>,
    /// The shown patch indexed by path, for anchoring threads and comments.
    patches: Rc<HashMap<String, FilePatch>>,
    /// Shown files in the viewer's order.
    shown: Rc<Vec<String>>,
    test_files: usize,
    hidden_tests: usize,
    ignore_whitespace: bool,
    wrap: bool,
    hide_tests: bool,
    /// Files fold by default ("Collapse all files", or the `diffFilesCollapsed` setting).
    folded: bool,
    seen: HashSet<String>,
    viewed: Option<HashMap<String, String>>,
    viewed_truncated: bool,
    viewed_error: Option<String>,
    /// Ticks pressed here, shown until the host's answer is read again.
    pressed: HashMap<String, bool>,
    last_viewed: Option<String>,
    orphans_open: bool,
    /// Threads whose disclosure was flipped from its default (open unless resolved).
    toggled: HashSet<String>,
    /// Later replies read with `pullRequests.threadComments`, by thread.
    pages: HashMap<String, (Vec<ThreadComment>, Option<String>)>,
    loading_more: Option<String>,
}

impl CodeTab {
    fn new(ignore_whitespace: bool, wrap: bool, folded: bool, mode: DiffMode, cx: &mut Context<Workspace>) -> Self {
        Self {
            diff: cx.new(|cx| DiffState::new(Vec::new(), cx).with_mode(mode)),
            commit: None,
            slices: Vec::new(),
            loading: false,
            error: None,
            _load: None,
            patches: Rc::default(),
            shown: Rc::default(),
            test_files: 0,
            hidden_tests: 0,
            ignore_whitespace,
            wrap,
            hide_tests: false,
            folded,
            seen: HashSet::new(),
            viewed: None,
            viewed_truncated: false,
            viewed_error: None,
            pressed: HashMap::new(),
            last_viewed: None,
            orphans_open: false,
            toggled: HashSet::new(),
            pages: HashMap::new(),
            loading_more: None,
        }
    }

    /// The first page of the diff has been read.
    pub(super) fn loaded(&self) -> bool {
        !self.slices.is_empty()
    }

    /// Unfolds a file and scrolls the viewer to it.
    pub(super) fn reveal(&self, path: &str, cx: &mut App) {
        self.diff.update(cx, |state, cx| {
            state.set_file_collapsed(path, false, cx);
            state.scroll_to_file(path, cx);
        });
    }

    pub(super) fn commit_selected(&self) -> bool {
        self.commit.is_some()
    }

    pub(super) fn patch(&self, path: &str) -> Option<&FilePatch> {
        self.patches.get(path)
    }

    fn next_cursor(&self) -> Option<&str> {
        self.slices.last().and_then(|s| s.next_cursor.as_deref())
    }

    fn is_viewed(&self, path: &str) -> bool {
        self.pressed
            .get(path)
            .copied()
            .unwrap_or_else(|| self.viewed.as_ref().is_some_and(|v| v.get(path).is_some_and(|s| s == "viewed")))
    }

    fn is_stale(&self, path: &str) -> bool {
        !self.pressed.contains_key(path)
            && self.viewed.as_ref().is_some_and(|v| v.get(path).is_some_and(|s| s == "dismissed"))
    }
}

fn to_viewer_side(side: Side) -> DiffSide {
    match side {
        Side::Left => DiffSide::Original,
        Side::Right => DiffSide::Modified,
    }
}

pub(super) fn from_viewer_side(side: DiffSide) -> Side {
    match side {
        DiffSide::Original => Side::Left,
        DiffSide::Modified => Side::Right,
    }
}

/// What sits under one line: its conversations, pending comments and the open draft.
#[derive(Clone)]
struct Group {
    path: String,
    side: Side,
    line: u64,
    /// Indexes into the activity's `review_threads`.
    threads: Vec<usize>,
    pending: Vec<super::write::PendingComment>,
    draft: bool,
}

fn group_at<'a>(groups: &'a mut HashMap<SharedString, Group>, path: &str, side: Side, line: u64) -> &'a mut Group {
    groups.entry(group_key(path, side, line)).or_insert_with(|| Group {
        path: path.to_owned(),
        side,
        line,
        threads: Vec::new(),
        pending: Vec::new(),
        draft: false,
    })
}

/// The annotation key of the conversations on one line.
fn group_key(path: &str, side: Side, line: u64) -> SharedString {
    format!("pr-review:{}:{line}:{path}", side.as_str()).into()
}

/// `mergePullRequestThreadComments`: later pages add what the first did not have.
fn merge_comments(first: &[ThreadComment], later: &[ThreadComment]) -> Vec<ThreadComment> {
    let mut merged = first.to_vec();
    for comment in later {
        match merged.iter_mut().find(|c| c.id == comment.id) {
            Some(held) => *held = comment.clone(),
            None => merged.push(comment.clone()),
        }
    }
    merged
}

/// `POST /api/pull-requests/diff` with this client's bearer session.
async fn read_diff(endpoint: Option<Endpoint>, input: Value) -> Result<Value, String> {
    let endpoint = endpoint.ok_or("Not connected to the environment.")?;
    let request = async move {
        static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
        let response = CLIENT
            .get_or_init(reqwest::Client::new)
            .post(format!("{}/api/pull-requests/diff", endpoint.http_base))
            .bearer_auth(&endpoint.bearer_token)
            .json(&input)
            .timeout(DIFF_TIMEOUT)
            .send()
            .await
            .map_err(|error| error.to_string())?;
        let status = response.status();
        let value: Value = match response.json().await {
            Ok(value) => value,
            Err(error) if status.is_success() => return Err(format!("Unreadable diff: {error}")),
            Err(_) => Value::Null,
        };
        if status.is_success() {
            return Ok(value);
        }
        let text = |key: &str| value[key].as_str().map(str::to_owned);
        Err(text("message").or_else(|| text("detail")).unwrap_or_else(|| format!("Request failed ({status}). {value}")))
    };
    crate::runtime().spawn(request).await.map_err(|error| error.to_string())?
}

/// `describePullRequestDiffError`.
fn describe_diff_error(error: &str, host: &str, term: &str) -> (String, String) {
    let lower = error.to_lowercase();
    if ["rate limit", "rate-limited", "rate_limited", "too many requests"].iter().any(|m| lower.contains(m)) {
        return (
            format!("{host} rate limit reached"),
            format!(
                "{host} is temporarily limiting requests, so this diff couldn't be loaded. Try again after the limit resets, or view the changes on {host}."
            ),
        );
    }
    (
        "Couldn't load the changes".into(),
        format!("The environment couldn't load this {term}'s diff. Try again, or view the changes on {host}."),
    )
}

/// A 28px ghost icon toggle (`Toggle variant="ghost" size="sm"`).
fn icon_toggle(
    id: &'static str,
    icon: Lucide,
    pressed: bool,
    tip: &'static str,
    t: &Tokens,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    let accent = t.accent;
    div()
        .id(id)
        .size(px(28.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(8.))
        .cursor_pointer()
        .text_color(t.foreground)
        .when(pressed, |el| el.bg(accent))
        .hover(move |style| style.bg(accent))
        .child(Icon::new(icon).with_size(px(14.)))
        .tooltip(move |window, cx| Tooltip::new(tip).build(window, cx))
}

impl Workspace {
    fn pr_code(&mut self) -> Option<&mut CodeTab> {
        self.pull_requests.panel.as_mut()?.code.as_mut()
    }

    /// The Code tab was shown: its state is made once, and its diff read once.
    pub(super) fn open_pr_code(&mut self, cx: &mut Context<Self>) {
        let settings = &self.client_settings.values;
        let (ignore, wrap, folded) =
            (settings.bool("diffIgnoreWhitespace"), settings.bool("wordWrap"), settings.bool("diffFilesCollapsed"));
        let mode =
            if settings.get("diffLayout").as_str() == Some("split") { DiffMode::Split } else { DiffMode::Unified };
        let Some(panel) = self.pull_requests.panel.as_mut() else { return };
        if panel.code.is_none() {
            panel.code = Some(CodeTab::new(ignore, wrap, folded, mode, cx));
        }
        let code = panel.code.as_ref().expect("made above");
        let viewed_files = panel.detail.as_ref().is_some_and(|d| d.capabilities.viewed_files.is_some());
        if code.slices.is_empty() && !code.loading {
            self.read_pr_diff(None, cx);
        }
        if viewed_files && self.pr_code().is_some_and(|c| c.viewed.is_none()) {
            self.read_pr_files_viewed(cx);
        }
    }

    /// Reads one page of the diff; the first page replaces what was loaded.
    fn read_pr_diff(&mut self, cursor: Option<String>, cx: &mut Context<Self>) {
        let Some(project_id) = self.pull_requests.panel.as_ref().map(|p| p.reference.project_id.clone()) else {
            return;
        };
        let endpoint = self.project_endpoint(&project_id).cloned();
        let Some(panel) = self.pull_requests.panel.as_mut() else { return };
        let key = panel.reference.key();
        let mut input = panel.reference.to_json();
        let Some(code) = panel.code.as_mut() else { return };
        if let Some(cursor) = &cursor {
            input["cursor"] = json!(cursor);
        }
        if let Some(commit) = &code.commit {
            input["commit"] = json!(commit);
        }
        code.loading = true;
        code._load = Some(cx.spawn(async move |this, cx| {
            let result = read_diff(endpoint, input).await;
            let _ = this.update(cx, |this, cx| {
                let Some(code) = this.pr_panel(&key).and_then(|p| p.code.as_mut()) else { return };
                code.loading = false;
                match result {
                    Err(error) => code.error = Some(error),
                    Ok(value) => {
                        code.error = None;
                        let text = |key: &str| value[key].as_str().map(str::to_owned);
                        let slice = Slice {
                            cursor: cursor.clone(),
                            patch: text("patch").unwrap_or_default(),
                            truncated: value["truncated"].as_bool() == Some(true),
                            next_cursor: text("nextCursor"),
                            omitted: value["omittedFileStats"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .filter_map(|f| {
                                    Some((
                                        f["path"].as_str()?.to_owned(),
                                        f["additions"].as_u64().unwrap_or(0),
                                        f["deletions"].as_u64().unwrap_or(0),
                                    ))
                                })
                                .collect(),
                        };
                        // An unchanged page leaves the viewer, and its scroll, where they are.
                        let same = code.slices.iter().any(|s| {
                            s.cursor == cursor
                                && s.patch == slice.patch
                                && s.truncated == slice.truncated
                                && s.next_cursor == slice.next_cursor
                                && s.omitted == slice.omitted
                        });
                        if same {
                            cx.notify();
                            return;
                        }
                        // A changed page drops the pages after it: their cursors name the old diff.
                        match code.slices.iter().position(|s| s.cursor == cursor) {
                            Some(index) => {
                                code.slices.truncate(index);
                                code.slices.push(slice);
                            }
                            None if cursor.is_none() => code.slices = vec![slice],
                            None => code.slices.push(slice),
                        }
                        this.rebuild_pr_code(cx);
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Parses the loaded pages into the viewer, hiding whitespace and tests as asked.
    fn rebuild_pr_code(&mut self, cx: &mut Context<Self>) {
        let Some(code) = self.pr_code() else { return };
        let mut text = String::new();
        for slice in &code.slices {
            text.push_str(&slice.patch);
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
        }
        if code.ignore_whitespace {
            text = diff_logic::hide_whitespace(&text);
        }
        let files =
            DiffFile::parse_with(&text, DiffParseOptions::new().truncated_hunks(true)).unwrap_or_else(|error| {
                log::warn!("unparseable pull request diff: {}", error.message());
                Vec::new()
            });
        let paths: Vec<String> = files.iter().map(|f| f.path().to_string()).collect();
        let tests: Vec<bool> = paths.iter().map(|p| diff_logic::is_test_file(p)).collect();
        code.test_files = tests.iter().filter(|t| **t).count();
        let hide = code.hide_tests;
        let kept: Vec<String> =
            paths.iter().zip(&tests).filter(|(_, t)| !hide || !**t).map(|(p, _)| p.clone()).collect();
        code.hidden_tests = paths.len() - kept.len();
        let order = diff_logic::order_files(&kept);
        let mut by_path: HashMap<String, DiffFile> = files.into_iter().map(|f| (f.path().to_string(), f)).collect();
        let ordered: Vec<DiffFile> = order.iter().filter_map(|&i| by_path.remove(&kept[i])).collect();
        code.shown = Rc::new(order.iter().map(|&i| kept[i].clone()).collect());
        code.patches = Rc::new(diff_logic::index_patch(&text).into_iter().map(|f| (f.path.clone(), f)).collect());
        let fresh: Vec<String> = code.shown.iter().filter(|p| !code.seen.contains(*p)).cloned().collect();
        code.seen.extend(fresh.iter().cloned());
        let fold = code.folded;
        code.diff.update(cx, |state, cx| {
            state.set_files(ordered, cx);
            if fold {
                for path in &fresh {
                    state.set_file_collapsed(path, true, cx);
                }
            }
        });
    }

    fn set_pr_code_commit(&mut self, commit: Option<String>, cx: &mut Context<Self>) {
        let Some(code) = self.pr_code() else { return };
        if code.commit == commit {
            return;
        }
        code.commit = commit;
        code.slices.clear();
        code.pages.clear();
        self.rebuild_pr_code(cx);
        self.read_pr_diff(None, cx);
    }

    /// Reads the diff and the viewed ticks again after the host was asked again.
    pub(super) fn refresh_pr_code(&mut self, cx: &mut Context<Self>) {
        if self.pr_code().is_none() {
            return;
        }
        self.read_pr_diff(None, cx);
        if self
            .pull_requests
            .panel
            .as_ref()
            .and_then(|p| p.detail.as_ref())
            .is_some_and(|d| d.capabilities.viewed_files.is_some())
        {
            self.read_pr_files_viewed(cx);
        }
    }

    fn read_pr_files_viewed(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.pull_request_client() else { return };
        let Some(panel) = self.pull_requests.panel.as_ref() else { return };
        let (key, input) = (panel.reference.key(), panel.reference.to_json());
        cx.spawn(async move |this, cx| {
            let result = client.request("pullRequests.filesViewed", input).await;
            let _ = this.update(cx, |this, cx| {
                let Some(code) = this.pr_panel(&key).and_then(|p| p.code.as_mut()) else { return };
                match result {
                    Ok(value) => {
                        code.viewed = Some(
                            value["files"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .filter_map(|f| Some((f["path"].as_str()?.to_owned(), f["state"].as_str()?.to_owned())))
                                .collect(),
                        );
                        code.viewed_truncated = value["truncated"].as_bool() == Some(true);
                        code.viewed_error = None;
                        code.pressed.clear();
                    }
                    Err(error) => code.viewed_error = Some(list_logic::error_message(&error)),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// A Viewed tick: shift extends it over the files since the last one; a viewed file folds.
    fn set_pr_file_viewed(
        &mut self,
        path: String,
        viewed: bool,
        extend: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.pull_request_client() else { return };
        let Some(panel) = self.pull_requests.panel.as_mut() else { return };
        let (key, mut input) = (panel.reference.key(), panel.reference.to_json());
        let Some(code) = panel.code.as_mut() else { return };
        let range =
            diff_logic::viewed_range(&code.shown, &path, if extend { code.last_viewed.as_deref() } else { None });
        code.last_viewed = Some(path);
        for path in &range {
            code.pressed.insert(path.clone(), viewed);
        }
        code.diff.update(cx, |state, cx| {
            for path in &range {
                state.set_file_collapsed(path, viewed, cx);
            }
        });
        input["files"] = Value::Array(range.iter().map(|path| json!({"path": path, "viewed": viewed})).collect());
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("pullRequests.setFilesViewed", input).await;
            let _ = this.update_in(cx, |this, window, cx| {
                if result.is_err() {
                    if let Some(code) = this.pr_panel(&key).and_then(|p| p.code.as_mut()) {
                        for path in &range {
                            code.pressed.remove(path);
                        }
                    }
                    toast_error(window, cx, "Could not save which files you viewed", "Try again.");
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn load_more_thread_comments(
        &mut self,
        thread_id: String,
        cursor: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.pull_request_client() else { return };
        let Some(panel) = self.pull_requests.panel.as_mut() else { return };
        let key = panel.reference.key();
        let mut input = panel.reference.to_json();
        input["threadId"] = json!(thread_id);
        input["cursor"] = json!(cursor);
        let Some(code) = panel.code.as_mut() else { return };
        if code.loading_more.is_some() {
            return;
        }
        code.loading_more = Some(thread_id.clone());
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("pullRequests.threadComments", input).await;
            let _ = this.update_in(cx, |this, window, cx| {
                let Some(code) = this.pr_panel(&key).and_then(|p| p.code.as_mut()) else { return };
                code.loading_more = None;
                let page = result.ok().and_then(|value| {
                    let comments: Vec<ThreadComment> = serde_json::from_value(value["comments"].clone()).ok()?;
                    Some((comments, value["nextCursor"].as_str().map(str::to_owned)))
                });
                match page {
                    Some((comments, next)) => {
                        let held = code.pages.remove(&thread_id).map(|p| p.0).unwrap_or_default();
                        code.pages.insert(thread_id, (merge_comments(&held, &comments), next));
                    }
                    None => toast_error(window, cx, "More comments could not be loaded", ""),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn set_pr_code_layout(&mut self, split: bool, cx: &mut Context<Self>) {
        let mut patch = serde_json::Map::new();
        patch.insert("diffLayout".into(), json!(if split { "split" } else { "stacked" }));
        self.client_settings.apply(&patch);
        if let Some(code) = self.pr_code() {
            let mode = if split { DiffMode::Split } else { DiffMode::Unified };
            code.diff.update(cx, |state, cx| state.set_mode(mode, cx));
        }
        cx.notify();
    }

    pub(super) fn render_pr_code(
        &mut self,
        detail: &Rc<Detail>,
        t: &Tokens,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(code) = self.pull_requests.panel.as_ref().and_then(|p| p.code.as_ref()) else {
            return div().into_any_element();
        };
        let term = logic::terminology(&detail.provider).0;
        let toolbar = self.render_pr_code_toolbar(detail, t, cx);
        let frame = |body: AnyElement| v_flex().size_full().min_h_0().child(toolbar).child(body).into_any_element();
        let quiet = |text: String| {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .px(px(20.))
                .text_center()
                .text_xs()
                .text_color(Hsla { a: 0.7, ..t.muted_foreground })
                .child(text)
                .into_any_element()
        };
        if code.slices.is_empty() {
            if let (Some(error), false) = (&code.error, code.loading) {
                return frame(self.render_pr_diff_unavailable(detail, error.clone(), t, cx));
            }
            return frame(quiet(format!("Loading {term} diff...")));
        }
        let shown = code.shown.clone();
        if shown.is_empty() && code.next_cursor().is_none() {
            return frame(
                div()
                    .px(px(16.))
                    .py(px(20.))
                    .text_sm()
                    .text_color(t.muted_foreground)
                    .child(if code.commit.is_none() {
                        format!("This {term} has no file changes.")
                    } else {
                        "This commit has no file changes.".into()
                    })
                    .into_any_element(),
            );
        }

        // Threads sit under their line when the shown diff has it; the rest list above the diff.
        let activity = self.pull_requests.panel.as_ref().and_then(|p| p.activity.clone());
        let threads = activity.as_deref().map_or(&[][..], |a| a.review_threads.as_slice());
        let code = self.pull_requests.panel.as_ref().and_then(|p| p.code.as_ref()).expect("checked above");
        let shown_set: HashSet<&str> = shown.iter().map(String::as_str).collect();
        let mut groups: HashMap<SharedString, Group> = HashMap::new();
        let mut orphans: Vec<&ReviewThread> = Vec::new();
        for (index, thread) in threads.iter().enumerate() {
            let side = Side::parse(&thread.side);
            let placed = code.commit.is_none()
                && shown_set.contains(thread.path.as_str())
                && thread.line.is_some_and(|line| {
                    code.patches.get(&thread.path).is_some_and(|f| diff_logic::line_in_diff(f, side, line))
                });
            match (placed, thread.line) {
                (true, Some(line)) => group_at(&mut groups, &thread.path, side, line).threads.push(index),
                _ => orphans.push(thread),
            }
        }
        if code.commit.is_none() {
            for comment in self.pr_pending_comments() {
                if shown_set.contains(comment.path()) {
                    let (side, line) = comment.anchor();
                    let path = comment.path().to_owned();
                    group_at(&mut groups, &path, side, line).pending.push(comment);
                }
            }
            let draft = self.pull_requests.panel.as_ref().and_then(|p| p.write.draft.as_ref());
            if let Some(draft) = draft {
                let (side, line) = draft.anchor();
                group_at(&mut groups, &draft.path.clone(), side, line).draft = true;
            }
        }
        let annotations: Vec<DiffAnnotation> = groups
            .iter()
            .map(|(key, group)| {
                DiffAnnotation::line(
                    ElementId::Name(key.clone()),
                    DiffLinePosition::new(group.path.clone(), to_viewer_side(group.side), group.line as usize),
                )
            })
            .collect();
        let groups: Rc<HashMap<SharedString, Group>> = Rc::new(groups);
        let orphan_list = (!orphans.is_empty()).then(|| self.render_pr_orphans(orphans, t, cx));

        let code = self.pull_requests.panel.as_ref().and_then(|p| p.code.as_ref()).expect("checked above");
        let view = cx.entity();
        let tokens = *t;
        let omitted: Rc<HashMap<String, (u64, u64)>> =
            Rc::new(code.slices.iter().flat_map(|s| s.omitted.iter().map(|(p, a, d)| (p.clone(), (*a, *d)))).collect());
        let viewed_enabled = detail.capabilities.viewed_files.is_some();
        let viewed: Rc<HashMap<String, (bool, bool)>> = Rc::new(if viewed_enabled {
            shown.iter().map(|p| (p.clone(), (code.is_viewed(p), code.is_stale(p)))).collect()
        } else {
            HashMap::new()
        });
        let header_view = view.clone();
        let add_view = view.clone();
        let can_comment = code.commit.is_none()
            && detail.capabilities.review.inline_comment
            && detail.viewer_permissions.comment
            && self.pull_requests.panel.as_ref().is_some_and(|p| p.write.draft.is_none());
        let code = self.pull_requests.panel.as_ref().and_then(|p| p.code.as_ref()).expect("checked above");
        let diff = Diff::new(&code.diff)
            .soft_wrap(code.wrap)
            .flex_1()
            .min_h_0()
            .text_size(px(13.))
            .annotations(annotations)
            .render_annotation(move |annotation, window, cx| {
                let ElementId::Name(key) = annotation.id() else { return div().into_any_element() };
                let Some(group) = groups.get(key) else { return div().into_any_element() };
                let threads = activity.as_deref().map_or(&[][..], |a| a.review_threads.as_slice());
                view.update(cx, |this, cx| {
                    let mut column = v_flex().py(px(4.));
                    for thread in group.threads.iter().filter_map(|&index| threads.get(index)) {
                        column = column.child(this.render_review_thread(thread, &tokens, window, cx));
                    }
                    for comment in &group.pending {
                        column = column.child(this.render_pr_pending_comment(comment, &tokens, cx));
                    }
                    if group.draft {
                        column = column.children(this.render_pr_line_draft(&tokens, cx));
                    }
                    column.into_any_element()
                })
            })
            .when(can_comment, |diff| {
                diff.on_add_annotation(move |range, window, cx| {
                    let path = range.path().to_string();
                    let start = (from_viewer_side(range.side()), range.start() as u64);
                    let end = (from_viewer_side(range.end_side()), range.end() as u64);
                    let () = add_view.update(cx, |this, cx| this.begin_pr_line_comment(path, start, end, window, cx));
                })
            })
            .render_header_suffix(move |file, _, _| {
                let path = file.path().to_string();
                let withheld =
                    (file.additions() + file.deletions() == 0).then(|| omitted.get(&path).copied()).flatten();
                let Some(&(is_viewed, stale)) = viewed.get(&path) else {
                    return h_flex()
                        .children(withheld.and_then(|(a, d)| diff_stat(a, d, 11., &tokens)))
                        .into_any_element();
                };
                let toggle_view = header_view.clone();
                let toggle_path = path.clone();
                h_flex()
                    .gap(px(12.))
                    .children(withheld.and_then(|(a, d)| diff_stat(a, d, 11., &tokens)))
                    .child(
                        h_flex()
                            .w(px(72.))
                            .flex_none()
                            .gap(px(6.))
                            .text_size(px(11.))
                            .text_color(tokens.muted_foreground)
                            .child(
                                Checkbox::new(ElementId::Name(format!("pr-viewed:{path}").into()))
                                    .checked(is_viewed)
                                    .on_click(move |checked, window, cx| {
                                        let extend = window.modifiers().shift;
                                        let (path, checked) = (toggle_path.clone(), *checked);
                                        let () = toggle_view.update(cx, |this, cx| {
                                            this.set_pr_file_viewed(path, checked, extend, window, cx);
                                        });
                                    }),
                            )
                            .child(if stale {
                                div()
                                    .id(ElementId::Name(format!("pr-viewed-stale:{path}").into()))
                                    .text_color(tokens.warning_foreground)
                                    .child("Changed")
                                    .tooltip(|window, cx| {
                                        Tooltip::new("This file has been pushed to since you marked it viewed.")
                                            .build(window, cx)
                                    })
                                    .into_any_element()
                            } else {
                                div().child("Viewed").into_any_element()
                            }),
                    )
                    .into_any_element()
            });

        let more = code.next_cursor().map(|cursor| {
            let cursor = cursor.to_owned();
            let failed = code.error.is_some();
            h_flex()
                .flex_none()
                .justify_center()
                .gap(px(8.))
                .py(px(8.))
                .border_t_1()
                .border_color(Hsla { a: t.border.a * 0.6, ..t.border })
                .text_xs()
                .text_color(t.muted_foreground)
                .when(failed, |el| el.child("The rest of this diff could not be loaded."))
                .child(
                    outline_button("pr-code-more", t)
                        .disabled(code.loading)
                        .child(if failed {
                            "Retry"
                        } else if code.loading {
                            "Loading more files..."
                        } else {
                            "Load more files"
                        })
                        .on_click(cx.listener(move |this, _, _, cx| this.read_pr_diff(Some(cursor.clone()), cx))),
                )
        });
        frame(v_flex().flex_1().min_h_0().children(orphan_list).child(diff).children(more).into_any_element())
    }

    /// `PullRequestDiffUnavailableState`.
    fn render_pr_diff_unavailable(
        &self,
        detail: &Detail,
        error: String,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let host = list_logic::open_on_host_label(&detail.provider).trim_start_matches("Open on ").to_owned();
        let (title, description) = describe_diff_error(&error, &host, logic::terminology(&detail.provider).0);
        let url = detail.url.clone();
        v_flex()
            .flex_1()
            .items_center()
            .justify_center()
            .gap(px(16.))
            .p(px(24.))
            .text_center()
            .child(
                div()
                    .size(px(44.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(Hsla { a: 0.12, ..t.warning })
                    .child(Icon::new(Lucide::TriangleAlert).with_size(px(20.)).text_color(t.warning)),
            )
            .child(div().text_base().font_weight(FontWeight::SEMIBOLD).text_color(t.foreground).child(title))
            .child(div().max_w(px(384.)).text_sm().text_color(t.muted_foreground).child(description))
            .child(
                h_flex()
                    .gap(px(8.))
                    .child(
                        Button::new("pr-diff-retry")
                            .primary()
                            .small()
                            .child(Icon::new(Lucide::RefreshCw).with_size(px(14.)))
                            .child("Try again")
                            .on_click(cx.listener(|this, _, _, cx| this.read_pr_diff(None, cx))),
                    )
                    .child(
                        outline_button("pr-diff-open-host", t)
                            .child(Icon::new(Lucide::ExternalLink).with_size(px(14.)))
                            .child(format!("Open on {host}"))
                            .on_click(move |_, _, cx| cx.open_url(&url)),
                    ),
            )
            .child(failure_details(error, t))
            .into_any_element()
    }

    fn render_pr_code_toolbar(&self, detail: &Rc<Detail>, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let panel = self.pull_requests.panel.as_ref().expect("a panel is open");
        let Some(code) = panel.code.as_ref() else { return div().into_any_element() };
        let activity = panel.activity.clone().unwrap_or_default();
        let mut newest_first: Vec<usize> = (0..activity.commits.len()).collect();
        newest_first
            .sort_by_cached_key(|&i| std::cmp::Reverse(parse_ms(&activity.commits[i].committed_date).unwrap_or(0)));
        let selected = code.commit.as_ref().and_then(|oid| activity.commits.iter().find(|c| &c.oid == oid));
        let scope_label: SharedString =
            selected.map_or_else(|| "All commits".into(), |c| c.message_headline.clone()).into();
        let view = cx.entity();
        let current = code.commit.clone();
        let scope = (!activity.commits.is_empty()).then(|| {
            Button::new("pr-code-scope")
                .custom(
                    gpui_kit::component::button::ButtonCustomVariant::new(cx)
                        .color(t.secondary)
                        .foreground(t.foreground)
                        .hover(t.secondary.opacity(0.9))
                        .active(t.secondary.opacity(0.8)),
                )
                .xsmall()
                .h(px(24.))
                .px(px(7.))
                .gap(px(4.))
                .rounded(px(8.))
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .child(div().max_w(px(256.)).truncate().child(scope_label))
                .child(Icon::new(Lucide::ChevronDown).with_size(px(14.)).opacity(0.7))
                .dropdown_menu(move |menu: PopupMenu, window, _| {
                    let pick =
                        |label: SharedString, oid: Option<String>, short: Option<String>, window: &mut Window| {
                            let checked = current == oid;
                            let item = match short {
                                Some(short) => PopupMenuItem::element(move |_, cx| {
                                    h_flex()
                                        .w_full()
                                        .gap(px(8.))
                                        .child(div().min_w_0().max_w(px(320.)).truncate().child(label.clone()))
                                        .child(
                                            div()
                                                .ml_auto()
                                                .flex_none()
                                                .font_family(FONT_MONO)
                                                .text_xs()
                                                .text_color(theme::tokens(cx).muted_foreground)
                                                .child(short.clone()),
                                        )
                                }),
                                None => PopupMenuItem::new(label),
                            };
                            item.checked(checked).on_click(window.listener_for(&view, move |this, _, _, cx| {
                                this.set_pr_code_commit(oid.clone(), cx);
                            }))
                        };
                    let mut menu = menu.min_w(px(220.)).item(pick("All commits".into(), None, None, window));
                    for commit in newest_first.iter().map(|&i| &activity.commits[i]) {
                        menu = menu.item(pick(
                            commit.message_headline.clone().into(),
                            Some(commit.oid.clone()),
                            Some(logic::short_oid(&commit.oid).to_owned()),
                            window,
                        ));
                    }
                    menu.max_h(px(360.)).scrollable(true)
                })
        });

        let shown = code.shown.len();
        let partial = code.next_cursor().is_some();
        let viewed_enabled = detail.capabilities.viewed_files.is_some();
        let environment_store = detail.capabilities.viewed_files.as_deref() == Some("environment");
        let viewed_count = code.shown.iter().filter(|p| code.is_viewed(p)).count();
        let withheld = code.slices.iter().any(|s| s.truncated);
        let warning = |id: &'static str, tip: String| {
            div()
                .id(id)
                .flex_none()
                .flex()
                .child(Icon::new(Lucide::TriangleAlert).with_size(px(14.)).text_color(t.warning_foreground))
                .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                .into_any_element()
        };
        let mut meta: Vec<Option<AnyElement>> = vec![Some(
            div()
                .flex_none()
                .child(format!(
                    "{shown} {}{}",
                    if shown == 1 { "file" } else { "files" },
                    if partial { "+" } else { "" }
                ))
                .into_any_element(),
        )];
        if code.hidden_tests > 0 {
            let hidden = code.hidden_tests;
            meta.push(Some(
                div()
                    .id("pr-code-hidden-tests")
                    .flex_none()
                    .cursor_pointer()
                    .text_color(t.foreground)
                    .hover(gpui_kit::Styled::underline)
                    .child(format!("{hidden} test {} hidden", if hidden == 1 { "file" } else { "files" }))
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(code) = this.pr_code() {
                            code.hide_tests = false;
                        }
                        this.rebuild_pr_code(cx);
                        cx.notify();
                    }))
                    .into_any_element(),
            ));
        }
        if viewed_enabled && shown > 0 {
            let mut viewed = h_flex()
                .min_w_0()
                .gap(px(4.))
                .child(div().flex_none().child(format!("{viewed_count} / {shown}")))
                .child(div().truncate().child(if environment_store { "viewed in T3 Code" } else { "viewed" }));
            if environment_store {
                viewed = viewed.child(
                    div()
                        .id("pr-code-viewed-store")
                        .flex_none()
                        .flex()
                        .child(Icon::new(Lucide::Info).with_size(px(14.)))
                        .tooltip(|window, cx| {
                            Tooltip::new("This host keeps no shared record of which files you have read, so these ticks are kept by this environment. They follow you between the apps connected to it, but the host's own web UI will not show them.").build(window, cx)
                        }),
                );
            }
            if let Some(error) = &code.viewed_error {
                viewed = viewed.child(warning(
                    "pr-code-viewed-error",
                    format!(
                        "The boxes below are whatever was last read, and empty if nothing has been read yet. {error}"
                    ),
                ));
            }
            if code.viewed_truncated {
                viewed = viewed.child(warning(
                    "pr-code-viewed-truncated",
                    "This change has more files than the host will report ticks for in one read, so the count is short and some boxes below start empty.".into(),
                ));
            }
            meta.push(Some(viewed.into_any_element()));
        }
        if withheld {
            meta.push(Some(warning(
                "pr-code-withheld",
                "The host withheld part of this diff: a binary file, or a change too large to inline.".into(),
            )));
        }
        if code.commit.is_some() && detail.capabilities.review.inline_comment && detail.viewer_permissions.comment {
            meta.push(Some(
                div()
                    .id("pr-code-commit-comments")
                    .flex_none()
                    .flex()
                    .child(Icon::new(Lucide::MessageSquareOff).with_size(px(14.)))
                    .tooltip(|window, cx| {
                        Tooltip::new(
                            "A comment is anchored to the whole change, so switch to All commits to write one.",
                        )
                        .build(window, cx)
                    })
                    .into_any_element(),
            ));
        }
        let mut line = h_flex().min_w_0().gap(px(6.));
        for (index, segment) in meta.into_iter().flatten().enumerate() {
            if index > 0 {
                line = line.child(div().flex_none().text_color(Hsla { a: 0.5, ..t.muted_foreground }).child("·"));
            }
            line = line.child(segment);
        }

        let split = self.client_settings.values.get("diffLayout").as_str() == Some("split");
        let segment = |id: &'static str, icon: Lucide, pressed: bool, value: bool, cx: &mut Context<Self>| {
            let pressed_bg = Hsla { a: t.input.a * 0.72, ..t.input };
            let hover = Hsla { a: t.input.a * 0.32, ..t.input };
            div()
                .id(id)
                .h(px(24.))
                .px(px(10.))
                .flex()
                .items_center()
                .rounded(px(6.))
                .cursor_pointer()
                .text_color(if pressed { t.foreground } else { t.muted_foreground })
                .when(pressed, |el| el.bg(pressed_bg))
                .when(!pressed, |el| el.hover(move |style| style.bg(hover)))
                .child(Icon::new(icon).with_size(px(14.)))
                .on_click(cx.listener(move |this, _, _, cx| this.set_pr_code_layout(value, cx)))
        };
        let layout = h_flex()
            .p(px(2.))
            .gap(px(2.))
            .rounded(px(8.))
            .bg(Hsla { a: t.input.a * 0.4, ..t.input })
            .child(segment("pr-code-stacked", Lucide::Rows3, !split, false, cx))
            .child(segment("pr-code-split", Lucide::Columns2, split, true, cx));
        let all_folded = !code.shown.is_empty() && code.shown.iter().all(|p| code.diff.read(cx).is_file_collapsed(p));
        let ignore = code.ignore_whitespace;
        let controls = h_flex()
            .flex_none()
            .gap(px(4.))
            .child(
                icon_toggle(
                    "pr-code-whitespace",
                    Lucide::Pilcrow,
                    ignore,
                    if ignore { "Show whitespace changes" } else { "Hide whitespace changes" },
                    t,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    if let Some(code) = this.pr_code() {
                        code.ignore_whitespace = !code.ignore_whitespace;
                    }
                    this.rebuild_pr_code(cx);
                    cx.notify();
                })),
            )
            .child(layout)
            .child(
                icon_toggle(
                    "pr-code-wrap",
                    Lucide::TextWrap,
                    code.wrap,
                    if code.wrap { "Disable line wrapping" } else { "Enable line wrapping" },
                    t,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    if let Some(code) = this.pr_code() {
                        code.wrap = !code.wrap;
                    }
                    cx.notify();
                })),
            )
            .when(!code.shown.is_empty(), |el| {
                el.child(
                    icon_toggle(
                        "pr-code-fold",
                        if all_folded { Lucide::ChevronsUpDown } else { Lucide::ChevronsDownUp },
                        false,
                        if all_folded { "Expand all files" } else { "Collapse all files" },
                        t,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let Some(code) = this.pr_code() else { return };
                        code.folded = !all_folded;
                        let (shown, fold) = (code.shown.clone(), code.folded);
                        code.diff.update(cx, |state, cx| {
                            for path in shown.iter() {
                                state.set_file_collapsed(path, fold, cx);
                            }
                        });
                        cx.notify();
                    })),
                )
            })
            .when(code.test_files > 0, |el| {
                let hide = code.hide_tests;
                el.child(
                    icon_toggle(
                        "pr-code-tests",
                        Lucide::FlaskConicalOff,
                        hide,
                        if hide { "Show test files" } else { "Hide test files" },
                        t,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(code) = this.pr_code() {
                            code.hide_tests = !code.hide_tests;
                        }
                        this.rebuild_pr_code(cx);
                        cx.notify();
                    })),
                )
            });
        h_flex()
            .h(px(40.))
            .flex_none()
            .justify_between()
            .gap(px(8.))
            .px(px(16.))
            .border_b_1()
            .border_color(Hsla { a: t.border.a * 0.6, ..t.border })
            .bg(t.background)
            .text_xs()
            .text_color(t.muted_foreground)
            .child(h_flex().flex_1().min_w_0().gap(px(12.)).children(scope).child(line))
            .child(controls)
            .into_any_element()
    }

    /// "Conversations not on the current diff", folded above the viewer.
    fn render_pr_orphans(&mut self, threads: Vec<&ReviewThread>, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let code = self.pull_requests.panel.as_ref().and_then(|p| p.code.as_ref()).expect("the Code tab is open");
        let open = code.orphans_open;
        let partial = code.next_cursor().is_some();
        let count = threads.len();
        let heading = h_flex()
            .id("pr-code-orphans")
            .w_full()
            .gap(px(6.))
            .px(px(16.))
            .py(px(8.))
            .text_xs()
            .text_color(t.muted_foreground)
            .cursor_pointer()
            .child(if partial {
                "Conversations not on the diff loaded so far"
            } else {
                "Conversations not on the current diff"
            })
            .child(Icon::new(if open { Lucide::ChevronDown } else { Lucide::ChevronRight }).with_size(px(14.)))
            .child(count.to_string())
            .on_click(cx.listener(|this, _, _, cx| {
                if let Some(code) = this.pr_code() {
                    code.orphans_open = !code.orphans_open;
                }
                cx.notify();
            }));
        let mut body = Vec::new();
        if open {
            let mut by_path: Vec<(String, Vec<&ReviewThread>)> = Vec::new();
            for thread in threads {
                match by_path.iter_mut().find(|(p, _)| *p == thread.path) {
                    Some((_, list)) => list.push(thread),
                    None => by_path.push((thread.path.clone(), vec![thread])),
                }
            }
            for (path, threads) in by_path {
                let mut cards = Vec::new();
                for thread in threads {
                    cards.push(
                        v_flex()
                            .children(thread.line.map(|line| {
                                div().px(px(12.)).text_xs().text_color(t.muted_foreground).child(format!("Line {line}"))
                            }))
                            .child(self.render_review_thread_in(thread, t, cx))
                            .into_any_element(),
                    );
                }
                body.push(
                    v_flex()
                        .child(div().px(px(12.)).truncate().text_xs().text_color(t.muted_foreground).child(path))
                        .child(v_flex().mt(px(4.)).gap(px(8.)).children(cards))
                        .into_any_element(),
                );
            }
        }
        v_flex()
            .flex_none()
            .border_b_1()
            .border_color(Hsla { a: t.border.a * 0.6, ..t.border })
            .child(heading)
            .when(open, |el| {
                el.child(
                    v_flex()
                        .id("pr-code-orphan-list")
                        .max_h(px(256.))
                        .overflow_y_scroll()
                        .px(px(16.))
                        .pb(px(12.))
                        .gap(px(12.))
                        .children(body),
                )
            })
            .into_any_element()
    }

    fn render_review_thread(
        &mut self,
        thread: &ReviewThread,
        t: &Tokens,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.render_review_thread_in(thread, t, cx)
    }

    /// `ReviewThreadCard`.
    fn render_review_thread_in(&mut self, thread: &ReviewThread, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let Some(code) = self.pull_requests.panel.as_ref().and_then(|p| p.code.as_ref()) else {
            return div().into_any_element();
        };
        let page = code.pages.get(&thread.id);
        let comments = merge_comments(&thread.comments, page.map_or(&[][..], |p| p.0.as_slice()));
        let next = match page {
            Some((_, next)) => next.clone(),
            None => thread.next_comments_cursor.clone(),
        };
        let count = thread.comment_count.unwrap_or(comments.len());
        let expanded = thread.is_resolved == code.toggled.contains(&thread.id);
        let loading_more = code.loading_more.as_deref() == Some(thread.id.as_str());
        let id = thread.id.clone();
        let success = t.success_foreground;
        let header = h_flex()
            .gap(px(8.))
            .text_xs()
            .text_color(t.muted_foreground)
            .child(
                Icon::new(if thread.is_resolved { Lucide::CircleCheck } else { Lucide::Circle })
                    .with_size(px(14.))
                    .when(thread.is_resolved, |icon| icon.text_color(success)),
            )
            .child(
                div()
                    .id(ElementId::Name(format!("pr-thread-toggle:{}", thread.id).into()))
                    .cursor_pointer()
                    .hover(|s| s.text_color(t.foreground))
                    .child(format!(
                        "{} · {count} {}",
                        if thread.is_resolved { "Resolved" } else { "Open" },
                        if count == 1 { "comment" } else { "comments" }
                    ))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(code) = this.pr_code()
                            && !code.toggled.remove(&id)
                        {
                            code.toggled.insert(id.clone());
                        }
                        cx.notify();
                    })),
            )
            .when(thread.is_outdated, |el| el.child("outdated"))
            .children(self.render_pr_fix_thread(thread, t, cx))
            .children(self.render_pr_thread_actions(thread, t, cx));
        let mut body = Vec::new();
        let mut reply_box = None;
        if expanded {
            let now = now_ms();
            for comment in &comments {
                let time = parse_ms(&comment.created_at).map(|at| relative_time(now - at)).unwrap_or_default();
                let markdown = logic::visible_body(&comment.body)
                    .and_then(|b| self.pr_markdown(&format!("c:{}", comment.id), &b, cx))
                    .map(|state| pr_markdown_view(&state, t, cx.entity()));
                let can_edit = self.pull_requests.panel.as_ref().and_then(|p| p.detail.as_ref()).is_some_and(|d| {
                    logic::can_edit_comment(d, "review-comment", comment.author.as_ref(), comment.can_edit)
                });
                let editor = if self.pr_editing(&comment.id) { self.render_pr_editor(t, cx) } else { None };
                let edit = (can_edit && editor.is_none())
                    .then(|| self.pr_edit_button(comment.id.clone(), "review-comment", comment.body.clone(), t, cx));
                let reactions = self.render_pr_reactions(Some(&comment.id), &comment.reactions, t, cx);
                body.push(
                    v_flex()
                        .min_w_0()
                        .child(
                            h_flex()
                                .flex_wrap()
                                .gap(px(8.))
                                .text_xs()
                                .text_color(t.muted_foreground)
                                .child(actor_label(comment.author.as_ref(), t))
                                .child(time)
                                .children(reactions.map(|r| div().ml_auto().child(r))),
                        )
                        .map(|el| match editor {
                            Some(editor) => el.child(div().mt(px(4.)).child(editor)),
                            None => el.child(
                                h_flex()
                                    .group(super::write::EDIT_GROUP)
                                    .mt(px(4.))
                                    .items_start()
                                    .gap(px(4.))
                                    .child(div().flex_1().min_w_0().text_sm().children(markdown))
                                    .children(edit),
                            ),
                        })
                        .into_any_element(),
                );
            }
            if let Some(reply) = self.render_pr_thread_reply(thread, t, cx) {
                reply_box = Some(reply);
            }
            if let Some(cursor) = next {
                let thread_id = thread.id.clone();
                body.push(
                    ghost_button(ElementId::Name(format!("pr-thread-more:{}", thread.id).into()), t)
                        .disabled(loading_more)
                        .child(if loading_more { "Loading..." } else { "Load more comments" })
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.load_more_thread_comments(thread_id.clone(), cursor.clone(), window, cx);
                        }))
                        .into_any_element(),
                );
            }
        }
        v_flex()
            .mx(px(12.))
            .my(px(8.))
            .p(px(12.))
            .rounded(px(12.))
            .border_1()
            .border_color(Hsla { a: t.border.a * 0.7, ..t.border })
            .bg(t.background)
            .shadow_sm()
            .text_sm()
            .text_color(t.foreground)
            .child(header)
            .when(!body.is_empty(), |el| el.child(v_flex().mt(px(8.)).gap(px(12.)).children(body)))
            .children(reply_box)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn comment(id: &str, body: &str) -> ThreadComment {
        serde_json::from_value(json!({"id": id, "body": body, "createdAt": "2026-10-01T00:00:00Z"})).unwrap()
    }

    #[test]
    fn later_pages_replace_what_they_repeat_and_add_the_rest() {
        let merged = merge_comments(
            &[comment("a", "one"), comment("b", "two")],
            &[comment("b", "edited"), comment("c", "three")],
        );
        let shape: Vec<(&str, &str)> = merged.iter().map(|c| (c.id.as_str(), c.body.as_str())).collect();
        assert_eq!(shape, [("a", "one"), ("b", "edited"), ("c", "three")]);
    }

    #[test]
    fn a_rate_limit_reads_as_one() {
        assert_eq!(
            describe_diff_error("GitHub API: 403 rate limit exceeded", "GitHub", "pull request").0,
            "GitHub rate limit reached"
        );
        assert_eq!(describe_diff_error("boom", "GitHub", "pull request").0, "Couldn't load the changes");
    }
}
