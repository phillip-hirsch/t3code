//! The editor's "Create a new worktree" workspace mode: its "Base branch" field with the
//! `WorktreeBaseBranchPicker` (`apps/web/src/components/WorktreeBaseBranchPicker.tsx`,
//! `BranchPicker.tsx`, `usePaginatedBranches` in `apps/web/src/state/queries.ts`).
//!
//! How the editor uses it: create `cx.new(|cx| NewWorktreeMode::new(client, cwd, task, window, cx))`
//! when the dialog opens (`cwd` is the selected project's `workspaceRoot`, `task` the edited task's
//! JSON or `None`), and keep it for the dialog's life. Show `render_new_worktree_mode(&mode)` while
//! the workspace mode is "worktree". Call `set_project` when the project changes (the picked branch
//! stays, as on the web), `set_disabled` while saving or disconnected, and on save use
//! `mode.read(cx).strategy()` as the upsert's `workspaceStrategy`. Switching environments resets the
//! branch to "main" from remote: create a fresh one.

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::Button;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Anchor, AnyElement, AppContext as _, Context, Entity, FontWeight, InteractiveElement as _, IntoElement,
    KeyDownEvent, ParentElement as _, Render, ScrollStrategy, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, Task, UniformListScrollHandle, Window, anchored, deferred, div, px, uniform_list,
};
use serde_json::{Value, json};

use crate::rpc::RpcClient;
use crate::theme::{Tokens, tokens};

/// `VCS_REF_LIST_LIMIT`.
const PAGE_LIMIT: u32 = 100;
/// `estimatedItemSize`; the list is `max-h-56`.
const ROW_HEIGHT: f32 = 28.;
const LIST_MAX_HEIGHT: f32 = 224.;

pub(super) struct NewWorktreeMode {
    client: RpcClient,
    cwd: Option<String>,
    base_ref: String,
    start_from_origin: bool,
    disabled: bool,
    open: bool,
    search: Entity<InputState>,
    refs: RefPages,
    /// Index into `refs.refs` that Enter picks (`autoHighlight`).
    highlighted: usize,
    /// `selectedRefQuery`: the picked ref, for its remote flag when it is not in the loaded pages.
    selected_ref: Option<Value>,
    _selected_ref_task: Option<Task<()>>,
    scroll: UniformListScrollHandle,
    _search_subscription: Subscription,
}

/// The ref pages loaded for one search, merged by name like `usePaginatedBranches`.
#[derive(Default)]
struct RefPages {
    refs: Vec<Value>,
    /// `None` until the first page lands.
    next_cursor: Option<Option<u64>>,
    total_count: u64,
    error: Option<SharedString>,
    /// The page in flight, and whether it is a next page.
    loading: Option<(Task<()>, bool)>,
}

/// The editor's "Base branch" field; `mode` is the editor's retained [`NewWorktreeMode`].
pub(super) fn render_new_worktree_mode(mode: &Entity<NewWorktreeMode>) -> impl IntoElement {
    mode.clone()
}

impl NewWorktreeMode {
    pub(super) fn new(
        client: RpcClient,
        cwd: Option<String>,
        task: Option<&Value>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (base_ref, start_from_origin) = draft_from_task(task);
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search refs..."));
        let subscription = cx.subscribe_in(&search, window, |this, _, event: &InputEvent, window, cx| match event {
            InputEvent::Change => this.load_first_page(cx),
            InputEvent::PressEnter { .. } => {
                if let Some(name) = this.refs.refs.get(this.highlighted).and_then(|r| r["name"].as_str()) {
                    this.pick(name.to_owned(), window, cx);
                }
            }
            _ => {}
        });
        let mut mode = Self {
            client,
            cwd,
            base_ref,
            start_from_origin,
            disabled: false,
            open: false,
            search,
            refs: RefPages::default(),
            highlighted: 0,
            selected_ref: None,
            _selected_ref_task: None,
            scroll: UniformListScrollHandle::new(),
            _search_subscription: subscription,
        };
        mode.load_first_page(cx);
        mode.load_selected_ref(cx);
        mode
    }

