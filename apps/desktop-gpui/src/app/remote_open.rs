//! The Open in controls' remote half (`useRemoteOpenState`, `useRemoteCapableEditors`,
//! `openRemoteEditorUrl` and `useRemoteOpenHint` in `apps/web/src/remoteOpen.ts`): which editors
//! an environment's Open in lists, and how a pick opens there.

use gpui_kit::component::notification::Notification;
use gpui_kit::component::{WindowExt as _, menu::PopupMenu, menu::PopupMenuItem};
use gpui_kit::{App, Context, Task, Window};
use serde_json::{Map, Value, json};

use super::Workspace;
use crate::environments::Address;
use crate::remote_open::{self, RemoteOpenState};

/// `useRemoteOpenHint`'s `t3code:remote-open-hint-seen`, kept with this device's client settings.
const HINT_SEEN_KEY: &str = "remoteOpenHintSeen";

/// `remoteCapableEditorsAtom`: the fallback until the probe, which runs once on first need.
pub(super) struct RemoteEditors {
    ids: Vec<String>,
    probe: Option<Task<()>>,
}

impl Default for RemoteEditors {
    fn default() -> Self {
        Self { ids: remote_open::FALLBACK_EDITORS.iter().map(|id| (*id).to_owned()).collect(), probe: None }
    }
}

/// What an environment's Open in control lists and opens (`OpenInPicker`'s derived state).
pub(super) struct OpenIn {
    pub state: RemoteOpenState,
    /// The host's editors when it opens paths itself, else this machine's remote-capable ones.
    pub editors: Vec<String>,
    /// `usePreferredEditor`: the last pick while listed, else the first.
    pub preferred: Option<String>,
    /// `isHostEditorDenied`: the host opens paths only with `orchestration:operate` there.
    pub host_denied: bool,
    /// `useEnvironment(environmentId)?.label`, for the menu's notes.
    pub environment_label: String,
    pub hint_seen: bool,
}

impl OpenIn {
    /// `canOpenEditor`.
    pub fn can_open(&self) -> bool {
        self.state != RemoteOpenState::RemoteUnavailable && !self.host_denied
    }

    /// The editors the menu lists; an unavailable route lists none.
    pub fn listed(&self) -> &[String] {
        if self.state == RemoteOpenState::RemoteUnavailable { &[] } else { &self.editors }
    }

    /// The menu's disabled notes after its editors: the missing route, no editors, and the SSH
    /// hint until a remote link first opened.
    pub fn menu_notes(&self, menu: PopupMenu) -> PopupMenu {
        let note = |menu: PopupMenu, text: String| menu.item(PopupMenuItem::new(text).disabled(true));
        if self.state == RemoteOpenState::RemoteUnavailable {
            return note(menu, format!("No SSH route to {}", self.environment_label));
        }
        let menu = if self.editors.is_empty() { note(menu, "No installed editors found".into()) } else { menu };
        if matches!(self.state, RemoteOpenState::RemoteLinks(_)) && !self.hint_seen {
            return note(menu, format!("Opens over SSH. Needs your key on {}", self.environment_label));
        }
        menu
    }
}

impl Workspace {
    /// `useRemoteOpenState`: the desktop's own server opens paths itself; a saved environment
    /// opens through its first route's SSH profile or the hosts its server advertises. An
    /// environment this client does not know yet reads as local, like the web's unresolved state.
    pub(super) fn remote_open_state(&self, environment_id: &str, cx: &App) -> RemoteOpenState {
        if self.primary_environment().is_some_and(|primary| primary.as_ref() == environment_id) {
            return RemoteOpenState::LocalExec;
        }
        let saved = self.saved_environments.read(cx);
        let Some(entry) = saved.environments().iter().find(|entry| entry.environment_id == environment_id) else {
            return RemoteOpenState::LocalExec;
        };
        let ssh = match entry.routes.first().map(|route| &route.address) {
            Some(Address::Ssh(target)) => Some(target),
            _ => None,
        };
        let advertised = self.env_info(environment_id).map_or(&Value::Null, |info| &info.config["remoteOpenTargets"]);
        remote_open::resolve(false, ssh, advertised)
    }

