//! `ArchivedThreadsPanel`: the selected environments' archived threads by project, with
//! Unarchive and Delete on each row and in its context menu.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenuItem};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, WindowExt as _, h_flex};
use gpui_kit::{
    AnyElement, App, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    Styled as _, Task, Window, div, px,
};

use serde::Deserialize;
use serde_json::{Value, json};

use super::super::widgets::{RowFrame, settings_section};
use super::{notify_error, now_ms, relative_label, text};
use crate::app::pages::settings::connections::logic::ORCHESTRATION_OPERATE;
use crate::app::pages::settings::widgets::SettingsView;
use crate::app::{Workspace, new_id, sidebar};
use crate::model::{DelegatedFrom, Lineage, ProjectShell};
use crate::project_groups::ProjectGroup;
use crate::rpc::RpcClient;
use crate::settings::scope::{self, ScopeKind};
use crate::theme::{self, Tokens};

/// `(environment id, thread id)`.
type Key<'a> = (&'a str, &'a str);

/// One environment's archived page, decoded once per load.
struct Page {
    environment_id: String,
    projects: Vec<ProjectShell>,
    threads: Vec<ArchivedThread>,
}

impl Page {
    fn decode(environment_id: &str, snapshot: &Value) -> Self {
        let decode = |key: &str| snapshot[key].as_array().map(Vec::as_slice).unwrap_or_default();
        let projects = decode("projects")
            .iter()
            .filter_map(|p| ProjectShell::deserialize(p).ok())
            .map(|mut project| {
                project.environment_id = environment_id.to_owned().into();
                project
            })
            .collect();
        let threads = decode("threads").iter().filter_map(|t| ArchivedThread::deserialize(t).ok()).collect();
        Self { environment_id: environment_id.to_owned(), projects, threads }
    }
}

enum PageState {
    /// Until the first load answers.
    Loading,
    Failed,
    Ready(Rc<Page>),
}

pub(super) struct ArchiveState {
    /// The scope's connected environments in id order (`makeArchivedThreadsEnvironmentKey`),
    /// each with its page.
    pages: Vec<(String, PageState)>,
    loads: HashMap<String, Task<()>>,
    /// Rows with an Unarchive or Delete running or awaiting confirmation; their actions wait.
    pending: HashSet<Target>,
}

impl ArchiveState {
    /// An environment without a connection reads as a failed page, as its query fails on the web.
    fn new(clients: Vec<(String, Option<RpcClient>)>, cx: &mut Context<Self>) -> Self {
        let mut state = Self {
            pages: clients
                .iter()
                .map(|(id, client)| (id.clone(), if client.is_some() { PageState::Loading } else { PageState::Failed }))
                .collect(),
            loads: HashMap::new(),
            pending: HashSet::new(),
        };
        for (id, client) in clients {
            if let Some(client) = client {
                state.load(id, client, cx);
            }
        }
        state
    }

    /// `orchestration.getArchivedShellSnapshot`; the live shell leaves archived threads out. A
    /// reload keeps the last page on screen until the new one answers.
    fn load(&mut self, environment_id: String, client: RpcClient, cx: &mut Context<Self>) {
        let id = environment_id.clone();
        let task = cx.spawn(async move |this, cx| {
            let result = client.request("orchestration.getArchivedShellSnapshot", json!({})).await;
            let _ = this.update(cx, |this, cx| {
                if let Some((_, page)) = this.pages.iter_mut().find(|(id, _)| *id == environment_id) {
                    *page = match result {
                        Ok(snapshot) => PageState::Ready(Rc::new(Page::decode(&environment_id, &snapshot))),
                        Err(_) => PageState::Failed,
                    };
                }
                cx.notify();
            });
        });
        self.loads.insert(id, task);
    }