    /// The editor's project changed; the web remounts the picker, keeping the picked branch.
    #[allow(dead_code, reason = "called by the task list's editor")]
    pub(super) fn set_project(&mut self, cwd: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        if self.cwd == cwd {
            return;
        }
        self.cwd = cwd;
        self.close(window, cx);
        self.load_first_page(cx);
        self.load_selected_ref(cx);
    }

    pub(super) fn set_disabled(&mut self, disabled: bool, cx: &mut Context<Self>) {
        if self.disabled != disabled {
            self.disabled = disabled;
            if disabled {
                self.open = false;
            }
            cx.notify();
        }
    }

    /// The upsert's `workspaceStrategy`.
    pub(super) fn strategy(&self) -> Value {
        strategy(&self.base_ref, self.start_from_origin)
    }

    fn query(&self, cx: &Context<Self>) -> String {
        sanitize_new_ref_name(&self.search.read(cx).value())
    }

    fn load_first_page(&mut self, cx: &mut Context<Self>) {
        self.refs = RefPages::default();
        self.highlighted = 0;
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
        self.load_page(None, cx);
    }

    fn load_next_page(&mut self, cx: &mut Context<Self>) {
        if let (Some(Some(cursor)), None) = (self.refs.next_cursor, &self.refs.loading) {
            self.load_page(Some(cursor), cx);
        }
    }

    fn load_page(&mut self, cursor: Option<u64>, cx: &mut Context<Self>) {
        let Some(cwd) = self.cwd.clone() else { return };
        let mut input = json!({"cwd": cwd, "limit": PAGE_LIMIT});
        let query = self.query(cx);
        if !query.is_empty() {
            input["query"] = query.into();
        }
        if let Some(cursor) = cursor {
            input["cursor"] = cursor.into();
        }
        let client = self.client.clone();
        let task = cx.spawn(async move |this, cx| {
            let result = client.request("vcs.listRefs", input).await;
            let _ = this.update(cx, |this, cx| {
                this.refs.loading = None;
                match result {
                    Ok(page) => this.refs.merge(&page),
                    Err(error) => {
                        log::warn!("vcs.listRefs failed: {error}");
                        let message = match &error {
                            crate::rpc::RpcError::Failure(f) => f["message"].as_str().map(str::to_owned),
                            _ => None,
                        };
                        this.refs.error = Some(
                            message.filter(|m| !m.trim().is_empty()).unwrap_or("Failed to load refs.".into()).into(),
                        );
                    }
                }
                cx.notify();
            });
        });
        self.refs.loading = Some((task, cursor.is_some()));
        cx.notify();
    }

    fn load_selected_ref(&mut self, cx: &mut Context<Self>) {
        self.selected_ref = None;
        let (Some(cwd), false) = (self.cwd.clone(), self.base_ref.is_empty()) else {
            self._selected_ref_task = None;
            return;
        };
        let (client, name) = (self.client.clone(), self.base_ref.clone());
        self._selected_ref_task = Some(cx.spawn(async move |this, cx| {
            let input = json!({"cwd": cwd, "query": name, "limit": 10});
            let Ok(page) = client.request("vcs.listRefs", input).await else { return };
            let _ = this.update(cx, |this, cx| {
                this.selected_ref =
                    page["refs"].as_array().and_then(|refs| refs.iter().find(|r| r["name"] == this.base_ref)).cloned();
                cx.notify();
            });
        }));
    }

    fn pick(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        self.base_ref = name;
        self.close(window, cx);
        self.load_selected_ref(cx);
    }

    fn set_open(&mut self, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !open {
            return self.close(window, cx);
        }
        self.open = true;
        self.search.update(cx, |search, cx| search.focus(window, cx));
        cx.notify();
    }

