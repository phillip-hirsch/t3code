//! `ArchivedThreadsPanel`: archived threads by project, with Unarchive and, from the
//! context menu, Delete. One environment, so the web's cross-environment grouping drops out.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariant};
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenuItem};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{Icon, Sizable as _, WindowExt as _, h_flex};
use gpui_kit::{
    AnyElement, App, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    Styled as _, Task, Window, div, px,
};
use std::collections::HashMap;

use serde::Deserialize;
use serde_json::{Value, json};

use super::super::widgets::{RowFrame, settings_section};
use super::{notify_error, now_ms, relative_label, text};
use crate::app::pages::settings::widgets::SettingsView;
use crate::app::{Workspace, new_id, sidebar};
use crate::model::ProjectShell;
use crate::rpc::RpcClient;
use crate::theme::{self, Tokens};

/// Projects with their archived threads, decoded once per load.
type Groups = std::rc::Rc<[(ProjectShell, Vec<ArchivedThread>)]>;

pub(super) struct ArchiveState {
    groups: Option<Result<Groups, String>>,
    _load: Option<Task<()>>,
}

impl ArchiveState {
    /// `orchestration.getArchivedShellSnapshot`; the live shell leaves archived threads out.
    fn load(&mut self, client: Option<RpcClient>, cx: &mut Context<Self>) {
        let Some(client) = client else {
            self.groups = Some(Err("Not connected to the server.".into()));
            return;
        };
        self._load = Some(cx.spawn(async move |this, cx| {
            let result = client.request("orchestration.getArchivedShellSnapshot", json!({})).await;
            let _ = this.update(cx, |this, cx| {
                this.groups = Some(result.map(|snapshot| groups(&snapshot).into()).map_err(|error| error.message()));
                cx.notify();
            });
        }));
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
}

impl ArchivedThread {
    fn archived(&self) -> &str {
        self.archived_at.as_deref().unwrap_or(&self.created_at)
    }
}

/// Projects in snapshot order, each with its archived threads, newest archive first.
fn groups(snapshot: &Value) -> Vec<(ProjectShell, Vec<ArchivedThread>)> {
    let decode = |key: &str| snapshot[key].as_array().map(Vec::as_slice).unwrap_or_default();
    let mut threads: Vec<ArchivedThread> =
        decode("threads").iter().filter_map(|t| ArchivedThread::deserialize(t).ok()).collect();
    threads.sort_by(|a, b| b.archived().cmp(a.archived()).then_with(|| b.id.cmp(&a.id)));
    let mut by_project: HashMap<String, Vec<ArchivedThread>> = HashMap::new();
    for thread in threads {
        by_project.entry(thread.project_id.clone()).or_default().push(thread);
    }
    decode("projects")
        .iter()
        .filter_map(|p| ProjectShell::deserialize(p).ok())
        .filter_map(|project| {
            let mine = by_project.remove(&project.id)?;
            Some((project, mine))
        })
        .collect()
}

impl Workspace {
    pub(super) fn render_archive(
        &mut self,
        view: &SettingsView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = *theme::tokens(cx);
        let client = self.settings_client();
        let state = window.use_keyed_state(self.settings_state_key("settings-archive"), cx, |_, cx| {
            let mut state = ArchiveState { groups: None, _load: None };
            state.load(client, cx);
            state
        });
        let loaded = state.read(cx).groups.clone();
        let groups = loaded.as_ref().and_then(|groups| groups.as_ref().ok()).filter(|groups| !groups.is_empty());
        let Some(groups) = groups else {
            let (icon, title, description) = match &loaded {
                None => (
                    Spinner::new().small().color(t.muted_foreground).into_any_element(),
                    "Loading archived threads",
                    "Checking connected environments.".to_owned(),
                ),
                Some(result) => (
                    Icon::new(IconName::Archive).size(px(14.)).text_color(t.muted_foreground).into_any_element(),
                    if result.is_err() { "Could not load archived threads" } else { "No archived threads" },
                    result.as_ref().err().cloned().unwrap_or_else(|| "Archived threads will appear here.".into()),
                ),
            };
            let title = h_flex().gap_2().child(icon).child(title);
            let row = RowFrame::new(title).description(text(description)).render(&t).into_any_element();
            return self.settings_page(
                "archived",
                view,
                [settings_section("archived", "Archived threads", [row], &t)],
                cx,
            );
        };
        let now = now_ms();
        let sections: Vec<AnyElement> = groups
            .iter()
            .map(|(project, threads)| {
                let rows: Vec<AnyElement> =
                    threads.iter().map(|thread| self.archived_row(thread, &state, now, &t, cx)).collect();
                let icon = sidebar::project_icon(project, &t, false, 16., self.favicon(project));
                let title = h_flex().gap_2().child(icon).child(project.title.clone());
                settings_section(SharedString::from(format!("archived-{}", project.id)), title, rows, &t)
            })
            .collect();
        self.settings_page("archived", view, sections, cx)
    }

