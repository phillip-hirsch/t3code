//! The composer's "Run on" picker (`BranchToolbarEnvironmentSelector.tsx`) and the draft routing
//! behind it (`useLogicalProjectEnvironments.ts`, `useDraftEnvironmentRouting.ts`): an unsent
//! draft moves to the same project's checkout on another machine, by hand or by load balancing.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::popover::Popover;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, Selectable as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Context, ElementId, FocusHandle, InteractiveElement as _, IntoElement, KeyDownEvent,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Task, Window, div, px,
};
use serde_json::json;

use super::{Target, Workspace};
use crate::load_balancing::{self, Candidate, HostResources};
use crate::theme::Tokens;

/// One machine the draft's project lives on (`EnvironmentOption`).
#[derive(Clone, Debug, PartialEq)]
pub(super) struct EnvironmentOption {
    pub environment_id: SharedString,
    /// The project's checkout there; `None` for a machine whose "No project" folder is not open yet.
    pub project_id: Option<String>,
    pub label: SharedString,
    pub is_primary: bool,
    pub machine: SharedString,
}

/// How a project's draft picks its machine (`environmentSelection`, `loadBalancedEnvironmentId`).
#[derive(Clone, Debug, PartialEq)]
enum Selection {
    Manual,
    Auto { picked: Option<SharedString> },
}

/// Where load balancing stands for the open draft.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum Sampling {
    #[default]
    Idle,
    Pending,
    Failed,
}

/// The last sample of the machines (`useLoadBalancedEnvironment`): it speaks for one draft and
/// one set of candidates only, so another draft, or a change in who can run it, samples again.
#[derive(Debug, PartialEq)]
struct Sample {
    target: Target,
    candidates: Vec<SharedString>,
    state: Sampling,
}

/// Where sampling stands for `target`'s draft with these `candidates`.
fn sampling_for(sample: Option<&Sample>, target: &Target, candidates: &[SharedString]) -> Sampling {
    sample.filter(|s| s.target == *target && s.candidates == candidates).map_or(Sampling::Idle, |s| s.state)
}

/// Records a sample that picked no machine. One whose draft is no longer open is dropped
/// instead, so coming back to the draft samples again: only the open draft can fail.
fn sample_unused(sample: &mut Option<Sample>, origin: &Target, still_open: bool) {
    if sample.as_ref().is_some_and(|s| s.target == *origin) {
        match sample.as_mut() {
            Some(sample) if still_open => sample.state = Sampling::Failed,
            _ => *sample = None,
        }
    }
}

/// `onEnvironmentChange`'s guard: a scratch project that lands moves the draft only for the newest
/// machine switch, while the draft is still open where that switch started.
fn change_lands(request: u64, latest: u64, origin: Option<&Target>, now: Option<&Target>) -> bool {
    request == latest && origin == now
}

pub(super) struct HostUi {
    open: bool,
    /// The newest machine switch (`environmentChangeRef`); an older one settling late is dropped.
    environment_change: u64,
    /// By draft, as the web keeps them on each draft session: they follow it from machine to
    /// machine, and a new draft starts without them.
    selections: HashMap<Target, Selection>,
    sample: Option<Sample>,
    _sample: Option<Task<()>>,
    /// The open menu takes the keys, as a select's list does.
    menu_focus: FocusHandle,
    highlight: Option<usize>,
    typeahead: Typeahead,
}

impl HostUi {
    pub(super) fn new(cx: &mut App) -> Self {
        Self {
            open: false,
            environment_change: 0,
            selections: HashMap::new(),
            sample: None,
            _sample: None,
            menu_focus: cx.focus_handle(),
            highlight: None,
            typeahead: Typeahead::default(),
        }
    }
}

/// The draft moved from `from` to `to`, and its routing goes with it.
fn carry(selections: &mut HashMap<Target, Selection>, from: &Target, to: &Target) {
    if let Some(selection) = selections.remove(from) {
        selections.insert(to.clone(), selection);
    }
}

/// A row of the open menu.
#[derive(Clone)]
enum HostItem {
    Auto(SharedString),
    Environment(EnvironmentOption),
    /// "Add to another machine", for the draft's project.
    AddToMachine(crate::model::ScopedId),
}

impl HostItem {
    fn label(&self) -> &str {
        match self {
            Self::Auto(label) => label,
            Self::Environment(option) => &option.label,
            Self::AddToMachine(_) => "Add to another machine",
        }
    }
}

