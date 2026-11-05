//! Thread menu actions beyond the shared ones in `sidebar::actions` (`useThreadActionMenu.ts`,
//! `Sidebar.tsx`): a new thread on the thread's branch and turning a No project thread into a
//! project. The draft row's menu is in `sidebar::drafts`.

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::notification::Notification;
use gpui_kit::{Context, Window};
use serde_json::json;

use super::Workspace;
use crate::new_thread::{EnvMode, WorkspaceChoice};

/// The draft's workspace for "New thread on `branch`": the thread's worktree, else its branch locally.
fn branch_choice(branch: Option<String>, worktree_path: Option<String>) -> WorkspaceChoice {
    match worktree_path {
        Some(path) => WorkspaceChoice {
            mode: Some(EnvMode::Worktree),
            existing_worktree: Some((path, branch.clone())),
            start_from_origin: Some(false),
            branch,
        },
        None => WorkspaceChoice {
            mode: Some(EnvMode::Local),
            branch,
            start_from_origin: Some(false),
            existing_worktree: None,
        },
    }
}

impl Workspace {
    /// A draft in the thread's project on its checkout: its worktree, else its branch locally.
    pub(in crate::app) fn new_thread_on_branch(
        &mut self,
        thread_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(thread) = self.shell.threads.iter().find(|t| t.id == thread_id) else { return };
        let choice = branch_choice(thread.branch.clone(), thread.worktree_path.clone());
        self.start_draft(thread.project_id.clone(), window, cx);
        self.sync_draft_workspace(cx);
        self.draft_workspace.choice = choice;
        cx.notify();
    }

    /// `projects.createFromThread`: the thread's folder becomes a project.
    pub(in crate::app) fn turn_thread_into_project(
        &mut self,
        thread_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.thread_client(thread_id) else { return };
        let title = self.shell.threads.iter().find(|t| t.id == thread_id).map(|t| t.title.clone()).unwrap_or_default();
        let input = json!({"threadId": thread_id});
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("projects.createFromThread", input).await;
            let _ = this.update_in(cx, |_, window, cx| {
                let note = match result {
                    Ok(_) => Notification::success(title).title("Project created"),
                    Err(error) => Notification::error(error.to_string()).title("Could not turn thread into a project"),
                };
                window.push_notification(note, cx);
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_branch_thread_reuses_its_worktree_or_switches_locally() {
        let worktree = branch_choice(Some("feat".into()), Some("C:/wt/feat".into()));
        assert_eq!(worktree.mode, Some(EnvMode::Worktree));
        assert_eq!(worktree.existing_worktree, Some(("C:/wt/feat".into(), Some("feat".into()))));
        let local = branch_choice(Some("feat".into()), None);
        assert_eq!(
            (local.mode, local.branch.as_deref(), local.start_from_origin),
            (Some(EnvMode::Local), Some("feat"), Some(false))
        );
    }
}