    /// The Open in control's state for a path on `environment_id`. A remote environment starts
    /// the editor probe the first time it shows.
    pub(super) fn open_in(&self, environment_id: &str, cx: &mut Context<Self>) -> OpenIn {
        let state = self.remote_open_state(environment_id, cx);
        let editors = if state == RemoteOpenState::LocalExec {
            self.env_info(environment_id).map(|info| info.available_editors.clone()).unwrap_or_default()
        } else {
            if self.remote_editors.probe.is_none() {
                let view = cx.entity();
                cx.defer(move |cx| view.update(cx, Self::probe_remote_editors));
            }
            self.remote_editors.ids.clone()
        };
        let preferred = self
            .preferred_editor
            .clone()
            .filter(|editor| editors.contains(editor))
            .or_else(|| editors.first().cloned());
        let host_denied = state == RemoteOpenState::LocalExec && !self.can_operate(environment_id);
        let saved_label = self
            .saved_environments
            .read(cx)
            .environments()
            .iter()
            .find(|entry| entry.environment_id == environment_id)
            .map(|entry| entry.label.clone());
        let environment_label = saved_label
            .or_else(|| self.env_info(environment_id).and_then(|info| info.environment_label.clone()))
            .unwrap_or_else(|| "this machine".to_owned());
        OpenIn {
            state,
            editors,
            preferred,
            host_denied,
            environment_label,
            hint_seen: self.client_settings.values.bool(HINT_SEEN_KEY),
        }
    }

    /// Electron's `probeRemoteEditors`, once per run: installed editors rarely change.
    fn probe_remote_editors(&mut self, cx: &mut Context<Self>) {
        if self.remote_editors.probe.is_some() {
            return;
        }
        let probe = crate::runtime().spawn(async {
            // A desktop launch has a bare PATH until the login shell's is read.
            crate::desktop::shell_env::probe().await;
            tokio::task::spawn_blocking(remote_open::probe_installed).await.unwrap_or_default()
        });
        self.remote_editors.probe = Some(cx.spawn(async move |this, cx| {
            let found = probe.await.unwrap_or_default();
            if found.is_empty() {
                return;
            }
            let _ = this.update(cx, |this, cx| {
                this.remote_editors.ids = found.into_iter().map(str::to_owned).collect();
                cx.notify();
            });
        }));
    }

    /// `OpenInPicker`'s `openInEditor`: a remote link opens in this machine's editor, the host's
    /// own editors through its `shell.openInEditor`. Returns whether it started.
    pub(super) fn open_in_editor_at(
        &mut self,
        environment_id: &str,
        path: String,
        is_file: bool,
        editor: String,
        reveal: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        match self.remote_open_state(environment_id, cx) {
            RemoteOpenState::RemoteUnavailable => false,
            RemoteOpenState::RemoteLinks(host) => {
                let Some(url) = remote_open::build_url(&editor, &host, &path, is_file) else { return false };
                cx.open_url(&url);
                self.preferred_editor = Some(editor);
                if !self.client_settings.values.bool(HINT_SEEN_KEY) {
                    self.client_settings.apply(&Map::from_iter([(HINT_SEEN_KEY.to_owned(), Value::Bool(true))]));
                }
                cx.notify();
                true
            }
            RemoteOpenState::LocalExec => {
                // Grants change on reconnect, so the click checks again.
                if !self.can_operate(environment_id) {
                    return false;
                }
                let title = format!("Could not open in {}", super::details::editor_label(&editor));
                self.preferred_editor = Some(editor.clone());
                self.open_path_in_editor(environment_id, path, editor, reveal, title, window, cx);
                true
            }
        }
    }

    /// `shell.openInEditor` on a path of `environment_id`'s host, with the web's failure toast.
    pub(in crate::app) fn open_path_in_editor(
        &mut self,
        environment_id: &str,
        path: String,
        editor: String,
        reveal: bool,
        failure: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.env_client(environment_id) else { return };
        let mut input = json!({"cwd": path, "editor": editor});
        if reveal {
            input["reveal"] = true.into();
        }
        cx.spawn_in(window, async move |_, cx| {
            if client.request("shell.openInEditor", input).await.is_err() {
                let _ = cx.update(|window, cx| {
                    window.push_notification(Notification::error(path.clone()).title(failure.clone()), cx);
                });
            }
        })
        .detach();
    }
}