    fn loaded(&self) -> Vec<Rc<Page>> {
        let ready = |page: &PageState| match page {
            PageState::Ready(page) => Some(page.clone()),
            _ => None,
        };
        self.pages.iter().filter_map(|(_, page)| ready(page)).collect()
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ArchivedThread {
    id: String,
    project_id: String,
    title: String,
    created_at: String,
    #[serde(default)]
    archived_at: Option<String>,
    #[serde(default)]
    lineage: Lineage,
    #[serde(default)]
    delegated_from: Option<DelegatedFrom>,
    #[serde(default, deserialize_with = "crate::model::lossy")]
    tags: Vec<String>,
}

impl ArchivedThread {
    fn archived(&self) -> &str {
        self.archived_at.as_deref().unwrap_or(&self.created_at)
    }
}

/// `hiddenThreadTagsAtom`: the tags an environment's tag filter hides, `None` while it is off.
fn hidden_tags(settings: &Value) -> Option<Vec<String>> {
    if settings["hideTaggedThreads"] != true {
        return None;
    }
    // Trimmed, so a hand-edited " private" still matches.
    let tags: Vec<String> = settings["hiddenThreadTags"]
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|tag| !tag.is_empty())
        .map(str::to_owned)
        .collect();
    (!tags.is_empty()).then_some(tags)
}

/// `visibilityParentKey`: the thread a subagent, side chat or delegated task takes its visibility from.
fn visibility_parent<'a>(
    environment_id: &'a str,
    lineage: &'a Lineage,
    delegated_from: Option<&'a DelegatedFrom>,
) -> Option<Key<'a>> {
    let listed = lineage.relationship_to_parent.as_deref() != Some("subagent") && lineage.side_chat != Some(true);
    match (&lineage.parent_thread_id, delegated_from) {
        (Some(parent), _) if !listed => Some((environment_id, parent.as_str())),
        (_, Some(from)) => Some((from.environment_id.as_str(), from.thread_id.as_str())),
        _ => None,
    }
}

/// A thread as the tag filter judges it.
struct Filtered<'a> {
    key: Key<'a>,
    parent: Option<Key<'a>>,
    tags: &'a [String],
}

/// `hiddenThreadKeys`. Children follow their parent; while any environment hides tags, one whose
/// parent isn't loaded stays hidden. For a partial list such as an archive page, `already_hidden`
/// and `loaded_elsewhere` are the live threads' hidden and loaded keys.
fn hidden_keys<'a>(
    threads: &[Filtered<'a>],
    hidden_tags: &HashMap<&str, Vec<String>>,
    already_hidden: &HashSet<Key<'_>>,
    loaded_elsewhere: &HashSet<Key<'_>>,
    hiding_anywhere: bool,
) -> HashSet<Key<'a>> {
    if already_hidden.is_empty() && !hiding_anywhere {
        return HashSet::new();
    }
    let judge = Judge {
        by_key: threads.iter().map(|thread| (thread.key, thread)).collect(),
        hidden_tags,
        already_hidden,
        loaded_elsewhere,
        hiding_anywhere,
    };
    let mut known = HashMap::new();
    threads.iter().map(|thread| thread.key).filter(|key| judge.is_hidden(*key, &mut known)).collect()
}

struct Judge<'a, 's> {
    by_key: HashMap<Key<'a>, &'s Filtered<'a>>,
    hidden_tags: &'s HashMap<&'s str, Vec<String>>,
    already_hidden: &'s HashSet<Key<'s>>,
    loaded_elsewhere: &'s HashSet<Key<'s>>,
    hiding_anywhere: bool,
}

impl<'a> Judge<'a, '_> {
    fn is_hidden(&self, key: Key<'a>, known: &mut HashMap<Key<'a>, bool>) -> bool {
        if self.already_hidden.contains(&key) {
            return true;
        }
        if let Some(hidden) = known.get(&key) {
            return *hidden;
        }
        let Some(thread) = self.by_key.get(&key) else { return false };
        let hidden = match thread.parent {
            Some(parent) if self.by_key.contains_key(&parent) || self.already_hidden.contains(&parent) => {
                // Seeded first, so a malformed cycle ends visible.
                known.insert(key, false);
                self.is_hidden(parent, known)
            }
            Some(parent) if self.loaded_elsewhere.contains(&parent) => false,
            Some(_) if self.hiding_anywhere => true,
            _ => self.hidden_tags.get(key.0).is_some_and(|tags| tags.iter().any(|tag| thread.tags.contains(tag))),
        };
        known.insert(key, hidden);
        hidden
    }
}

