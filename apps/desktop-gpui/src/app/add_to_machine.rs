//! Add to another machine (`AddProjectToMachineDialog.tsx`, `useAddProjectTargets.ts` and
//! `lib/addProjectToMachine.ts`): clones a project onto another connected machine, then copies
//! the ignored files the user picked from the source. The two servers never talk to each other,
//! so the client reads the files before the clone starts and writes them once it finishes; the
//! new project's composer holds sends until they land and shows a failed copy until dismissed.

use std::collections::{HashMap, HashSet};

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::{DialogDescription, DialogFooter, DialogHeader, DialogTitle};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{Disableable as _, Icon, Selectable as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, ElementId, Entity, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, ScrollHandle, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription,
    Task, WeakEntity, Window, div, px,
};
use serde_json::{Value, json};

use super::details::commit_dialog::{Check, check_box};
use super::host_picker::machine_icon;
use super::pages::settings::{format_bytes, resolve_pasted_path};
use super::project_clone::Phase;
use super::project_clone::paths::{self, CloneProtocol};
use super::{Target, Workspace, new_id};
use crate::model::{ProjectShell, ScopedId};
use crate::theme::Tokens;

/// `COPYABLE_FILE_MAX_BYTES`: the server's read cap; a larger file would arrive truncated.
const CLONE_START: &str = "projectClone.start";
const COPYABLE_FILE_MAX_BYTES: u64 = 1024 * 1024;

/// A machine the project can be cloned to (`AddProjectTarget`).
#[derive(Clone, Debug, PartialEq)]
pub(super) struct AddTarget {
    pub environment_id: SharedString,
    pub label: SharedString,
    pub machine: SharedString,
}

/// One `ProjectIgnoredFile`.
#[derive(Clone, Debug, PartialEq)]
struct IgnoredFile {
    path: String,
    bytes: u64,
}

impl IgnoredFile {
    fn too_large(&self) -> bool {
        self.bytes > COPYABLE_FILE_MAX_BYTES
    }
}

/// `projects.listIgnoredFiles` for the source checkout.
#[derive(Clone, Debug, PartialEq)]
enum Listing {
    /// The source server cannot list them (`capabilities.ignoredFileListing`).
    Unsupported,
    Loading,
    Loaded {
        files: Vec<IgnoredFile>,
        truncated: bool,
    },
    Failed(String),
}

impl Listing {
    fn files(&self) -> &[IgnoredFile] {
        match self {
            Self::Loaded { files, .. } => files,
            _ => &[],
        }
    }
}

/// `isDefaultCopiedFile`: env files are the ignored files people almost always want on a new machine.
fn is_default_copied_file(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    matches!(name, ".env" | ".envrc" | ".dev.vars") || name.strip_prefix(".env.").is_some_and(|rest| !rest.is_empty())
}

/// `defaultSelection`: the copyable env files.
fn default_selection(files: &[IgnoredFile]) -> HashSet<String> {
    files
        .iter()
        .filter(|file| !file.too_large() && is_default_copied_file(&file.path))
        .map(|f| f.path.clone())
        .collect()
}

/// `projectSubdirectory`: where a project rooted below its repository's top folder sits there, so
/// copied files keep their place in the clone, which lands at the top.
fn project_subdirectory(repository_root: Option<&str>, workspace_root: &str) -> String {
    let Some(root) = repository_root else { return String::new() };
    let rest = workspace_root.strip_prefix(root.trim_end_matches(['/', '\\']));
    match rest.and_then(|rest| rest.strip_prefix(['/', '\\'])) {
        Some(rest) => rest.replace('\\', "/"),
        None => String::new(),
    }
}

/// The diagram's count under its arrow.
fn files_summary(count: usize) -> String {
    match count {
        0 => "no files".to_owned(),
        1 => "+ 1 file".to_owned(),
        n => format!("+ {n} files"),
    }
}

/// A clone's ignored-file copy that the composer shows (`ProjectFileCopy`).
#[derive(Clone, Debug, PartialEq)]
pub(super) enum FileCopy {
    Copying,
    Failed { title: String, failures: Vec<String> },
}

/// Files read from the source, waiting for their clone to finish.
struct PendingCopy {
    /// The clone's checkout (`ProjectCloneStartResult.cwd`).
    cwd: String,
    subdirectory: String,
    label: SharedString,
    /// Each file and its contents, or why it could not be read.
    reads: Vec<(String, Result<String, String>)>,
    /// The clone has been listed: its going away means it was removed.
    seen: bool,
}

/// What a waiting copy does on its clone's latest state (`waitForClone`).
#[derive(Debug, PartialEq, Eq)]
enum CopyStep {
    Wait {
        seen: bool,
    },
    Write,
    /// The clone was removed; a failed or cancelled one keeps waiting, since it can be retried.
    Drop,
}

