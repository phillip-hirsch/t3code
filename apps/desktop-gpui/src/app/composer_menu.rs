//! The `@` files, machines and models, `$` skills, `/` commands and `#` pull requests menu above
//! the composer
//! (`ComposerCommandMenu.tsx`, `ComposerCommandMenuLayer.tsx`,
//! `useComposerMenuItems.ts`).

use std::collections::HashSet;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::input::InlineToken;
use gpui_kit::component::{Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, Context, ElementId, FontWeight, Hsla, InteractiveElement as _, IntoElement, Keystroke,
    ParentElement as _, ScrollHandle, SharedString, StatefulInteractiveElement as _, Styled as _, Task, Window, div,
    linear_color_stop, linear_gradient, prelude::FluentBuilder as _, px, relative,
};
use serde_json::json;

use super::{Target, Workspace};
use crate::composer_search::{self, MenuAction, MenuItem};
use crate::composer_text::{self, Chip, Trigger, TriggerKind};
use crate::model::Provider;
use crate::pull_request_completions::{self, Reference};
use crate::pull_requests::ListResult;
use crate::rpc::RpcClient;
use crate::theme::Tokens;

/// `useComposerPathSearch` waits this long after the last keystroke.
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(120);
const SEARCH_LIMIT: u32 = 80;
/// The `#` menu's `useDebouncedValue` delay for a host search or a number.
const PULL_REQUEST_DEBOUNCE: Duration = Duration::from_millis(180);
/// `projects.searchEntries` takes at most this many UTF-16 units of query.
const SEARCH_QUERY_MAX: usize = 256;
/// `--chat-composer-drawer-inset` and the 17px the composer covers.
const DRAWER_INSET: f32 = 22.;
const OVERLAP: f32 = 17.;

#[derive(Default)]
pub(super) struct MenuState {
    trigger: Option<Trigger>,
    /// Escape hid the trigger starting here, until the caret leaves it.
    dismissed: Option<(TriggerKind, usize)>,
    items: Rc<[MenuItem]>,
    highlighted: usize,
    scroll: ScrollHandle,
    loading: bool,
    /// What the empty `#` menu says once nothing is loading.
    empty: SharedString,
    /// The last `#` listings and number lookups, by environment and project.
    pull_requests: PullRequestReads,
    /// The listing and query the running `#` read serves.
    pull_request_search: Option<(ListKey, String)>,
    /// The `#` query whose debounce ran out (`useDebouncedValue`).
    settled_query: Option<String>,
    _search: Option<Task<()>>,
    /// Provider/checkout pairs already asked for a workspace skills snapshot.
    snapshots_requested: HashSet<(Option<SharedString>, String, String)>,
}

/// `usePullRequestList` and `pullRequestEnvironment.detail` for the `#` menu, held while the
/// draft is open so typing narrows without asking again. Keys carry the environment and
/// project, since ids repeat across environments.
#[derive(Default)]
struct PullRequestReads {
    /// A listing per host search (`None` for the plain one); `Err` when it could not be read.
    lists: Vec<(ListKey, Result<Rc<ListResult>, ()>)>,
    /// A number's own lookup, `None` when the host has no such pull request.
    exact: Vec<((ListKey, u64), Option<Reference>)>,
}

/// The environment, project and host search a listing answers.
type ListKey = (SharedString, String, Option<String>);

/// What one `#` read brought back.
enum Read {
    List(Result<Rc<ListResult>, ()>),
    Exact(u64, Option<Box<Reference>>),
}

/// Keeps `value` under `key`, dropping the oldest past a few dozen.
fn remember<K: PartialEq, V>(held: &mut Vec<(K, V)>, key: K, value: V) {
    held.retain(|(k, _)| *k != key);
    if held.len() >= 32 {
        held.remove(0);
    }
    held.push((key, value));
}