/// One project section: a checkout, or a logical project once threads come from several environments.
struct Group<'a> {
    key: String,
    project: &'a ProjectShell,
    title: String,
    threads: Vec<(&'a str, &'a ArchivedThread)>,
}

/// The panel's grouping: the scope's projects that have visible archived threads, newest archive
/// first. Whether those threads come from more than one environment rides along, since rows then
/// say where each one lives.
fn group_pages<'a>(
    pages: &'a [Rc<Page>],
    members: Option<&[scope::Project]>,
    logical: &[ProjectGroup],
    hidden: &HashSet<Key<'_>>,
) -> (Vec<Group<'a>>, bool) {
    let mut projects: Vec<&ProjectShell> = Vec::new();
    let mut seen: HashSet<Key> = HashSet::new();
    for page in pages {
        let env = page.environment_id.as_str();
        for project in &page.projects {
            let selected = members.is_none_or(|m| m.iter().any(|m| m.environment_id == env && m.id == project.id));
            if selected && seen.insert((env, project.id.as_str())) {
                projects.push(project);
            }
        }
    }
    let mut by_project: HashMap<Key, Vec<(&str, &ArchivedThread)>> = HashMap::new();
    for page in pages {
        let env = page.environment_id.as_str();
        for thread in page.threads.iter().filter(|t| !hidden.contains(&(env, t.id.as_str()))) {
            if seen.contains(&(env, thread.project_id.as_str())) {
                by_project.entry((env, thread.project_id.as_str())).or_default().push((env, thread));
            }
        }
    }
    let multiple = by_project.keys().map(|(env, _)| *env).collect::<HashSet<_>>().len() > 1;
    let mut groups: Vec<Group> = Vec::new();
    for project in projects {
        let env = project.environment_id.as_ref();
        let Some(threads) = by_project.remove(&(env, project.id.as_str())) else { continue };
        let on_env = |group: &&ProjectGroup| group.members.iter().any(|m| m.project.environment_id.as_ref() == env);
        let logical =
            if multiple { logical.iter().find(|group| on_env(group) && group.contains(&project.id)) } else { None };
        let key = logical.map_or_else(|| format!("{env}:{}", project.id), |group| group.key.clone());
        match groups.iter_mut().find(|group| group.key == key) {
            Some(group) => group.threads.extend(threads),
            None => groups.push(Group {
                key,
                project,
                title: logical.map_or_else(|| project.title.clone(), |group| group.display_name.clone()),
                threads,
            }),
        }
    }
    for group in &mut groups {
        group.threads.sort_by(|(_, a), (_, b)| b.archived().cmp(a.archived()).then_with(|| b.id.cmp(&a.id)));
    }
    (groups, multiple)
}

/// `settingsScopeEnvironmentLabels` as the archive joins them: `label (secondary)` when another
/// environment shares the label. Each entry is `(id, label, address url)`.
fn environment_label(id: &str, environments: &[(&str, &str, Option<&str>)]) -> String {
    let Some(&(_, label, url)) = environments.iter().find(|e| e.0 == id) else { return id.to_owned() };
    let address = |id: &str, url: Option<&str>| match url {
        Some(url) => url.split_once("://").map_or(url, |(_, rest)| rest.split('/').next().unwrap_or(rest)).to_owned(),
        None => id.to_owned(),
    };
    let mine = address(id, url);
    let duplicates: Vec<_> = environments.iter().filter(|e| e.0 != id && e.1 == label).collect();
    if duplicates.is_empty() {
        return label.to_owned();
    }
    let secondary =
        if duplicates.iter().any(|e| address(e.0, e.2) == mine) { url.unwrap_or(id).to_owned() } else { mine };
    format!("{label} ({secondary})")
}