fn copy_step(seen: bool, phase: Option<Phase>) -> CopyStep {
    match phase {
        Some(Phase::Done) => CopyStep::Write,
        Some(_) => CopyStep::Wait { seen: true },
        None if seen => CopyStep::Drop,
        None => CopyStep::Wait { seen: false },
    }
}

/// Every cloned project's file copy, by project on its environment.
#[derive(Default)]
pub(super) struct FileCopies {
    states: HashMap<ScopedId, FileCopy>,
    pending: HashMap<ScopedId, PendingCopy>,
}

impl FileCopies {
    pub(super) fn of(&self, project: &ScopedId) -> Option<&FileCopy> {
        self.states.get(project)
    }
}

impl Workspace {
    /// `useAddProjectTargets`: connected machines that can clone (`source-control:write`) and do
    /// not have the project yet, by name. A project without a git remote has none.
    pub(super) fn add_project_targets(&self, project: &ProjectShell, existing: &[SharedString]) -> Vec<AddTarget> {
        let has_remote = project.repository_identity.as_ref().and_then(|identity| identity.locator.as_ref()).is_some();
        if !has_remote {
            return Vec::new();
        }
        let mut targets: Vec<AddTarget> = self
            .sessions()
            .iter()
            .filter(|session| session.is_connected() && !session.server_info.config.is_null())
            .filter_map(|session| session.id.clone())
            .filter(|id| *id != project.environment_id && !existing.contains(id))
            .filter(|id| self.env_permits(id, CLONE_START, &json!({"projectId": project.id})))
            .map(|id| {
                let option = self.option_for(&id, None);
                AddTarget { environment_id: id, label: option.label, machine: option.machine }
            })
            .collect();
        targets.sort_by_cached_key(|target| target.label.to_lowercase());
        targets
    }

    /// The machines a project's group already has a checkout on.
    fn project_machines(&self, project: &ProjectShell) -> Vec<SharedString> {
        let groups = self.project_groups();
        let group = groups.iter().find(|group| group.contains_project(project));
        group.map_or_else(Vec::new, |group| group.members.iter().map(|m| m.project.environment_id.clone()).collect())
    }

    /// `openAddProjectToMachine`: the dialog for `project`, on `target` when one was picked.
    pub(super) fn open_add_to_machine(
        &mut self,
        project: ScopedId,
        target: Option<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(shell) = self.shell.project(&project) else { return };
        // Read here: the dialog cannot read the workspace while this update holds it.
        let url = source_url(shell);
        let existing = self.project_machines(shell);
        let targets = self.add_project_targets(shell, &existing);
        let picked = targets.iter().find(|t| Some(&t.environment_id) == target.as_ref()).or_else(|| targets.first());
        let can_list = self.env_info(&project.env).is_some_and(|info| info.capability("ignoredFileListing"));
        let init = DialogInit {
            default_path: self.default_clone_path(picked.map(|t| t.environment_id.as_ref()), &url),
            listing: can_list.then(|| (shell.workspace_root.clone(), self.env_client(&project.env))),
            source_url: url,
        };
        let workspace = cx.entity();
        let view = cx.new(|cx| AddToMachineDialog::new(&workspace, project, target, init, window, cx));
        window.open_dialog(cx, move |dialog, _, _| {
            let view = view.clone();
            dialog.w(px(576.)).p_0().on_ok(move |_, window, cx| {
                view.update(cx, |view, cx| view.submit(window, cx));
                false
            })
        });
    }

    /// The folder on `target` a clone of `source_url` lands in by default: its add-project base
    /// plus the clone's own folder (`<host>/<owner>/<repo>` with the ghq layout).
    fn default_clone_path(&self, target: Option<&str>, source_url: &str) -> String {
        let info = target.and_then(|target| self.env_info(target));
        let base = info.and_then(|info| info.settings.get("addProjectBaseDirectory").as_str().map(str::to_owned));
        let ghq = info.is_some_and(|info| info.settings.get("ghqCloneLayout").as_bool() == Some(true));
        paths::clone_destination_path(
            &paths::add_project_initial_query(base.as_deref()),
            &paths::clone_directory_path(source_url, ghq, source_url),
        )
    }

