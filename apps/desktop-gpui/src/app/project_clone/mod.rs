//! Projects that are still being cloned (`ProjectCloneSnapshot`): each environment's
//! `subscribeProjectClones`, the lifecycle toasts (`ProjectCloneToastCoordinator.tsx`), the
//! composer banner and send hold of the project on screen (`useProjectCloneBanner.tsx`), their
//! Cancel, Retry and Remove project actions (`useRemoveClonedProject.ts`), the forge app
//! suggestion after a clone starts (`sourceControlAppAccessToast.ts`), and the ignored-file copy
//! Add to another machine owes a clone (`useProjectFileCopy`).

pub(super) mod paths;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::notification::{Notification, NotificationType};
use gpui_kit::component::{Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, Context, ElementId, IntoElement as _, ParentElement as _, SharedString, Styled as _, Task, Window,
    div, px,
};
use serde_json::{Value, json};

use super::add_to_machine::FileCopy;
use super::banners::{Variant, composer_banner};
use super::{Target, Workspace, new_id};
use crate::model::ScopedId;
use crate::rpc::{RpcClient, StreamEvent};
use crate::theme::Tokens;

/// A finished clone's toast stays this long (`timeout: 8_000`).
const DONE_TOAST: Duration = Duration::from_secs(8);
const NEEDS_SOURCE_CONTROL: &str = "This connection needs permission to write source control.";

/// `ProjectClonePhase`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Phase {
    Running,
    Done,
    Failed,
    Cancelled,
}

/// One `ProjectCloneSnapshot`, as far as the client shows it.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct CloneSnapshot {
    pub(super) project_id: String,
    destination_path: String,
    /// `repository.nameWithOwner`.
    repository: Option<String>,
    pub(super) phase: Phase,
    stage: String,
    percent: Option<u64>,
    detail: Option<String>,
    /// Git has printed nothing for a while; older servers omit it.
    stalled: bool,
    error: Option<String>,
}

impl CloneSnapshot {
    fn from_json(value: &Value) -> Option<Self> {
        let text = |key: &str| value[key].as_str().map(str::to_owned);
        let phase = match value["phase"].as_str()? {
            "running" => Phase::Running,
            "done" => Phase::Done,
            "failed" => Phase::Failed,
            "cancelled" => Phase::Cancelled,
            _ => return None,
        };
        Some(Self {
            project_id: text("projectId")?,
            destination_path: text("destinationPath")?,
            repository: value["repository"]["nameWithOwner"].as_str().map(str::to_owned),
            phase,
            stage: text("stage").unwrap_or_default(),
            percent: value["percent"].as_u64(),
            detail: text("detail"),
            stalled: value["stalled"].as_bool() == Some(true),
            error: text("error"),
        })
    }

    /// `projectCloneDisplayName`: the repository, else the destination's last folder.
    fn display_name(&self) -> String {
        if let Some(repository) = &self.repository {
            return repository.clone();
        }
        let last = self.destination_path.split(['/', '\\']).rfind(|segment| !segment.is_empty());
        last.unwrap_or(&self.destination_path).to_owned()
    }

    /// `projectCloneProgressSummary`: the stage, the percent, and git's last detail unless it stalled.
    fn progress_summary(&self) -> String {
        let stage = match self.stage.as_str() {
            "connecting" => "Connecting",
            "counting" => "Counting objects",
            "receiving" => "Receiving objects",
            "resolving" => "Resolving deltas",
            "checkout" => "Checking out files",
            _ => "Cloning",
        };
        let mut parts = vec![stage.to_owned()];
        if let Some(percent) = self.percent {
            parts.push(format!("{percent}%"));
        }
        // The detail carries git's last transfer speed, which a stall makes a lie.
        if self.stalled {
            parts.push("Stalled".to_owned());
        } else if let Some(detail) = self.detail.as_ref().filter(|detail| !detail.is_empty()) {
            parts.push(detail.clone());
        }
        parts.join(" · ")
    }