/// One `#` row: `#123` and the title, after the repository when it is another one.
fn pull_request_item(reference: Reference, repository: &str) -> MenuItem {
    let own = reference.repository.trim().eq_ignore_ascii_case(repository.trim());
    MenuItem {
        id: format!("pull-request:{}:{}:{}", reference.project_id, reference.repository, reference.number),
        label: format!("#{}", reference.number),
        description: if own {
            reference.title.clone()
        } else {
            format!("{} · {}", reference.repository, reference.title)
        },
        action: MenuAction::PullRequest(Box::new(reference)),
    }
}

/// `pullRequestTextQuery` and the number `#123` names (`pullRequestTriggerNumber`).
fn pull_request_query(query: &str) -> (Option<&str>, Option<u64>) {
    if query.chars().all(|c| c.is_ascii_digit()) {
        // `Number.isSafeInteger(n) && n > 0`.
        (None, query.parse().ok().filter(|n| (1..=(1 << 53) - 1).contains(n)))
    } else {
        (Some(query), None)
    }
}

impl Workspace {
    /// The checkout a thread or draft works in: its worktree (a draft's reused one), else the
    /// project root.
    pub(super) fn composer_cwd(&self) -> Option<String> {
        let worktree = match &self.target {
            Some(Target::Thread { .. }) => {
                self.thread.as_ref()?.state.projection.as_ref().and_then(|p| p.thread.worktree_path.clone())
            }
            Some(Target::Draft { .. }) => {
                self.draft_workspace.existing_worktree(self.target.as_ref()).map(str::to_owned)
            }
            None => None,
        };
        worktree.or_else(|| self.target_project().map(|p| p.workspace_root.clone()))
    }

    pub(super) fn composer_menu_open(&self) -> bool {
        self.menu.trigger.is_some()
    }

    /// Re-reads the trigger under the caret; called on every edit and caret move.
    pub(super) fn sync_trigger(&mut self, cx: &mut Context<Self>) {
        let state = self.composer.read(cx);
        let cursor = state.cursor();
        let in_chip = |trigger: &Trigger| {
            state
                .tokens()
                .iter()
                .any(|span| span.range().start < trigger.range.end && trigger.range.start <= span.range().end)
        };
        let trigger = (self.question.is_none())
            .then(|| composer_text::detect(&state.value(), cursor))
            .flatten()
            .filter(|t| !in_chip(t));
        if trigger.is_none() || trigger.as_ref().map(|t| (t.kind, t.range.start)) != self.menu.dismissed {
            self.menu.dismissed = None;
        }
        let trigger = trigger.filter(|t| self.menu.dismissed != Some((t.kind, t.range.start)));
        if trigger == self.menu.trigger {
            return;
        }
        let same_query = trigger.as_ref().map(|t| (t.kind, t.query.trim().to_lowercase()))
            == self.menu.trigger.as_ref().map(|t| (t.kind, t.query.trim().to_lowercase()));
        self.menu.trigger = trigger;
        if !same_query {
            self.menu.highlighted = 0;
            self.refresh_menu_items(cx);
        }
        cx.notify();
    }

