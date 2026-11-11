//! The Welcome page's Projects band (`WelcomePage.tsx`: `ProjectsSection`, `useProjectImport`,
//! `ImportCandidateList`; `useProjectScans`): projects found in the computer's agent history,
//! recent ones picked by default, created and given their history when setup starts.
//!
//! Scans are kept per computer for 30 s, as the web's query. The picks are kept per computer
//! on this device until setup finishes, so a visit to Settings or a restart keeps them. The
//! import runs while the page shows; leaving it during the import is refused, as the web's
//! `useBlocker` does.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Div, ElementId, FontWeight, InteractiveElement as _, IntoElement as _,
    ParentElement as _, SharedString, Stateful, StatefulInteractiveElement as _, Styled as _, Task, WeakEntity, Window,
    div, px, svg,
};
use serde_json::{Value, json};

use super::import::{self, Candidate, KnownRoots, ScanResult, Selection};
use super::welcome::{self, AgentState};
use crate::app::Workspace;
use crate::app::pages::settings::connections::ui::{Size, Variant, WebButton};
use crate::rpc::{RpcClient, RpcError};
use crate::theme::Tokens;

/// The agent session query's `staleTimeMs`.
const SCAN_STALE: Duration = Duration::from_secs(30);
const ORCHESTRATION_OPERATE: &str = "orchestration:operate";
const SCAN_LIMIT_MESSAGE: &str = "Scan limit reached. Some projects or conversations may be missing.";
const IMPORT_PERMISSION_MESSAGE: &str = "This connection cannot import projects or thread history.";
/// `formatEnvironmentQueryError`'s fallback.
const QUERY_FAILED: &str = "The environment request failed.";

#[derive(Default)]
pub(super) struct ProjectsStage {
    scans: HashMap<SharedString, Scan>,
    /// Each computer's picks as last read or saved; absent until read from this device.
    stored: HashMap<SharedString, Option<Selection>>,
    view: Option<View>,
    /// Numbers each mount, so an import from an earlier one never lands in this one.
    mounts: u64,
    /// The latest save of the picks; each save waits for the one before it.
    writes: Option<Task<()>>,
}

impl ProjectsStage {
    /// From Start until the landing: leaving now would drop the import.
    pub(super) fn holding(&self) -> bool {
        self.view.as_ref().is_some_and(|view| view.hold)
    }

    /// Leaving the page ends the band; its scans and picks stay.
    pub(super) fn leave(&mut self) {
        self.view = None;
    }
}

/// `useProjectScans`: one computer's last scan.
#[derive(Default)]
struct Scan {
    result: Option<ScanResult>,
    error: Option<String>,
    at: Option<Instant>,
    /// Bumped by each request, so a late answer to an older one is dropped.
    generation: u64,
    task: Option<Task<()>>,
}

impl Scan {
    fn pending(&self) -> bool {
        self.task.is_some() || self.at.is_none()
    }
}

/// The band while the page shows it for one computer.
struct View {
    environment: SharedString,
    reviewing: bool,
    /// Repositories folded shut (they open by default).
    closed_groups: HashSet<String>,
    other_open: bool,
    /// Import-source agents that were on when last seen; one more turned on scans again.
    sources_seen: Option<Vec<String>>,
    /// Starting before the scan answered keeps the stored picks after finishing.
    keep_choices: bool,
    hold: bool,
    run: ImportRun,
    _load: Option<Task<()>>,
}

/// `useProjectImport`'s refs: what earlier runs did, so a second Start resumes.
#[derive(Default)]
struct ImportRun {
    importing: bool,
    mount: u64,
    warning: String,
    imported_threads: HashMap<String, usize>,
    /// Picks imported in full.
    imported: HashMap<String, ProjectRef>,
    with_history: HashMap<String, ProjectRef>,
    last_selection: Vec<String>,
    /// A pick's project id and create command, kept so a retry repeats the same command.
    attempts: HashMap<String, (String, String)>,
    task: Option<Task<()>>,
}

type ProjectRef = (SharedString, String);

impl ImportRun {
    fn thread_total(&self) -> usize {
        self.last_selection.iter().filter_map(|key| self.imported_threads.get(key)).sum()
    }
}

/// What one pick needs next.
enum Step {
    Skip,
    Create {
        client: RpcClient,
        project_id: String,
        command_id: String,
    },
    Import {
        client: RpcClient,
        project_id: String,
    },
    /// The pick's computer is not connected.
    Failed,
}

/// What the Start button shows.
pub(super) struct StartState {
    pub importing: bool,
    pub label: String,
}

struct Finishing;

/// Where a computer's picks are kept on this device (`welcomeImportSelectionKey`).
fn selection_path(environment: &str) -> Option<PathBuf> {
    let name = format!("{}.json", crate::hex::lower_hex(environment.as_bytes()));
    Some(crate::settings::userdata_dir()?.join("welcome-import-selection").join(name))
}

fn read_selection(path: Option<PathBuf>) -> Option<Selection> {
    let bytes = std::fs::read(path?).ok()?;
    serde_json::from_slice::<Option<Selection>>(&bytes).ok().flatten()
}