/// What Delete does (`confirmAndDeleteThread`): refuse without `orchestration:operate`, else ask
/// first unless `confirmThreadDelete` is off.
#[derive(Debug, PartialEq, Eq)]
enum DeleteStep {
    Refuse,
    Confirm,
    Delete,
}

fn delete_step(can_operate: bool, confirm: bool) -> DeleteStep {
    match (can_operate, confirm) {
        (false, _) => DeleteStep::Refuse,
        (true, true) => DeleteStep::Confirm,
        (true, false) => DeleteStep::Delete,
    }
}

impl Workspace {
    pub(super) fn render_archive(
        &mut self,
        view: &SettingsView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = *theme::tokens(cx);
        let mut environment_ids: Vec<&str> = view.scope.environment_ids.iter().map(String::as_str).collect();
        environment_ids.sort_unstable();
        environment_ids.dedup();
        let clients: Vec<(String, Option<RpcClient>)> =
            environment_ids.iter().map(|id| ((*id).to_owned(), self.env_client(id))).collect();
        // A connection coming or going starts a fresh load.
        let key: Vec<String> =
            clients.iter().map(|(id, client)| format!("{id}{}", if client.is_some() { "" } else { "!" })).collect();
        let key = SharedString::from(format!("settings-archive:{}", key.join("\u{1f}")));
        let state = window.use_keyed_state(key, cx, |_, cx| ArchiveState::new(clients, cx));
        let (pages, loading, failed) = {
            let state = state.read(cx);
            let loading = state.pages.iter().any(|(_, page)| matches!(page, PageState::Loading));
            let failed = state.pages.iter().any(|(_, page)| matches!(page, PageState::Failed));
            (state.loaded(), loading, failed)
        };
        let hidden = self.hidden_archived(&pages);
        let members = match &view.scope.kind {
            ScopeKind::Project { members } => Some(members.as_slice()),
            _ => None,
        };
        let (groups, multiple) = group_pages(&pages, members, &view.groups, &hidden);
        if groups.is_empty() {
            let (icon, title, description) = if loading {
                (
                    Spinner::new().small().color(t.muted_foreground).into_any_element(),
                    "Loading archived threads",
                    "Checking connected environments.",
                )
            } else {
                let (title, description) = if failed {
                    ("Could not load archived threads", "Failed to load archived threads.")
                } else {
                    ("No archived threads", "Archived threads will appear here.")
                };
                (
                    Icon::new(IconName::Archive).size(px(14.)).text_color(t.muted_foreground).into_any_element(),
                    title,
                    description,
                )
            };
            let title = h_flex().gap_2().child(icon).child(title);
            let row = RowFrame::new(title).description(text(description)).render(&t).into_any_element();
            return self.settings_page(
                "archived",
                view,
                [settings_section(if loading { "archived" } else { "archive" }, "Archived threads", [row], &t)],
                cx,
            );
        }
        let labels: HashMap<&str, String> = if multiple {
            let environments: Vec<(&str, &str, Option<&str>)> = self
                .sessions
                .iter()
                .filter_map(|s| {
                    let id = s.id.as_deref()?;
                    let label = s.server_info.environment_label.as_deref().unwrap_or(id);
                    Some((id, label, s.endpoint.as_ref().map(|e| e.http_base.as_str())))
                })
                .collect();
            environment_ids.iter().map(|id| (*id, environment_label(id, &environments))).collect()
        } else {
            HashMap::new()
        };
        let now = now_ms();
        let sections: Vec<AnyElement> = groups
            .iter()
            .enumerate()
            .map(|(index, group)| {
                let rows: Vec<AnyElement> = group
                    .threads
                    .iter()
                    .map(|(env, thread)| {
                        let label = labels.get(env).map(String::as_str);
                        self.archived_row(env, label, thread, &state, now, &t, cx)
                    })
                    .collect();
                let icon = sidebar::project_icon(group.project, &t, false, 16., self.favicon(group.project));
                let title = h_flex().gap_2().child(icon).child(group.title.clone());
                // The first group is the search's "Archived threads" target.
                let id =
                    if index == 0 { "archive".into() } else { SharedString::from(format!("archived-{}", group.key)) };
                settings_section(id, title, rows, &t)
            })
            .collect();
        self.settings_page("archived", view, sections, cx)
    }

