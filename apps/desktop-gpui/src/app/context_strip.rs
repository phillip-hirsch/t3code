//! The row under a draft's composer (`BranchToolbar.tsx`, `ComposerSurface.ContextStrip`):
//! the environment chip, the workspace mode and branch pickers, and the
//! permissions control (`ComposerContextPermissions.tsx`), which the composer
//! footer reuses for started threads.

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Disableable as _, Icon, Selectable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, AppContext as _, Context, ElementId, Entity, Focusable as _, FontWeight, InteractiveElement as _,
    IntoElement, Keystroke, ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, Task, Window, div, prelude::FluentBuilder as _, px,
};
use serde_json::{Value, json};

use super::composer::runtime_modes;
use super::{Target, Workspace};
use crate::new_thread::{self, DraftWorkspace, EnvMode, WorkspaceChoice};
use crate::theme::Tokens;

/// `runtimeModeConfig.ts` descriptions, in `runtime_modes()` order.
const MODE_DESCRIPTIONS: [&str; 4] = [
    "Ask before commands and file changes.",
    "Auto-approve edits, ask before other actions.",
    "Supported providers approve routine actions; others still ask.",
    "Allow commands and edits without prompts.",
];

/// The draft's workspace: what the controls picked and what they read.
#[derive(Default)]
pub(super) struct DraftWorkspaceState {
    project_id: Option<String>,
    pub(super) choice: WorkspaceChoice,
    default_mode: Option<EnvMode>,
    start_from_origin_default: bool,
    /// `vcs.listRefs` for the project's checkout.
    refs: Option<Value>,
    _load: Option<Task<()>>,
    branch_open: bool,
    branch_search: Option<Entity<InputState>>,
    branch_query: String,
    _branch_search_changes: Option<Subscription>,
    /// The permissions popover that is open, by element id, and the mode it previews.
    permissions_open: Option<&'static str>,
    permissions_preview: Option<&'static str>,
    /// `vcs.listRefs` for an open thread's checkout, by cwd (`persistComposerContextStrip`).
    thread_refs: Option<(String, Value)>,
}

impl Workspace {
    /// Loads the draft's default workspace mode and refs when its project changes.
    pub(super) fn sync_draft_workspace(&mut self, cx: &mut Context<Self>) {
        let Some(Target::Draft { project_id }) = self.target.clone() else { return };
        if self.draft_workspace.project_id.as_ref() == Some(&project_id) {
            return;
        }
        let (Some(client), Some(project)) = (self.client(), self.shell.projects.iter().find(|p| p.id == project_id))
        else {
            return;
        };
        self.draft_workspace = DraftWorkspaceState { project_id: Some(project_id.clone()), ..Default::default() };
        let has_threads = self.shell.threads.iter().any(|t| t.project_id == project_id);
        let workspace = DraftWorkspace::new(&self.server_info, project, has_threads);
        self.draft_workspace.start_from_origin_default = workspace.start_from_origin();
        let cwd = project.workspace_root.clone();
        self.draft_workspace._load = Some(cx.spawn(async move |this, cx| {
            let mode = workspace.default_mode(&client).await;
            let refs = new_thread::list_refs(&client, &cwd).await;
            let _ = this.update(cx, |this, cx| {
                if this.draft_workspace.project_id.as_ref() != Some(&project_id) {
                    return;
                }
                this.draft_workspace.default_mode = Some(mode);
                match refs {
                    Ok(refs) => this.draft_workspace.refs = Some(refs),
                    Err(error) => log::warn!("could not list refs: {error}"),
                }
                cx.notify();
            });
        }));
    }

    /// Escape closes the branch picker.
    pub(super) fn draft_workspace_key(
        &mut self,
        keystroke: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if keystroke.key != "escape" || !self.draft_workspace.branch_open {
            return false;
        }
        self.draft_workspace.branch_open = false;
        self.focus_composer(window, cx);
        cx.notify();
        true
    }

    pub(super) fn draft_workspace_open_permissions(&mut self, cx: &mut Context<Self>) {
        self.draft_workspace.permissions_open = Some("strip-permissions");
        cx.notify();
    }