/// Typing a row's first letters highlights it (floating-ui's `useTypeahead`, which Base UI's
/// Select uses): letters typed within `RESET` add up; a repeated letter cycles its rows.
#[derive(Default)]
struct Typeahead {
    typed: String,
    at: Option<Instant>,
    /// The highlight when this run of letters began; the search starts after it.
    from: Option<usize>,
}

impl Typeahead {
    const RESET: Duration = Duration::from_millis(750);

    fn type_char(&mut self, c: char, labels: &[&str], highlight: Option<usize>, now: Instant) -> Option<usize> {
        if self.at.is_none_or(|at| now.duration_since(at) > Self::RESET) {
            self.typed.clear();
        }
        if self.typed.is_empty() {
            self.from = highlight;
        }
        self.at = Some(now);
        self.typed.extend(c.to_lowercase());
        let first = self.typed.chars().next()?;
        let cycling = self.typed.chars().all(|ch| ch == first);
        // A repeated letter moves on from the current row; more letters refine from the start of the run.
        let (needle, from) = if cycling { (first.to_string(), highlight) } else { (self.typed.clone(), self.from) };
        let start = from.map_or(0, |i| i + 1);
        (0..labels.len()).map(|k| (start + k) % labels.len()).find(|&i| labels[i].to_lowercase().starts_with(&needle))
    }
}

impl Workspace {
    pub(super) fn option_for(&self, environment_id: &SharedString, project_id: Option<String>) -> EnvironmentOption {
        let info = self.session(environment_id).map(|s| &s.server_info);
        EnvironmentOption {
            environment_id: environment_id.clone(),
            project_id,
            label: info
                .and_then(|i| i.environment_label.clone())
                .map_or_else(|| environment_id.clone(), SharedString::from),
            is_primary: self.primary_environment() == Some(environment_id),
            machine: SharedString::from(info.map_or("server", |i| i.machine_kind()).to_owned()),
        }
    }

    /// `useLogicalProjectEnvironments`: the machines the target's project lives on, the primary
    /// first. A "No project" draft can go to any machine with a "No project" folder.
    pub(super) fn logical_project_environments(&self) -> Vec<EnvironmentOption> {
        let Some(project) = self.target_project() else { return Vec::new() };
        let groups = self.project_groups();
        let Some(group) = groups.iter().find(|g| g.contains_project(project)) else { return Vec::new() };
        let scratch_draft = group.members[0].is_scratch && matches!(self.target, Some(Target::Draft { .. }));
        let mut options: Vec<EnvironmentOption> = Vec::new();
        if scratch_draft {
            for session in self.sessions() {
                let Some(id) = &session.id else { continue };
                let root = session.server_info.scratch_workspace_root.as_deref().filter(|_| session.is_connected());
                if root.is_none() && *id != project.environment_id {
                    continue;
                }
                let existing = if *id == project.environment_id {
                    Some(project.id.clone())
                } else {
                    let root = root.map(crate::project_groups::normalize_path);
                    self.shell
                        .projects
                        .iter()
                        .find(|p| {
                            p.environment_id == *id
                                && Some(crate::project_groups::normalize_path(&p.workspace_root)) == root
                        })
                        .map(|p| p.id.clone())
                };
                options.push(self.option_for(id, existing));
            }
        } else {
            for member in &group.members {
                let checkout = &member.project;
                if options.iter().all(|o| o.environment_id != checkout.environment_id) {
                    options.push(self.option_for(&checkout.environment_id, Some(checkout.id.clone())));
                }
            }
        }
        options.sort_by(|a, b| b.is_primary.cmp(&a.is_primary).then_with(|| a.label.cmp(&b.label)));
        options
    }

    /// `canChangeEnvironment`: only an unsent draft moves, and only between several machines.
    fn can_change_environment(&self, options: &[EnvironmentOption]) -> bool {
        matches!(self.target, Some(Target::Draft { .. })) && options.len() > 1
    }

    /// The draft's project, when it can be added to another machine (`addTargets`).
    fn add_to_machine_project(&self, options: &[EnvironmentOption]) -> Option<crate::model::ScopedId> {
        if !matches!(self.target, Some(Target::Draft { .. })) {
            return None;
        }
        let project = self.target_project()?;
        let existing: Vec<SharedString> = options.iter().map(|option| option.environment_id.clone()).collect();
        (!self.add_project_targets(project, &existing).is_empty()).then(|| crate::model::ScopedId::of_project(project))
    }