    /// Closing clears the search, like `handleOpenChange(false)`.
    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let had_query = !self.search.read(cx).value().is_empty();
        self.open = false;
        if had_query {
            self.search.update(cx, |search, cx| search.set_value("", window, cx));
        }
        cx.notify();
    }

    fn move_highlight(&mut self, down: bool, cx: &mut Context<Self>) {
        let count = self.refs.refs.len();
        if count == 0 {
            return;
        }
        self.highlighted =
            if down { (self.highlighted + 1).min(count - 1) } else { self.highlighted.saturating_sub(1) };
        self.scroll.scroll_to_item(self.highlighted, ScrollStrategy::Nearest);
        if down && self.highlighted + 1 == count {
            self.load_next_page(cx);
        }
        cx.notify();
    }

    /// `statusText`.
    fn status(&self) -> Option<SharedString> {
        if let Some(error) = &self.refs.error {
            return Some(error.clone());
        }
        match (&self.refs.loading, self.refs.next_cursor) {
            (Some(_), None) => Some("Loading refs...".into()),
            (Some((_, true)), _) => Some("Loading more refs...".into()),
            (_, Some(Some(_))) => {
                Some(format!("Showing {} of {} refs", self.refs.refs.len(), self.refs.total_count).into())
            }
            _ => None,
        }
    }

    fn label(&self) -> String {
        let selected =
            self.refs.refs.iter().find(|r| r["name"] == self.base_ref.as_str()).or(self.selected_ref.as_ref());
        trigger_label(&self.base_ref, selected.map(|r| r["isRemote"].as_bool() == Some(true)), self.start_from_origin)
    }

    fn render_popup(&self, t: &Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let count = self.refs.refs.len();
        let list = uniform_list(
            "base-branch-refs",
            count,
            cx.processor(|this, range: std::ops::Range<usize>, window, cx| {
                // Rows near the end ask for the next page, like `shouldLoadNextBranchPageAfterScroll`.
                if range.end + 4 >= this.refs.refs.len() {
                    cx.defer_in(window, |this, _, cx| this.load_next_page(cx));
                }
                let t = *tokens(cx);
                range.map(|index| this.render_ref(index, &t, cx)).collect::<Vec<AnyElement>>()
            }),
        )
        .track_scroll(&self.scroll)
        .h(px((count as f32 * ROW_HEIGHT).min(LIST_MAX_HEIGHT)))
        .px_1();
        v_flex()
            .id("base-branch-popup")
            .w(px(320.))
            .mt_1()
            .rounded(px(8.))
            .border_1()
            .border_color(t.border)
            .bg(t.popover)
            .shadow_lg()
            .overflow_hidden()
            .text_color(t.foreground)
            .occlude()
            .on_mouse_down_out(cx.listener(|this, _, window, cx| this.close(window, cx)))
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                match event.keystroke.key.as_str() {
                    "escape" => this.close(window, cx),
                    "up" => this.move_highlight(false, cx),
                    "down" => this.move_highlight(true, cx),
                    _ => return,
                }
                cx.stop_propagation();
            }))
            // `ComboboxSearchInput`.
            .child(
                div().px_3().pt(px(10.)).child(
                    h_flex()
                        .gap_1()
                        .pb(px(6.))
                        .border_b_1()
                        .border_color(t.border.opacity(0.7))
                        .child(
                            Icon::new(Lucide::Search).with_size(px(16.)).text_color(t.muted_foreground.opacity(0.55)),
                        )
                        .child(div().flex_1().child(Input::new(&self.search).appearance(false).small())),
                ),
            )
            .when(count == 0, |popup| {
                popup.child(div().p_2().text_center().text_sm().text_color(t.muted_foreground).child("No refs found."))
            })
            .when(count > 0, |popup| popup.child(div().py_2().child(list)))
            .child(self.render_origin_control(t, cx))
            .children(self.status().map(|status| {
                div()
                    .px_3()
                    .py_2()
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(t.muted_foreground)
                    .child(status)
            }))
    }

    /// `BranchPickerRefItem`.
    fn render_ref(&self, index: usize, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let item = &self.refs.refs[index];
        let name = item["name"].as_str().unwrap_or_default().to_owned();
        let flag = |key: &str| item[key].as_bool() == Some(true);
        let other_worktree =
            item["worktreePath"].as_str().is_some_and(|path| self.cwd.as_deref().is_some_and(|cwd| path != cwd));
        let badge = if flag("current") {
            Some("current")
        } else if other_worktree {
            Some("worktree")
        } else if flag("isRemote") {
            Some("remote")
        } else if flag("isDefault") {
            Some("default")
        } else {
            None
        };
        let highlighted = index == self.highlighted;
        h_flex()
            .id(SharedString::from(format!("base-ref-{name}")))
            .w_full()
            .h(px(ROW_HEIGHT))
            .px_2()
            .gap_2()
            .rounded(px(4.))
            .cursor_pointer()
            .text_sm()
            .when(highlighted, |row| row.bg(t.foreground.opacity(0.09)))
            .hover(|row| row.bg(t.foreground.opacity(0.09)))
            .child(div().flex_1().min_w_0().truncate().child(name.clone()))
            .children(badge.map(|badge| {
                div().flex_none().text_size(px(10.)).text_color(t.muted_foreground.opacity(0.45)).child(badge)
            }))
            .on_click(cx.listener(move |this, _, window, cx| this.pick(name.clone(), window, cx)))
            .into_any_element()
    }

    fn render_origin_control(&self, t: &Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .id("base-branch-origin")
            .justify_between()
            .gap_3()
            .px_3()
            .py_2()
            .border_t_1()
            .border_color(t.border.opacity(0.6))
            .text_xs()
            .cursor_pointer()
            .tooltip(|window, cx| {
                gpui_kit::component::tooltip::Tooltip::new(
                    "Uses the latest commit of the base branch\u{2019}s tracked remote branch, even if its name differs. Otherwise uses the same-named branch on origin. Uses your local branch if that remote branch is missing.",
                )
                .build(window, cx)
            })
            .child(
                h_flex()
                    .min_w_0()
                    .gap(px(6.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(t.muted_foreground)
                    .child(Icon::new(Lucide::RefreshCw).with_size(px(12.)))
                    .child(div().truncate().child("Start from remote")),
            )
            .child(
                Switch::new("base-branch-origin-switch")
                    .small()
                    .checked(self.start_from_origin)
                    .on_change(cx.listener(|this, checked: &bool, _, cx| {
                        this.start_from_origin = *checked;
                        cx.notify();
                    })),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.start_from_origin = !this.start_from_origin;
                cx.notify();
            }))
    }
}