    /// The archived threads the tag filter hides (`createArchivedThreadSnapshotsAtomFamily`): the
    /// live threads' rules apply, so archived children of hidden parents stay hidden.
    fn hidden_archived<'a>(&self, pages: &'a [Rc<Page>]) -> HashSet<Key<'a>> {
        let tags: HashMap<&str, Vec<String>> = self
            .sessions
            .iter()
            .filter_map(|s| Some((s.id.as_deref()?, hidden_tags(&s.server_info.settings)?)))
            .collect();
        if tags.is_empty() {
            return HashSet::new();
        }
        let live: Vec<Filtered> = self
            .shell
            .threads
            .iter()
            .map(|t| {
                let env = t.environment_id.as_ref();
                Filtered {
                    key: (env, t.id.as_str()),
                    parent: visibility_parent(env, &t.lineage, t.delegated_from.as_ref()),
                    tags: &t.tags,
                }
            })
            .collect();
        let live_hidden = hidden_keys(&live, &tags, &HashSet::new(), &HashSet::new(), true);
        let loaded: HashSet<Key> = live.iter().map(|t| t.key).collect();
        let archived: Vec<Filtered<'a>> = pages
            .iter()
            .flat_map(|page| {
                let env = page.environment_id.as_str();
                page.threads.iter().map(move |t| Filtered {
                    key: (env, t.id.as_str()),
                    parent: visibility_parent(env, &t.lineage, t.delegated_from.as_ref()),
                    tags: &t.tags,
                })
            })
            .collect();
        hidden_keys(&archived, &tags, &live_hidden, &loaded, true)
    }

    #[expect(clippy::too_many_arguments, reason = "one row's pieces, each from a different place")]
    fn archived_row(
        &self,
        environment_id: &str,
        environment_label: Option<&str>,
        thread: &ArchivedThread,
        state: &Entity<ArchiveState>,
        now: i64,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let place = environment_label.map(|label| format!("{label} · ")).unwrap_or_default();
        let description = format!(
            "{place}Archived {} · Created {}",
            relative_label(thread.archived(), now),
            relative_label(&thread.created_at, now)
        );
        let target = Target { environment_id: environment_id.to_owned().into(), thread_id: thread.id.clone().into() };
        let row_id = format!("{environment_id}-{}", thread.id);
        let title = SharedString::from(thread.title.clone());
        let pending = state.read(cx).pending.contains(&target);
        let view = cx.entity();
        let action = |action: Action, icon: Icon| {
            let (view, state, target, title) = (view.clone(), state.clone(), target.clone(), title.clone());
            let label = action.label();
            let button = Button::new(SharedString::from(format!("{}-{row_id}", action.id())))
                .outline()
                .size(px(28.))
                .icon(icon.size(px(14.)))
                .tooltip(label)
                .accessibility_label(format!("{label} {title}"))
                .disabled(pending)
                .on_click(move |_, window, cx| act(action, &view, &state, &target, &title, window, cx));
            if action == Action::Delete { button.danger() } else { button }
        };
        let controls = h_flex()
            .flex_none()
            .gap_2()
            .child(action(Action::Unarchive, Icon::new(IconName::ArchiveX)))
            .child(action(Action::Delete, Icon::empty().path("icons/trash-2.svg")));
        let (state, error) = (state.clone(), t.error);
        div()
            .id(SharedString::from(format!("archived-{row_id}")))
            .child(RowFrame::new(title.clone()).description(text(description)).control(controls).render(t))
            .context_menu(move |menu, _, cx| {
                // No menu while the row's action runs, so it can't be sent twice.
                if state.read(cx).pending.contains(&target) {
                    return menu;
                }
                let item = |action: Action| {
                    let (view, state, target, title) = (view.clone(), state.clone(), target.clone(), title.clone());
                    move |_: &gpui_kit::ClickEvent, window: &mut Window, cx: &mut App| {
                        act(action, &view, &state, &target, &title, window, cx);
                    }
                };
                let delete = PopupMenuItem::element(move |_, _| div().text_color(error).child(Action::Delete.label()));
                menu.item(PopupMenuItem::new(Action::Unarchive.label()).on_click(item(Action::Unarchive)))
                    .item(delete.on_click(item(Action::Delete)))
            })
            .into_any_element()
    }
}