    fn refresh_refs(&mut self, cx: &mut Context<Self>) {
        let (Some(client), Some(cwd)) = (self.client(), self.composer_cwd()) else { return };
        let thread = matches!(self.target, Some(Target::Thread(_)));
        cx.spawn(async move |this, cx| {
            let refs = new_thread::list_refs(&client, &cwd).await;
            let _ = this.update(cx, |this, cx| {
                if let Ok(refs) = refs {
                    match thread {
                        true => this.draft_workspace.thread_refs = Some((cwd, refs)),
                        false => this.draft_workspace.refs = Some(refs),
                    }
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// The refs the branch picker lists: the thread checkout's, or the draft project's.
    fn refs(&self) -> Option<&Value> {
        match &self.target {
            Some(Target::Thread(_)) => {
                let (cwd, refs) = self.draft_workspace.thread_refs.as_ref()?;
                (Some(cwd) == self.composer_cwd().as_ref()).then_some(refs)
            }
            _ => self.draft_workspace.refs.as_ref(),
        }
    }

    pub(super) fn draft_env_mode(&self) -> EnvMode {
        let state = &self.draft_workspace;
        state.choice.mode.or(state.default_mode).unwrap_or(EnvMode::Local)
    }

    pub(super) fn draft_start_from_origin(&self) -> bool {
        self.draft_workspace.choice.start_from_origin.unwrap_or(self.draft_workspace.start_from_origin_default)
    }

    /// The draft launch's `workspaceStrategy`, once the controls have loaded.
    pub(super) fn draft_launch_strategy(&self) -> Option<Result<Value, String>> {
        let state = &self.draft_workspace;
        let mode = state.choice.mode.or(state.default_mode)?;
        let refs = state.refs.as_ref()?;
        Some(new_thread::launch_strategy(mode, &state.choice, refs, self.draft_start_from_origin()))
    }

    fn is_git_repo(&self) -> bool {
        self.draft_workspace.refs.as_ref().and_then(|r| r.get("isRepo")).and_then(Value::as_bool).unwrap_or(true)
    }

    fn ref_list(&self) -> &[Value] {
        self.refs().and_then(|r| r.get("refs")).and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default()
    }

    fn set_env_mode(&mut self, mode: EnvMode, cx: &mut Context<Self>) {
        let choice = &mut self.draft_workspace.choice;
        choice.mode = Some(mode);
        choice.existing_worktree = None;
        choice.branch = None;
        cx.notify();
    }

    /// The newest other thread of this project that ran in a worktree (`resolvePreviousWorktreeSeed`).
    fn previous_worktree(&self) -> Option<(String, Option<String>)> {
        let Some(Target::Draft { project_id }) = &self.target else { return None };
        self.shell
            .threads
            .iter()
            .filter(|t| &t.project_id == project_id && t.archived_at.is_none() && t.deleted_at.is_none())
            .filter_map(|t| Some((t.updated_at.as_str(), t.worktree_path.clone()?, t.branch.clone())))
            .max_by(|a, b| a.0.cmp(b.0))
            .map(|(_, path, branch)| (path, branch))
    }

    fn pick_previous_worktree(&mut self, path: String, branch: Option<String>, cx: &mut Context<Self>) {
        self.draft_workspace.choice = WorkspaceChoice {
            mode: Some(EnvMode::Worktree),
            branch: branch.clone(),
            existing_worktree: Some((path, branch)),
            start_from_origin: None,
        };
        cx.notify();
    }

    /// `composer.previousWorktree` (`BranchToolbarHandle.usePreviousWorktree`): a draft reuses the
    /// newest worktree of its project, then the composer takes focus.
    pub(super) fn use_previous_worktree(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((path, branch)) = self.previous_worktree() else { return };
        self.pick_previous_worktree(path, branch, cx);
        self.focus_composer(window, cx);
    }

    /// Worktree mode just records the base; local mode switches the checkout (`vcs.switchRef`).
    fn select_branch(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        self.draft_workspace.branch_open = false;
        if let Some(Target::Thread(thread_id)) = self.target.clone() {
            self.select_thread_branch(thread_id, name, cx);
            self.focus_composer(window, cx);
            return;
        }
        if self.draft_env_mode() == EnvMode::Worktree && self.draft_workspace.choice.existing_worktree.is_none() {
            self.draft_workspace.choice.branch = Some(name);
            self.focus_composer(window, cx);
            cx.notify();
            return;
        }
        let elsewhere = self
            .ref_list()
            .iter()
            .find(|r| r["name"].as_str() == Some(&name))
            .and_then(|r| r["worktreePath"].as_str().map(str::to_owned));
        let root = self.composer_cwd();
        if let Some(path) = elsewhere {
            // Already checked out in a worktree: reuse it instead of switching.
            self.draft_workspace.choice.existing_worktree =
                (Some(&path) != root.as_ref()).then(|| (path, Some(name.clone())));
            self.draft_workspace.choice.branch = Some(name);
            cx.notify();
            return;
        }
        self.run_ref_command("vcs.switchRef", json!({"cwd": root, "refName": name}), name, "Failed to switch ref.", cx);
    }

    fn create_branch(&mut self, name: String, cx: &mut Context<Self>) {
        self.draft_workspace.branch_open = false;
        let cwd = self.composer_cwd();
        if let Some(Target::Thread(thread_id)) = self.target.clone() {
            let worktree = self.open_projection().and_then(|p| p.thread.worktree_path.clone());
            let input = json!({"cwd": cwd, "refName": name, "switchRef": true});
            self.run_thread_ref_command(thread_id, "vcs.createRef", input, name, worktree, cx);
            return;
        }
        self.run_ref_command(
            "vcs.createRef",
            json!({"cwd": cwd, "refName": name, "switchRef": true}),
            name,
            "Failed to create and switch ref.",
            cx,
        );
    }

    fn run_ref_command(
        &mut self,
        method: &'static str,
        input: Value,
        name: String,
        failure: &'static str,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.client() else { return };
        self.draft_workspace.choice.branch = Some(name.clone());
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = client.request(method, input).await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(value) => {
                        let switched = value.get("refName").and_then(Value::as_str).map(str::to_owned);
                        this.draft_workspace.choice.branch = Some(switched.unwrap_or(name));
                        this.refresh_refs(cx);
                    }
                    Err(error) => {
                        log::warn!("{method} failed: {error}");
                        this.draft_workspace.choice.branch = None;
                        this.error = Some(failure.into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn open_branch_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let search = match self.draft_workspace.branch_search.clone() {
            Some(search) => search,
            None => {
                let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search refs..."));
                self.draft_workspace._branch_search_changes =
                    Some(cx.subscribe_in(&search, window, |this, search, event, _, cx| {
                        if let InputEvent::Change = event {
                            this.draft_workspace.branch_query = search.read(cx).value().trim().to_owned();
                            cx.notify();
                        }
                    }));
                self.draft_workspace.branch_search = Some(search.clone());
                search
            }
        };
        search.update(cx, |search, cx| {
            search.set_value("", window, cx);
            search.focus(window, cx);
        });
        self.draft_workspace.branch_query.clear();
        self.draft_workspace.branch_open = true;
        self.refresh_refs(cx);
        cx.notify();
    }

    /// `selectBranch` on a started thread: reuse the worktree that has the ref checked
    /// out, else switch the thread's checkout (`resolveBranchSelectionTarget`).
    fn select_thread_branch(&mut self, thread_id: String, name: String, cx: &mut Context<Self>) {
        let Some(root) = self.target_project().map(|p| p.workspace_root.clone()) else { return };
        let active = self.open_projection().and_then(|p| p.thread.worktree_path.clone());
        let picked = self.ref_list().iter().find(|r| r["name"].as_str() == Some(name.as_str()));
        let elsewhere = picked.and_then(|r| r["worktreePath"].as_str()).map(str::to_owned);
        let is_default = picked.is_some_and(|r| r["isDefault"].as_bool() == Some(true));
        if let Some(path) = elsewhere {
            let next = (!is_within(&root, &path)).then_some(path);
            self.set_thread_branch(&thread_id, &name, next, cx);
            return;
        }
        let next = active.filter(|_| !is_default);
        let checkout = next.clone().unwrap_or(root);
        let input = json!({"cwd": checkout, "refName": name});
        self.run_thread_ref_command(thread_id, "vcs.switchRef", input, name, next, cx);
    }

    fn run_thread_ref_command(
        &mut self,
        thread_id: String,
        method: &'static str,
        input: Value,
        name: String,
        worktree: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.client() else { return };
        cx.spawn(async move |this, cx| {
            let result = client.request(method, input).await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(value) => {
                        let switched = value.get("refName").and_then(Value::as_str).map(str::to_owned);
                        this.set_thread_branch(&thread_id, &switched.unwrap_or(name), worktree, cx);
                        this.refresh_refs(cx);
                    }
                    Err(error) => {
                        log::warn!("{method} failed: {error}");
                        let failure = match method {
                            "vcs.createRef" => "Failed to create and switch ref.",
                            _ => "Failed to switch ref.",
                        };
                        this.error = Some(failure.into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// `setThreadBranch`: a move to another checkout detaches the live provider
    /// sessions first, then the thread records its branch and worktree.
    fn set_thread_branch(&mut self, thread_id: &str, branch: &str, worktree: Option<String>, cx: &mut Context<Self>) {
        let Some(projection) = self.open_projection() else { return };
        let mut commands = Vec::new();
        if worktree != projection.thread.worktree_path {
            let command_id = super::new_id();
            for session in
                projection.provider_sessions.iter().filter(|s| !matches!(s.status.as_str(), "stopped" | "error"))
            {
                commands.push(json!({
                    "type": "provider-session.detach",
                    "commandId": format!("{command_id}:detach:{}", session.id),
                    "threadId": thread_id,
                    "providerSessionId": session.id,
                    "reason": "client-requested",
                }));
            }
        }
        commands.push(json!({
            "type": "thread.metadata.update",
            "commandId": super::new_id(),
            "threadId": thread_id,
            "branch": branch,
            "worktreePath": worktree,
        }));
        for command in commands {
            self.dispatch(command, cx);
        }
    }

    /// `shouldShowComposerContextStrip`: always under a draft; under a started thread
    /// only with `persistComposerContextStrip`, and then without permissions.
    pub(super) fn render_context_strip(&self, t: &Tokens, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.target_project()?;
        if matches!(self.target, Some(Target::Thread(_))) {
            return self.render_thread_context_strip(t, cx);
        }
        let git = self.is_git_repo();
        let environment = self.environment_remote.then(|| self.render_environment_chip(t));
        let mode = self.runtime_mode();
        let (_, label, icon) = runtime_modes().into_iter().find(|(id, ..)| *id == mode).unwrap_or(runtime_modes()[3]);
        Some(
            h_flex()
                .w_full()
                .px(px(8.))
                .pt(px(8.))
                .gap(px(4.))
                .text_xs()
                .text_color(t.muted_foreground)
                .when(git || environment.is_some(), |el| {
                    el.child(
                        h_flex()
                            .flex_1()
                            .min_w(px(40.))
                            .min_h(px(24.))
                            .gap(px(4.))
                            .children(environment)
                            .when(git && self.environment_remote, |el| {
                                el.child(div().w(px(1.)).h(px(14.)).mx(px(2.)).bg(t.border))
                            })
                            .when(git, |el| el.child(self.render_env_mode_selector(t, cx))),
                    )
                })
                .when(git, |el| el.child(self.render_branch_selector(t, cx)))
                .child(div().ml_auto().child(self.render_permissions_control(
                    "strip-permissions",
                    icon,
                    label,
                    false,
                    t,
                    cx,
                )))
                .into_any_element(),
        )
    }

    fn render_thread_context_strip(&self, t: &Tokens, cx: &mut Context<Self>) -> Option<AnyElement> {
        let git = self.vcs.status.as_ref().is_none_or(|status| status.local.is_repo);
        let persist = self.client_settings.values.bool("persistComposerContextStrip");
        if !persist || !(git || self.environment_remote) {
            return None;
        }
        let environment = self.environment_remote.then(|| self.render_environment_chip(t));
        // A started thread keeps its workspace (`resolveLockedWorkspaceLabel`).
        let worktree = self.git_cwd() != self.target_project().map(|p| p.workspace_root.clone());
        let (icon, label) = if worktree { (Lucide::FolderGit, "Worktree") } else { (Lucide::Folder, "Local checkout") };
        let locked = h_flex()
            .id("env-mode-locked")
            .min_w_0()
            .h(px(24.))
            .px(px(7.))
            .gap(px(6.))
            .text_color(t.muted_foreground.opacity(0.7))
            .child(Icon::new(icon).with_size(px(12.)).flex_none())
            .child(div().max_w(px(240.)).truncate().child(label))
            .tooltip(move |window, cx| Tooltip::new(label).build(window, cx));
        Some(
            h_flex()
                .w_full()
                .px(px(8.))
                .pt(px(8.))
                .gap(px(4.))
                .text_xs()
                .text_color(t.muted_foreground)
                .child(
                    h_flex()
                        .flex_1()
                        .min_w(px(40.))
                        .min_h(px(24.))
                        .gap(px(4.))
                        .children(environment)
                        .when(git && self.environment_remote, |el| {
                            el.child(div().w(px(1.)).h(px(14.)).mx(px(2.)).bg(t.border))
                        })
                        .when(git, |el| el.child(locked)),
                )
                .when(git, |el| el.child(self.render_branch_selector(t, cx)))
                .into_any_element(),
        )
    }

    /// The machine the draft runs on; a locked label while this is the only environment.
    fn render_environment_chip(&self, t: &Tokens) -> AnyElement {
        let label: SharedString = self.server_info.environment_label.clone().unwrap_or_else(|| "Run on".into()).into();
        let icon = match self.server_info.machine_kind() {
            "desktop" | "mac-studio" | "mac-mini" => Lucide::Monitor,
            "laptop" => Lucide::Laptop,
            "cloud" => Lucide::Cloud,
            _ => Lucide::Server,
        };
        let tooltip = label.clone();
        h_flex()
            .id("environment-chip")
            .min_w_0()
            .h(px(24.))
            .px(px(7.))
            .gap(px(6.))
            .text_color(t.muted_foreground.opacity(0.7))
            .child(Icon::new(icon).with_size(px(12.)).flex_none())
            .child(div().max_w(px(240.)).truncate().child(label))
            .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
            .into_any_element()
    }

    fn render_env_mode_selector(&self, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let choice = &self.draft_workspace.choice;
        let (icon, label) = match (&choice.existing_worktree, self.draft_env_mode()) {
            (Some(_), _) => (Lucide::FolderGit, "Previous worktree"),
            (None, EnvMode::Worktree) => (Lucide::FolderGit2, "New worktree"),
            (None, EnvMode::Local) => (Lucide::Folder, "Current checkout"),
        };
        let previous = self.previous_worktree();
        let view = cx.entity();
        strip_button("env-mode", t)
            .child(Icon::new(icon).with_size(px(12.)))
            .child(div().max_w(px(240.)).truncate().child(label))
            .tooltip(match self.shortcut_label("composer.workspace") {
                Some(keys) => format!("{label} · {keys}"),
                None => label.to_owned(),
            })
            .dropdown_menu_with_anchor(gpui_kit::Anchor::BottomLeft, move |menu, window, _| {
                let mut menu = menu
                    .label("Workspace")
                    .item(
                        PopupMenuItem::new("Current checkout")
                            .icon(Icon::new(Lucide::Folder))
                            .checked(label == "Current checkout")
                            .on_click(
                                window.listener_for(&view, |this, _, _, cx| this.set_env_mode(EnvMode::Local, cx)),
                            ),
                    )
                    .item(
                        PopupMenuItem::new("New worktree")
                            .icon(Icon::new(Lucide::FolderGit2))
                            .checked(label == "New worktree")
                            .on_click(
                                window.listener_for(&view, |this, _, _, cx| this.set_env_mode(EnvMode::Worktree, cx)),
                            ),
                    );
                if let Some((path, branch)) = previous.clone() {
                    let text = match &branch {
                        Some(branch) => format!("Previous worktree · {branch}"),
                        None => "Previous worktree".into(),
                    };
                    menu = menu.item(
                        PopupMenuItem::new(text)
                            .icon(Icon::new(Lucide::RotateCcwClock))
                            .checked(label == "Previous worktree")
                            .on_click(window.listener_for(&view, move |this, _, _, cx| {
                                this.pick_previous_worktree(path.clone(), branch.clone(), cx)
                            })),
                    );
                }
                menu
            })
            .into_any_element()
    }

    fn render_branch_selector(&self, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let state = &self.draft_workspace;
        let thread = matches!(self.target, Some(Target::Thread(_)));
        let worktree_base =
            !thread && self.draft_env_mode() == EnvMode::Worktree && state.choice.existing_worktree.is_none();
        let current =
            self.ref_list().iter().find(|r| r["current"].as_bool() == Some(true)).and_then(|r| r["name"].as_str());
        let thread_branch = self
            .open_projection()
            .and_then(|p| p.thread.branch.as_deref())
            .or_else(|| self.vcs.status.as_ref().and_then(|s| s.local.ref_name.as_deref()));
        let branch = match (thread, worktree_base) {
            (true, _) => thread_branch.or(current),
            (false, true) => {
                state.choice.branch.as_deref().or_else(|| state.refs.as_ref().and_then(new_thread::default_base))
            }
            (false, false) => state.choice.branch.as_deref().or(current),
        };
        let is_remote = branch
            .and_then(|b| self.ref_list().iter().find(|r| r["name"].as_str() == Some(b)))
            .map(|r| r["isRemote"].as_bool() == Some(true));
        let label: SharedString = match (branch, worktree_base) {
            (None, _) => "Select ref".into(),
            (Some(b), true) if self.draft_start_from_origin() && is_remote == Some(false) => {
                format!("From {b} (remote)").into()
            }
            (Some(b), true) => format!("From {b}").into(),
            (Some(b), false) => b.to_owned().into(),
        };
        let trigger = strip_button("branch-selector", t)
            .disabled(!thread && state.refs.is_none())
            .selected(state.branch_open)
            .child(Icon::new(Lucide::GitBranch).with_size(px(12.)))
            .child(div().max_w(px(240.)).truncate().child(label));
        let view = cx.entity();
        let mut popover = Popover::new("branch-popover")
            .anchor(gpui_kit::Anchor::BottomRight)
            .open(state.branch_open)
            .on_open_change(move |open, window, cx| {
                view.update(cx, |this, cx| {
                    if *open {
                        this.open_branch_picker(window, cx);
                    } else {
                        this.draft_workspace.branch_open = false;
                        cx.notify();
                    }
                })
            })
            .trigger(trigger)
            .p_0()
            .rounded(px(8.));
        if let Some(search) = &state.branch_search {
            popover = popover.track_focus(&search.focus_handle(cx));
        }
        if state.branch_open {
            popover = popover.child(self.render_branch_popover(branch, worktree_base, t, cx));
        }
        div().min_w_0().child(popover).into_any_element()
    }

    fn render_branch_popover(
        &self,
        selected: Option<&str>,
        worktree_base: bool,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let query = self.draft_workspace.branch_query.to_lowercase();
        let refs: Vec<&Value> = self
            .ref_list()
            .iter()
            .filter(|r| r["name"].as_str().is_some_and(|n| n.to_lowercase().contains(&query)))
            .collect();
        let exact =
            self.ref_list().iter().any(|r| r["name"].as_str() == Some(self.draft_workspace.branch_query.as_str()));
        let create = (!query.is_empty() && !exact && !worktree_base).then(|| self.draft_workspace.branch_query.clone());
        let total = self.refs().and_then(|r| r["totalCount"].as_u64());
        let rows = refs.iter().map(|r| {
            let name = r["name"].as_str().unwrap_or_default().to_owned();
            let badge = if r["current"].as_bool() == Some(true) {
                Some("current")
            } else if r["worktreePath"].as_str().is_some() {
                Some("worktree")
            } else if r["isRemote"].as_bool() == Some(true) {
                Some("remote")
            } else if r["isDefault"].as_bool() == Some(true) {
                Some("default")
            } else {
                None
            };
            let checked = selected == Some(name.as_str());
            let pick = name.clone();
            h_flex()
                .id(ElementId::Name(format!("ref-{name}").into()))
                .h(px(28.))
                .flex_none()
                .px(px(8.))
                .gap(px(8.))
                .rounded(px(4.))
                .text_sm()
                .cursor_pointer()
                .hover(|s| s.bg(t.accent))
                .when(checked, |el| el.bg(t.foreground.opacity(0.08)))
                .child(div().flex_1().min_w_0().truncate().text_color(t.foreground).child(name))
                .children(badge.map(|badge| {
                    div()
                        .px(px(4.))
                        .rounded(px(4.))
                        .bg(t.muted.opacity(0.45))
                        .text_size(px(10.))
                        .text_color(t.muted_foreground)
                        .child(badge)
                }))
                .on_click(cx.listener(move |this, _, window, cx| this.select_branch(pick.clone(), window, cx)))
        });
        let start_from_origin = self.draft_start_from_origin();
        v_flex()
            .id("branch-popover-body")
            .w(px(320.))
            .bg(t.popover)
            .child(div().px(px(12.)).pt(px(10.)).pb(px(6.)).border_b_1().border_color(t.border).children(
                self.draft_workspace.branch_search.clone().map(|search| {
                    div().child(Input::new(&search).appearance(false).small().text_sm())
                }),
            ))
            .child(
                v_flex()
                    .id("branch-list")
                    .max_h(px(224.))
                    .overflow_y_scroll()
                    .py(px(8.))
                    .px(px(4.))
                    .when(refs.is_empty() && create.is_none(), |el| {
                        el.child(div().p(px(8.)).text_sm().text_color(t.muted_foreground).child("No refs found."))
                    })
                    .children(rows)
                    .children(create.map(|name| {
                        let label = format!("Create new ref \"{name}\"");
                        div()
                            .id("create-ref")
                            .h(px(28.))
                            .px(px(8.))
                            .flex()
                            .items_center()
                            .rounded(px(4.))
                            .text_sm()
                            .cursor_pointer()
                            .hover(|s| s.bg(t.accent))
                            .child(label)
                            .on_click(cx.listener(move |this, _, _, cx| this.create_branch(name.clone(), cx)))
                    })),
            )
            .children(total.map(|total| {
                div().px(px(12.)).pb(px(6.)).text_xs().text_color(t.muted_foreground).child(format!("Showing {} of {total} refs", self.ref_list().len()))
            }))
            .when(worktree_base, |el| {
                el.child(
                    h_flex()
                        .id("start-from-remote")
                        .px(px(12.))
                        .py(px(8.))
                        .gap(px(6.))
                        .border_t_1()
                        .border_color(t.border.opacity(0.6))
                        .text_xs()
                        .cursor_pointer()
                        .child(Icon::new(Lucide::RefreshCw).with_size(px(12.)))
                        .child(div().flex_1().child("Start from remote"))
                        .child(mini_switch(start_from_origin, t))
                        .tooltip(|window, cx| {
                            Tooltip::new("Uses the latest commit of the base branch’s tracked remote branch, even if its name differs. Otherwise uses the same-named branch on origin. Uses your local branch if that remote branch is missing.").build(window, cx)
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.draft_workspace.choice.start_from_origin = Some(!start_from_origin);
                            cx.notify();
                        })),
                )
            })
            .into_any_element()
    }

    /// The permissions trigger and its popover (`ComposerPermissionsSection`):
    /// a 24px strip control, or the 28px footer form inside the model picker.
    pub(super) fn render_permissions_control(
        &self,
        id: &'static str,
        icon: Lucide,
        label: &'static str,
        footer: bool,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let open = self.draft_workspace.permissions_open == Some(id);
        let trigger = strip_button(id, t)
            .when(footer, |b| b.h(px(28.)).px(px(8.)).rounded(px(6.)))
            .selected(open)
            .child(Icon::new(icon).with_size(px(if footer { 14. } else { 12. })))
            .child(div().max_w(px(240.)).truncate().child(label))
            .when(footer, |b| b.child(Icon::new(Lucide::ChevronUp).with_size(px(12.))))
            .when(!footer, |b| {
                b.tooltip(match self.shortcut_label("composer.mode") {
                    Some(keys) => format!("Permissions · {keys}"),
                    None => "Permissions".to_owned(),
                })
            });
        let view = cx.entity();
        let mut popover = Popover::new(SharedString::from(format!("{id}-popover")))
            .anchor(gpui_kit::Anchor::BottomRight)
            .open(open)
            .on_open_change(move |open, _, cx| {
                view.update(cx, |this, cx| {
                    this.draft_workspace.permissions_open = open.then_some(id);
                    this.draft_workspace.permissions_preview = None;
                    cx.notify();
                })
            })
            .trigger(trigger)
            .p_0()
            .rounded(px(8.));
        if open {
            popover = popover.child(self.render_permissions_section(t, cx));
        }
        popover.into_any_element()
    }

    fn render_permissions_section(&self, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let current = self.runtime_mode();
        let supported = self
            .composer_model()
            .and_then(|s| self.server_info.provider(&s.instance_id))
            .map(|p| p.supported_runtime_modes.clone())
            .unwrap_or_default();
        let shown = self.draft_workspace.permissions_preview.map(str::to_owned).unwrap_or_else(|| current.clone());
        let index = runtime_modes().iter().position(|(id, ..)| *id == shown).unwrap_or(0);
        let (_, shown_label, _) = runtime_modes()[index];
        let segments = runtime_modes().into_iter().enumerate().map(|(i, (mode, label, icon))| {
            let checked = mode == current;
            let enabled = supported.is_empty() || supported.iter().any(|m| m == mode);
            h_flex()
                .id(ElementId::Name(format!("mode-{mode}").into()))
                .flex_1()
                .h(px(24.))
                .justify_center()
                .when(i == 0, |el| el.rounded_l(px(6.)))
                .when(i == 3, |el| el.rounded_r(px(6.)))
                .text_color(if checked { t.foreground } else { t.muted_foreground })
                .when(checked, |el| el.bg(t.input.opacity(0.72)))
                .when(!enabled, |el| el.opacity(0.4))
                .when(enabled && !checked, |el| el.cursor_pointer().hover(|s| s.bg(t.accent).text_color(t.foreground)))
                .child(Icon::new(icon).with_size(px(14.)))
                .tooltip(move |window, cx| Tooltip::new(label).build(window, cx))
                .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                    let preview = hovered.then_some(mode);
                    if this.draft_workspace.permissions_preview != preview {
                        this.draft_workspace.permissions_preview = preview;
                        cx.notify();
                    }
                }))
                .when(enabled, |el| {
                    el.on_click(cx.listener(move |this, _, window, cx| {
                        this.draft_workspace.permissions_open = None;
                        this.set_runtime_mode(mode, cx);
                        this.focus_composer(window, cx);
                    }))
                })
        });
        v_flex()
            .w(px(288.))
            .gap(px(6.))
            .px(px(8.))
            .py(px(6.))
            .bg(t.popover)
            .child(div().text_xs().font_weight(FontWeight::MEDIUM).text_color(t.muted_foreground).child("Permissions"))
            .child(h_flex().p(px(2.)).rounded(px(8.)).bg(t.input.opacity(0.4)).children(segments))
            .child(
                v_flex()
                    .gap(px(2.))
                    .child(
                        div()
                            .text_sm()
                            .text_color(if shown == current { t.foreground } else { t.muted_foreground })
                            .child(shown_label),
                    )
                    .child(
                        div()
                            .min_h(px(32.))
                            .text_xs()
                            .line_height(px(16.))
                            .text_color(t.muted_foreground)
                            .child(MODE_DESCRIPTIONS[index]),
                    ),
            )
            .into_any_element()
    }
}

/// `workspaceRelativeFilePath(root, path) !== null`: `path` is `root` or inside it.
fn is_within(root: &str, path: &str) -> bool {
    let normalize = |p: &str| p.replace('\\', "/").trim_end_matches('/').to_lowercase();
    let (root, path) = (normalize(root), normalize(path));
    path == root || path.starts_with(&format!("{root}/"))
}

/// `ComposerControl size="xs"`: 24px, 7px padding, 6px gap, text-xs, muted until hovered.
fn strip_button(id: &'static str, t: &Tokens) -> Button {
    Button::new(id)
        .ghost()
        .xsmall()
        .h(px(24.))
        .px(px(7.))
        .gap(px(6.))
        .rounded(px(8.))
        .text_xs()
        .font_weight(FontWeight::NORMAL)
        .text_color(t.muted_foreground)
}

fn mini_switch(on: bool, t: &Tokens) -> impl IntoElement {
    div()
        .w(px(22.))
        .h(px(12.))
        .p(px(2.))
        .rounded_full()
        .flex()
        .when(on, |el| el.justify_end().bg(t.primary))
        .when(!on, |el| el.bg(t.input))
        .child(div().size(px(8.)).rounded_full().bg(if on { t.primary_foreground } else { t.muted_foreground }))
}