    /// Rebuilds the open menu's rows, e.g. after the provider's skills arrive.
    pub(super) fn refresh_menu_items(&mut self, cx: &mut Context<Self>) {
        let Some(trigger) = self.menu.trigger.clone() else {
            self.menu.items = Rc::default();
            self.menu._search = None;
            return;
        };
        let cwd = self.composer_cwd();
        if trigger.kind == TriggerKind::Path {
            let mut leading = self.machine_items(&trigger.query);
            leading.extend(self.model_items(&trigger.query));
            self.search_files(trigger.query.trim().to_owned(), cwd, leading, cx);
            return;
        }
        if trigger.kind == TriggerKind::PullRequest {
            self.search_pull_requests(&trigger.query, cx);
            return;
        }
        self.menu._search = None;
        let Some(provider) = self.composer_provider() else {
            self.menu.items = Rc::default();
            self.menu.loading = false;
            return;
        };
        let has_snapshot = provider.workspace_snapshots.iter().any(|s| Some(&s.cwd) == cwd.as_ref());
        if let (false, Some(cwd)) = (has_snapshot, &cwd) {
            let instance_id = provider.instance_id.clone();
            self.request_workspace_snapshot(&instance_id, cwd, cx);
        }
        self.menu.loading = !has_snapshot && cwd.is_some();
        let Some(provider) = self.composer_provider() else { return };
        let (skills, commands) = composer_search::provider_lists(provider, cwd.as_deref());
        let at_start = trigger.range.start == 0;
        let fresh_session = self.fresh_session_command_available();
        let side_chat = self.side_chat_menu_state();
        // `getProviderSkillsForSlashMenu`: skills join `/` only when the setting allows.
        let slash_skills = if self.client_settings.values.bool("showSkillsInSlashMenu") { skills } else { &[] };
        let items = match trigger.kind {
            TriggerKind::Skill => composer_search::skill_items(skills, &trigger.query),
            _ => {
                composer_search::slash_items(slash_skills, commands, &trigger.query, at_start, side_chat, fresh_session)
            }
        };
        self.menu.highlighted = self.menu.highlighted.min(items.len().saturating_sub(1));
        self.menu.items = Rc::from(items);
    }

    /// `server.refreshProviders` for one checkout, so `$` and `/` list its skills.
    fn request_workspace_snapshot(&mut self, instance_id: &str, cwd: &str, cx: &mut Context<Self>) {
        let key = (self.active_environment().cloned(), instance_id.to_owned(), cwd.to_owned());
        let Some(client) = self.client().filter(|_| self.menu.snapshots_requested.insert(key)) else { return };
        let input = json!({"instanceId": instance_id, "cwd": cwd, "workspaceSnapshotsOnDemand": true});
        cx.spawn(async move |this, cx| {
            let result = client.request("server.refreshProviders", input).await;
            let _ = this.update(cx, |this, cx| {
                if let Err(error) = result {
                    log::warn!("could not load workspace skills: {error}");
                }
                // The new snapshot arrives on the config stream; without one, stop loading.
                this.refresh_menu_items(cx);
                this.menu.loading = false;
                cx.notify();
            });
        })
        .detach();
    }

    /// `@` lists other machines first while this one offers links (`offersLinkedEnvironments`).
    fn machine_items(&self, query: &str) -> Vec<MenuItem> {
        let info = self.server_info();
        let Some(here) = self.active_environment() else { return Vec::new() };
        if !info.capability("peerLinks") || !info.settings.bool("linkedEnvironmentsEnabled") {
            return Vec::new();
        }
        let machines: Vec<(&str, &str, &str)> = (self.sessions().iter())
            .filter_map(|session| {
                let id = session.id.as_deref()?;
                let label = session.server_info.environment_label.as_deref().unwrap_or(id);
                Some((id, label, session.server_info.machine_kind()))
            })
            .collect();
        composer_search::environment_items(machines, here, query)
    }

    /// `@` names the models of this environment's ready providers that the picker would list,
    /// leaving out legacy and unavailable ones so they don't fill the cap.
    fn model_items(&self, query: &str) -> Vec<MenuItem> {
        let info = self.server_info();
        let ready =
            |p: &&Provider| p.enabled && p.availability.as_deref() != Some("unavailable") && p.status == "ready";
        let candidates: Vec<_> = (info.providers.iter().filter(ready))
            .flat_map(|p| p.models.iter().map(move |m| (p, m)))
            .filter(|(p, m)| !m.is_legacy && !m.is_unavailable && !Self::model_hidden(info, p, m))
            .collect();
        composer_search::model_items(&candidates, query)
    }

