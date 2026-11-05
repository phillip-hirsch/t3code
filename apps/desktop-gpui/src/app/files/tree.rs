//! The Files tree (`FileBrowserPanel.tsx`, `useDirectoryEntries.ts`): folders list as they open
//! (`projects.listEntries`), a search box finds paths anywhere (`projects.searchEntries`), and a
//! right click offers the editors, Copy mention and Add to chat.

use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::component::input::{InlineToken, Input, InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, WindowExt as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, AppContext as _, ClipboardItem, Context, ElementId, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, ScrollStrategy, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task,
    UniformListScrollHandle, Window, div, prelude::FluentBuilder as _, px, uniform_list,
};
use serde_json::json;

use super::logic::{self, Entry, EntryKind, TreeRow};
use crate::app::Workspace;
use crate::composer_text::{self, Chip};
use crate::theme::Tokens;

const ROW_HEIGHT: f32 = 24.;
/// `useDirectoryEntries` reads at most four folders at once.
const MAX_LOADS: usize = 4;
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(120);
const SEARCH_LIMIT: usize = 200;

#[derive(Default)]
struct PathSearch {
    query: String,
    hits: Vec<Entry>,
    truncated: bool,
    pending: bool,
    error: Option<String>,
    generation: u64,
    _task: Option<Task<()>>,
}

pub(super) struct TreeState {
    /// Each listed folder's own entries; `""` is the root.
    listed: HashMap<String, Vec<Entry>>,
    /// Folders asked for, for Refresh.
    requested: HashSet<String>,
    loading: HashSet<String>,
    queue: VecDeque<String>,
    in_flight: usize,
    errors: HashMap<String, String>,
    expanded: HashSet<String>,
    /// "Expand all" keeps opening folders as they list.
    expand_all: bool,
    query: Entity<InputState>,
    search: PathSearch,
    rows: Rc<Vec<TreeRow>>,
    rows_dirty: bool,
    scroll: UniformListScrollHandle,
    /// The selection last revealed, with its request.
    revealed: Option<(String, u64)>,
    mutation: Option<(Option<String>, Option<String>)>,
    _subscription: Subscription,
}

impl TreeState {
    /// Every listed entry reachable from the root.
    fn reachable(&self) -> Vec<Entry> {
        let mut out = Vec::new();
        let mut stack = vec![String::new()];
        while let Some(folder) = stack.pop() {
            for entry in self.listed.get(&folder).into_iter().flatten() {
                if entry.kind == EntryKind::Directory {
                    stack.push(entry.path.clone());
                }
                out.push(entry.clone());
            }
        }
        out
    }

    fn rebuild_rows(&mut self) {
        let entries = self.reachable();
        let searching = !self.search.query.is_empty() && !self.search.pending;
        self.rows = Rc::new(if searching {
            let merged = logic::merge_search_entries(&entries, &self.search.hits);
            logic::tree_rows(&merged, &self.expanded, Some(&logic::search_paths(&self.search.hits)))
        } else {
            logic::tree_rows(&entries, &self.expanded, None)
        });
        self.rows_dirty = false;
    }

    fn expandable(&self) -> Vec<String> {
        self.reachable().into_iter().filter(|e| e.kind == EntryKind::Directory && !e.ignored).map(|e| e.path).collect()
    }

    fn error(&self) -> Option<&String> {
        let reachable: HashSet<String> = std::iter::once(String::new())
            .chain(self.reachable().into_iter().filter(|e| e.kind == EntryKind::Directory).map(|e| e.path))
            .collect();
        self.errors.iter().find(|(path, _)| reachable.contains(*path)).map(|(_, error)| error)
    }
}

impl Workspace {
    fn tree(&mut self, cwd: &str) -> Option<&mut TreeState> {
        self.files.trees.get_mut(cwd)
    }