    /// `renderKey`: what the toast shows, so an unchanged clone leaves it alone.
    fn render_key(&self) -> String {
        format!(
            "{:?}:{}:{}:{}:{}:{}",
            self.phase,
            self.stage,
            self.percent.map(|p| p.to_string()).unwrap_or_default(),
            self.detail.as_deref().unwrap_or_default(),
            self.stalled,
            self.error.as_deref().unwrap_or_default()
        )
    }

    /// The toast's description for the clone's phase.
    fn toast_description(&self) -> String {
        match self.phase {
            Phase::Running => self.progress_summary(),
            Phase::Done | Phase::Cancelled => self.destination_path.clone(),
            Phase::Failed => self.error.clone().unwrap_or_else(|| "The clone failed.".to_owned()),
        }
    }
}

/// The `ProjectCloneListEvent`: every clone the environment tracks.
fn parse_clones(value: &Value) -> Vec<CloneSnapshot> {
    value.as_array().into_iter().flatten().filter_map(CloneSnapshot::from_json).collect()
}

/// Marks clone toasts' ids.
struct CloneToast;

fn toast_id(key: &ScopedId) -> ElementId {
    ElementId::Name(format!("{}\u{1f}{}", key.env, key.id).into())
}

/// A clone's open toast: what it shows and the description it reads while it stays open.
struct Tracked {
    key: String,
    phase: Phase,
    description: Rc<RefCell<SharedString>>,
}

#[derive(Debug, PartialEq, Eq)]
enum Effect {
    Close(ScopedId),
    /// A toast for the clone's new phase, replacing any it had.
    Show(ScopedId),
}

/// Every environment's clones and the toasts open for them.
#[derive(Default)]
pub(super) struct ProjectClones {
    by_env: HashMap<SharedString, Vec<CloneSnapshot>>,
    toasts: HashMap<ScopedId, Tracked>,
    /// `silenceProjectCloneToasts`: clones whose project the user has looked at, whose progress
    /// and success the composer showed instead. Failures still toast.
    silenced: HashSet<ScopedId>,
    /// Closes each finished clone's toast after [`DONE_TOAST`].
    expiries: HashMap<ScopedId, Task<()>>,
    /// Ignored files Add to another machine copies into its clones.
    pub(super) copies: super::add_to_machine::FileCopies,
}

impl ProjectClones {
    pub(super) fn clone_of(&self, project: &ScopedId) -> Option<&CloneSnapshot> {
        self.by_env.get(&project.env)?.iter().find(|clone| clone.project_id == project.id)
    }

    /// `silenceProjectCloneToasts`: the clone's project is where the user lands, whose composer
    /// shows its progress and success.
    pub(super) fn silence(&mut self, project: ScopedId) {
        self.silenced.insert(project);
    }

    /// `EnvironmentCloneToasts`' effect for one environment, `viewing` being the project whose
    /// draft is on screen there.
    fn plan(&mut self, env: &SharedString, viewing: Option<&str>) -> Vec<Effect> {
        let mut effects = Vec::new();
        let mut seen = HashSet::new();
        for clone in self.by_env.get(env).into_iter().flatten() {
            let key = ScopedId::new(env.clone(), clone.project_id.as_str());
            seen.insert(key.clone());
            let render = clone.render_key();
            let silenced = viewing == Some(clone.project_id.as_str())
                || (self.silenced.contains(&key) && matches!(clone.phase, Phase::Running | Phase::Done));
            if viewing == Some(clone.project_id.as_str()) {
                self.silenced.insert(key.clone());
            } else if self.toasts.get(&key).is_some_and(|tracked| tracked.key == render) {
                continue;
            }
            if silenced {
                if self.toasts.remove(&key).is_some() {
                    effects.push(Effect::Close(key));
                }
                continue;
            }
            let description: SharedString = clone.toast_description().into();
            match self.toasts.get_mut(&key) {
                // The open toast reads its description, so progress updates it in place.
                Some(tracked) if tracked.phase == clone.phase => {
                    tracked.key = render;
                    *tracked.description.borrow_mut() = description;
                }
                _ => {
                    let description = Rc::new(RefCell::new(description));
                    self.toasts.insert(key.clone(), Tracked { key: render, phase: clone.phase, description });
                    effects.push(Effect::Show(key));
                }
            }
        }
        let gone: Vec<ScopedId> =
            self.toasts.keys().filter(|key| key.env == *env && !seen.contains(*key)).cloned().collect();
        for key in gone {
            // A finished clone's toast runs out on its own.
            if self.toasts.remove(&key).is_some_and(|tracked| tracked.phase != Phase::Done) {
                effects.push(Effect::Close(key));
            }
        }
        effects
    }