impl RefPages {
    fn merge(&mut self, page: &Value) {
        for item in page["refs"].as_array().into_iter().flatten() {
            match self.refs.iter_mut().find(|known| known["name"] == item["name"]) {
                Some(known) => *known = item.clone(),
                None => self.refs.push(item.clone()),
            }
        }
        self.next_cursor = Some(page["nextCursor"].as_u64());
        self.total_count = self.total_count.max(page["totalCount"].as_u64().unwrap_or(0));
        self.error = None;
    }
}

impl Render for NewWorktreeMode {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *tokens(cx);
        let trigger = Button::new("base-branch-trigger")
            .outline()
            .small()
            .w_full()
            .h(px(28.))
            .justify_between()
            .disabled(self.disabled || self.cwd.is_none())
            .child(Icon::new(Lucide::GitBranch).with_size(px(14.)).text_color(t.muted_foreground))
            .child(div().flex_1().min_w_0().truncate().text_left().child(self.label()))
            .child(Icon::new(Lucide::ChevronDown).with_size(px(14.)).text_color(t.muted_foreground))
            .on_click(cx.listener(|this, _, window, cx| {
                let open = !this.open;
                this.set_open(open, window, cx)
            }));
        v_flex()
            .gap(px(6.))
            .child(
                div()
                    .text_sm()
                    .line_height(px(16.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(t.foreground)
                    .child("Base branch"),
            )
            .child(div().relative().child(trigger).when(self.open && !self.disabled, |field| {
                field.child(
                    deferred(anchored().anchor(Anchor::TopLeft).snap_to_window().child(self.render_popup(&t, cx)))
                        // Above dialogs: the editor is one.
                        .with_priority(gpui_kit::base::POPUP_PRIORITY),
                )
            }))
    }
}

/// The worktree half of `taskToDraft`: the base branch and "start from remote".
fn draft_from_task(task: Option<&Value>) -> (String, bool) {
    match task.map(|task| &task["workspaceStrategy"]).filter(|s| s["type"] == "worktree") {
        Some(strategy) => (
            strategy["baseRef"].as_str().unwrap_or("main").to_owned(),
            strategy["startFromOrigin"].as_bool().unwrap_or(false),
        ),
        None => ("main".into(), true),
    }
}

fn strategy(base_ref: &str, start_from_origin: bool) -> Value {
    let base_ref = base_ref.trim();
    json!({"type": "worktree", "baseRef": if base_ref.is_empty() { "main" } else { base_ref }, "startFromOrigin": start_from_origin})
}

/// `sanitizeNewRefName`: trimmed, with ASCII whitespace runs as one dash.
fn sanitize_new_ref_name(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut in_space = false;
    for c in raw.trim().chars() {
        if matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0c' | '\x0b') {
            if !in_space {
                out.push('-');
            }
            in_space = true;
        } else {
            out.push(c);
            in_space = false;
        }
    }
    out
}