    /// Fetches files after the `leading` rows (machines, then models) the query already matched.
    fn search_files(&mut self, query: String, cwd: Option<String>, leading: Vec<MenuItem>, cx: &mut Context<Self>) {
        let (Some(cwd), Some(client), false) = (cwd, self.client(), query.is_empty()) else {
            self.menu._search = None;
            self.menu.loading = false;
            self.menu.items = Rc::from(leading);
            return;
        };
        self.menu.loading = true;
        self.menu._search = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SEARCH_DEBOUNCE).await;
            let input = json!({"cwd": cwd, "query": bounded_query(&query), "limit": SEARCH_LIMIT});
            let result = client.request("projects.searchEntries", input).await;
            let _ = this.update(cx, |this, cx| {
                // A newer query replaced this one.
                if this.menu.trigger.as_ref().map(|t| t.query.trim()) != Some(query.as_str()) {
                    return;
                }
                this.menu.loading = false;
                let files = match result {
                    Ok(value) => composer_search::file_items(&value),
                    Err(error) => {
                        log::warn!("file search failed: {error}");
                        Vec::new()
                    }
                };
                this.menu.items = leading.into_iter().chain(files).collect();
                this.menu.highlighted = 0;
                cx.notify();
            });
        }));
    }

    /// The environment, project, repository and connection the `#` menu reads pull requests
    /// from (`pullRequestProjectId`, `pullRequestRepository`); `None` when it can't.
    fn pull_request_scope(&self) -> Option<(SharedString, String, String, RpcClient)> {
        let project = self.target_project()?;
        let repository =
            project.repository_identity.as_ref().and_then(pull_request_completions::repository_selector)?;
        Some((self.active_environment()?.clone(), project.id.clone(), repository, self.client()?))
    }

    /// The `#` menu (`useComposerMenuItems`): what is known shows at once; the listing, then a
    /// number's own lookup when the listing lacks it, follow 180ms after the last keystroke.
    fn search_pull_requests(&mut self, query: &str, cx: &mut Context<Self>) {
        let Some((environment, project_id, repository, client)) = self.pull_request_scope() else {
            self.menu._search = None;
            self.menu.pull_request_search = None;
            self.menu.loading = false;
            self.menu.items = Rc::default();
            self.menu.empty = "Pull requests are not available for this project.".into();
            return;
        };
        let (text, number) = pull_request_query(query);
        let key: ListKey = (environment, project_id.clone(), text.map(str::to_owned));
        let reads = &self.menu.pull_requests;
        let list = reads.lists.iter().find(|(held, _)| *held == key).map(|(_, list)| list.clone());
        let exact = number.and_then(|n| reads.exact.iter().find(|(held, _)| held.0 == key && held.1 == n));
        let exact = exact.map(|(_, found)| found.clone());
        let listed = list.as_ref().and_then(|list| list.as_ref().ok()).map(Rc::as_ref);
        let in_listing = |n: u64| {
            listed.is_some_and(|list| {
                list.entries.iter().any(|e| {
                    e.number == n
                        && e.project_id == project_id
                        && e.repository.trim().eq_ignore_ascii_case(repository.trim())
                })
            })
        };
        let linked = match &self.target {
            Some(Target::Thread { env, id }) => {
                self.shell.thread(&crate::model::ScopedId::new(env.clone(), id)).map_or_else(Vec::new, |thread| {
                    pull_request_completions::linked_references(&thread.pull_requests, &project_id)
                })
            }
            _ => Vec::new(),
        };
        let found = pull_request_completions::chat_matches(
            query,
            listed,
            exact.as_ref().and_then(Option::as_ref),
            &linked,
            &project_id,
            &repository,
        );
        self.menu.items = found.into_iter().map(|reference| pull_request_item(reference, &repository)).collect();
        self.menu.highlighted = self.menu.highlighted.min(self.menu.items.len().saturating_sub(1));
        let unreadable = list.as_ref().is_some_and(|list| {
            list.as_ref().map_or(true, |list| list.errors.iter().any(|error| error.project_id == project_id))
        });
        self.menu.empty = if unreadable {
            "Pull requests could not be read for this project.".into()
        } else if query.is_empty() {
            "No pull requests found in this repository.".into()
        } else {
            format!("No pull request matches {query}.").into()
        };
        let lookup = number.filter(|n| list.is_some() && exact.is_none() && !in_listing(*n));
        self.menu.loading = list.is_none() || lookup.is_some();
        if !self.menu.loading {
            self.menu._search = None;
            self.menu.pull_request_search = None;
            return;
        }
        // The read for this query is already on its way.
        let search = (key.clone(), query.to_owned());
        if self.menu._search.is_some() && self.menu.pull_request_search.as_ref() == Some(&search) {
            return;
        }
        self.menu.pull_request_search = Some(search);
        let debounce = (text.is_some() || number.is_some()) && self.menu.settled_query.as_deref() != Some(query);
        let (settled, list_key) = (query.to_owned(), key.clone());
        self.menu._search = Some(cx.spawn(async move |this, cx| {
            if debounce {
                cx.background_executor().timer(PULL_REQUEST_DEBOUNCE).await;
                let settled = settled.clone();
                let _ = this.update(cx, |this, _| this.menu.settled_query = Some(settled));
            }
            let read = match lookup {
                Some(number) => {
                    let input = json!({"projectId": list_key.1, "repository": repository, "number": number});
                    let found = client.request("pullRequests.detail", input).await;
                    let found = found.inspect_err(|error| log::warn!("pull request lookup failed: {error}")).ok();
                    Read::Exact(number, found.as_ref().and_then(Reference::of_detail).map(Box::new))
                }
                None => {
                    let limit = crate::pull_requests::PAGE_SIZE;
                    let mut input = json!({"state": "all", "projectId": list_key.1, "limit": limit});
                    if let Some(text) = &list_key.2 {
                        input["query"] = text.as_str().into();
                    }
                    let listed = client.request("pullRequests.list", input).await;
                    let listed = listed.map_err(|error| log::warn!("pull request listing failed: {error}"));
                    Read::List(listed.and_then(|value| serde_json::from_value(value).map_err(|_| ())).map(Rc::new))
                }
            };
            let _ = this.update(cx, |this, cx| {
                let reads = &mut this.menu.pull_requests;
                match read {
                    Read::List(list) => remember(&mut reads.lists, list_key, list),
                    Read::Exact(number, found) => {
                        remember(&mut reads.exact, (list_key, number), found.map(|found| *found));
                    }
                }
                this.menu.pull_request_search = None;
                // Lands only in the menu still asking: same draft, same `#` query.
                if this.menu.trigger.as_ref().is_some_and(|t| t.kind == TriggerKind::PullRequest && t.query == settled)
                {
                    this.refresh_menu_items(cx);
                    cx.notify();
                }
            });
        }));
    }

    /// Hides the menu for the trigger now under the caret, as Escape does.
    pub(super) fn hide_menu(&mut self, cx: &mut Context<Self>) {
        if let Some(trigger) = self.menu.trigger.take() {
            self.menu.dismissed = Some((trigger.kind, trigger.range.start));
            self.refresh_menu_items(cx);
            cx.notify();
        }
    }

    /// Arrows, Enter/Tab and Escape while the menu is open; `true` when handled.
    pub(super) fn menu_key(&mut self, keystroke: &Keystroke, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.menu.trigger.is_none() {
            return false;
        }
        let modifiers = keystroke.modifiers;
        let count = self.menu.items.len();
        match keystroke.key.as_str() {
            "escape" => {
                self.hide_menu(cx);
                true
            }
            "up" | "down" if count > 0 && !modifiers.control && !modifiers.alt && !modifiers.platform => {
                let step = if keystroke.key == "up" { count - 1 } else { 1 };
                self.menu.highlighted = (self.menu.highlighted + step) % count;
                cx.notify();
                true
            }
            "enter" | "tab" if count > 0 && !modifiers.control && !modifiers.platform => {
                self.pick_menu_item(self.menu.highlighted, window, cx);
                true
            }
            _ => false,
        }
    }

    pub(super) fn pick_menu_item(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(trigger), Some(item)) = (self.menu.trigger.clone(), self.menu.items.get(index).cloned()) else {
            return;
        };
        // `/usage-limits` opens the panel and leaves nothing behind.
        let usage_limits = matches!(&item.action, MenuAction::Command { name } if name == "usage-limits")
            && self.usage_limits_offered();
        let (replacement, chip) = match &item.action {
            _ if usage_limits => (String::new(), None),
            MenuAction::File { path, .. } => (composer_text::file_link(path), Some(Chip::File { path: path.clone() })),
            MenuAction::Skill { name, .. } => (format!("${name}"), Some(Chip::Skill { name: name.clone() })),
            MenuAction::Command { name } => (format!("/{name} "), None),
            // `/side` waits for its prompt; it does nothing while side chats can't start.
            MenuAction::SideChat(_) if self.side_chat_disabled_reason().is_some() => return,
            MenuAction::SideChat(command) => (format!("/{command} "), None),
            MenuAction::Model | MenuAction::Clear => (String::new(), None),
            // `environmentContextRecord`: the record rebuilds from the reference at send.
            MenuAction::Environment { environment_id, .. } => {
                let id = format!("environment_{environment_id}");
                let link = composer_text::context_link("environment", &id, &item.label);
                (link, Some(Chip::Context { kind: "environment".into(), id, label: item.label.clone() }))
            }
            // `buildPullRequestReferenceContext`: the chip's record joins the draft with it.
            MenuAction::PullRequest(reference) => {
                let record = reference.context_record();
                let (kind, id) = ("review-comment", record["contextId"].as_str().unwrap_or_default().to_owned());
                let link = composer_text::context_link(kind, &id, &item.label);
                self.context.records.insert(record);
                (link, Some(Chip::Context { kind: kind.into(), id, label: item.label.clone() }))
            }
            // It names the model for the agent; the thread's own model stays.
            MenuAction::ModelMention { instance_id, model, .. } => (
                composer_text::model_mention(instance_id, model),
                Some(Chip::Model { instance_id: instance_id.clone(), model: model.clone(), label: item.label.clone() }),
            ),
        };
        self.menu.trigger = None;
        self.composer.update(cx, |state, cx| {
            let text = state.value();
            let typed = if chip.is_some() { format!("{replacement} ") } else { replacement.clone() };
            let range = if usage_limits {
                trigger.range.clone()
            } else {
                composer_text::extend_for_trailing_space(&text, trigger.range.clone(), &typed)
            };
            match chip {
                Some(chip) => {
                    let token = InlineToken::new(chip.id(), replacement).with_label(chip.label().to_owned());
                    if state.replace_range_with_token(range, token, window, cx).is_ok() {
                        state.insert(" ", window, cx);
                    }
                }
                None => {
                    state.set_selected_range(range, cx);
                    state.replace(replacement, window, cx);
                }
            }
        });
        match item.action {
            MenuAction::Model => self.open_model_picker(window, cx),
            MenuAction::Clear => self.arm_fresh_session(cx),
            _ if usage_limits => {
                self.open_usage_limits(window, cx);
            }
            _ => {}
        }
        self.sync_composer_empty(cx);
        self.sync_trigger(cx);
        cx.notify();
    }

    /// The menu, drawn above the composer and tucked 17px under its top edge.
    pub(super) fn render_command_menu(&self, t: &Tokens, cx: &mut Context<Self>) -> Option<AnyElement> {
        let trigger = self.menu.trigger.as_ref()?;
        let items = self.menu.items.clone();
        let outline = t.composer_outline();
        let surface = t.background.blend(t.composer_glass().opacity(t.glass));
        let body: AnyElement = if items.is_empty() {
            let text: SharedString = match (self.menu.loading, trigger.kind) {
                (true, TriggerKind::Path) => "Searching workspace files...".into(),
                (true, TriggerKind::PullRequest) => "Finding pull request...".into(),
                (true, _) => "Searching workspace skills...".into(),
                (false, TriggerKind::Path) => "No matching files or folders.".into(),
                (false, TriggerKind::Skill) => "No skills found. Try / to browse provider commands.".into(),
                (false, TriggerKind::SlashCommand) => "No matching command.".into(),
                (false, TriggerKind::PullRequest) => self.menu.empty.clone(),
            };
            div()
                .px(px(20.))
                .pt(px(14.))
                .pb(px(28.))
                .text_xs()
                .text_color(t.muted_foreground)
                .child(text)
                .into_any_element()
        } else {
            let highlighted = self.menu.highlighted;
            let kind = trigger.kind;
            // `ScrollArea scrollFade`: the list fades out at an edge with more rows past it.
            let (offset, max) = (self.menu.scroll.offset().y, self.menu.scroll.max_offset().y);
            let fades = (offset < px(-1.), max > px(1.) && offset > -max + px(1.));
            let list = v_flex()
                .id("composer-menu-list")
                .track_scroll(&self.menu.scroll)
                .max_h(px(288.))
                .overflow_y_scroll()
                .p(px(8.))
                .children(items.iter().enumerate().map(|(index, item)| {
                    menu_row(item, kind, index == highlighted, t)
                        .on_mouse_move(cx.listener(move |this, _, _, cx| {
                            if this.menu.highlighted != index {
                                this.menu.highlighted = index;
                                cx.notify();
                            }
                        }))
                        .on_mouse_down(gpui_kit::MouseButton::Left, |_, window, _| window.prevent_default())
                        .on_click(cx.listener(move |this, _, window, cx| this.pick_menu_item(index, window, cx)))
                        .into_any_element()
                }));
            div()
                .relative()
                .child(list)
                .when(fades.0, |el| el.child(edge_fade(surface, true)))
                .when(fades.1, |el| el.child(edge_fade(surface, false)))
                .into_any_element()
        };
        Some(
            div()
                .absolute()
                .left(px(DRAWER_INSET))
                .right(px(DRAWER_INSET))
                .bottom(relative(1.))
                .mb(px(-OVERLAP))
                .child(
                    v_flex()
                        .id("composer-menu")
                        .occlude()
                        .pb(px(OVERLAP))
                        .rounded_t(px(16.))
                        .border_1()
                        .border_color(outline)
                        .bg(surface)
                        .child(body),
                )
                .into_any_element(),
        )
    }
}