    /// Drops the environments `live` no longer lists, returning their toasts to close.
    fn forget_environments(&mut self, live: impl Fn(&str) -> bool) -> Vec<ScopedId> {
        self.by_env.retain(|env, _| live(env));
        self.silenced.retain(|key| live(&key.env));
        let gone: Vec<ScopedId> = self.toasts.keys().filter(|key| !live(&key.env)).cloned().collect();
        for key in &gone {
            self.toasts.remove(key);
        }
        gone
    }
}

fn toast_error(window: &mut Window, cx: &mut App, title: &'static str, message: impl Into<SharedString>) {
    window.push_notification(Notification::error(message).title(title), cx);
}

impl Workspace {
    /// `projectClones` for one environment. A server without clone tracking, or a connection
    /// not allowed to read it, ends the stream, which leaves the environment without clones.
    pub(super) fn subscribe_project_clones(
        &mut self,
        environment: SharedString,
        client: &RpcClient,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        let stream = client.stream("subscribeProjectClones", json!({}));
        cx.spawn_in(window, async move |this, cx| {
            while let Some(event) = stream.next().await {
                let mut latest = match event {
                    StreamEvent::Item(value) => value,
                    StreamEvent::End(result) => {
                        if let Err(error) = result {
                            log::debug!("subscribeProjectClones ended: {error}");
                        }
                        break;
                    }
                };
                // Each event lists every clone, so only the newest of a burst matters.
                while let Some(StreamEvent::Item(next)) = stream.try_next() {
                    latest = next;
                }
                let clones = parse_clones(&latest);
                let updated = this.update_in(cx, |this, window, cx| {
                    this.clones.by_env.insert(environment.clone(), clones);
                    this.sync_clone_toasts(window, cx);
                    this.advance_file_copies(cx);
                    cx.notify();
                });
                if updated.is_err() {
                    break;
                }
            }
        })
    }