    /// The picker opens for a draft with a machine to move to, or a machine to add its project to
    /// (`showEnvironmentPicker`).
    fn can_pick_host(&self, options: &[EnvironmentOption]) -> bool {
        !self.target_sending() && self.has_host_choice(options)
    }

    /// The draft has somewhere to go, whether or not a send holds it in place for now.
    fn has_host_choice(&self, options: &[EnvironmentOption]) -> bool {
        self.can_change_environment(options) || self.add_to_machine_project(options).is_some()
    }

    /// The draft is a "No project" draft (`activeProjectIsScratch`), which never auto-balances.
    pub(super) fn draft_is_scratch(&self) -> bool {
        let Some(project) = self.target_project() else { return false };
        self.project_groups().iter().any(|g| g.contains_project(project) && g.members[0].is_scratch)
    }

    fn selection(&self) -> Option<&Selection> {
        self.host.selections.get(self.target.as_ref()?)
    }

    /// A draft opened afresh on `target` starts on load balancing again.
    pub(super) fn forget_draft_routing(&mut self, target: &Target) {
        self.host.selections.remove(target);
    }

    fn sampling(&self, options: &[EnvironmentOption]) -> Sampling {
        let Some(target) = &self.target else { return Sampling::Idle };
        let candidates: Vec<SharedString> =
            self.load_balancing_candidates(options).into_iter().map(|(id, _, _)| id).collect();
        sampling_for(self.host.sample.as_ref(), target, &candidates)
    }

    /// `automaticEnvironment`: the draft's machine is load balancing's choice.
    fn automatic_environment(&self, options: &[EnvironmentOption]) -> bool {
        let selection = self.selection();
        let picked = matches!(selection, Some(Selection::Auto { picked: Some(_) }));
        let choice = &self.draft_workspace.choice;
        self.client_settings.values.bool("loadBalancingEnabled")
            && self.can_change_environment(options)
            && !self.draft_is_scratch()
            && selection != Some(&Selection::Manual)
            && (self.attachments.is_empty() || picked)
            && (choice.branch.is_none() || selection.is_some())
            && choice.existing_worktree.is_none()
    }

    /// `needsLoadBalancing` and `loadBalancingSendWarning`: an automatic draft without its machine
    /// yet waits to send, with the title and description of why.
    pub(super) fn load_balancing_send_block(&self) -> Option<(&'static str, &'static str)> {
        let options = self.logical_project_environments();
        send_block(self.automatic_environment(&options), self.selection(), self.sampling(&options))
    }