/// `--fade-size: 1.5rem` of the list's edge, faded into the menu's own surface.
fn edge_fade(surface: Hsla, top: bool) -> gpui_kit::Div {
    let (from, to) = if top { (surface, surface.opacity(0.)) } else { (surface.opacity(0.), surface) };
    let fade = div().absolute().left_0().right_0().h(px(24.)).bg(linear_gradient(
        180.,
        linear_color_stop(from, 0.),
        linear_color_stop(to, 1.),
    ));
    if top { fade.top_0() } else { fade.bottom_0() }
}

fn menu_row(item: &MenuItem, kind: TriggerKind, active: bool, t: &Tokens) -> gpui_kit::Stateful<gpui_kit::Div> {
    let icon = match &item.action {
        MenuAction::File { directory: true, .. } => Some(Icon::new(Lucide::Folder).text_color(t.muted_foreground)),
        MenuAction::Environment { machine, .. } => {
            Some(Icon::new(super::host_picker::machine_icon(machine)).text_color(t.muted_foreground))
        }
        MenuAction::PullRequest(reference) => {
            let (glyph, _, light, dark, alpha) =
                super::composer::pull_request_state(&reference.state, reference.is_draft);
            Some(Icon::new(glyph).text_color(super::composer::pull_request_tone(light, dark, alpha, t)))
        }
        // `PierreEntryIcon`: the file tree's per-type glyph.
        MenuAction::File { path, .. } => {
            let (asset, color) = crate::file_icons::file_icon(path, t.dark);
            Some(Icon::empty().path(asset).text_color(color))
        }
        // `ProviderInstanceIcon`: the provider's logo, in its own color where it has one.
        MenuAction::ModelMention { driver, .. } => Some(match crate::assets::provider_icon(driver) {
            Some((asset, tint)) => Icon::empty().path(asset).text_color(tint.unwrap_or(t.foreground)),
            None => Icon::new(Lucide::Bot).text_color(t.muted_foreground),
        }),
        _ => None,
    };
    let badge = match (&item.action, kind) {
        (MenuAction::Skill { scope, path, .. }, TriggerKind::Skill) => Some(skill_badge(scope.as_deref(), path)),
        (MenuAction::Skill { scope, path, .. }, _) => Some((skill_badge(scope.as_deref(), path).0, None)),
        _ => None,
    };
    let label: AnyElement = match item.label.strip_prefix("/skill:") {
        Some(name) => h_flex()
            .child(div().text_color(t.muted_foreground).child("/skill:"))
            .child(name.to_owned())
            .into_any_element(),
        None => div().child(item.label.clone()).into_any_element(),
    };
    h_flex()
        .id(ElementId::Name(item.id.clone().into()))
        .min_h(px(28.))
        .px(px(8.))
        .py(px(6.))
        .gap(px(8.))
        .rounded(px(8.))
        .cursor_pointer()
        .when(active, |el| el.bg(t.accent))
        .children(icon.map(|icon| icon.with_size(px(16.)).flex_none()))
        .child(
            h_flex()
                .flex_1()
                .min_w_0()
                .gap(px(8.))
                .text_xs()
                .child(
                    div()
                        .flex_none()
                        .max_w(relative(0.45))
                        .truncate()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(t.foreground)
                        .child(label),
                )
                .child(
                    div().flex_1().min_w_0().truncate().text_color(t.muted_foreground).child(item.description.clone()),
                ),
        )
        .children(badge.map(|(source, suffix)| {
            let (icon, word) = source;
            h_flex()
                .flex_none()
                .ml_auto()
                .h(px(18.))
                .px(px(3.))
                .gap(px(4.))
                .rounded(px(4.))
                .bg(t.secondary)
                .text_xs()
                .text_color(t.foreground)
                .child(Icon::new(icon).with_size(px(12.)).opacity(0.8))
                .child(format!("{word}{}", suffix.unwrap_or_default()))
        }))
}