    /// Opens, updates and closes clone toasts to match every environment's clones and the
    /// draft on screen.
    pub(super) fn sync_clone_toasts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let live: HashSet<SharedString> = self.sessions().iter().filter_map(|session| session.id.clone()).collect();
        let mut effects: Vec<Effect> =
            self.clones.forget_environments(|env| live.contains(env)).into_iter().map(Effect::Close).collect();
        let viewing = match &self.target {
            Some(Target::Draft { env, project_id, .. }) if self.page.is_none() => {
                Some((env.clone(), project_id.clone()))
            }
            _ => None,
        };
        let envs: Vec<SharedString> = self.clones.by_env.keys().cloned().collect();
        for env in envs {
            let viewed = viewing.as_ref().filter(|(viewed, _)| *viewed == env).map(|(_, project)| project.as_str());
            effects.extend(self.clones.plan(&env, viewed));
        }
        for effect in effects {
            match effect {
                Effect::Close(key) => {
                    self.clones.expiries.remove(&key);
                    window.remove_notification1::<CloneToast>(toast_id(&key), cx);
                }
                Effect::Show(key) => self.show_clone_toast(key, window, cx),
            }
        }
    }

    fn show_clone_toast(&mut self, key: ScopedId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(clone) = self.clones.clone_of(&key).cloned() else { return };
        let Some(description) = self.clones.toasts.get(&key).map(|tracked| tracked.description.clone()) else {
            return;
        };
        let name = clone.display_name();
        let workspace = cx.weak_entity();
        let muted = crate::theme::tokens(cx).muted_foreground;
        let base = Notification::new().id1::<CloneToast>(toast_id(&key));
        // `dismiss`: the toast closes as the action runs, as Open project's does.
        let action = |label: &'static str, dismiss: bool, run: ToastAction| {
            let (workspace, key) = (workspace.clone(), key.clone());
            move |_: &mut Notification, _: &mut Window, cx: &mut Context<Notification>| {
                let (workspace, key) = (workspace.clone(), key.clone());
                Button::new(SharedString::from(format!("clone-toast-{label}"))).label(label).on_click(cx.listener(
                    move |toast, _, window, cx| {
                        if dismiss {
                            toast.dismiss(window, cx);
                        }
                        let key = key.clone();
                        let _ = workspace.update(cx, |this, cx| run(this, key, window, cx));
                    },
                ))
            }
        };
        let toast = match clone.phase {
            Phase::Running => {
                self.clones.expiries.remove(&key);
                base.icon(Icon::new(IconName::LoaderCircle).text_color(muted))
                    .title(format!("Cloning {name}"))
                    .content(description_content(description, None))
                    .action(action("Cancel", false, |this, key, window, cx| {
                        this.run_clone_action(key, "projectClone.cancel", "Failed to cancel clone", window, cx);
                    }))
            }
            Phase::Done => {
                self.expire_clone_toast(key.clone(), window, cx);
                base.with_type(NotificationType::Success)
                    .title(format!("Cloned {name}"))
                    .content(description_content(description, None))
                    .action(action("Open project", true, open_project))
            }
            Phase::Failed | Phase::Cancelled => {
                self.clones.expiries.remove(&key);
                let cancelled = clone.phase == Phase::Cancelled;
                let remove = {
                    let (workspace, key) = (workspace.clone(), key.clone());
                    Rc::new(move |window: &mut Window, cx: &mut App| {
                        let key = key.clone();
                        let _ = workspace.update(cx, |this, cx| this.remove_cloned_project(key, window, cx));
                    }) as RemoveAction
                };
                base.with_type(if cancelled { NotificationType::Info } else { NotificationType::Error })
                    .title(if cancelled {
                        format!("Cancelled cloning {name}")
                    } else {
                        format!("Failed to clone {name}")
                    })
                    .content(description_content(description, Some(remove)))
                    .action(action("Retry", false, |this, key, window, cx| {
                        this.run_clone_action(key, "projectClone.retry", "Failed to retry clone", window, cx);
                    }))
            }
        };
        window.push_notification(toast, cx);
    }

    /// Closes a finished clone's toast once [`DONE_TOAST`] has passed.
    fn expire_clone_toast(&mut self, key: ScopedId, window: &mut Window, cx: &mut Context<Self>) {
        let task_key = key.clone();
        let task = cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(DONE_TOAST).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.clones.expiries.remove(&task_key);
                window.remove_notification1::<CloneToast>(toast_id(&task_key), cx);
            });
        });
        self.clones.expiries.insert(key, task);
    }

    /// `cancelProjectClone` / `retryProjectClone`, which need `source-control:write` there.
    fn run_clone_action(
        &mut self,
        key: ScopedId,
        method: &'static str,
        title: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.env_grants(&key.env, super::git::SOURCE_CONTROL_WRITE) {
            return toast_error(window, cx, title, NEEDS_SOURCE_CONTROL);
        }
        let Some(client) = self.env_client(&key.env) else {
            return toast_error(window, cx, title, "This environment is not connected.");
        };
        cx.spawn_in(window, async move |this, cx| {
            if let Err(error) = client.request(method, json!({"projectId": key.id})).await {
                let _ = this.update_in(cx, |_, window, cx| toast_error(window, cx, title, error.message()));
            }
        })
        .detach();
    }

    /// `useRemoveClonedProject`: deletes the project, unforced so a thread made since the clone
    /// failed is not deleted with it, and lets its drafts go.
    fn remove_cloned_project(&mut self, key: ScopedId, window: &mut Window, cx: &mut Context<Self>) {
        const TITLE: &str = "Failed to remove project";
        if !self.can_operate(&key.env) {
            return toast_error(window, cx, TITLE, "This connection cannot change projects.");
        }
        let Some(client) = self.env_client(&key.env) else {
            return toast_error(window, cx, TITLE, "This environment is not connected.");
        };
        let mutation = json!({"type": "project.delete", "commandId": new_id(), "projectId": key.id});
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("projects.mutate", mutation).await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok(_) => this.forget_scoped_project_drafts(&key, cx),
                Err(error) => toast_error(window, cx, TITLE, error.message()),
            });
        })
        .detach();
    }

    /// `sourceControl.appAccess` after a clone starts: when the server is signed in with the
    /// GitHub app but the app does not reach the repository, offers to install it there.
    pub(super) fn suggest_app_access(
        &mut self,
        environment: &str,
        remote_url: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        const APP_NAME: &str = "T3 Code for GitHub";
        const INSTALL_URL: &str = "https://github.com/apps/t3-code-for-github/installations/new";
        let Some(repository) = paths::github_app_repository(remote_url) else { return };
        let Some(client) = self.env_client(environment) else { return };
        let input = json!({"provider": "github", "host": "github.com", "repository": repository});
        cx.spawn_in(window, async move |_, cx| {
            let Ok(access) = client.request("sourceControl.appAccess", input).await else { return };
            if access["status"] != "not-covered" {
                return;
            }
            let owner = repository.split('/').next().unwrap_or(&repository).to_owned();
            let _ = cx.update(|window, cx| {
                let description = format!(
                    "It isn't installed there, so GitHub requests for {repository} use your own login and its lower rate limit."
                );
                let toast = Notification::info(description).title(format!("Give {APP_NAME} access to {owner}")).action(
                    |_, _, cx| {
                        Button::new("clone-app-install").label("Install").on_click(cx.listener(
                            |toast, _, window, cx| {
                                toast.dismiss(window, cx);
                                cx.open_url(INSTALL_URL);
                            },
                        ))
                    },
                );
                window.push_notification(toast, cx);
            });
        })
        .detach();
    }

    /// The open thread's or draft's project, on its environment.
    fn active_project_key(&self) -> Option<ScopedId> {
        Some(match self.target.as_ref()? {
            Target::Draft { env, project_id, .. } => ScopedId::new(env.clone(), project_id.as_str()),
            Target::Thread { env, id } => {
                ScopedId::thread_project(self.shell.thread(&ScopedId::new(env.clone(), id.as_str()))?)
            }
        })
    }

    /// The clone of the open thread's or draft's project, on its environment.
    fn active_project_clone(&self) -> Option<&CloneSnapshot> {
        self.clones.clone_of(&self.active_project_key()?)
    }

    /// `projectCloneSendBlockReason`: nothing sends into a project still arriving, or still
    /// waiting for its copied files.
    pub(super) fn clone_send_block(&self) -> Option<&'static str> {
        let project = self.active_project_key()?;
        match self.clones.clone_of(&project).map(|clone| clone.phase) {
            Some(Phase::Running) => return Some("Cloning repository"),
            Some(Phase::Failed | Phase::Cancelled) => return Some("Repository not cloned"),
            Some(Phase::Done) | None => {}
        }
        matches!(self.clones.copies.of(&project), Some(FileCopy::Copying)).then_some("Copying files")
    }

    /// `projectCloneBannerItem`: a failed file copy until dismissed, else the clone's progress,
    /// failure or cancellation over the composer.
    pub(super) fn render_clone_banner(&self, joined: bool, t: &Tokens, cx: &mut Context<Self>) -> Option<AnyElement> {
        if let Some(banner) = self.render_file_copy_banner(joined, t, cx) {
            return Some(banner);
        }
        let clone = self.active_project_clone()?;
        let key = self.target.as_ref().map(|target| target.environment().clone())?;
        let key = ScopedId::new(key, clone.project_id.as_str());
        let name = clone.display_name();
        let button = |id: &'static str, label: &'static str| Button::new(id).ghost().xsmall().label(label);
        let (variant, title, description, actions) = match clone.phase {
            Phase::Done => return None,
            Phase::Running => {
                let cancel =
                    button("clone-banner-cancel", "Cancel").on_click(cx.listener(move |this, _, window, cx| {
                        this.run_clone_action(key.clone(), "projectClone.cancel", "Failed to cancel clone", window, cx);
                    }));
                (Variant::Info, format!("Cloning {name}"), Some(clone.progress_summary()), cancel.into_any_element())
            }
            Phase::Failed | Phase::Cancelled => {
                let cancelled = clone.phase == Phase::Cancelled;
                let remove_key = key.clone();
                let remove = button("clone-banner-remove", "Remove project").on_click(
                    cx.listener(move |this, _, window, cx| this.remove_cloned_project(remove_key.clone(), window, cx)),
                );
                let retry = button("clone-banner-retry", "Retry").on_click(cx.listener(move |this, _, window, cx| {
                    this.run_clone_action(key.clone(), "projectClone.retry", "Failed to retry clone", window, cx);
                }));
                let (variant, title, description) = if cancelled {
                    (
                        Variant::Warning,
                        format!("Cancelled cloning {name}"),
                        Some("Retry to bring in the repository.".to_owned()),
                    )
                } else {
                    (Variant::Error, format!("Failed to clone {name}"), clone.error.clone())
                };
                (variant, title, description, h_flex().gap(px(2.)).child(remove).child(retry).into_any_element())
            }
        };
        let icon = Icon::new(IconName::Download);
        let description = description.map(Into::into);
        Some(composer_banner(variant, icon, SharedString::from(title), description, Some(actions), joined, t))
    }

    /// A copy that left files behind: which ones and why, until dismissed.
    fn render_file_copy_banner(&self, joined: bool, t: &Tokens, cx: &mut Context<Self>) -> Option<AnyElement> {
        let project = self.active_project_key()?;
        let Some(FileCopy::Failed { title, failures }) = self.clones.copies.of(&project) else { return None };
        let dismiss = Button::new("file-copy-banner-dismiss")
            .ghost()
            .xsmall()
            .icon(Icon::new(IconName::X))
            .tooltip("Dismiss")
            .on_click(cx.listener(move |this, _, _, cx| this.dismiss_file_copy(&project, cx)));
        let icon = Icon::new(IconName::Download);
        let description = SharedString::from(failures.join("\n"));
        Some(composer_banner(
            Variant::Error,
            icon,
            SharedString::from(title.clone()),
            Some(description),
            Some(dismiss.into_any_element()),
            joined,
            t,
        ))
    }
}