/// The archived thread a row acts on.
#[derive(Clone, PartialEq, Eq, Hash)]
struct Target {
    environment_id: SharedString,
    thread_id: SharedString,
}

/// `ARCHIVE_ACTIONS`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    Unarchive,
    Delete,
}

impl Action {
    fn id(self) -> &'static str {
        match self {
            Self::Unarchive => "unarchive",
            Self::Delete => "delete",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Unarchive => "Unarchive",
            Self::Delete => "Delete",
        }
    }
}

/// `handleThreadAction`: one action per row at a time, held through Delete's confirmation.
fn act(
    action: Action,
    view: &Entity<Workspace>,
    state: &Entity<ArchiveState>,
    target: &Target,
    title: &SharedString,
    window: &mut Window,
    cx: &mut App,
) {
    let claimed = state.update(cx, |state, cx| {
        cx.notify();
        state.pending.insert(target.clone())
    });
    if !claimed {
        return;
    }
    match action {
        Action::Unarchive => {
            let command = json!({"type": "thread.unarchive", "commandId": new_id(), "threadId": target.thread_id});
            dispatch_and_reload(view, state, target, command, "Failed to unarchive thread", window, cx);
        }
        Action::Delete => delete(view.clone(), state.clone(), target.clone(), title.clone(), window, cx),
    }
}

fn release(state: &Entity<ArchiveState>, target: &Target, cx: &mut App) {
    state.update(cx, |state, cx| {
        state.pending.remove(target);
        cx.notify();
    });
}

/// `confirmAndDeleteThread`.
fn delete(
    view: Entity<Workspace>,
    state: Entity<ArchiveState>,
    target: Target,
    title: SharedString,
    window: &mut Window,
    cx: &mut App,
) {
    let workspace = view.read(cx);
    let step = delete_step(
        workspace.environment_grants(&target.environment_id, ORCHESTRATION_OPERATE),
        workspace.client_settings.values.get("confirmThreadDelete").as_bool() != Some(false),
    );
    let command = json!({"type": "thread.delete", "commandId": new_id(), "threadId": target.thread_id});
    match step {
        DeleteStep::Refuse => {
            release(&state, &target, cx);
            notify_error(window, cx, "Failed to delete thread", "This connection cannot change threads.");
        }
        DeleteStep::Delete => {
            dispatch_and_reload(&view, &state, &target, command, "Failed to delete thread", window, cx);
        }
        DeleteStep::Confirm => window.open_alert_dialog(cx, move |dialog, _, _| {
            let (view, state, target, command) = (view.clone(), state.clone(), target.clone(), command.clone());
            let (cancel_state, cancel_target) = (state.clone(), target.clone());
            dialog
                .title(format!("Delete thread \"{title}\"?"))
                .description("This permanently clears conversation history for this thread.")
                .ok_text("Delete")
                .ok_variant(ButtonVariant::Danger)
                .show_cancel(true)
                .on_ok(move |_, window, cx| {
                    dispatch_and_reload(&view, &state, &target, command.clone(), "Failed to delete thread", window, cx);
                    true
                })
                // Cancel, Escape and the backdrop all land here.
                .on_cancel(move |_, _, cx| {
                    release(&cancel_state, &cancel_target, cx);
                    true
                })
        }),
    }
}