/// A long pasted path cut to the request's bound, never inside a surrogate pair.
fn bounded_query(query: &str) -> &str {
    let mut units = 0;
    let end = query
        .char_indices()
        .find(|(_, c)| {
            units += c.len_utf16();
            units > SEARCH_QUERY_MAX
        })
        .map_or(query.len(), |(at, _)| at);
    &query[..end]
}

/// `resolveProviderSkillSourceKind`: where a skill comes from, as its badge shows it.
fn skill_badge(scope: Option<&str>, path: &str) -> ((Lucide, &'static str), Option<&'static str>) {
    let path = path.replace('\\', "/");
    let kind = if path.contains("/.codex/plugins/") || path.contains("/.agents/plugins/") {
        (Lucide::Blocks, "App")
    } else {
        match scope.map(str::to_lowercase).as_deref() {
            Some("repo" | "repository") => (Lucide::Folder, "Repo"),
            Some("project" | "workspace" | "local") => (Lucide::Folder, "Project"),
            Some("user" | "personal") => (Lucide::UserRound, "Personal"),
            Some("system") => (Lucide::Settings, "System"),
            _ => (Lucide::Package, "Provider"),
        }
    };
    (kind, Some(" Skill"))
}

#[cfg(test)]
mod tests {
    use super::bounded_query;

    #[test]
    fn path_queries_stop_at_256_utf16_units_without_splitting_a_pair() {
        let a = |n: usize| "a".repeat(n);
        assert_eq!(bounded_query("src"), "src");
        assert_eq!(bounded_query(&a(256)), a(256));
        let long = [a(86), "b".repeat(86), "c".repeat(86), "unique.ts".into()].join("/");
        assert_eq!(bounded_query(&long), [a(86), "b".repeat(86), "c".repeat(82)].join("/"));
        assert_eq!(bounded_query(&format!("{}😀/boundary.ts", a(255))), a(255));
        assert_eq!(bounded_query(&format!("{}😀/boundary.ts", a(254))), format!("{}😀", a(254)));
    }
}