    /// `addProjectToMachine` once its files are read: marks the copy, starts the clone, then
    /// lands on the new project. Errors go back to the dialog, which stays open.
    fn start_add_to_machine(&mut self, request: Request, window: &mut Window, cx: &mut Context<Self>) {
        let target = ScopedId::new(request.target.environment_id.clone(), request.project_id.as_str());
        let input = json!({
            "projectId": request.project_id,
            "title": request.title,
            "createdAt": crate::timestamp::iso(crate::timestamp::now_ms()),
            "remoteUrl": request.remote_url,
            "destinationPath": request.destination,
        });
        if !self.env_permits(&target.env, CLONE_START, &input) {
            let message = "This connection needs permission to write source control.".to_owned();
            return fail_add(self, &target, &request.dialog, message, cx);
        }
        let Some(client) = self.env_client(&target.env) else {
            let message = format!("{} is not connected.", request.target.label);
            return fail_add(self, &target, &request.dialog, message, cx);
        };
        // Marked before the start request, since the clone list can arrive before its reply.
        self.clones.silence(target.clone());
        if !request.reads.is_empty() {
            self.clones.copies.states.insert(target.clone(), FileCopy::Copying);
        }
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request(CLONE_START, input).await;
            let _ = this.update_in(cx, |this, window, cx| {
                let started = match result {
                    Ok(started) => started,
                    Err(error) => return fail_add(this, &target, &request.dialog, error.message(), cx),
                };
                this.suggest_app_access(&target.env, &request.remote_url, window, cx);
                if !request.reads.is_empty() {
                    let cwd = started["cwd"].as_str().unwrap_or(&request.destination).to_owned();
                    let pending = PendingCopy {
                        cwd,
                        subdirectory: request.subdirectory,
                        label: request.target.label,
                        reads: request.reads,
                        seen: false,
                    };
                    this.clones.copies.pending.insert(target.clone(), pending);
                    this.advance_file_copies(cx);
                }
                // A dialog the user closed meanwhile is gone already.
                if request.dialog.upgrade().is_some() {
                    window.close_dialog(cx);
                }
                this.open_cloned_project(&request.source, target, window, cx);
            });
        })
        .detach();
    }

    /// `openClonedProject`: a draft open on the source project moves over with what was typed
    /// in it, picked by hand; otherwise a new draft opens in the clone, whose composer shows it.
    fn open_cloned_project(
        &mut self,
        source: &ScopedId,
        target: ScopedId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let on_source = matches!(
            &self.target,
            Some(Target::Draft { env, project_id, .. }) if *env == source.env && *project_id == source.id
        );
        self.settings = None;
        if on_source && self.page.is_none() && !self.target_sending() {
            self.move_draft_by_hand(target.env, target.id, cx);
            self.focus_composer(window, cx);
        } else {
            self.start_draft_in(target.env, target.id, window, cx);
        }
    }

    /// Writes waiting copies whose clone finished and drops those whose clone was removed.
    pub(super) fn advance_file_copies(&mut self, cx: &mut Context<Self>) {
        let keys: Vec<ScopedId> = self.clones.copies.pending.keys().cloned().collect();
        for key in keys {
            let phase = self.clones.clone_of(&key).map(|clone| clone.phase);
            let Some(pending) = self.clones.copies.pending.get_mut(&key) else { continue };
            match copy_step(pending.seen, phase) {
                CopyStep::Wait { seen } => pending.seen = seen,
                CopyStep::Drop => {
                    self.clones.copies.pending.remove(&key);
                    self.clones.copies.states.remove(&key);
                    cx.notify();
                }
                CopyStep::Write => {
                    if let Some(pending) = self.clones.copies.pending.remove(&key) {
                        self.write_file_copy(key, pending, cx);
                    }
                }
            }
        }
    }

    /// Writes each read file into the clone in turn, then clears the copy or records what failed.
    fn write_file_copy(&mut self, key: ScopedId, pending: PendingCopy, cx: &mut Context<Self>) {
        let client = self.env_client(&key.env);
        cx.spawn(async move |this, cx| {
            let mut failures = Vec::new();
            for (path, read) in pending.reads {
                let contents = match read {
                    Ok(contents) => contents,
                    Err(error) => {
                        failures.push(format!("{path}: {error}"));
                        continue;
                    }
                };
                let relative = if pending.subdirectory.is_empty() {
                    path.clone()
                } else {
                    format!("{}/{path}", pending.subdirectory)
                };
                let input = json!({"cwd": pending.cwd, "relativePath": relative, "contents": contents});
                let written = match &client {
                    Some(client) => client.request("projects.writeFile", input).await.map_err(|error| error.message()),
                    None => Err("This environment is not connected.".to_owned()),
                };
                if let Err(error) = written {
                    failures.push(format!("{path}: {error}"));
                }
            }
            let _ = this.update(cx, |this, cx| {
                if failures.is_empty() {
                    this.clones.copies.states.remove(&key);
                } else {
                    let files = if failures.len() == 1 { "file" } else { "files" };
                    let title = format!("Could not copy {} {files} to {}", failures.len(), pending.label);
                    this.clones.copies.states.insert(key, FileCopy::Failed { title, failures });
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// `dismissProjectFileCopy`.
    pub(super) fn dismiss_file_copy(&mut self, project: &ScopedId, cx: &mut Context<Self>) {
        if self.clones.copies.states.remove(project).is_some() {
            cx.notify();
        }
    }
}

/// A clone that did not start: no copy waits for it, and the dialog says why.
fn fail_add(
    this: &mut Workspace,
    target: &ScopedId,
    dialog: &WeakEntity<AddToMachineDialog>,
    message: String,
    cx: &mut Context<Workspace>,
) {
    this.clones.copies.states.remove(target);
    dialog.update(cx, |dialog, cx| dialog.failed(message, cx)).ok();
}

/// What the dialog starts from, read before it exists.
struct DialogInit {
    source_url: String,
    default_path: String,
    /// The source checkout and its connection, when its server lists ignored files.
    listing: Option<(String, Option<crate::rpc::RpcClient>)>,
}

/// What a submit needs once the source files are read.
struct Request {
    dialog: WeakEntity<AddToMachineDialog>,
    source: ScopedId,
    target: AddTarget,
    project_id: String,
    title: String,
    remote_url: String,
    destination: String,
    subdirectory: String,
    reads: Vec<(String, Result<String, String>)>,
}

/// What the dialog shows of its project, read from the workspace on each render.
struct Snapshot {
    name: String,
    title: String,
    workspace_root: String,
    source_url: String,
    repository_root: Option<String>,
    source_label: SharedString,
    source_machine: SharedString,
    targets: Vec<AddTarget>,
}

pub(super) struct AddToMachineDialog {
    workspace: WeakEntity<Workspace>,
    project: ScopedId,
    /// `targetId`: the machine picked, else the first.
    target: Option<SharedString>,
    protocol: Option<CloneProtocol>,
    path: Entity<InputState>,
    /// The folder as last set, which a paste resolves against.
    path_value: String,
    /// `pathOverride`: the user edited the folder, so a new machine keeps it.
    path_edited: bool,
    listing: Listing,
    /// `selected`; `None` keeps the default selection.
    selected: Option<HashSet<String>>,
    submitting: bool,
    error: Option<String>,
    /// Its project went away and the dialog closed, so it acts no more.
    ended: bool,
    scroll: ScrollHandle,
    _list: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl AddToMachineDialog {
    fn new(
        workspace: &Entity<Workspace>,
        project: ScopedId,
        target: Option<SharedString>,
        init: DialogInit,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let path = cx.new(|cx| InputState::new(window, cx).default_value(init.default_path.clone()));
        let subscriptions = vec![
            cx.subscribe_in(&path, window, |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Change => this.path_changed(window, cx),
                InputEvent::PressEnter { .. } => this.submit(window, cx),
                _ => {}
            }),
            // The request ends with its project, so one that comes back cannot reopen it.
            cx.observe_in(workspace, window, |this, workspace, window, cx| {
                if this.ended {
                    return;
                }
                if workspace.read(cx).shell.project(&this.project).is_none() {
                    this.ended = true;
                    return window.close_dialog(cx);
                }
                this.sync_default_path(window, cx);
                cx.notify();
            }),
        ];
        let (listing, list_task) = match init.listing {
            None => (Listing::Unsupported, None),
            Some((_, None)) => (Listing::Failed("This environment is not connected.".to_owned()), None),
            Some((cwd, Some(client))) => {
                let task = cx.spawn(async move |this, cx| {
                    let result = client.request("projects.listIgnoredFiles", json!({"cwd": cwd})).await;
                    let _ = this.update(cx, |this, cx| {
                        this.listing = match result {
                            Ok(value) => parse_listing(&value),
                            Err(error) => Listing::Failed(error.message()),
                        };
                        cx.notify();
                    });
                });
                (Listing::Loading, Some(task))
            }
        };
        Self {
            workspace: workspace.downgrade(),
            project,
            target,
            protocol: paths::clone_url_protocol(&init.source_url),
            path,
            path_value: init.default_path,
            path_edited: false,
            listing,
            selected: None,
            submitting: false,
            error: None,
            ended: false,
            scroll: ScrollHandle::new(),
            _list: list_task,
            _subscriptions: subscriptions,
        }
    }

    fn snapshot(&self, cx: &App) -> Option<Snapshot> {
        let workspace = self.workspace.upgrade()?;
        let this = workspace.read(cx);
        let project = this.shell.project(&self.project)?;
        let identity = project.repository_identity.as_ref();
        let existing = this.project_machines(project);
        let info = this.env_info(&self.project.env);
        Some(Snapshot {
            name: identity.and_then(|i| i.display_name.clone()).unwrap_or_else(|| project.title.clone()),
            title: project.title.clone(),
            workspace_root: project.workspace_root.clone(),
            source_url: source_url(project),
            repository_root: identity.and_then(|i| i.root_path.clone()),
            source_label: info
                .and_then(|info| info.environment_label.clone())
                .map_or_else(|| "This machine".into(), SharedString::from),
            source_machine: info.map_or("server", |info| info.machine_kind()).to_owned().into(),
            targets: this.add_project_targets(project, &existing),
        })
    }

    /// The machine the clone goes to: the one picked while it can still take it, else the first.
    fn target_of<'a>(&self, targets: &'a [AddTarget]) -> Option<&'a AddTarget> {
        targets.iter().find(|t| Some(&t.environment_id) == self.target.as_ref()).or_else(|| targets.first())
    }

    /// Keeps the folder on the target's default until the user edits it.
    fn sync_default_path(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.path_edited {
            return;
        }
        let Some(snapshot) = self.snapshot(cx) else { return };
        let Some(workspace) = self.workspace.upgrade() else { return };
        let target = self.target_of(&snapshot.targets).map(|t| t.environment_id.as_ref());
        let path = workspace.read(cx).default_clone_path(target, &snapshot.source_url);
        if path != self.path_value {
            self.path_value.clone_from(&path);
            self.path.update(cx, |input, cx| input.set_value(path, window, cx));
        }
    }

    fn path_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let typed = self.path.read(cx).value().to_string();
        let resolved = resolve_pasted_path(&self.path_value, &typed);
        if resolved != typed {
            self.path.update(cx, |input, cx| input.set_value(resolved.clone(), window, cx));
        }
        self.path_value = resolved;
        self.path_edited = true;
        cx.notify();
    }

    fn pick_target(&mut self, environment: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        self.target = Some(environment);
        self.sync_default_path(window, cx);
        cx.notify();
    }

    fn selection(&self) -> HashSet<String> {
        self.selected.clone().unwrap_or_else(|| default_selection(self.listing.files()))
    }

    fn toggle_file(&mut self, path: &str, cx: &mut Context<Self>) {
        let mut selected = self.selection();
        if !selected.remove(path) {
            selected.insert(path.to_owned());
        }
        self.selected = Some(selected);
        cx.notify();
    }

    /// All picks every copyable file; None, shown once they all are, picks none.
    fn toggle_all(&mut self, cx: &mut Context<Self>) {
        let copyable: HashSet<String> =
            self.listing.files().iter().filter(|f| !f.too_large()).map(|f| f.path.clone()).collect();
        self.selected = Some(if self.selection().len() == copyable.len() { HashSet::new() } else { copyable });
        cx.notify();
    }

    fn failed(&mut self, message: String, cx: &mut Context<Self>) {
        self.error = Some(if message.trim().is_empty() { "Could not start the clone.".to_owned() } else { message });
        self.submitting = false;
        cx.notify();
    }

    /// Submitting before the file list loads would silently skip the default env file copies.
    fn unavailable(&self, snapshot: Option<&Snapshot>) -> bool {
        snapshot.is_none_or(|s| s.targets.is_empty()) || self.listing == Listing::Loading
    }

    /// `submit`: reads the picked files from the source first, so the copy reflects them as they
    /// were when the user asked, then hands over to the workspace.
    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let snapshot = self.snapshot(cx);
        if self.submitting || self.unavailable(snapshot.as_ref()) {
            return;
        }
        let Some(snapshot) = snapshot else { return };
        let Some(target) = self.target_of(&snapshot.targets).cloned() else { return };
        let destination = self.path.read(cx).value().trim().to_owned();
        if destination.is_empty() {
            self.error = Some("Choose a folder on the other machine.".to_owned());
            return cx.notify();
        }
        let Some(workspace) = self.workspace.upgrade() else { return };
        let source_client = workspace.read(cx).env_client(&self.project.env);
        let selected = self.selection();
        let files: Vec<String> =
            self.listing.files().iter().filter(|f| selected.contains(&f.path)).map(|f| f.path.clone()).collect();
        self.submitting = true;
        self.error = None;
        cx.notify();
        let request = Request {
            dialog: cx.weak_entity(),
            source: self.project.clone(),
            remote_url: self.remote_url(&snapshot.source_url),
            subdirectory: project_subdirectory(snapshot.repository_root.as_deref(), &snapshot.workspace_root),
            target,
            project_id: new_id(),
            title: snapshot.title,
            destination,
            reads: Vec::new(),
        };
        let cwd = snapshot.workspace_root;
        cx.spawn_in(window, async move |_, cx| {
            let reads = futures::future::join_all(files.into_iter().map(|path| {
                let (client, cwd) = (source_client.clone(), cwd.clone());
                async move {
                    let read = match client {
                        Some(client) => {
                            let input = json!({"cwd": cwd, "relativePath": path});
                            match client.request("projects.readFile", input).await {
                                Ok(file) if file["truncated"] == true => Err("Too large to copy.".to_owned()),
                                Ok(file) => Ok(file["contents"].as_str().unwrap_or_default().to_owned()),
                                Err(error) => Err(error.message()),
                            }
                        }
                        None => Err("This environment is not connected.".to_owned()),
                    };
                    (path, read)
                }
            }))
            .await;
            let _ = cx.update(|window, cx| {
                workspace.update(cx, |this, cx| this.start_add_to_machine(Request { reads, ..request }, window, cx));
            });
        })
        .detach();
    }

    /// The URL over the picked protocol, else as the source has it.
    fn remote_url(&self, source_url: &str) -> String {
        self.protocol
            .and_then(|protocol| paths::clone_url_for_protocol(source_url, protocol))
            .unwrap_or_else(|| source_url.to_owned())
    }

    fn render_diagram(
        &self,
        snapshot: Option<&Snapshot>,
        target: Option<&AddTarget>,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let muted = t.muted_foreground;
        let mono =
            |text: String| div().min_w_0().truncate().font_family(t.fonts.mono).text_xs().text_color(muted).child(text);
        let machine = |machine: &str, label: SharedString| {
            h_flex()
                .min_w_0()
                .gap(px(6.))
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .child(Icon::new(machine_icon(machine)).size(px(14.)).flex_none())
                .child(div().min_w_0().truncate().child(label))
        };
        let source = v_flex()
            .flex_1()
            .min_w_0()
            .gap(px(4.))
            .child(machine(
                snapshot.map_or("server", |s| s.source_machine.as_ref()),
                snapshot.map_or_else(|| "This machine".into(), |s| s.source_label.clone()),
            ))
            .child(mono(snapshot.map(|s| s.workspace_root.clone()).unwrap_or_default()));
        let count = self.selection().len();
        let arrow = v_flex()
            .w(px(112.))
            .flex_none()
            .items_center()
            .gap(px(2.))
            .text_xs()
            .text_color(muted)
            .child("git clone")
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .child(div().h(px(1.)).flex_1().bg(t.border))
                    .child(Icon::new(IconName::ArrowRight).size(px(14.)).ml(px(-4.))),
            )
            .child(files_summary(count));
        let targets = snapshot.map_or(&[][..], |s| s.targets.as_slice());
        let destination: AnyElement = if targets.len() > 1 {
            let view = cx.entity();
            let picked = target.map(|t| t.environment_id.clone());
            let choices: Vec<AddTarget> = targets.to_vec();
            let mut trigger = Button::new("add-to-machine-target").outline().small().dropdown_caret(true);
            if let Some(target) = target {
                trigger = trigger.icon(Icon::new(machine_icon(&target.machine))).label(target.label.clone());
            }
            trigger
                .tooltip("Machine to add the project to")
                .dropdown_menu_with_anchor(gpui_kit::Anchor::TopLeft, move |menu, window, _| {
                    choices.iter().fold(menu, |menu, choice| {
                        let id = choice.environment_id.clone();
                        let item = PopupMenuItem::new(choice.label.clone())
                            .icon(Icon::new(machine_icon(&choice.machine)))
                            .checked(picked.as_ref() == Some(&id))
                            .on_click(window.listener_for(&view, move |this, _, window, cx| {
                                this.pick_target(id.clone(), window, cx);
                            }));
                        menu.item(item)
                    })
                })
                .into_any_element()
        } else {
            match target {
                Some(target) => machine(&target.machine, target.label.clone()).into_any_element(),
                None => {
                    h_flex().text_sm().font_weight(FontWeight::MEDIUM).child("No machine available").into_any_element()
                }
            }
        };
        h_flex()
            .items_center()
            .gap(px(12.))
            .child(source)
            .child(arrow)
            .child(v_flex().flex_1().min_w_0().gap(px(4.)).child(destination).child(mono(self.path_value.clone())))
            .into_any_element()
    }

    fn render_protocol(&self, snapshot: Option<&Snapshot>, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let source_url = snapshot.map(|s| s.source_url.as_str()).unwrap_or_default();
        let both = paths::clone_url_for_protocol(source_url, CloneProtocol::Ssh).is_some()
            && paths::clone_url_for_protocol(source_url, CloneProtocol::Https).is_some();
        let toggle = |id: &'static str, label: &'static str, protocol: CloneProtocol| {
            Button::new(id).ghost().small().label(label).selected(self.protocol == Some(protocol)).on_click(
                cx.listener(move |this, _, _, cx| {
                    this.protocol = Some(protocol);
                    cx.notify();
                }),
            )
        };
        let toggles = both.then(|| {
            h_flex().gap(px(4.)).child(toggle("add-to-machine-ssh", "SSH", CloneProtocol::Ssh)).child(toggle(
                "add-to-machine-https",
                "HTTPS",
                CloneProtocol::Https,
            ))
        });
        v_flex()
            .gap(px(6.))
            .child(field_label("Clone over", t))
            .children(toggles)
            .child(
                div()
                    .truncate()
                    .font_family(t.fonts.mono)
                    .text_xs()
                    .text_color(t.muted_foreground)
                    .child(self.remote_url(source_url)),
            )
            .into_any_element()
    }

    fn render_files(&self, t: &Tokens, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.listing == Listing::Unsupported {
            return None;
        }
        let muted = t.muted_foreground;
        let files = self.listing.files();
        let selected = self.selection();
        let copyable = files.iter().filter(|f| !f.too_large()).count();
        let selected_bytes: u64 = files.iter().filter(|f| selected.contains(&f.path)).map(|f| f.bytes).sum();
        let summary = (copyable > 0).then(|| {
            let mut text = format!("{} of {}", selected.len(), files.len());
            if selected_bytes > 0 {
                #[expect(clippy::cast_precision_loss, reason = "file sizes far below 2^52")]
                let bytes = format_bytes(selected_bytes as f64);
                text.push_str(" \u{b7} ");
                text.push_str(&bytes);
            }
            let all = selected.len() == copyable;
            h_flex().gap(px(8.)).items_center().child(div().text_xs().text_color(muted).child(text)).child(
                Button::new("add-to-machine-all")
                    .ghost()
                    .xsmall()
                    .label(if all { "None" } else { "All" })
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_all(cx))),
            )
        });
        let body: AnyElement = match &self.listing {
            Listing::Failed(error) => {
                div().text_xs().text_color(t.error_foreground).child(error.clone()).into_any_element()
            }
            _ if files.is_empty() => {
                let text = if matches!(self.listing, Listing::Loaded { .. }) {
                    "No ignored files to copy."
                } else {
                    "Finding ignored files\u{2026}"
                };
                div().text_xs().text_color(muted).child(text).into_any_element()
            }
            _ => {
                let last = files.len() - 1;
                let rows = files.iter().enumerate().map(|(index, file)| {
                    self.render_file_row(file, selected.contains(&file.path), index == last, t, cx)
                });
                div()
                    .relative()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(t.border)
                    .child(
                        v_flex()
                            .id("add-to-machine-files")
                            .max_h(px(208.))
                            .overflow_y_scroll()
                            .track_scroll(&self.scroll)
                            .children(rows),
                    )
                    .vertical_scrollbar(&self.scroll)
                    .into_any_element()
            }
        };
        let truncated = matches!(self.listing, Listing::Loaded { truncated: true, .. })
            .then(|| div().text_xs().text_color(muted).child(format!("Showing the first {} files.", files.len())));
        Some(
            v_flex()
                .gap(px(6.))
                .child(h_flex().items_center().gap(px(8.)).child(div().flex_1().child(field_label("Copy ignored files", t))).children(summary))
                .child(body)
                .children(truncated)
                .child(div().text_xs().text_color(muted).child(
                    "Files are copied after the clone finishes. Folders git ignores whole, like node_modules, are left out.",
                ))
                .into_any_element(),
        )
    }

    /// `IgnoredFileRow`: the whole row toggles; a file over the copy limit cannot be picked.
    fn render_file_row(
        &self,
        file: &IgnoredFile,
        checked: bool,
        last: bool,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let too_large = file.too_large();
        let size: SharedString = if too_large {
            "Over 1 MB".into()
        } else {
            #[expect(clippy::cast_precision_loss, reason = "file sizes far below 2^52")]
            let bytes = file.bytes as f64;
            format_bytes(bytes).into()
        };
        let check = if too_large {
            div()
                .flex_none()
                .size(px(16.))
                .rounded(px(4.))
                .border_1()
                .border_color(t.input)
                .bg(t.background)
                .into_any_element()
        } else {
            let path = file.path.clone();
            check_box(
                ElementId::Name(format!("add-to-machine-check-{}", file.path).into()),
                if checked { Check::Checked } else { Check::Unchecked },
                format!("Copy {}", file.path),
                t,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.toggle_file(&path, cx);
            }))
            .into_any_element()
        };
        let path = file.path.clone();
        h_flex()
            .id(ElementId::Name(format!("add-to-machine-file-{}", file.path).into()))
            .items_center()
            .gap(px(8.))
            .px(px(10.))
            .py(px(6.))
            .text_sm()
            .when(!last, |el| el.border_b_1().border_color(t.border))
            .map(|el| {
                if too_large {
                    el.opacity(0.64).cursor_not_allowed()
                } else {
                    el.cursor_pointer().on_click(cx.listener(move |this, _, _, cx| this.toggle_file(&path, cx)))
                }
            })
            .child(check)
            .child(div().flex_1().min_w_0().truncate().font_family(t.fonts.mono).text_xs().child(file.path.clone()))
            .child(div().flex_none().text_xs().text_color(t.muted_foreground).child(size))
            .into_any_element()
    }
}