    /// `autoEnvironmentLabel`.
    fn auto_environment_label(&self, options: &[EnvironmentOption]) -> Option<&'static str> {
        if !self.automatic_environment(options) {
            return None;
        }
        let picked = matches!(self.selection(), Some(Selection::Auto { picked: Some(_) }));
        Some(match self.sampling(options) {
            Sampling::Pending if !picked => "Checking machines…",
            Sampling::Failed if !picked => "Auto balance unavailable",
            _ => "Auto balance",
        })
    }

    /// Moves the draft to `option`'s checkout, opening its "No project" folder first if needed
    /// (`onEnvironmentChange`). A pick by hand turns load balancing off for this draft.
    fn change_environment(&mut self, option: EnvironmentOption, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target @ Target::Draft { .. }) = self.target.clone() else { return };
        if self.target_sending() {
            return;
        }
        self.host.open = false;
        self.host.environment_change += 1;
        self.host.selections.insert(target, Selection::Manual);
        self.host._sample = None;
        self.host.sample = None;
        let Some(project_id) = option.project_id else {
            self.open_scratch_then_retarget(option.environment_id, window, cx);
            return;
        };
        self.retarget_draft(option.environment_id, project_id, cx);
    }

    /// The draft moves to `project_id` on `env` as if picked there by hand, so load balancing
    /// leaves it (`environmentSelection: "manual"`).
    pub(super) fn move_draft_by_hand(&mut self, env: SharedString, project_id: String, cx: &mut Context<Self>) {
        let Some(target @ Target::Draft { .. }) = self.target.clone() else { return };
        if self.target_sending() {
            return;
        }
        self.host.open = false;
        self.host.selections.insert(target, Selection::Manual);
        self.host._sample = None;
        self.host.sample = None;
        self.retarget_draft(env, project_id, cx);
    }

    /// The draft keeps its identity and text and moves to another checkout, becoming that
    /// project's draft (`setLogicalProjectDraftThreadId`). A draft being sent stays put: the
    /// launch promotes it where it started.
    pub(super) fn retarget_draft(&mut self, env: SharedString, project_id: String, cx: &mut Context<Self>) {
        let Some(from @ Target::Draft { .. }) = self.target.clone() else { return };
        if self.target_sending() {
            return;
        }
        let Some(draft) = from.draft_id().cloned() else { return };
        self.drafts.assign(crate::model::ScopedId::new(env.clone(), project_id.as_str()), &draft);
        let to = Target::draft(env, project_id, draft);
        carry(&mut self.host.selections, &from, &to);
        self.carry_draft_panel(&from, &to);
        self.retarget_closed_views(&from, &to);
        self.target = Some(to);
        self.sync_draft_workspace(cx);
        self.sync_attachment_uploads(cx);
        self.model_override = None;
        self.model_explicit = false;
        self.drafts_changed(cx);
        self.notify_sidebar(cx);
        cx.notify();
    }

    /// `selectProject` for a draft with content: it moves to the project and keeps an explicit model;
    /// an inherited one resolves again for the destination. The draft keeps its identity; a draft
    /// the project already holds stays as its own sidebar row.
    pub(super) fn move_draft_to_project(&mut self, env: SharedString, project_id: String, cx: &mut Context<Self>) {
        let Some(Target::Draft { env: from_env, project_id: from, .. }) = &self.target else { return };
        if (from_env, from) == (&env, &project_id) || self.target_sending() {
            return;
        }
        let (model, explicit) = (self.model_override.take(), self.model_explicit);
        self.retarget_draft(env, project_id, cx);
        self.model_override = crate::new_thread::moved_draft_model(
            self.server_info(),
            self.target_project(),
            model,
            explicit,
            self.sticky_model.as_ref(),
        );
        self.model_explicit = explicit && self.model_override.is_some();
    }

    fn open_scratch_then_retarget(&mut self, env: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.env_client(&env) else { return };
        let (origin, request) = (self.target.clone(), self.host.environment_change);
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("projects.ensureScratch", json!({})).await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                // A draft that moved on, or a newer switch, keeps the machine it has.
                Ok(value)
                    if change_lands(request, this.host.environment_change, origin.as_ref(), this.target.as_ref()) =>
                {
                    if let Some(project_id) = value["projectId"].as_str() {
                        this.retarget_draft(env, project_id.to_owned(), cx);
                    }
                }
                Ok(_) => {}
                Err(error) => window
                    .push_notification(Notification::error(error.to_string()).title("Could not switch machine"), cx),
            });
        })
        .detach();
    }

    /// `onAutoEnvironment`: back to load balancing, which samples the machines again.
    fn choose_auto_environment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.host.open = false;
        if !matches!(self.target, Some(Target::Draft { .. })) || self.target_sending() {
            return;
        }
        if !self.attachments.is_empty() {
            let message =
                "Remove attachments before choosing automatic routing, then attach them on the selected machine.";
            window.push_notification(Notification::warning(message).title("Keep attachments on this machine"), cx);
            return;
        }
        let Some(target) = self.target.clone() else { return };
        self.host.selections.insert(target, Selection::Auto { picked: None });
        self.draft_workspace.choice.branch = None;
        self.draft_workspace.choice.existing_worktree = None;
        self.host._sample = None;
        self.host.sample = None;
        self.sync_load_balancing(cx);
        cx.notify();
    }

    /// Samples the candidate machines once an automatic draft has no machine yet, then moves
    /// the draft to the pick (`useLoadBalancedEnvironment` and its effect). Cheap when idle.
    pub(super) fn sync_load_balancing(&mut self, cx: &mut Context<Self>) {
        let Some(origin @ Target::Draft { .. }) = self.target.clone() else { return };
        if !self.client_settings.values.bool("loadBalancingEnabled")
            || matches!(self.selection(), Some(Selection::Auto { picked: Some(_) }))
        {
            return;
        }
        let options = self.logical_project_environments();
        if !self.automatic_environment(&options) {
            return;
        }
        let candidates = self.load_balancing_candidates(&options);
        let ids: Vec<SharedString> = candidates.iter().map(|(id, _, _)| id.clone()).collect();
        if sampling_for(self.host.sample.as_ref(), &origin, &ids) != Sampling::Idle {
            return;
        }
        self.host.sample = Some(Sample { target: origin.clone(), candidates: ids, state: Sampling::Pending });
        self.host._sample = Some(cx.spawn(async move |this, cx| {
            let requests = candidates.iter().map(|(id, client, weight)| async move {
                let resources = client.request("server.getHostResources", json!({})).await.ok();
                let received_at = super::sidebar::now_ms();
                Candidate {
                    environment_id: id.to_string(),
                    resources: resources.as_ref().and_then(HostResources::parse),
                    received_at: Some(received_at),
                    weight: *weight,
                }
            });
            let sampled = futures::future::join_all(requests).await;
            let _ = this.update(cx, |this, cx| {
                let picked = load_balancing::choose(&sampled, super::sidebar::now_ms()).map(SharedString::from);
                let target = picked.as_ref().and_then(|id| options.iter().find(|o| o.environment_id == *id));
                let still_open = this.target.as_ref() == Some(&origin);
                match target.and_then(|o| Some((o.environment_id.clone(), o.project_id.clone()?))) {
                    Some((env, project_id)) if still_open => {
                        this.host.sample = None;
                        this.host.selections.insert(origin, Selection::Auto { picked: Some(env.clone()) });
                        this.retarget_draft(env, project_id, cx);
                    }
                    _ => sample_unused(&mut this.host.sample, &origin, still_open),
                }
                cx.notify();
            });
        }));
    }

    /// Connected machines that can run the draft's provider, with their weights
    /// (`loadBalancingCandidates`).
    fn load_balancing_candidates(
        &self,
        options: &[EnvironmentOption],
    ) -> Vec<(SharedString, crate::rpc::RpcClient, f64)> {
        let weights = self.client_settings.values.get("loadBalancingWeights");
        let model = self.composer_model();
        let driver = model.as_ref().and_then(|m| self.server_info().provider(&m.instance_id)).map(|p| p.driver.clone());
        options
            .iter()
            .filter_map(|option| {
                let session = self.session(&option.environment_id)?;
                let client = session.client.clone()?;
                let weight = load_balancing::weight(weights, &option.environment_id);
                let settings = &session.server_info.settings;
                let runs_provider = session.server_info.providers.iter().any(|provider| {
                    model.as_ref().is_none_or(|m| m.instance_id == provider.instance_id)
                        && driver.as_ref().is_none_or(|d| *d == provider.driver)
                        && crate::settings::scope::provider_instance_enabled(
                            settings,
                            option.project_id.as_deref(),
                            &provider.instance_id,
                        )
                        && provider.installed
                        && provider.status != "error"
                        && provider.auth.status != "unauthenticated"
                        && provider.availability.as_deref() != Some("unavailable")
                });
                (weight > 0. && runs_provider).then(|| (option.environment_id.clone(), client, weight))
            })
            .collect()
    }

    /// `composer.host`: opens the picker while there is a choice to make.
    pub(super) fn open_host_picker(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.can_pick_host(&self.logical_project_environments()) {
            return false;
        }
        self.set_host_open(true, cx);
        true
    }

    /// Opens the menu on its selected row, as a select opens on its value, or closes it.
    fn set_host_open(&mut self, open: bool, cx: &mut Context<Self>) {
        self.host.open = open;
        self.host.typeahead = Typeahead::default();
        self.host.highlight = open.then(|| self.host_items().iter().position(|(_, checked)| *checked).unwrap_or(0));
        cx.notify();
    }

    /// The menu's rows and which one is checked: Auto balance where it can be chosen, the
    /// machines, then Add to another machine where the project can go to one.
    fn host_items(&self) -> Vec<(HostItem, bool)> {
        let options = self.logical_project_environments();
        let auto = self.auto_environment_label(&options);
        let active = self.active_environment();
        let balancing = self.client_settings.values.bool("loadBalancingEnabled") && !self.draft_is_scratch();
        let auto_row = balancing.then(|| (HostItem::Auto(auto.unwrap_or("Auto balance").into()), auto.is_some()));
        let add = self.add_to_machine_project(&options).map(|project| (HostItem::AddToMachine(project), false));
        let machines = options.into_iter().map(|option| {
            let checked = auto.is_none() && Some(&option.environment_id) == active;
            (HostItem::Environment(option), checked)
        });
        auto_row.into_iter().chain(machines).chain(add).collect()
    }

    fn pick_host_item(&mut self, item: HostItem, window: &mut Window, cx: &mut Context<Self>) {
        match item {
            HostItem::Auto(_) => self.choose_auto_environment(window, cx),
            HostItem::Environment(option) => self.change_environment(option, window, cx),
            HostItem::AddToMachine(project) => {
                self.host.open = false;
                cx.notify();
                self.open_add_to_machine(project, None, window, cx);
            }
        }
    }

    /// `SelectPopup`'s keys: the arrows, Home and End move the highlight, Enter or Space picks
    /// it, and letters jump to a row. Escape belongs to the popover, which closes.
    fn host_menu_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        if keystroke.modifiers.modified() {
            return;
        }
        let items = self.host_items();
        let Some(last) = items.len().checked_sub(1) else { return };
        let highlight = self.host.highlight;
        let next = match keystroke.key.as_str() {
            "down" => Some(highlight.map_or(0, |i| if i >= last { 0 } else { i + 1 })),
            "up" => Some(highlight.map_or(last, |i| if i == 0 { last } else { i - 1 })),
            "home" => Some(0),
            "end" => Some(last),
            "enter" | "space" => {
                if let Some((item, _)) = highlight.and_then(|i| items.get(i)).cloned() {
                    self.pick_host_item(item, window, cx);
                }
                cx.stop_propagation();
                return;
            }
            _ => {
                let Some(c) = keystroke.key_char.as_deref().and_then(|s| s.chars().next()).filter(|c| !c.is_control())
                else {
                    return;
                };
                let labels: Vec<&str> = items.iter().map(|(item, _)| item.label()).collect();
                self.host.typeahead.type_char(c, &labels, highlight, Instant::now()).or(highlight)
            }
        };
        self.host.highlight = next;
        cx.stop_propagation();
        cx.notify();
    }

    /// `composer.cycleHost`: the draft moves to the project's next machine.
    pub(super) fn cycle_host(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let options = self.logical_project_environments();
        if !self.can_change_environment(&options) || self.target_sending() {
            return false;
        }
        let current = self.active_environment().cloned();
        let index = options.iter().position(|o| Some(&o.environment_id) == current.as_ref());
        let next = index.map_or(0, |i| (i + 1) % options.len());
        self.change_environment(options[next].clone(), window, cx);
        true
    }

    /// The "Run on" control: a locked label with nothing to choose, else the picker. `None`
    /// while the strip shows no machine (`shouldShowEnvironmentIndicator`).
    pub(super) fn render_host_picker(&self, t: &Tokens, cx: &mut Context<Self>) -> Option<AnyElement> {
        let options = self.logical_project_environments();
        let active = self.active_environment();
        let current = options.iter().find(|o| Some(&o.environment_id) == active);
        let can_pick = self.can_pick_host(&options);
        let shown = self.has_host_choice(&options) || current.is_some_and(|o| !o.is_primary) || self.environment_remote;
        if !shown {
            return None;
        }
        let fallback = || {
            self.server_info()
                .environment_label
                .clone()
                .map_or_else(|| SharedString::from("Run on"), SharedString::from)
        };
        let label = current.map_or_else(fallback, |o| o.label.clone());
        let machine = current
            .map_or_else(|| SharedString::from(self.server_info().machine_kind().to_owned()), |o| o.machine.clone());
        if !can_pick {
            let tooltip = label.clone();
            return Some(
                h_flex()
                    .id("environment-chip")
                    .min_w_0()
                    .h(px(24.))
                    .px(px(7.))
                    .gap(px(6.))
                    .text_color(t.muted_foreground.opacity(0.7))
                    .child(Icon::new(machine_icon(&machine)).with_size(px(12.)).flex_none())
                    .child(div().max_w(px(240.)).truncate().child(label))
                    .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                    .into_any_element(),
            );
        }
        let auto = self.auto_environment_label(&options);
        let shown_label: SharedString = auto.map_or(label, SharedString::from);
        let tooltip = match self.shortcut_label("composer.host") {
            Some(keys) => format!("{shown_label} · {keys}"),
            None => shown_label.to_string(),
        };
        let icon = if auto.is_some() { Lucide::Scale } else { machine_icon(&machine) };
        let trigger = super::context_strip::strip_button("host-picker", t)
            .selected(self.host.open)
            .child(Icon::new(icon).with_size(px(12.)))
            .child(div().max_w(px(240.)).truncate().child(shown_label))
            .tooltip(tooltip);
        let view = cx.entity();
        let mut popover = Popover::new("host-popover")
            .anchor(gpui_kit::Anchor::BottomLeft)
            .open(self.host.open)
            .on_open_change(move |open, _, cx| {
                view.update(cx, |this, cx| this.set_host_open(*open, cx));
            })
            .track_focus(&self.host.menu_focus)
            .trigger(trigger)
            .p(px(4.))
            .rounded(px(8.));
        if self.host.open {
            popover = popover.child(self.render_host_menu(t, cx));
        }
        Some(div().id("host-picker-wrap").min_w_0().child(popover).into_any_element())
    }

    fn render_host_menu(&self, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let mut menu = v_flex()
            .id("host-menu")
            .track_focus(&self.host.menu_focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| this.host_menu_key(event, window, cx)))
            .min_w(px(180.))
            .child(div().px(px(8.)).py(px(4.)).text_xs().text_color(t.muted_foreground).child("Run on"));
        for (index, (item, checked)) in self.host_items().into_iter().enumerate() {
            let (id, icon): (ElementId, Lucide) = match &item {
                HostItem::Auto(_) => ("host-auto".into(), Lucide::Scale),
                HostItem::Environment(option) => {
                    (ElementId::Name(format!("host-{}", option.environment_id).into()), machine_icon(&option.machine))
                }
                HostItem::AddToMachine(_) => ("host-add-machine".into(), Lucide::Plus),
            };
            let label = SharedString::from(item.label().to_owned());
            // `ADD_TO_MACHINE_LABEL_CLASS`: faded until highlighted.
            let faded = matches!(item, HostItem::AddToMachine(_)) && self.host.highlight != Some(index);
            // The pointer highlights the row it moves over, so one row paints at a time.
            menu = menu.child(
                h_flex()
                    .id(id)
                    .h(px(28.))
                    .px(px(8.))
                    .gap(px(6.))
                    .rounded(px(4.))
                    .text_xs()
                    .cursor_pointer()
                    .when(self.host.highlight == Some(index), |el| el.bg(t.accent))
                    .on_mouse_move(cx.listener(move |this, _, _, cx| {
                        if this.host.highlight != Some(index) {
                            this.host.highlight = Some(index);
                            cx.notify();
                        }
                    }))
                    .child(div().flex().when(faded, |el| el.opacity(0.55)).child(Icon::new(icon).with_size(px(12.))))
                    .child(div().flex_1().truncate().when(faded, |el| el.opacity(0.55)).child(label))
                    .when(checked, |el| el.child(Icon::new(Lucide::Check).with_size(px(12.))))
                    .on_click(cx.listener(move |this, _, window, cx| this.pick_host_item(item.clone(), window, cx))),
            );
        }
        menu.into_any_element()
    }
}