impl Workspace {
    /// Leaving the Welcome page while an import runs is refused (`useBlocker`); otherwise it
    /// ends the bands.
    pub(super) fn sync_welcome_projects(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.welcome_open() && self.settings.is_none() {
            return;
        }
        if self.onboarding.projects.holding() {
            let toast = Notification::info("One moment.").title("Finishing setup").id::<Finishing>();
            window.push_notification(toast, cx);
            self.show_welcome(cx);
            return;
        }
        self.onboarding.projects.leave();
    }

    fn projects_view(&mut self, mount: u64) -> Option<&mut View> {
        self.onboarding.projects.view.as_mut().filter(|view| view.run.mount == mount)
    }

    /// Mounting the band reads the computer's stored picks and scans it unless a fresh scan is kept.
    fn mount_projects(&mut self, environment: &SharedString, cx: &mut Context<Self>) {
        let stage = &mut self.onboarding.projects;
        if stage.view.as_ref().is_some_and(|view| view.environment == *environment) {
            return;
        }
        stage.mounts += 1;
        let load = (!stage.stored.contains_key(environment)).then(|| {
            let (path, environment) = (selection_path(environment), environment.clone());
            cx.spawn(async move |this, cx| {
                let stored = cx.background_spawn(async move { read_selection(path) }).await;
                let _ = this.update(cx, |this, cx| {
                    // A pick made meanwhile wins over what was stored.
                    this.onboarding.projects.stored.entry(environment).or_insert(stored);
                    cx.notify();
                });
            })
        });
        stage.view = Some(View {
            environment: environment.clone(),
            reviewing: false,
            closed_groups: HashSet::new(),
            other_open: false,
            sources_seen: None,
            keep_choices: false,
            hold: false,
            run: ImportRun { mount: stage.mounts, ..ImportRun::default() },
            _load: load,
        });
        let fresh = stage.scans.get(environment).is_some_and(|scan| {
            scan.task.is_some() || (scan.error.is_none() && scan.at.is_some_and(|at| at.elapsed() < SCAN_STALE))
        });
        if !fresh {
            self.scan_projects(environment, cx);
        }
    }