/// `resolveBranchTriggerLabel` for a new worktree. Only the server knows a local branch's remote.
fn trigger_label(base_ref: &str, is_remote: Option<bool>, start_from_origin: bool) -> String {
    if base_ref.is_empty() {
        return "Select ref".into();
    }
    if start_from_origin && is_remote == Some(false) {
        format!("From {base_ref} (remote)")
    } else {
        format!("From {base_ref}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_strategy_round_trips() {
        assert_eq!(draft_from_task(None), ("main".into(), true));
        let task = json!({"workspaceStrategy": {"type": "worktree", "baseRef": "dev"}});
        // A stored strategy without the flag means "local branch", unlike a new task.
        assert_eq!(draft_from_task(Some(&task)), ("dev".into(), false));
        assert_eq!(draft_from_task(Some(&json!({"workspaceStrategy": {"type": "root"}}))), ("main".into(), true));
        assert_eq!(strategy("  ", true), json!({"type": "worktree", "baseRef": "main", "startFromOrigin": true}));
        assert_eq!(strategy(" dev ", false)["baseRef"], "dev");
    }

    #[test]
    fn labels_and_queries_match_the_web() {
        assert_eq!(trigger_label("", None, true), "Select ref");
        assert_eq!(trigger_label("main", Some(false), true), "From main (remote)");
        assert_eq!(trigger_label("origin/main", Some(true), true), "From origin/main");
        assert_eq!(trigger_label("main", None, true), "From main");
        assert_eq!(sanitize_new_ref_name("  feat  new\tthing "), "feat-new-thing");
    }

    #[test]
    fn pages_merge_by_name_and_keep_the_largest_total() {
        let mut pages = RefPages::default();
        pages.merge(&json!({"refs": [{"name": "a"}, {"name": "b"}], "nextCursor": 2, "totalCount": 3}));
        pages.merge(
            &json!({"refs": [{"name": "b", "isRemote": true}, {"name": "c"}], "nextCursor": null, "totalCount": 2}),
        );
        let names: Vec<_> = pages.refs.iter().map(|r| r["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["a", "b", "c"]);
        assert_eq!(pages.refs[1]["isRemote"], true);
        assert_eq!((pages.next_cursor, pages.total_count), (Some(None), 3));
    }
}