fn send_block(
    automatic: bool,
    selection: Option<&Selection>,
    sampling: Sampling,
) -> Option<(&'static str, &'static str)> {
    if !automatic || matches!(selection, Some(Selection::Auto { picked: Some(_) })) {
        return None;
    }
    Some(match sampling {
        Sampling::Failed => (
            "Choose a machine to continue",
            "No eligible machine has available resources. Choose a machine in the composer to override.",
        ),
        // Sampling starts once the draft shows, so a draft not sampled yet is still checking.
        Sampling::Idle | Sampling::Pending => (
            "Checking machine resources",
            "Resource checks are still running. You can choose a machine in the composer.",
        ),
    })
}

/// `EnvironmentMachineIcon`.
pub(in crate::app) fn machine_icon(machine: &str) -> Lucide {
    match machine {
        "desktop" | "mac-studio" | "mac-mini" => Lucide::Monitor,
        "laptop" => Lucide::Laptop,
        "cloud" => Lucide::Cloud,
        _ => Lucide::Server,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(env: &str, project: &str) -> Target {
        Target::draft(env.to_owned().into(), project.to_owned(), crate::app::draft_sessions::DraftId::for_test(project))
    }

    #[test]
    fn a_scratch_switch_lands_only_while_it_is_the_newest_for_its_draft() {
        let (a, b) = (draft("local", "web"), draft("remote", "web"));
        assert!(change_lands(2, 2, Some(&a), Some(&a)));
        assert!(!change_lands(1, 2, Some(&a), Some(&a)), "a newer switch superseded it");
        assert!(!change_lands(2, 2, Some(&a), Some(&b)), "the draft moved meanwhile");
    }

    #[test]
    fn a_failed_sample_holds_only_for_its_draft_and_candidates() {
        let (a, b) = (draft("local", "web"), draft("local", "api"));
        let candidates: Vec<SharedString> = vec!["local".into(), "remote".into()];
        let failed = Sample { target: a.clone(), candidates: candidates.clone(), state: Sampling::Failed };
        assert_eq!(sampling_for(Some(&failed), &a, &candidates), Sampling::Failed);
        assert_eq!(sampling_for(Some(&failed), &b, &candidates), Sampling::Idle, "another draft samples");
        assert_eq!(sampling_for(Some(&failed), &a, &candidates[..1]), Sampling::Idle, "the candidates changed");
        assert_eq!(sampling_for(None, &a, &candidates), Sampling::Idle);
    }

    #[test]
    fn a_sample_that_finishes_after_its_draft_was_left_samples_again_on_return() {
        let (a, b) = (draft("local", "web"), draft("local", "api"));
        let candidates: Vec<SharedString> = vec!["local".into(), "remote".into()];
        let pending = || Some(Sample { target: a.clone(), candidates: candidates.clone(), state: Sampling::Pending });
        let mut left = pending();
        sample_unused(&mut left, &a, false);
        assert_eq!(sampling_for(left.as_ref(), &a, &candidates), Sampling::Idle);
        let mut open = pending();
        sample_unused(&mut open, &a, true);
        assert_eq!(sampling_for(open.as_ref(), &a, &candidates), Sampling::Failed);
        // Another draft's sample is not this one's to settle.
        let mut other = pending();
        sample_unused(&mut other, &b, false);
        assert_eq!(other, pending());
    }

    #[test]
    fn routing_follows_its_draft_and_stays_off_other_drafts() {
        let mut selections = HashMap::new();
        let (here, there) = (draft("local", "web"), draft("remote", "web-copy"));
        selections.insert(here.clone(), Selection::Manual);
        carry(&mut selections, &here, &there);
        assert_eq!(selections.get(&there), Some(&Selection::Manual));
        assert_eq!(selections.get(&here), None, "a new draft back on the first machine starts fresh");
        carry(&mut selections, &draft("local", "api"), &here);
        assert_eq!(selections.get(&here), None, "a draft without a choice carries none");
    }

    #[test]
    fn typing_jumps_to_the_row_its_letters_start() {
        let labels = ["Auto balance", "Laptop", "Lab server", "Mac Studio"];
        let now = Instant::now();
        let mut typeahead = Typeahead::default();
        assert_eq!(typeahead.type_char('L', &labels, Some(0), now), Some(1));
        assert_eq!(typeahead.type_char('a', &labels, Some(1), now), Some(1), "\"la\" still names Laptop");
        assert_eq!(typeahead.type_char('b', &labels, Some(1), now), Some(2), "\"lab\" refines to Lab server");
        let later = now + Typeahead::RESET + Duration::from_millis(1);
        assert_eq!(typeahead.type_char('m', &labels, Some(2), later), Some(3), "a pause starts over");
        assert_eq!(typeahead.type_char('x', &labels, Some(3), later), None, "no row starts \"mx\"");

        // A repeated letter cycles the rows it starts, wrapping past the end.
        let mut typeahead = Typeahead::default();
        assert_eq!(typeahead.type_char('l', &labels, Some(0), now), Some(1));
        assert_eq!(typeahead.type_char('l', &labels, Some(1), now), Some(2));
        assert_eq!(typeahead.type_char('l', &labels, Some(2), now), Some(1));
    }

    #[test]
    fn an_automatic_draft_sends_only_once_balancing_picked_its_machine() {
        let picked = Selection::Auto { picked: Some("mac".into()) };
        let waiting = Selection::Auto { picked: None };
        assert_eq!(send_block(false, None, Sampling::Pending), None, "a machine picked by hand sends");
        assert_eq!(send_block(true, Some(&picked), Sampling::Idle), None);
        assert_eq!(
            send_block(true, Some(&waiting), Sampling::Pending).map(|w| w.0),
            Some("Checking machine resources")
        );
        assert_eq!(send_block(true, None, Sampling::Idle).map(|w| w.0), Some("Checking machine resources"));
        assert_eq!(
            send_block(true, Some(&waiting), Sampling::Failed).map(|w| w.0),
            Some("Choose a machine to continue")
        );
    }
}