    /// `agentSessions.scan`; a failure keeps the last result beside its error.
    fn scan_projects(&mut self, environment: &SharedString, cx: &mut Context<Self>) {
        let client = self.env_client(environment);
        let scan = self.onboarding.projects.scans.entry(environment.clone()).or_default();
        scan.generation += 1;
        let generation = scan.generation;
        // A computer not connected yet is scanned once it is (`render_projects`).
        let Some(client) = client else {
            scan.task = None;
            if scan.at.is_some() {
                scan.error = Some(QUERY_FAILED.into());
            }
            cx.notify();
            return;
        };
        let environment = environment.clone();
        scan.task = Some(cx.spawn(async move |this, cx| {
            let result = client.request("agentSessions.scan", json!({})).await;
            let _ = this.update(cx, |this, cx| {
                let scans = &mut this.onboarding.projects.scans;
                let Some(scan) = scans.get_mut(&environment).filter(|s| s.generation == generation) else { return };
                scan.task = None;
                scan.at = Some(Instant::now());
                match result.map_err(|error| error.message()).and_then(|value| ScanResult::parse(&environment, value)) {
                    Ok(result) => {
                        scan.result = Some(result);
                        scan.error = None;
                    }
                    Err(error) => {
                        let error = error.trim();
                        scan.error = Some(if error.is_empty() { QUERY_FAILED.into() } else { error.to_owned() });
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// The computer's scanned projects and the current picks among them.
    fn import_candidates(&self, environment: &str) -> Vec<&Candidate> {
        let scan = self.onboarding.projects.scans.get(environment).and_then(|scan| scan.result.as_ref());
        scan.map(|scan| scan.candidates.iter().collect()).unwrap_or_default()
    }

    fn stored_selection(&self, environment: &str) -> Option<&Selection> {
        self.onboarding.projects.stored.get(environment).and_then(Option::as_ref)
    }

    fn selected_keys(&self, environment: &str) -> HashSet<String> {
        let candidates = self.import_candidates(environment);
        let roots = KnownRoots::new(&self.shell.projects);
        let picks = import::picks(&candidates, &roots, self.stored_selection(environment), crate::timestamp::now_ms());
        picks.selected.iter().map(|c| c.key.clone()).collect()
    }

    /// `setImportSelection`: kept in memory first, so a failed save cannot undo a choice.
    fn store_import_selection(
        &mut self,
        environment: &SharedString,
        selection: Option<Selection>,
        cx: &mut Context<Self>,
    ) {
        let stage = &mut self.onboarding.projects;
        stage.stored.insert(environment.clone(), selection.clone());
        let path = selection_path(environment);
        let previous = stage.writes.take();
        stage.writes = Some(cx.background_spawn(async move {
            if let Some(previous) = previous {
                previous.await;
            }
            let Some(path) = path else { return };
            let value = serde_json::to_value(&selection).unwrap_or(Value::Null);
            if let Err(error) = crate::settings::write_atomically(&path, &value) {
                log::warn!("could not save the Welcome page's picks: {error}");
            }
        }));
        cx.notify();
    }

    /// `setSelectedKeys` among what the scan lists now.
    fn pick_projects(&mut self, keys: HashSet<String>, opted_out: bool, cx: &mut Context<Self>) {
        let Some(environment) = self.onboarding.projects.view.as_ref().map(|v| v.environment.clone()) else { return };
        if self.onboarding.projects.view.as_ref().is_some_and(|v| v.run.importing) {
            return;
        }
        let candidates = self.import_candidates(&environment);
        let selection = Selection::picked(&candidates, keys, opted_out);
        self.store_import_selection(&environment, Some(selection), cx);
    }

    /// A checkbox over `keys` (one project, a repository, or the other folders).
    fn toggle_projects(&mut self, keys: &[String], checked: bool, cx: &mut Context<Self>) {
        let Some(environment) = self.onboarding.projects.view.as_ref().map(|v| v.environment.clone()) else { return };
        let mut next = self.selected_keys(&environment);
        for key in keys {
            if checked {
                next.insert(key.clone());
            } else {
                next.remove(key);
            }
        }
        self.pick_projects(next, false, cx);
    }

    /// The main box: on picks the defaults, off leaves out every project, even later ones.
    fn toggle_all_projects(&mut self, cx: &mut Context<Self>) {
        let Some(environment) = self.onboarding.projects.view.as_ref().map(|v| v.environment.clone()) else { return };
        if !self.env_grants(&environment, ORCHESTRATION_OPERATE) {
            return;
        }
        let candidates = self.import_candidates(&environment);
        let roots = KnownRoots::new(&self.shell.projects);
        let picks = import::picks(&candidates, &roots, self.stored_selection(&environment), crate::timestamp::now_ms());
        // An indeterminate box turns on.
        let on = picks.partial || picks.selected.is_empty();
        let keys = if on { picks.default_keys.into_iter().collect() } else { HashSet::new() };
        self.pick_projects(keys, !on, cx);
    }

    /// `start`: imports the picks, then finishes setup. Without import access the projects
    /// are skipped.
    pub(super) fn start_welcome(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(view) = self.onboarding.projects.view.as_ref() else {
            self.finish_welcome(None, &super::welcome::ImportOutcome::default(), window, cx);
            return;
        };
        if view.run.importing {
            return;
        }
        let environment = view.environment.clone();
        let keep_choices = self.onboarding.projects.scans.get(&environment).is_none_or(|s| s.result.is_none());
        let selected: Vec<Candidate> = if self.env_grants(&environment, ORCHESTRATION_OPERATE) {
            let candidates = self.import_candidates(&environment);
            let roots = KnownRoots::new(&self.shell.projects);
            let picks =
                import::picks(&candidates, &roots, self.stored_selection(&environment), crate::timestamp::now_ms());
            picks.selected.into_iter().cloned().collect()
        } else {
            Vec::new()
        };
        if let Some(view) = self.onboarding.projects.view.as_mut() {
            view.keep_choices = keep_choices;
        }
        if selected.is_empty() {
            self.finish_after_import(window, cx);
            return;
        }
        // Stored now, so after a restart mid-import these stay checked even once they exist.
        self.pick_projects(selected.iter().map(|c| c.key.clone()).collect(), false, cx);
        self.run_import(selected, window, cx);
    }

    /// `runImport`: creates each pick's project unless it exists, then imports its history.
    fn run_import(&mut self, selection: Vec<Candidate>, window: &mut Window, cx: &mut Context<Self>) {
        let access = selection.iter().all(|c| self.env_grants(&c.environment, ORCHESTRATION_OPERATE));
        let Some(view) = self.onboarding.projects.view.as_mut() else { return };
        // Without access the permission message shows from the session's grants.
        if view.run.importing || !access {
            return;
        }
        let run = &mut view.run;
        run.importing = true;
        run.warning.clear();
        run.last_selection = selection.iter().map(|c| c.key.clone()).collect();
        let mount = run.mount;
        let mut complete = selection.iter().filter(|c| run.imported.contains_key(&c.key)).count();
        view.hold = true;
        run.task = Some(cx.spawn_in(window, async move |this, cx| {
            let mut skipped = 0;
            let mut rescan: HashSet<SharedString> = HashSet::new();
            for candidate in &selection {
                let Ok(Some(step)) = this.update(cx, |this, _| this.import_step(mount, candidate)) else { return };
                let (client, project_id) = match step {
                    Step::Skip => continue,
                    Step::Failed => {
                        rescan.insert(candidate.environment.clone());
                        continue;
                    }
                    Step::Import { client, project_id } => (client, project_id),
                    Step::Create { client, project_id, command_id } => {
                        let create = json!({
                            "type": "project.create",
                            "commandId": command_id,
                            "projectId": project_id,
                            "title": candidate.title,
                            "workspaceRoot": candidate.path,
                            "createWorkspaceRootIfMissing": false,
                            "defaultModelSelection": Value::Null,
                        });
                        let result = client.request("projects.mutate", create).await;
                        let Ok(Some(())) = this.update(cx, |this, _| this.projects_view(mount).map(|_| ())) else {
                            return;
                        };
                        if let Err(error) = result {
                            let failed = this.update(cx, |this, _| this.import_failed(mount, &candidate.key, &error));
                            if failed.unwrap_or(false) {
                                rescan.insert(candidate.environment.clone());
                            }
                            continue;
                        }
                        let Ok(true) =
                            this.update(cx, |this, cx| this.import_allowed(mount, &candidate.environment, cx))
                        else {
                            return;
                        };
                        (client, project_id)
                    }
                };
                let input = json!({ "projectId": project_id, "expectedWorkspaceRoot": candidate.path });
                let result = client.request("agentSessions.import", input).await;
                let Ok(Some(())) = this.update(cx, |this, _| this.projects_view(mount).map(|_| ())) else { return };
                match result {
                    Ok(value) => {
                        let count = |key: &str| value[key].as_u64().and_then(|n| usize::try_from(n).ok()).unwrap_or(0);
                        let (imported, skipped_here) = (count("importedCount"), count("skippedCount"));
                        skipped += skipped_here;
                        let reference: ProjectRef = (candidate.environment.clone(), project_id);
                        let _ = this.update(cx, |this, _| {
                            let Some(view) = this.projects_view(mount) else { return };
                            *view.run.imported_threads.entry(candidate.key.clone()).or_default() += imported;
                            if imported > 0 {
                                view.run.with_history.insert(candidate.key.clone(), reference.clone());
                            }
                            if skipped_here == 0 {
                                view.run.imported.insert(candidate.key.clone(), reference);
                            }
                        });
                        complete += usize::from(skipped_here == 0);
                    }
                    Err(error) => {
                        let failed = this.update(cx, |this, _| this.import_failed(mount, &candidate.key, &error));
                        if failed.unwrap_or(false) {
                            rescan.insert(candidate.environment.clone());
                        }
                    }
                }
            }
            let _ = this.update_in(cx, |this, window, cx| {
                for environment in &rescan {
                    this.scan_projects(environment, cx);
                }
                let Some(view) = this.projects_view(mount) else { return };
                view.hold = false;
                view.run.importing = false;
                let total = view.run.thread_total();
                if complete < view.run.last_selection.len() {
                    view.run.warning = import::import_warning(total, skipped);
                }
                this.finish_after_import(window, cx);
            });
        }));
        cx.notify();
    }

    /// The next step for one pick, or `None` when this run must stop.
    fn import_step(&mut self, mount: u64, candidate: &Candidate) -> Option<Step> {
        let access = self.env_grants(&candidate.environment, ORCHESTRATION_OPERATE);
        let client = self.env_client(&candidate.environment);
        let existing = import::resolve_project_id(&self.shell.projects, candidate);
        let view = self.projects_view(mount)?;
        if !access {
            view.hold = false;
            view.run.importing = false;
            return None;
        }
        if view.run.imported.contains_key(&candidate.key) {
            return Some(Step::Skip);
        }
        let Some(client) = client else {
            view.run.attempts.remove(&candidate.key);
            return Some(Step::Failed);
        };
        if let Some(project_id) = existing {
            return Some(Step::Import { client, project_id });
        }
        let (project_id, command_id) = view.run.attempts.entry(candidate.key.clone()).or_insert_with(|| {
            let project_id = crate::app::new_id();
            let command_id = format!("onboarding:project:create:{project_id}");
            (project_id, command_id)
        });
        Some(Step::Create { client, project_id: project_id.clone(), command_id: command_id.clone() })
    }

    /// Between creating and importing: the run goes on only while access remains.
    fn import_allowed(&mut self, mount: u64, environment: &str, cx: &mut Context<Self>) -> bool {
        let access = self.env_grants(environment, ORCHESTRATION_OPERATE);
        let Some(view) = self.projects_view(mount) else { return false };
        if !access {
            view.hold = false;
            view.run.importing = false;
            cx.notify();
        }
        access
    }

    /// A failed step forgets its attempt so a retry starts fresh; true when its computer
    /// should be scanned again. A dropped connection keeps the attempt to repeat it as is.
    fn import_failed(&mut self, mount: u64, key: &str, error: &RpcError) -> bool {
        if matches!(error, RpcError::Disconnected(_)) {
            return false;
        }
        log::debug!("welcome import step failed: {error}");
        let Some(view) = self.projects_view(mount) else { return false };
        view.run.attempts.remove(key);
        true
    }

    /// `finishAfterImport`: finishes setup in the project the import brought in, if any, and
    /// reports what was left out.
    fn finish_after_import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(view) = self.onboarding.projects.view.as_mut() else {
            self.finish_welcome(None, &super::welcome::ImportOutcome::default(), window, cx);
            return;
        };
        let run = &view.run;
        let landing = import::landing_project(&run.last_selection, &run.with_history, &run.imported);
        let warning = Some(run.warning.clone()).filter(|w| !w.is_empty());
        let outcome = super::welcome::ImportOutcome { warning, imported_threads: run.thread_total() };
        let (environment, keep_choices) = (view.environment.clone(), view.keep_choices);
        // Busy until setup is saved, so Start cannot run the import again meanwhile.
        view.run.importing = true;
        if self.finish_welcome(landing, &outcome, window, cx) {
            if !keep_choices {
                self.store_import_selection(&environment, None, cx);
            }
        } else if let Some(view) = self.onboarding.projects.view.as_mut() {
            view.run.importing = false;
        }
        cx.notify();
    }

    /// One more import-source agent turned on can find more history (`onImportSourceEnabled`).
    fn follow_import_sources(&mut self, environment: &SharedString, cx: &mut Context<Self>) {
        let Some(info) = self.env_info(environment).filter(|info| !info.config.is_null()) else { return };
        let mut on: Vec<String> = info
            .providers
            .iter()
            .filter(|p| import::SOURCES.iter().any(|(driver, _)| *driver == p.driver))
            .filter(|p| welcome::agent_state(p, &info.settings) != AgentState::Disabled)
            .map(|p| p.instance_id.clone())
            .collect();
        on.sort();
        let Some(view) = self.onboarding.projects.view.as_mut() else { return };
        let more = view.sources_seen.as_ref().is_some_and(|seen| on.iter().any(|id| !seen.contains(id)));
        view.sources_seen = Some(on);
        if more {
            self.scan_projects(environment, cx);
        }
    }

    /// `ProjectsSection` for `environment`, and what Start shows.
    pub(super) fn render_projects(
        &mut self,
        environment: SharedString,
        wide: bool,
        cx: &mut Context<Self>,
    ) -> (Option<AnyElement>, StartState) {
        self.mount_projects(&environment, cx);
        let unasked =
            self.onboarding.projects.scans.get(&environment).is_none_or(|s| s.task.is_none() && s.at.is_none());
        if unasked && self.env_client(&environment).is_some() {
            self.scan_projects(&environment, cx);
        }
        self.follow_import_sources(&environment, cx);
        let t = *crate::theme::tokens(cx);
        let can_write = self.env_grants(&environment, ORCHESTRATION_OPERATE);
        let stage = &self.onboarding.projects;
        let Some(view) = stage.view.as_ref() else {
            return (None, StartState { importing: false, label: "Start".into() });
        };
        let importing = view.run.importing;
        let scan = stage.scans.get(&environment);
        let candidates = self.import_candidates(&environment);
        let roots = KnownRoots::new(&self.shell.projects);
        let picks = import::picks(&candidates, &roots, self.stored_selection(&environment), crate::timestamp::now_ms());
        let (selected, new_count) = if can_write { (picks.selected.len(), picks.new_count) } else { (0, 0) };
        let start = StartState { importing, label: import::start_label(importing, selected, new_count) };
        let workspace = cx.weak_entity();
        let retry = {
            let (workspace, environment) = (workspace.clone(), environment.clone());
            WebButton::new("welcome-projects-retry", "Retry").variant(Variant::Ghost).on_click(move |_, _, cx| {
                let _ = workspace.update(cx, |this, cx| this.scan_projects(&environment, cx));
            })
        };
        let section =
            |content: AnyElement| super::agents::welcome_section("Projects", wide, &t, content).into_any_element();
        let error = scan.and_then(|scan| scan.error.clone());
        let truncated = scan.and_then(|scan| scan.result.as_ref()).is_some_and(ScanResult::truncated);

        if scan.is_none_or(Scan::pending) && candidates.is_empty() {
            let text = format!("Looking for projects in your {} history…", import::SOURCES_LABEL);
            let note = div().text_sm().line_height(px(40.)).text_color(t.muted_foreground).child(text);
            return (Some(section(note.into_any_element())), start);
        }
        if let Some(error) = error.as_ref().filter(|_| candidates.is_empty()) {
            let alert = h_flex()
                .min_h(px(40.))
                .justify_between()
                .gap(px(12.))
                .text_sm()
                .line_height(px(20.))
                .text_color(t.muted_foreground)
                .child(div().min_w_0().child(format!("Could not check projects. {error}")))
                .child(retry);
            return (Some(section(alert.into_any_element())), start);
        }
        if candidates.is_empty() {
            let note = truncated.then(|| {
                section(
                    div()
                        .text_xs()
                        .line_height(px(40.))
                        .text_color(t.muted_foreground)
                        .child(SCAN_LIMIT_MESSAGE)
                        .into_any_element(),
                )
            });
            return (note, start);
        }

        let checked = can_write && !picks.selected.is_empty();
        let mark = if can_write && picks.partial {
            Mark::Indeterminate
        } else if checked {
            Mark::Checked
        } else {
            Mark::Unchecked
        };
        let title = if checked {
            let n = picks.selected.len();
            format!("Import {n} {} and their threads", if n == 1 { "project" } else { "projects" })
        } else {
            "Import projects and their threads".to_owned()
        };
        let main_disabled = importing || !can_write;
        let main = clickable_row("welcome-projects-import", main_disabled, &t, {
            let workspace = workspace.clone();
            move |_, cx| {
                let _ = workspace.update(cx, Self::toggle_all_projects);
            }
        })
        .flex_1()
        .min_w_0()
        .gap(px(12.))
        .child(row_icon(check_mark(mark, main_disabled, &t)))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .text_sm()
                        .line_height(px(20.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(t.foreground)
                        .child(title),
                )
                .child(
                    div()
                        .text_xs()
                        .line_height(px(16.))
                        .text_color(t.muted_foreground)
                        .child(format!("From your {} history", import::SOURCES_LABEL)),
                ),
        );
        let reviewing = view.reviewing && can_write;
        let review = can_write.then(|| {
            let workspace = workspace.clone();
            let label = if view.reviewing { "Hide".to_owned() } else { format!("Review all {}", candidates.len()) };
            WebButton::new("welcome-projects-review", label)
                .variant(Variant::GhostMuted)
                .size(Size::Xs)
                .trailing_icon(if view.reviewing { Lucide::ChevronUp } else { Lucide::ChevronDown })
                .on_click(move |_, _, cx| {
                    let _ = workspace.update(cx, |this, cx| {
                        if let Some(view) = this.onboarding.projects.view.as_mut() {
                            view.reviewing = !view.reviewing;
                            cx.notify();
                        }
                    });
                })
        });
        let header = h_flex().min_h(px(40.)).py(px(4.)).gap(px(12.)).child(main).children(review);

        let list = reviewing.then(|| {
            let selected_keys: HashSet<&str> = picks.selected.iter().map(|c| c.key.as_str()).collect();
            let all = candidates.len();
            let count = picks.selected.len();
            let select = |id: &'static str, label: &'static str, disabled: bool, keys: HashSet<String>| {
                let workspace = workspace.clone();
                WebButton::new(id, label).variant(Variant::Ghost).disabled(disabled || importing).on_click(
                    move |_, _, cx| {
                        let keys = keys.clone();
                        let _ = workspace.update(cx, |this, cx| this.pick_projects(keys, false, cx));
                    },
                )
            };
            let every: HashSet<String> = candidates.iter().map(|c| c.key.clone()).collect();
            let bar = h_flex()
                .justify_between()
                .gap(px(12.))
                .px(px(8.))
                .text_xs()
                .line_height(px(16.))
                .text_color(t.muted_foreground)
                .child(format!("{count} of {all} selected"))
                .child(
                    h_flex()
                        .gap(px(4.))
                        .child(select("welcome-projects-all", "Select all", count == all, every))
                        .child(select("welcome-projects-none", "Select none", count == 0, HashSet::new())),
                );
            let rows =
                CandidateList { selected: &selected_keys, disabled: importing, view, workspace: &workspace, t: &t };
            let (groups, other) = import::group_projects(&candidates);
            let mut items: Vec<AnyElement> = Vec::new();
            for group in &groups {
                rows.group(group, &mut items);
            }
            rows.other(&other, &mut items);
            // The negative margin lines the list's checkboxes up with the one above it.
            v_flex().mx(px(-6.)).mt(px(4.)).min_w_0().child(bar).child(
                div()
                    .id("welcome-projects-list")
                    .mt(px(4.))
                    .max_h(px(320.))
                    .overflow_y_scroll()
                    .child(v_flex().gap(px(2.)).pr(px(12.)).children(items)),
            )
        });
        let truncated_note = truncated.then(|| {
            div().pl(px(32.)).text_xs().line_height(px(16.)).text_color(t.muted_foreground).child(SCAN_LIMIT_MESSAGE)
        });
        let error_again = error.map(|error| {
            h_flex()
                .justify_between()
                .gap(px(12.))
                .pl(px(32.))
                .text_sm()
                .line_height(px(20.))
                .text_color(t.muted_foreground)
                .child(div().min_w_0().child(format!("Could not check projects again. {error}")))
                .child(retry)
        });
        let denied = (!can_write).then(|| {
            div()
                .pt(px(4.))
                .pl(px(32.))
                .text_sm()
                .line_height(px(20.))
                .text_color(t.error_foreground)
                .child(IMPORT_PERMISSION_MESSAGE)
        });
        let content =
            v_flex().child(header).children(list).children(truncated_note).children(error_again).children(denied);
        (Some(section(content.into_any_element())), start)
    }
}

/// The review list's rows (`ImportCandidateList`, `ImportRepositoryGroup`, `ImportCandidateRow`).
struct CandidateList<'a> {
    selected: &'a HashSet<&'a str>,
    disabled: bool,
    view: &'a View,
    workspace: &'a WeakEntity<Workspace>,
    t: &'a Tokens,
}

impl CandidateList<'_> {
    fn toggle(&self, keys: Vec<String>, checked: bool) -> impl Fn(&mut Window, &mut App) + 'static {
        let workspace = self.workspace.clone();
        move |_, cx| {
            let _ = workspace.update(cx, |this, cx| this.toggle_projects(&keys, checked, cx));
        }
    }

    fn mark_of(&self, keys: &[&Candidate]) -> Mark {
        let count = keys.iter().filter(|c| self.selected.contains(c.key.as_str())).count();
        match count {
            0 => Mark::Unchecked,
            n if n == keys.len() => Mark::Checked,
            _ => Mark::Indeterminate,
        }
    }

    fn group(&self, group: &import::Group<'_>, items: &mut Vec<AnyElement>) {
        if let [only] = group.candidates.as_slice() {
            items.push(self.row(only, Some(&group.label), false));
            return;
        }
        let open = !self.view.closed_groups.contains(&group.key);
        let mark = self.mark_of(&group.candidates);
        let keys: Vec<String> = group.candidates.iter().map(|c| c.key.clone()).collect();
        let fold = {
            let (workspace, key) = (self.workspace.clone(), group.key.clone());
            move |_: &mut Window, cx: &mut App| {
                let _ = workspace.update(cx, |this, cx| {
                    if let Some(view) = this.onboarding.projects.view.as_mut() {
                        if !view.closed_groups.remove(&key) {
                            view.closed_groups.insert(key.clone());
                        }
                        cx.notify();
                    }
                });
            }
        };
        let label = div()
            .min_w_0()
            .truncate()
            .text_sm()
            .line_height(px(20.))
            .font_weight(FontWeight::MEDIUM)
            .text_color(self.t.foreground)
            .child(group.label.clone());
        let id = format!("welcome-projects-group-{}", group.key);
        let meta = row_meta(Some(&group.sources()), group.thread_count, group.last_active_at, self.t);
        items.push(self.header(&id, mark, keys, open, [label.into_any_element(), meta], fold));
        if open {
            items.extend(group.candidates.iter().map(|c| self.row(c, None, true)));
        }
    }

    fn other(&self, other: &[&Candidate], items: &mut Vec<AnyElement>) {
        if other.is_empty() {
            return;
        }
        let open = self.view.other_open;
        let mark = self.mark_of(other);
        let keys: Vec<String> = other.iter().map(|c| c.key.clone()).collect();
        let fold = {
            let workspace = self.workspace.clone();
            move |_: &mut Window, cx: &mut App| {
                let _ = workspace.update(cx, |this, cx| {
                    if let Some(view) = this.onboarding.projects.view.as_mut() {
                        view.other_open = !view.other_open;
                        cx.notify();
                    }
                });
            }
        };
        let label = div()
            .min_w_0()
            .truncate()
            .text_sm()
            .line_height(px(20.))
            .text_color(self.t.muted_foreground)
            .child("Other folders");
        let n = other.len();
        let count = div()
            .ml_auto()
            .flex_none()
            .text_xs()
            .line_height(px(16.))
            .text_color(self.t.muted_foreground)
            .child(format!("{n} {}", if n == 1 { "folder" } else { "folders" }));
        let title = [label.into_any_element(), count.into_any_element()];
        items.push(self.header("welcome-projects-other", mark, keys, open, title, fold));
        if open {
            items.extend(other.iter().map(|c| self.row(c, None, true)));
        }
    }

    /// A fold's row: its own checkbox, then the trigger that opens it.
    fn header(
        &self,
        id: &str,
        mark: Mark,
        keys: Vec<String>,
        open: bool,
        title: [AnyElement; 2],
        fold: impl Fn(&mut Window, &mut App) + 'static,
    ) -> AnyElement {
        let t = self.t;
        let check =
            clickable_box(format!("{id}-check"), mark, self.disabled, t, self.toggle(keys, mark != Mark::Checked));
        let trigger = h_flex()
            .id(SharedString::from(format!("{id}-fold")))
            .flex_1()
            .min_w_0()
            .gap(px(6.))
            .cursor_pointer()
            .tab_index(0)
            .rounded(px(4.))
            .focus_visible(focus_ring(t))
            .on_click(move |_, window, cx| fold(window, cx))
            .child(
                Icon::new(if open { Lucide::ChevronDown } else { Lucide::ChevronRight })
                    .size(px(14.))
                    .flex_none()
                    .text_color(t.muted_foreground),
            )
            .children(title);
        let hover = t.muted.opacity(0.4);
        h_flex()
            .gap(px(10.))
            .rounded(px(6.))
            .px(px(8.))
            .py(px(6.))
            .hover(move |style| style.bg(hover))
            .child(check)
            .child(trigger)
            .into_any_element()
    }

    /// `ImportCandidateRow`: the whole row toggles, and hovering its text shows the full path.
    fn row(&self, candidate: &Candidate, label: Option<&str>, nested: bool) -> AnyElement {
        let t = self.t;
        let checked = self.selected.contains(candidate.key.as_str());
        let mark = if checked { Mark::Checked } else { Mark::Unchecked };
        let (name, parent) = import::split_path(&candidate.path);
        let path_text = |size: f32, color| {
            h_flex()
                .min_w_0()
                .font_family(t.fonts.mono)
                .text_size(px(size))
                .text_color(color)
                .when(!parent.is_empty(), |el| el.child(div().min_w_0().truncate().child(parent.clone())))
                .child(div().flex_none().max_w_full().truncate().child(name.clone()))
        };
        let full_path: SharedString = candidate.path.clone().into();
        let text = h_flex()
            .id(SharedString::from(format!("welcome-projects-path-{}", candidate.key)))
            .flex_1()
            .min_w_0()
            .gap(px(8.))
            .tooltip(move |window, cx| Tooltip::new(full_path.clone()).build(window, cx))
            .when_some(label, |el, label| {
                el.child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .line_height(px(20.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(t.foreground)
                        .child(label.to_owned()),
                )
            })
            .child(if label.is_some() { path_text(11., t.muted_foreground) } else { path_text(12., t.foreground) });
        let sources: Vec<&str> = candidate.sources.iter().map(String::as_str).collect();
        let meta =
            row_meta((!nested).then_some(&sources), candidate.thread_count, candidate.last_active_at.as_deref(), t);
        let id = format!("welcome-projects-row-{}", candidate.key);
        let toggle = self.toggle(vec![candidate.key.clone()], !checked);
        let hover = t.muted.opacity(0.4);
        clickable_row(id, self.disabled, t, toggle)
            .gap(px(10.))
            .rounded(px(6.))
            .px(px(8.))
            .py(px(6.))
            .when(nested, |el| el.pl(px(32.)))
            .when(!self.disabled, |el| el.hover(move |style| style.bg(hover)))
            .child(check_mark(mark, self.disabled, t))
            .child(text)
            .child(meta)
            .into_any_element()
    }
}

/// `ImportRowMeta`: source icons, thread count and age in fixed columns, so rows line up.
fn row_meta(sources: Option<&Vec<&str>>, threads: u64, last_active_at: Option<&str>, t: &Tokens) -> AnyElement {
    let age = last_active_at
        .and_then(crate::timestamp::parse_ms)
        .map(|at| crate::inbox::format_relative(crate::timestamp::now_ms() - at))
        .unwrap_or_default();
    let cell = |width: f32| h_flex().w(px(width)).flex_none().justify_end();
    h_flex()
        .ml_auto()
        .flex_none()
        .gap(px(4.))
        .text_xs()
        .line_height(px(16.))
        .text_color(t.muted_foreground)
        .children(import::SOURCES.iter().map(|(driver, _)| {
            let icon = sources.filter(|s| s.contains(driver)).and_then(|_| crate::assets::provider_icon(driver));
            div().size(px(16.)).flex_none().flex().items_center().justify_center().children(
                icon.map(|(path, tint)| svg().path(path).size(px(12.)).text_color(tint.unwrap_or(t.foreground))),
            )
        }))
        .child(cell(40.).child(threads.to_string()))
        .child(cell(36.).whitespace_nowrap().child(age))
        .into_any_element()
}

/// `RowIcon`.
fn row_icon(child: impl gpui_kit::IntoElement) -> Div {
    div().size(px(20.)).flex_none().flex().items_center().justify_center().child(child)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mark {
    Unchecked,
    Checked,
    Indeterminate,
}

fn focus_ring(t: &Tokens) -> impl Fn(gpui_kit::StyleRefinement) -> gpui_kit::StyleRefinement + 'static {
    let ring = t.ring;
    move |style| style.shadow(vec![gpui_kit::BoxShadow::new(px(0.), px(0.), ring).spread_radius(px(2.))])
}

/// `Checkbox`'s look (`ui/checkbox.tsx`): a dash when indeterminate.
fn check_mark(mark: Mark, disabled: bool, t: &Tokens) -> Div {
    let unchecked_bg = if t.dark { t.input.opacity(0.32) } else { t.background };
    let base = div()
        .size(px(16.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.))
        .border_1()
        .when(disabled, |el| el.opacity(0.64));
    match mark {
        Mark::Checked => base
            .border_color(t.primary)
            .bg(t.primary)
            .child(Icon::new(Lucide::Check).size(px(12.)).text_color(t.primary_foreground)),
        Mark::Indeterminate => base
            .border_color(t.input)
            .bg(unchecked_bg)
            .child(Icon::new(Lucide::Minus).size(px(12.)).text_color(t.foreground)),
        Mark::Unchecked => base.border_color(t.input).bg(unchecked_bg),
    }
}

/// A checkbox that is its own tab stop.
fn clickable_box(
    id: String,
    mark: Mark,
    disabled: bool,
    t: &Tokens,
    on_toggle: impl Fn(&mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let ring = focus_ring(t);
    check_mark(mark, disabled, t).id(ElementId::Name(id.into())).when(!disabled, |el| {
        el.cursor_pointer().tab_index(0).focus_visible(ring).on_click(move |_, window, cx| on_toggle(window, cx))
    })
}

/// A `<label>` around a checkbox: the whole row is one tab stop that toggles it.
fn clickable_row(
    id: impl Into<SharedString>,
    disabled: bool,
    t: &Tokens,
    on_toggle: impl Fn(&mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let ring = focus_ring(t);
    h_flex().id(ElementId::Name(id.into())).when(!disabled, |el| {
        el.cursor_pointer().tab_index(0).focus_visible(ring).on_click(move |_, window, cx| on_toggle(window, cx))
    })
}