    /// The tree for `cwd`, created (and its root listed) on first use.
    fn ensure_tree(&mut self, cwd: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.files.trees.contains_key(cwd) {
            return;
        }
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search files"));
        let key = cwd.to_owned();
        let subscription = cx.subscribe_in(&query, window, move |this, input, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::Change) {
                let value = input.read(cx).value().to_string();
                this.search_tree(&key, value, cx);
            }
        });
        self.files.trees.insert(
            cwd.to_owned(),
            TreeState {
                listed: HashMap::new(),
                requested: HashSet::new(),
                loading: HashSet::new(),
                queue: VecDeque::new(),
                in_flight: 0,
                errors: HashMap::new(),
                expanded: HashSet::new(),
                expand_all: false,
                query,
                search: PathSearch::default(),
                rows: Rc::default(),
                rows_dirty: true,
                scroll: UniformListScrollHandle::new(),
                revealed: None,
                mutation: None,
                _subscription: subscription,
            },
        );
        self.load_folder(cwd, String::new(), false, cx);
    }

    /// Lists `folder` once (or again with `refresh`), at most four at a time.
    fn load_folder(&mut self, cwd: &str, folder: String, refresh: bool, cx: &mut Context<Self>) {
        let Some(tree) = self.tree(cwd) else { return };
        if tree.loading.contains(&folder) || (!refresh && tree.listed.contains_key(&folder)) {
            return;
        }
        tree.requested.insert(folder.clone());
        tree.loading.insert(folder.clone());
        tree.queue.push_back(folder);
        self.pump_folder_loads(cwd, cx);
    }

    fn pump_folder_loads(&mut self, cwd: &str, cx: &mut Context<Self>) {
        let client = self.client();
        let Some(tree) = self.tree(cwd) else { return };
        while tree.in_flight < MAX_LOADS {
            let Some(folder) = tree.queue.pop_front() else { break };
            let Some(client) = client.clone() else {
                tree.loading.remove(&folder);
                tree.errors.insert(folder, "Not connected.".into());
                continue;
            };
            tree.in_flight += 1;
            let cwd = cwd.to_owned();
            let mut input = json!({"cwd": cwd});
            if !folder.is_empty() {
                input["directoryPath"] = folder.clone().into();
            }
            cx.spawn(async move |this, cx| {
                let result = client.request("projects.listEntries", input).await;
                let _ = this.update(cx, |this, cx| {
                    let Some(tree) = this.tree(&cwd) else { return };
                    tree.in_flight -= 1;
                    tree.loading.remove(&folder);
                    match result {
                        Ok(value) => {
                            let entries: Vec<Entry> = logic::entries_from_json(&value)
                                .into_iter()
                                .filter(|e| logic::parent_of(&e.path) == folder)
                                .collect();
                            let opens: Vec<String> = if tree.expand_all {
                                entries
                                    .iter()
                                    .filter(|e| e.kind == EntryKind::Directory && !e.ignored)
                                    .map(|e| e.path.clone())
                                    .collect()
                            } else {
                                Vec::new()
                            };
                            tree.expanded.extend(opens);
                            tree.listed.insert(folder.clone(), entries);
                            tree.errors.remove(&folder);
                        }
                        Err(error) => {
                            tree.errors.insert(folder.clone(), logic::error_message(&error));
                        }
                    }
                    tree.rows_dirty = true;
                    this.pump_folder_loads(&cwd, cx);
                    this.load_open_folders(&cwd, cx);
                    cx.notify();
                });
            })
            .detach();
        }
    }

    /// Lists every open folder not listed yet (a chain grows as its folders list).
    fn load_open_folders(&mut self, cwd: &str, cx: &mut Context<Self>) {
        let Some(tree) = self.tree(cwd) else { return };
        if tree.rows_dirty {
            tree.rebuild_rows();
        }
        let missing: Vec<String> = tree
            .rows
            .iter()
            .filter(|row| row.expanded && !tree.listed.contains_key(&row.path) && !tree.errors.contains_key(&row.path))
            .map(|row| row.path.clone())
            .collect();
        for folder in missing {
            self.load_folder(cwd, folder, false, cx);
        }
    }

    fn toggle_tree_folder(&mut self, cwd: &str, row: &TreeRow, cx: &mut Context<Self>) {
        let Some(tree) = self.tree(cwd) else { return };
        if row.expanded {
            let (top, path) = (row.top.clone(), row.path.clone());
            tree.expanded.retain(|p| !(p == &path || (p.starts_with(&top) && path.starts_with(&format!("{p}/")))));
            tree.expand_all = false;
        } else {
            tree.expanded.insert(row.path.clone());
        }
        tree.rows_dirty = true;
        self.load_open_folders(cwd, cx);
        cx.notify();
    }

    fn toggle_all_folders(&mut self, cwd: &str, cx: &mut Context<Self>) {
        let Some(tree) = self.tree(cwd) else { return };
        let expandable = tree.expandable();
        let all_open = tree.expand_all || expandable.iter().all(|p| tree.expanded.contains(p));
        if all_open {
            tree.expanded.clear();
            tree.expand_all = false;
        } else {
            tree.expand_all = true;
            tree.expanded.extend(expandable);
        }
        tree.rows_dirty = true;
        self.load_open_folders(cwd, cx);
        cx.notify();
    }

    /// Refresh: lists every requested folder still in the tree again, and the search.
    fn refresh_tree(&mut self, cwd: &str, cx: &mut Context<Self>) {
        let Some(tree) = self.tree(cwd) else { return };
        let reachable: HashSet<String> = std::iter::once(String::new())
            .chain(tree.reachable().into_iter().filter(|e| e.kind == EntryKind::Directory).map(|e| e.path))
            .collect();
        let folders: Vec<String> = tree.requested.iter().filter(|p| reachable.contains(*p)).cloned().collect();
        let query = std::mem::take(&mut tree.search.query);
        for folder in folders {
            self.load_folder(cwd, folder, true, cx);
        }
        self.search_tree(cwd, query, cx);
    }

    /// `useProjectPathSearch` for the tree's search box.
    fn search_tree(&mut self, cwd: &str, value: String, cx: &mut Context<Self>) {
        let client = self.client();
        let Some(tree) = self.tree(cwd) else { return };
        let query: String = value.chars().take(256).collect::<String>().trim().to_owned();
        if tree.search.query == query {
            return;
        }
        tree.search.generation += 1;
        let generation = tree.search.generation;
        tree.search.query = query.clone();
        tree.search._task = None;
        tree.search.error = None;
        tree.rows_dirty = true;
        if query.is_empty() {
            tree.search.pending = false;
            tree.search.hits.clear();
            cx.notify();
            return;
        }
        tree.search.pending = true;
        let Some(client) = client else { return };
        let cwd = cwd.to_owned();
        let payload = json!({"cwd": cwd, "query": query, "limit": SEARCH_LIMIT});
        tree.search._task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SEARCH_DEBOUNCE).await;
            let result = client.request("projects.searchEntries", payload).await;
            let _ = this.update(cx, |this, cx| {
                let Some(tree) = this.tree(&cwd).filter(|t| t.search.generation == generation) else { return };
                tree.search.pending = false;
                match result {
                    Ok(value) => {
                        tree.search.hits = logic::entries_from_json(&value);
                        tree.search.truncated = value["truncated"].as_bool().unwrap_or(false);
                    }
                    Err(error) => tree.search.error = Some(logic::error_message(&error)),
                }
                tree.rows_dirty = true;
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn clear_tree_search(&mut self, cwd: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tree) = self.tree(cwd) else { return };
        let input = tree.query.clone();
        input.update(cx, |input, cx| input.set_value("", window, cx));
        self.search_tree(cwd, String::new(), cx);
        window.blur(cx);
    }

    /// Keeps the tree current: a new workspace mutation lists the open folders again, and a new
    /// selection opens the folders above it and scrolls to it.
    fn sync_tree(
        &mut self,
        cwd: &str,
        selected: Option<&str>,
        reveal: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mutation = self.current_workspace_mutation();
        let Some(tree) = self.tree(cwd) else { return };
        let previous = tree.mutation.replace(mutation.clone());
        if previous.is_some_and(|p| p != mutation) {
            self.refresh_tree(cwd, cx);
        }
        let Some(selected) = selected.filter(|s| !s.is_empty()) else { return };
        let Some(tree) = self.tree(cwd) else { return };
        let request = (selected.to_owned(), reveal);
        if tree.revealed.as_ref() == Some(&request) {
            return;
        }
        let ancestors = logic::ancestors(selected);
        // Revealing a selection made elsewhere closes the search, as the web's tree does.
        if !tree.search.query.is_empty() {
            let input = tree.query.clone();
            input.update(cx, |input, cx| input.set_value("", window, cx));
            self.search_tree(cwd, String::new(), cx);
        }
        let Some(tree) = self.tree(cwd) else { return };
        tree.expanded.extend(ancestors.iter().cloned());
        tree.rows_dirty = true;
        let known = ancestors.iter().all(|a| tree.listed.contains_key(a)) && tree.listed.contains_key("");
        if tree.rows_dirty {
            tree.rebuild_rows();
        }
        if let Some(index) = tree.rows.iter().position(|r| r.path == selected) {
            tree.scroll.scroll_to_item(index, ScrollStrategy::Center);
            tree.revealed = Some(request);
        } else if known {
            tree.revealed = Some(request);
        }
        self.load_open_folders(cwd, cx);
    }

    /// The tree pane: the search bar, status lines and the rows.
    pub(super) fn render_tree(
        &mut self,
        cwd: &str,
        selected: Option<&str>,
        reveal: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.ensure_tree(cwd, window, cx);
        self.sync_tree(cwd, selected, reveal, window, cx);
        let tokens = *crate::theme::tokens(cx);
        let Some(tree) = self.tree(cwd) else { return div().into_any_element() };
        if tree.rows_dirty {
            tree.rebuild_rows();
        }
        let rows = tree.rows.clone();
        let ready = tree.listed.contains_key("");
        let pending = !tree.loading.is_empty();
        let error = tree.error().cloned().or_else(|| tree.search.error.clone());
        let searching = !tree.search.query.is_empty();
        let has_folders = rows.iter().any(|r| r.kind == EntryKind::Directory);
        let all_open = tree.expand_all || {
            let expandable = tree.expandable();
            !expandable.is_empty() && expandable.iter().all(|p| tree.expanded.contains(p))
        };
        let no_matches = searching && !tree.search.pending && tree.search.hits.is_empty();
        let truncated = searching && tree.search.truncated && !tree.search.pending;
        let loading_line = (!ready || tree.search.pending) && (rows.is_empty() || no_matches);
        let query = tree.query.clone();
        let loading_folders: Rc<HashSet<String>> = Rc::new(tree.loading.clone());
        let scroll = tree.scroll.clone();
        let muted = tokens.muted_foreground;
        let key = cwd.to_owned();
        let icon_button = |id: &'static str, icon: IconName, label: SharedString| {
            div()
                .id(id)
                .size(px(24.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(6.))
                .cursor_pointer()
                .text_color(muted)
                .hover(|s| s.bg(tokens.accent).text_color(tokens.foreground))
                .child(Icon::new(icon).size(px(14.)))
                .tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx))
        };
        let refresh_key = key.clone();
        let toggle_key = key.clone();
        let clear_key = key.clone();
        let subheader = h_flex()
            .flex_none()
            .h(px(36.))
            .mb(px(4.))
            .gap(px(4.))
            .px(px(8.))
            .child(
                icon_button(
                    "files-refresh",
                    IconName::RefreshCw,
                    if pending { "Refreshing…" } else { "Refresh files" }.into(),
                )
                .on_click(cx.listener(move |this, _, _, cx| this.refresh_tree(&refresh_key, cx))),
            )
            .child(
                div()
                    .id("files-search")
                    .flex_1()
                    .min_w_0()
                    .h(px(28.))
                    .on_key_down(cx.listener(move |this, event: &gpui_kit::KeyDownEvent, window, cx| {
                        if event.keystroke.key == "escape" {
                            this.clear_tree_search(&clear_key, window, cx);
                            cx.stop_propagation();
                        }
                    }))
                    .child(Input::new(&query).appearance(false).text_xs().h(px(28.)).px(px(8.))),
            )
            .when(has_folders, |el| {
                el.child(
                    icon_button(
                        "files-expand-all",
                        if all_open { IconName::ChevronsDownUp } else { IconName::ChevronsUpDown },
                        if all_open { "Collapse all folders" } else { "Expand all folders" }.into(),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_all_folders(&toggle_key, cx))),
                )
            });
        let retry_key = key.clone();
        let list: AnyElement = if no_matches {
            div().px(px(12.)).py(px(16.)).text_xs().text_color(muted).child("No matching files.").into_any_element()
        } else {
            let selected = selected.map(str::to_owned);
            let list_key = key.clone();
            uniform_list(
                ElementId::Name(format!("files-tree:{cwd}").into()),
                rows.len(),
                cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                    let tokens = *crate::theme::tokens(cx);
                    range
                        .map(|index| {
                            let row = &rows[index];
                            let loading = row.expanded && loading_folders.contains(&row.path);
                            this.tree_row(
                                &list_key,
                                row,
                                selected.as_deref() == Some(row.path.as_str()),
                                loading,
                                &tokens,
                                cx,
                            )
                        })
                        .collect::<Vec<_>>()
                }),
            )
            .track_scroll(&scroll)
            .flex_1()
            .px(px(8.))
            .pb(px(8.))
            .into_any_element()
        };
        v_flex()
            .size_full()
            .min_h_0()
            .bg(tokens.background)
            .child(subheader)
            .children(error.map(|error| {
                div()
                    .id("files-error")
                    .p(px(16.))
                    .text_xs()
                    .line_height(gpui_kit::relative(1.625))
                    .text_color(tokens.error)
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| this.refresh_tree(&retry_key, cx)))
                    .child(format!("{error} Click to retry."))
            }))
            .when(truncated, |el| {
                el.child(
                    div()
                        .px(px(12.))
                        .py(px(4.))
                        .text_xs()
                        .text_color(muted)
                        .child("More matches available. Refine your search."),
                )
            })
            .when(loading_line, |el| {
                el.child(div().px(px(12.)).py(px(4.)).text_xs().text_color(muted).child("Loading files…"))
            })
            .child(list)
            .into_any_element()
    }

    fn tree_row(
        &mut self,
        cwd: &str,
        row: &TreeRow,
        selected: bool,
        loading: bool,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let folder = row.kind == EntryKind::Directory;
        let glyph = match (folder, row.expanded, loading) {
            (true, true, true) => IconName::LoaderCircle,
            (true, true, false) => IconName::ChevronDown,
            (true, false, _) => IconName::ChevronRight,
            (false, _, _) => IconName::File,
        };
        let fg = tokens.foreground;
        let (click_key, click_row) = (cwd.to_owned(), row.clone());
        let menu = self.tree_context_menu(cwd, &row.path, cx);
        h_flex()
            .id(ElementId::Name(format!("files-row:{}", row.path).into()))
            .h(px(ROW_HEIGHT))
            .w_full()
            .min_w_0()
            .gap(px(4.))
            .pl(px(6. + row.depth as f32 * 16.))
            .pr(px(6.))
            .rounded(px(6.))
            .cursor_pointer()
            .text_xs()
            .text_color(if row.ignored { tokens.muted_foreground } else { fg })
            .when(selected, |el| el.bg(fg.opacity(0.12)))
            .when(!selected, |el| el.hover(move |s| s.bg(fg.opacity(0.07))))
            .on_click(cx.listener(move |this, _, window, cx| {
                if click_row.kind == EntryKind::Directory {
                    this.toggle_tree_folder(&click_key, &click_row, cx);
                } else {
                    this.open_file(&click_row.path, None, window, cx);
                }
            }))
            .child(
                Icon::new(glyph)
                    .size(px(16.))
                    .flex_none()
                    .text_color(tokens.muted_foreground)
                    .when(loading, |el| el.opacity(0.6)),
            )
            .child(div().min_w_0().truncate().child(row.label.clone()))
            .context_menu(menu)
            .into_any_element()
    }

    /// The row's menu: Open, Reveal, Open with, then Copy mention and Add to chat.
    fn tree_context_menu(
        &self,
        cwd: &str,
        path: &str,
        cx: &mut Context<Self>,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        let view = cx.entity();
        let editors = self.server_info.available_editors.clone();
        let config = &self.server_info.config;
        let file_manager = editors.iter().any(|e| e == "file-manager");
        let reveal = (file_manager && config["shellRevealInFileManager"] == true).then(|| {
            logic::reveal_label(
                config["shellRevealInFileManagerKind"].as_str(),
                self.server_info.platform_os.as_deref(),
            )
        });
        let absolute = logic::absolute_path(cwd, path);
        let path = path.to_owned();
        move |mut menu, window, cx| {
            let open_in = |editor: String, reveal: bool, title: String| {
                let absolute = absolute.clone();
                window.listener_for(&view, move |this: &mut Workspace, _, window, cx| {
                    this.open_path_in_editor(absolute.clone(), editor.clone(), reveal, title.clone(), window, cx)
                })
            };
            if file_manager {
                menu = menu.item(PopupMenuItem::new("Open").icon(IconName::Pencil).on_click(open_in(
                    "file-manager".into(),
                    false,
                    "Could not open file".into(),
                )));
            }
            if let Some(label) = reveal {
                menu = menu.item(PopupMenuItem::new(label).icon(IconName::FolderTree).on_click(open_in(
                    "file-manager".into(),
                    true,
                    "Unable to reveal file".into(),
                )));
            }
            let others: Vec<&String> = editors.iter().filter(|e| *e != "file-manager").collect();
            if !others.is_empty() {
                let items: Vec<(String, String)> =
                    others.iter().map(|e| ((*e).clone(), crate::app::details::editor_label(e).to_owned())).collect();
                let view = view.clone();
                let absolute = absolute.clone();
                menu = menu.submenu("Open with", window, cx, move |mut sub, window, _| {
                    for (editor, label) in &items {
                        let (editor, absolute) = (editor.clone(), absolute.clone());
                        let title = format!("Could not open in {label}");
                        sub = sub.item(PopupMenuItem::new(label.clone()).on_click(window.listener_for(
                            &view,
                            move |this: &mut Workspace, _, window, cx| {
                                this.open_path_in_editor(
                                    absolute.clone(),
                                    editor.clone(),
                                    false,
                                    title.clone(),
                                    window,
                                    cx,
                                )
                            },
                        )));
                    }
                    sub
                });
            }
            let mention = composer_text::file_link(&path);
            let (copy_path, add_path) = (path.clone(), path.clone());
            menu.item(PopupMenuItem::new("Copy mention").on_click(move |_, window, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(mention.clone()));
                window.push_notification(Notification::success(copy_path.clone()).title("Mention copied"), cx);
            }))
            .item(PopupMenuItem::new("Add to chat").on_click(
                window.listener_for(&view, move |this: &mut Workspace, _, window, cx| {
                    this.add_file_to_chat(&add_path, window, cx)
                }),
            ))
        }
    }

    /// `shell.openInEditor` on a path, with the web's failure toast.
    pub(super) fn open_path_in_editor(
        &mut self,
        path: String,
        editor: String,
        reveal: bool,
        failure: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.client() else { return };
        let mut input = json!({"cwd": path, "editor": editor});
        if reveal {
            input["reveal"] = true.into();
        }
        cx.spawn_in(window, async move |_, cx| {
            if client.request("shell.openInEditor", input).await.is_err() {
                let _ = cx.update(|window, cx| {
                    window.push_notification(Notification::error(path.clone()).title(failure.clone()), cx)
                });
            }
        })
        .detach();
    }

    /// Add to chat: the file's mention at the end of the composer, after a space.
    pub(super) fn add_file_to_chat(&mut self, path: &str, window: &mut Window, cx: &mut Context<Self>) {
        let chip = Chip::File { path: path.to_owned() };
        let link = composer_text::file_link(path);
        self.composer.update(cx, |state, cx| {
            let end = state.value().len();
            state.set_selected_range(end..end, cx);
            if end > 0 && !state.value().ends_with(char::is_whitespace) {
                state.insert(" ", window, cx);
            }
            let at = state.value().len();
            let token = InlineToken::new(chip.id(), link).with_label(chip.label().to_owned());
            if state.replace_range_with_token(at..at, token, window, cx).is_ok() {
                state.insert(" ", window, cx);
            }
        });
        self.sync_composer_empty(cx);
        cx.notify();
    }
}