/// A field's label (`Label`).
fn field_label(text: impl Into<SharedString>, t: &Tokens) -> impl IntoElement {
    div().text_sm().font_weight(FontWeight::MEDIUM).text_color(t.foreground).child(text.into())
}

/// The project's remote: the origin's URL, else the locator's.
fn source_url(project: &ProjectShell) -> String {
    let identity = project.repository_identity.as_ref();
    identity
        .and_then(|i| i.origin.as_ref().and_then(|origin| origin.remote_url.clone()))
        .or_else(|| identity.and_then(|i| i.locator.as_ref()).map(|locator| locator.remote_url.clone()))
        .unwrap_or_default()
}

fn parse_listing(value: &Value) -> Listing {
    let files = value["files"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|file| {
            Some(IgnoredFile { path: file["path"].as_str()?.to_owned(), bytes: file["byteLength"].as_u64()? })
        })
        .collect();
    Listing::Loaded { files, truncated: value["truncated"] == true }
}

impl Render for AddToMachineDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *crate::theme::tokens(cx);
        let snapshot = self.snapshot(cx);
        let targets = snapshot.as_ref().map_or(&[][..], |s| s.targets.as_slice());
        let target = self.target_of(targets).cloned();
        let name = snapshot.as_ref().map(|s| s.name.clone()).unwrap_or_default();
        let folder_label = format!("Folder on {}", target.as_ref().map_or("the other machine", |t| t.label.as_ref()));
        let submit_label = if self.submitting {
            "Starting\u{2026}".to_owned()
        } else {
            format!("Clone to {}", target.as_ref().map_or("machine", |t| t.label.as_ref()))
        };
        let disabled = self.unavailable(snapshot.as_ref()) || self.submitting;
        v_flex()
            .child(DialogHeader::new().p_6().pb_3().child(DialogTitle::new().child("Add to another machine")).child(
                DialogDescription::new().child(format!("Clone {name} and bring the local files git does not track.")),
            ))
            .child(
                v_flex()
                    .px_6()
                    .pb_1()
                    .gap(px(16.))
                    .child(self.render_diagram(snapshot.as_ref(), target.as_ref(), &t, cx))
                    .child(self.render_protocol(snapshot.as_ref(), &t, cx))
                    .child(
                        v_flex().gap(px(6.)).child(field_label(folder_label, &t)).child(Input::new(&self.path).small()),
                    )
                    .children(self.render_files(&t, cx))
                    .children(
                        self.error.clone().map(|error| div().text_sm().text_color(t.error_foreground).child(error)),
                    ),
            )
            .child(
                DialogFooter::new()
                    .px_6()
                    .py_4()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new("add-to-machine-cancel")
                            .outline()
                            .small()
                            .label("Cancel")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("add-to-machine-submit")
                            .primary()
                            .small()
                            .label(submit_label)
                            .disabled(disabled)
                            .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, bytes: u64) -> IgnoredFile {
        IgnoredFile { path: path.to_owned(), bytes }
    }

    #[test]
    fn env_files_under_the_copy_limit_are_picked_by_default() {
        let files = [
            file(".env", 10),
            file("apps/web/.env.local", 10),
            file(".envrc", 10),
            file("worker/.dev.vars", 10),
            file(".env.", 10),
            file(".environment", 10),
            file("dist/bundle.js", 10),
            file(".env.production", COPYABLE_FILE_MAX_BYTES + 1),
        ];
        let mut picked: Vec<String> = default_selection(&files).into_iter().collect();
        picked.sort();
        assert_eq!(picked, [".env", ".envrc", "apps/web/.env.local", "worker/.dev.vars"]);
        assert!(!file("a", COPYABLE_FILE_MAX_BYTES).too_large(), "the limit itself still copies");
    }

    #[test]
    fn copied_files_keep_their_place_below_the_repository_root() {
        assert_eq!(project_subdirectory(Some("/code/mono"), "/code/mono/apps/web"), "apps/web");
        assert_eq!(project_subdirectory(Some("C:\\code\\mono\\"), "C:\\code\\mono\\apps\\web"), "apps/web");
        assert_eq!(project_subdirectory(Some("/code/mono"), "/code/mono"), "");
        assert_eq!(project_subdirectory(Some("/code/mono"), "/code/monorepo"), "", "a sibling is not below it");
        assert_eq!(project_subdirectory(None, "/code/mono/apps/web"), "");
    }

    #[test]
    fn a_copy_waits_for_its_clone_to_finish_and_ends_with_it() {
        assert_eq!(copy_step(false, None), CopyStep::Wait { seen: false }, "the clone list may not have it yet");
        assert_eq!(copy_step(false, Some(Phase::Running)), CopyStep::Wait { seen: true });
        assert_eq!(copy_step(true, Some(Phase::Failed)), CopyStep::Wait { seen: true }, "a failed clone can retry");
        assert_eq!(copy_step(true, Some(Phase::Cancelled)), CopyStep::Wait { seen: true });
        assert_eq!(copy_step(true, Some(Phase::Done)), CopyStep::Write);
        assert_eq!(copy_step(false, Some(Phase::Done)), CopyStep::Write);
        assert_eq!(copy_step(true, None), CopyStep::Drop, "a removed clone takes its copy with it");
    }

    #[test]
    fn listings_read_files_and_truncation() {
        let value = json!({"files": [{"path": ".env", "byteLength": 12}, {"path": "bad"}], "truncated": true});
        assert_eq!(parse_listing(&value), Listing::Loaded { files: vec![file(".env", 12)], truncated: true });
        assert_eq!(files_summary(0), "no files");
        assert_eq!(files_summary(1), "+ 1 file");
        assert_eq!(files_summary(3), "+ 3 files");
    }
}