/// Remove project on a failed or cancelled clone's toast.
type RemoveAction = Rc<dyn Fn(&mut Window, &mut App)>;

type ToastAction = fn(&mut Workspace, ScopedId, &mut Window, &mut Context<Workspace>);

/// "Open project": a new draft in the cloned project.
fn open_project(this: &mut Workspace, key: ScopedId, window: &mut Window, cx: &mut Context<Workspace>) {
    this.settings = None;
    this.start_draft_in(key.env, key.id, window, cx);
}

/// A clone toast's body: the description it reads while open, and Remove project after a
/// failure or cancellation.
fn description_content(
    description: Rc<RefCell<SharedString>>,
    remove: Option<RemoveAction>,
) -> impl Fn(&mut Notification, &mut Window, &mut Context<Notification>) -> AnyElement + 'static {
    move |_, _, _| {
        let remove = remove.clone().map(|remove| {
            Button::new("clone-toast-remove")
                .outline()
                .xsmall()
                .label("Remove project")
                .on_click(move |_, window, cx| remove(window, cx))
        });
        v_flex()
            .gap(px(8.))
            .child(div().text_sm().child(description.borrow().clone()))
            .children(remove.map(|remove| h_flex().child(remove)))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn clone(project: &str, phase: &str, percent: Option<u64>) -> Value {
        json!({
            "projectId": project, "remoteUrl": "https://github.com/o/r.git", "destinationPath": "/code/r",
            "repository": {"provider": "github", "nameWithOwner": "o/r", "url": "u", "sshUrl": "s"},
            "phase": phase, "stage": "receiving", "percent": percent, "detail": "1 MiB/s",
            "error": if phase == "failed" { json!("auth failed") } else { Value::Null },
            "startedAt": "2026-01-01T00:00:00.000Z", "endedAt": null, "sequence": 1,
        })
    }

    fn set(clones: &mut ProjectClones, env: &str, list: Vec<Value>) {
        clones.by_env.insert(env.into(), parse_clones(&Value::Array(list)));
    }

    fn key(env: &str, project: &str) -> ScopedId {
        ScopedId::new(env, project)
    }

    #[test]
    fn snapshots_read_their_name_and_progress() {
        let snapshot = CloneSnapshot::from_json(&clone("p", "running", Some(42))).unwrap();
        assert_eq!(snapshot.display_name(), "o/r");
        assert_eq!(snapshot.progress_summary(), "Receiving objects · 42% · 1 MiB/s");
        let mut stalled = clone("p", "running", None);
        stalled["stalled"] = true.into();
        stalled["repository"] = Value::Null;
        stalled["destinationPath"] = "C:\\code\\repo\\".into();
        let stalled = CloneSnapshot::from_json(&stalled).unwrap();
        assert_eq!(stalled.progress_summary(), "Receiving objects · Stalled");
        assert_eq!(stalled.display_name(), "repo");
        assert!(CloneSnapshot::from_json(&json!({"projectId": "p", "phase": "paused"})).is_none());
    }

    #[test]
    fn progress_updates_the_open_toast_and_a_new_phase_replaces_it() {
        let mut clones = ProjectClones::default();
        set(&mut clones, "e", vec![clone("p", "running", Some(1))]);
        assert_eq!(clones.plan(&"e".into(), None), [Effect::Show(key("e", "p"))]);
        set(&mut clones, "e", vec![clone("p", "running", Some(50))]);
        assert!(clones.plan(&"e".into(), None).is_empty(), "progress lands in place");
        assert_eq!(clones.toasts[&key("e", "p")].description.borrow().as_ref(), "Receiving objects · 50% · 1 MiB/s");
        assert!(clones.plan(&"e".into(), None).is_empty(), "an unchanged clone is left alone");
        set(&mut clones, "e", vec![clone("p", "failed", Some(50))]);
        assert_eq!(clones.plan(&"e".into(), None), [Effect::Show(key("e", "p"))]);
        assert_eq!(clones.toasts[&key("e", "p")].description.borrow().as_ref(), "auth failed");
    }

    #[test]
    fn viewing_the_draft_silences_progress_but_not_failure() {
        let mut clones = ProjectClones::default();
        set(&mut clones, "e", vec![clone("p", "running", Some(1))]);
        clones.plan(&"e".into(), None);
        assert_eq!(clones.plan(&"e".into(), Some("p")), [Effect::Close(key("e", "p"))]);
        set(&mut clones, "e", vec![clone("p", "running", Some(9))]);
        assert!(clones.plan(&"e".into(), None).is_empty(), "progress stays quiet after the draft was seen");
        set(&mut clones, "e", vec![clone("p", "done", Some(100))]);
        assert!(clones.plan(&"e".into(), None).is_empty(), "so does success");
        set(&mut clones, "e", vec![clone("p", "failed", None)]);
        assert_eq!(clones.plan(&"e".into(), None), [Effect::Show(key("e", "p"))], "a failure toasts off screen");
        assert_eq!(clones.plan(&"e".into(), Some("p")), [Effect::Close(key("e", "p"))]);
    }

    #[test]
    fn vanished_clones_close_unless_finished_and_environments_stay_apart() {
        let mut clones = ProjectClones::default();
        set(&mut clones, "a", vec![clone("p", "running", None), clone("q", "done", None)]);
        set(&mut clones, "b", vec![clone("p", "running", None)]);
        assert_eq!(clones.plan(&"a".into(), None).len(), 2);
        assert_eq!(clones.plan(&"b".into(), None), [Effect::Show(key("b", "p"))], "the same id on another machine");
        set(&mut clones, "a", Vec::new());
        assert_eq!(clones.plan(&"a".into(), None), [Effect::Close(key("a", "p"))], "a done toast runs out on its own");
        assert!(clones.toasts.contains_key(&key("b", "p")));
        assert_eq!(clones.forget_environments(|env| env == "a"), [key("b", "p")]);
        assert!(clones.clone_of(&key("b", "p")).is_none());
    }
}
