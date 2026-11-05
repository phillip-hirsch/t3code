//! Thread menu actions beyond the shared ones in `sidebar::actions` (`useThreadActionMenu.ts`,
//! `Sidebar.tsx`): a new thread on the thread's branch, turning a No project thread into a
//! project, and the draft row's menu (`buildDraftActionMenuItems`).

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{Icon, WindowExt as _};
use gpui_kit::{ClipboardItem, Context, Window, px};
use serde_json::json;

use super::{Target, Workspace};
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
        let Some(client) = self.client() else { return };
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

    /// The draft row's menu: Copy (path, branch), Project settings, Discard draft.
    pub(in crate::app) fn draft_menu(
        &self,
        cx: &mut Context<Self>,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        let view = cx.weak_entity();
        move |menu, window, cx| {
            let Some(workspace) = view.upgrade() else { return menu };
            let (path, branch, has_project) = {
                let this = workspace.read(cx);
                let Some(Target::Draft { project_id }) = &this.target else { return menu };
                let project = this.shell.projects.iter().find(|p| &p.id == project_id);
                let choice = &this.draft_workspace.choice;
                let worktree = choice.existing_worktree.as_ref();
                let path = worktree.map(|(path, _)| path.clone()).or_else(|| project.map(|p| p.workspace_root.clone()));
                let branch = worktree.and_then(|(_, branch)| branch.clone()).or_else(|| choice.branch.clone());
                (path, branch, project.is_some())
            };
            let menu = menu.min_w(px(200.)).submenu_with_icon(
                Some(Icon::new(Lucide::Copy)),
                "Copy",
                window,
                cx,
                move |mut menu, _, _| {
                    let copy = |text: String| {
                        move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut gpui_kit::App| {
                            cx.write_to_clipboard(ClipboardItem::new_string(text.clone()))
                        }
                    };
                    if let Some(path) = &path {
                        menu = menu.item(
                            PopupMenuItem::new("Path").icon(Icon::new(Lucide::Folder)).on_click(copy(path.clone())),
                        );
                    }
                    if let Some(branch) = &branch {
                        menu = menu.item(
                            PopupMenuItem::new("Branch")
                                .icon(Icon::new(Lucide::GitBranch))
                                .on_click(copy(branch.clone())),
                        );
                    }
                    menu
                },
            );
            let (settings, discard) = (view.clone(), view.clone());
            let menu = if has_project {
                menu.item(PopupMenuItem::new("Project settings").icon(Icon::new(Lucide::Settings)).on_click(
                    move |_, _, cx| {
                        let _ = settings.update(cx, |this, cx| this.open_settings("projects", cx));
                    },
                ))
            } else {
                menu
            };
            menu.separator().item(PopupMenuItem::new("Discard draft").icon(Icon::new(Lucide::Trash)).on_click(
                move |_, window, cx| {
                    let _ = discard.update(cx, |this, cx| this.discard_draft(window, cx));
                },
            ))
        }
    }

    /// `discardComposerDraft`: the draft's text, attachments and workspace choice go.
    fn discard_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target @ Target::Draft { .. }) = self.target.clone() else { return };
        self.composer_drafts.remove(&target);
        self.attachments.clear();
        self.set_composer_text("", window, cx);
        self.draft_workspace.choice = WorkspaceChoice::default();
        self.notify_sidebar(cx);
        cx.notify();
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