    fn archived_row(
        &self,
        thread: &ArchivedThread,
        state: &Entity<ArchiveState>,
        now: i64,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let description = format!(
            "Archived {} · Created {}",
            relative_label(thread.archived(), now),
            relative_label(&thread.created_at, now)
        );
        let (id, title) = (thread.id.clone(), SharedString::from(thread.title.clone()));
        let view = cx.entity();
        let unarchive_button = {
            let (view, state, id) = (view.clone(), state.clone(), id.clone());
            Button::new(SharedString::from(format!("unarchive-{id}")))
                .outline()
                .small()
                .icon(Icon::new(IconName::ArchiveX))
                .label("Unarchive")
                .on_click(move |_, window, cx| unarchive(&view, &state, &id, window, cx))
        };
        let state = state.clone();
        div()
            .id(SharedString::from(format!("archived-{id}")))
            .child(RowFrame::new(title.clone()).description(text(description)).control(unarchive_button).render(t))
            .context_menu(move |menu, _, _| {
                let unarchive_item = {
                    let (view, state, id) = (view.clone(), state.clone(), id.clone());
                    move |_: &gpui_kit::ClickEvent, window: &mut Window, cx: &mut App| {
                        unarchive(&view, &state, &id, window, cx);
                    }
                };
                let delete_item = {
                    let (view, state, id, title) = (view.clone(), state.clone(), id.clone(), title.clone());
                    move |_: &gpui_kit::ClickEvent, window: &mut Window, cx: &mut App| {
                        confirm_delete(view.clone(), state.clone(), id.clone(), title.clone(), window, cx);
                    }
                };
                menu.item(PopupMenuItem::new("Unarchive").on_click(unarchive_item))
                    .item(PopupMenuItem::new("Delete").on_click(delete_item))
            })
            .into_any_element()
    }
}

fn unarchive(
    view: &Entity<Workspace>,
    state: &Entity<ArchiveState>,
    thread_id: &str,
    window: &mut Window,
    cx: &mut App,
) {
    let command = json!({"type": "thread.unarchive", "commandId": new_id(), "threadId": thread_id});
    dispatch_and_reload(view, state, command, "Failed to unarchive thread", window, cx);
}

/// The web asks first (`confirmAndDeleteThread`, the `confirmThreadDelete` default).
fn confirm_delete(
    view: Entity<Workspace>,
    state: Entity<ArchiveState>,
    thread_id: String,
    title: SharedString,
    window: &mut Window,
    cx: &mut App,
) {
    window.open_alert_dialog(cx, move |dialog, _, _| {
        let (view, state, thread_id) = (view.clone(), state.clone(), thread_id.clone());
        dialog
            .title(format!("Delete thread \"{title}\"?"))
            .description("This permanently clears conversation history for this thread.")
            .ok_text("Delete")
            .ok_variant(ButtonVariant::Danger)
            .show_cancel(true)
            .on_ok(move |_, window, cx| {
                let command = json!({"type": "thread.delete", "commandId": new_id(), "threadId": thread_id});
                dispatch_and_reload(&view, &state, command, "Failed to delete thread", window, cx);
                true
            })
    });
}

fn dispatch_and_reload(
    view: &Entity<Workspace>,
    state: &Entity<ArchiveState>,
    command: Value,
    failure: &'static str,
    window: &mut Window,
    cx: &mut App,
) {
    // The settings environment, whose archive the state shows.
    let Some(client) = view.read(cx).settings_client() else { return };
    let state = state.clone();
    window
        .spawn(cx, async move |cx| {
            let result = client.request("orchestration.dispatchCommand", command).await;
            let _ = cx.update(|window, cx| match result {
                Ok(_) => state.update(cx, |state, cx| state.load(Some(client), cx)),
                // The web shows the error's message, else a generic line; never the tagged form.
                Err(crate::rpc::RpcError::Disconnected(_)) => {}
                Err(crate::rpc::RpcError::Failure(error)) => {
                    let message = error["message"].as_str().unwrap_or("An error occurred.").to_owned();
                    notify_error(window, cx, failure, message);
                }
                Err(crate::rpc::RpcError::Defect(_)) => notify_error(window, cx, failure, "An error occurred."),
            });
        })
        .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_threads_under_their_projects_newest_archive_first() {
        let project = |id: &str| json!({"id": id, "title": id.to_uppercase(), "workspaceRoot": "/r"});
        let thread = |id: &str, project: &str, archived: Option<&str>| json!({"id": id, "projectId": project, "title": id, "createdAt": "2026-01-01T00:00:00Z", "archivedAt": archived});
        let snapshot = json!({
            "projects": [project("a"), project("empty"), project("b")],
            "threads": [
                thread("old", "a", Some("2026-02-01T00:00:00Z")),
                thread("new", "a", Some("2026-03-01T00:00:00Z")),
                thread("never", "a", None),
                thread("b1", "b", Some("2026-01-05T00:00:00Z")),
            ],
        });
        let groups = groups(&snapshot);
        let shape: Vec<(&str, Vec<&str>)> = groups
            .iter()
            .map(|(p, threads)| (p.id.as_str(), threads.iter().map(|t| t.id.as_str()).collect()))
            .collect();
        assert_eq!(
            shape,
            vec![("a", vec!["new", "old", "never"]), ("b", vec!["b1"])],
            "projects without threads drop out"
        );
    }
}
