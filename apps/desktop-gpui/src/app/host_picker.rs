//! The composer's "Run on" picker (`BranchToolbarEnvironmentSelector.tsx`) and the draft routing
//! behind it (`useLogicalProjectEnvironments.ts`, `useDraftEnvironmentRouting.ts`): an unsent
//! draft moves to the same project's checkout on another machine, by hand or by load balancing.

use std::collections::HashMap;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::popover::Popover;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, Selectable as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, ElementId, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Task, Window, div, px,
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

#[derive(Default)]
pub(super) struct HostUi {
    open: bool,
    /// By project group key, as the web keeps one draft per logical project.
    selections: HashMap<String, Selection>,
    sampling: Sampling,
    _sample: Option<Task<()>>,
}

impl Workspace {
    /// The open draft's or thread's project group key.
    fn target_group_key(&self) -> Option<String> {
        let project = self.target_project()?;
        let groups = self.project_groups();
        groups.iter().find(|g| g.contains(&project.id)).map(|g| g.key.clone())
    }

    fn option_for(&self, environment_id: &SharedString, project_id: Option<String>) -> EnvironmentOption {
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
        let Some(group) = groups.iter().find(|g| g.contains(&project.id)) else { return Vec::new() };
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

    /// The draft is a "No project" draft (`activeProjectIsScratch`), which never auto-balances.
    fn draft_is_scratch(&self) -> bool {
        let Some(project) = self.target_project() else { return false };
        self.project_groups().iter().any(|g| g.contains(&project.id) && g.members[0].is_scratch)
    }

    fn selection(&self) -> Option<&Selection> {
        self.host.selections.get(&self.target_group_key()?)
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

    /// `autoEnvironmentLabel`.
    fn auto_environment_label(&self, options: &[EnvironmentOption]) -> Option<&'static str> {
        if !self.automatic_environment(options) {
            return None;
        }
        let picked = matches!(self.selection(), Some(Selection::Auto { picked: Some(_) }));
        Some(match self.host.sampling {
            Sampling::Pending if !picked => "Checking machines…",
            Sampling::Failed if !picked => "Auto balance unavailable",
            _ => "Auto balance",
        })
    }

    /// Moves the draft to `option`'s checkout, opening its "No project" folder first if needed
    /// (`onEnvironmentChange`). A pick by hand turns load balancing off for this draft.
    fn change_environment(&mut self, option: EnvironmentOption, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.target_group_key() else { return };
        self.host.open = false;
        self.host.selections.insert(key, Selection::Manual);
        self.host._sample = None;
        self.host.sampling = Sampling::Idle;
        let Some(project_id) = option.project_id else {
            self.open_scratch_then_retarget(option.environment_id, window, cx);
            return;
        };
        self.retarget_draft(option.environment_id, project_id, cx);
    }

    /// The draft keeps its text and moves to another checkout of its project.
    fn retarget_draft(&mut self, env: SharedString, project_id: String, cx: &mut Context<Self>) {
        if !matches!(self.target, Some(Target::Draft { .. })) {
            return;
        }
        self.target = Some(Target::Draft { env, project_id });
        self.sync_attachment_uploads(cx);
        self.model_override = None;
        self.notify_sidebar(cx);
        cx.notify();
    }

    fn open_scratch_then_retarget(&mut self, env: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.env_client(&env) else { return };
        let origin = self.target.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("projects.ensureScratch", json!({})).await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                // A draft that moved on meanwhile keeps its machine.
                Ok(value) if this.target == origin => {
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
        if !matches!(self.target, Some(Target::Draft { .. })) {
            return;
        }
        if !self.attachments.is_empty() {
            let message =
                "Remove attachments before choosing automatic routing, then attach them on the selected machine.";
            window.push_notification(Notification::warning(message).title("Keep attachments on this machine"), cx);
            return;
        }
        let Some(key) = self.target_group_key() else { return };
        self.host.selections.insert(key, Selection::Auto { picked: None });
        self.draft_workspace.choice.branch = None;
        self.draft_workspace.choice.existing_worktree = None;
        self.host._sample = None;
        self.host.sampling = Sampling::Idle;
        self.sync_load_balancing(cx);
        cx.notify();
    }

    /// Samples the candidate machines once an automatic draft has no machine yet, then moves
    /// the draft to the pick (`useLoadBalancedEnvironment` and its effect). Cheap when idle.
    pub(super) fn sync_load_balancing(&mut self, cx: &mut Context<Self>) {
        if !self.client_settings.values.bool("loadBalancingEnabled")
            || !matches!(self.target, Some(Target::Draft { .. }))
            || self.host.sampling != Sampling::Idle
            || matches!(self.selection(), Some(Selection::Auto { picked: Some(_) }))
        {
            return;
        }
        let options = self.logical_project_environments();
        if !self.automatic_environment(&options) {
            return;
        }
        let Some(key) = self.target_group_key() else { return };
        let candidates = self.load_balancing_candidates(&options);
        self.host.sampling = Sampling::Pending;
        let origin = self.target.clone();
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
                match target.and_then(|o| Some((o.environment_id.clone(), o.project_id.clone()?))) {
                    Some((env, project_id)) if this.target == origin => {
                        this.host.sampling = Sampling::Idle;
                        this.host.selections.insert(key, Selection::Auto { picked: Some(env.clone()) });
                        this.retarget_draft(env, project_id, cx);
                    }
                    _ => this.host.sampling = Sampling::Failed,
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
        if !self.can_change_environment(&self.logical_project_environments()) {
            return false;
        }
        self.host.open = true;
        cx.notify();
        true
    }

    /// `composer.cycleHost`: the draft moves to the project's next machine.
    pub(super) fn cycle_host(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let options = self.logical_project_environments();
        if !self.can_change_environment(&options) {
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
        let can_pick = self.can_change_environment(&options);
        let shown = can_pick || current.is_some_and(|o| !o.is_primary) || self.environment_remote;
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
                view.update(cx, |this, cx| {
                    this.host.open = *open;
                    cx.notify();
                });
            })
            .trigger(trigger)
            .p(px(4.))
            .rounded(px(8.));
        if self.host.open {
            let balancing = self.client_settings.values.bool("loadBalancingEnabled") && !self.draft_is_scratch();
            popover = popover.child(self.render_host_menu(&options, active, auto, balancing, t, cx));
        }
        Some(div().id("host-picker-wrap").min_w_0().child(popover).into_any_element())
    }

    fn render_host_menu(
        &self,
        options: &[EnvironmentOption],
        active: Option<&SharedString>,
        auto: Option<&'static str>,
        balancing: bool,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let row = |id: ElementId, icon: Lucide, label: SharedString, checked: bool| {
            h_flex()
                .id(id)
                .h(px(28.))
                .px(px(8.))
                .gap(px(6.))
                .rounded(px(4.))
                .text_xs()
                .cursor_pointer()
                .hover(|s| s.bg(t.accent))
                .child(Icon::new(icon).with_size(px(12.)))
                .child(div().flex_1().truncate().child(label))
                .when(checked, |el| el.child(Icon::new(Lucide::Check).with_size(px(12.))))
        };
        let mut menu = v_flex()
            .min_w(px(180.))
            .child(div().px(px(8.)).py(px(4.)).text_xs().text_color(t.muted_foreground).child("Run on"));
        if balancing {
            let label = SharedString::from(auto.unwrap_or("Auto balance"));
            menu = menu.child(
                row("host-auto".into(), Lucide::Scale, label, auto.is_some())
                    .on_click(cx.listener(|this, _, window, cx| this.choose_auto_environment(window, cx))),
            );
        }
        for option in options {
            let checked = auto.is_none() && Some(&option.environment_id) == active;
            let id = ElementId::Name(format!("host-{}", option.environment_id).into());
            let picked = option.clone();
            menu =
                menu.child(row(id, machine_icon(&option.machine), option.label.clone(), checked).on_click(
                    cx.listener(move |this, _, window, cx| this.change_environment(picked.clone(), window, cx)),
                ));
        }
        menu.into_any_element()
    }
}

/// `EnvironmentMachineIcon`.
fn machine_icon(machine: &str) -> Lucide {
    match machine {
        "desktop" | "mac-studio" | "mac-mini" => Lucide::Monitor,
        "laptop" => Lucide::Laptop,
        "cloud" => Lucide::Cloud,
        _ => Lucide::Server,
    }
}