/// Runs a row's command, then frees the row and, once it lands, reloads that environment's page.
fn dispatch_and_reload(
    view: &Entity<Workspace>,
    state: &Entity<ArchiveState>,
    target: &Target,
    command: Value,
    failure: &'static str,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(client) = view.read(cx).env_client(&target.environment_id) else {
        release(state, target, cx);
        return;
    };
    let (state, target) = (state.clone(), target.clone());
    window
        .spawn(cx, async move |cx| {
            let result = client.request("orchestration.dispatchCommand", command).await;
            let _ = cx.update(|window, cx| match result {
                Ok(_) => {
                    release(&state, &target, cx);
                    state.update(cx, |state, cx| state.load(target.environment_id.to_string(), client, cx));
                }
                // The web shows the error's message, else a generic line; never the tagged form.
                Err(error) => {
                    release(&state, &target, cx);
                    match error {
                        crate::rpc::RpcError::Disconnected(_) => {}
                        crate::rpc::RpcError::Failure(error) => {
                            let message = error["message"].as_str().unwrap_or("An error occurred.").to_owned();
                            notify_error(window, cx, failure, message);
                        }
                        crate::rpc::RpcError::Defect(_) => notify_error(window, cx, failure, "An error occurred."),
                    }
                }
            });
        })
        .detach();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project_groups::Member;

    fn project(id: &str) -> Value {
        json!({"id": id, "title": id.to_uppercase(), "workspaceRoot": "/r"})
    }

    fn thread(id: &str, project: &str, archived: Option<&str>) -> Value {
        json!({"id": id, "projectId": project, "title": id, "createdAt": "2026-01-01T00:00:00Z", "archivedAt": archived})
    }

    fn page(env: &str, projects: &[Value], threads: &[Value]) -> Rc<Page> {
        Rc::new(Page::decode(env, &json!({"projects": projects, "threads": threads})))
    }

    fn shape<'a>(groups: &'a [Group]) -> Vec<(&'a str, Vec<(&'a str, &'a str)>)> {
        groups
            .iter()
            .map(|g| (g.title.as_str(), g.threads.iter().map(|(env, t)| (*env, t.id.as_str())).collect()))
            .collect()
    }

    #[test]
    fn groups_the_scopes_projects_and_merges_logical_projects_across_environments() {
        let pages = [
            page(
                "a",
                &[project("web"), project("empty"), project("api")],
                &[
                    thread("old", "web", Some("2026-02-01T00:00:00Z")),
                    thread("new", "web", Some("2026-03-01T00:00:00Z")),
                    thread("never", "web", None),
                    thread("a1", "api", Some("2026-01-05T00:00:00Z")),
                ],
            ),
            page("b", &[project("web-b")], &[thread("b1", "web-b", Some("2026-02-15T00:00:00Z"))]),
        ];
        let checkout = |env: &str, id: &str| {
            let mut project: ProjectShell = serde_json::from_value(project(id)).unwrap();
            project.environment_id = env.to_owned().into();
            Member { project, physical_key: format!("{env}:{id}"), is_scratch: false }
        };
        let logical = [ProjectGroup {
            key: "repo:web".into(),
            display_name: "Web".into(),
            members: vec![checkout("a", "web"), checkout("b", "web-b")],
            project_ids: vec!["web".into(), "web-b".into()],
            pinned: false,
            archived: false,
        }];
        let none = HashSet::new();

        let (groups, multiple) = group_pages(&pages, None, &logical, &none);
        assert!(multiple);
        assert_eq!(
            shape(&groups),
            [("Web", vec![("a", "new"), ("b", "b1"), ("a", "old"), ("a", "never")]), ("API", vec![("a", "a1")])],
            "the two checkouts share a section, newest archive first; projects without threads drop out"
        );

        // A project scope keeps only its members' checkouts, so one environment shows its own titles.
        let member = |env: &str, id: &str| scope::Project {
            id: id.into(),
            environment_id: env.into(),
            name: String::new(),
            workspace_root: "/r".into(),
            legacy_model_selection: None,
            legacy_thread_env_mode: None,
        };
        let members = [member("a", "web")];
        let (groups, multiple) = group_pages(&pages, Some(&members), &logical, &none);
        assert!(!multiple);
        assert_eq!(shape(&groups), [("WEB", vec![("a", "new"), ("a", "old"), ("a", "never")])]);

        let hidden = HashSet::from([("b", "b1")]);
        let (groups, multiple) = group_pages(&pages, None, &logical, &hidden);
        assert!(!multiple, "hidden threads don't count toward environments");
        assert_eq!(shape(&groups)[0].0, "WEB");
    }

    #[test]
    fn archived_threads_follow_the_tag_filter_and_their_parents() {
        let threads: Vec<ArchivedThread> = [
            json!({"tags": ["private"]}),
            json!({"tags": ["work"]}),
            json!({"lineage": {"parentThreadId": "private", "relationshipToParent": "subagent"}}),
            json!({"lineage": {"parentThreadId": "live-private", "relationshipToParent": "subagent"}}),
            json!({"lineage": {"parentThreadId": "live-plain", "relationshipToParent": "subagent"}}),
            json!({"lineage": {"parentThreadId": "missing", "relationshipToParent": "subagent"}}),
            json!({"lineage": {"parentThreadId": "private", "relationshipToParent": "fork"}}),
            json!({"delegatedFrom": {"environmentId": "a", "threadId": "private", "title": "p"}}),
        ]
        .into_iter()
        .zip(["private", "plain", "child", "live-child", "visible-child", "orphan", "fork", "delegated"])
        .map(|(mut fields, id)| {
            fields.as_object_mut().unwrap().extend(thread(id, "p", None).as_object().unwrap().clone());
            serde_json::from_value(fields).unwrap()
        })
        .collect();
        let filtered: Vec<Filtered> = threads
            .iter()
            .map(|t| Filtered {
                key: ("a", t.id.as_str()),
                parent: visibility_parent("a", &t.lineage, t.delegated_from.as_ref()),
                tags: &t.tags,
            })
            .collect();
        let tags = HashMap::from([("a", vec!["private".to_owned()])]);
        let live_hidden = HashSet::from([("a", "live-private")]);
        let loaded = HashSet::from([("a", "live-private"), ("a", "live-plain")]);
        let mut hidden: Vec<_> = hidden_keys(&filtered, &tags, &live_hidden, &loaded, true).into_iter().collect();
        hidden.sort_unstable();
        assert_eq!(
            hidden,
            [("a", "child"), ("a", "delegated"), ("a", "live-child"), ("a", "orphan"), ("a", "private")],
            "a listed fork and a child of a visible live thread stay visible"
        );
        assert!(hidden_keys(&filtered, &HashMap::new(), &HashSet::new(), &HashSet::new(), false).is_empty());

        assert_eq!(hidden_tags(&json!({"hiddenThreadTags": ["private"]})), None);
        assert_eq!(
            hidden_tags(&json!({"hideTaggedThreads": true, "hiddenThreadTags": [" private ", "", 3]})),
            Some(vec!["private".to_owned()])
        );
        assert_eq!(hidden_tags(&json!({"hideTaggedThreads": true, "hiddenThreadTags": [" "]})), None);
    }

    #[test]
    fn environment_labels_name_the_address_only_for_shared_labels() {
        let environments = [
            ("a", "Laptop", Some("http://10.0.0.2:3773")),
            ("b", "Laptop", Some("http://10.0.0.3:3773/")),
            ("c", "Desk", None),
        ];
        assert_eq!(environment_label("a", &environments), "Laptop (10.0.0.2:3773)");
        assert_eq!(environment_label("c", &environments), "Desk");
        let same = [("a", "Laptop", None), ("b", "Laptop", None)];
        assert_eq!(environment_label("b", &same), "Laptop (b)");
    }

    #[test]
    fn delete_needs_operate_and_asks_unless_confirmation_is_off() {
        assert_eq!(delete_step(false, true), DeleteStep::Refuse);
        assert_eq!(delete_step(false, false), DeleteStep::Refuse);
        assert_eq!(delete_step(true, true), DeleteStep::Confirm);
        assert_eq!(delete_step(true, false), DeleteStep::Delete);
    }
}
