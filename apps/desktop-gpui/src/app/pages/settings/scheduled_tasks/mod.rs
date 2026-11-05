//! HARNESS ONLY. The task-list agent owns the real `mod.rs`; at merge keep theirs and add
//! `mod webhook_logic; mod webhooks; mod worktree_mode;`. This file only exercises the webhook and
//! worktree pieces live: it lists the webhook tasks, edits one in a dialog, and creates or deletes
//! the harness's own disabled test task.

mod webhook_logic;
mod webhooks;
mod worktree_mode;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, Task, Window, div, px,
};
use serde_json::{Value, json};

use crate::app::Workspace;
use crate::rpc::StreamEvent;
use webhooks::{WebhookContext, WebhookFields, render_webhook_fields, webhook_menu_item};
use worktree_mode::{NewWorktreeMode, render_new_worktree_mode};

const HARNESS_TITLE: &str = "GPUI harness webhook";

impl Workspace {
    pub(super) fn render_scheduled_tasks_harness(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(context) = self.webhook_context(cx) else { return div().child("Not connected").into_any_element() };
        // A scratch project when there is one, so the test task never targets a real repository.
        let project = self
            .shell
            .projects
            .iter()
            .find(|p| p.title.contains("scratch"))
            .or(self.shell.projects.first())
            .map(|p| (p.id.clone(), p.workspace_root.clone()));
        let replay = self
            .server_info
            .providers
            .iter()
            .find(|p| p.display_name.as_deref().is_some_and(|name| name.starts_with("Replay")))
            .and_then(|p| Some(json!({"instanceId": p.instance_id, "model": p.models.first()?.slug})));
        let refs_root =
            self.shell.projects.iter().find(|p| !p.title.contains("scratch")).map(|p| p.workspace_root.clone());
        window
            .use_keyed_state("scheduled-tasks-harness", cx, |_, cx| {
                Harness::new(context, project, replay, refs_root, cx)
            })
            .into_any_element()
    }
}

struct Harness {
    context: WebhookContext,
    project: Option<(String, String)>,
    replay: Option<Value>,
    refs_root: Option<String>,
    tasks: Vec<Value>,
    _stream: Task<()>,
}

impl Harness {
    fn new(
        context: WebhookContext,
        project: Option<(String, String)>,
        replay: Option<Value>,
        refs_root: Option<String>,
        cx: &mut Context<Self>,
    ) -> Self {
        let stream = context.client().stream("scheduledTasks.subscribe", json!({}));
        let pump = cx.spawn(async move |this, cx| {
            while let Some(StreamEvent::Item(list)) = stream.next().await {
                let ok = this.update(cx, |this, cx| {
                    this.tasks = list["tasks"].as_array().cloned().unwrap_or_default();
                    cx.notify();
                });
                if ok.is_err() {
                    return;
                }
            }
        });
        Self { context, project, replay, refs_root, tasks: Vec::new(), _stream: pump }
    }

    fn open_editor(&self, task: Option<Value>, window: &mut Window, cx: &mut Context<Self>) {
        log::info!("harness: open editor in {:?}", self.project);
        // The picker only reads refs, so it may browse a real repository.
        let cwd = self.refs_root.clone().or_else(|| self.project.as_ref().map(|(_, root)| root.clone()));
        let fields = cx.new(|cx| WebhookFields::new(self.context.clone(), task.clone(), window, cx));
        let worktree = cx.new(|cx| NewWorktreeMode::new(self.context.client(), cwd, task.as_ref(), window, cx));
        let editor = cx.new(|_| Editor {
            context: self.context.clone(),
            task,
            project: self.project.clone(),
            replay: self.replay.clone(),
            fields,
            worktree,
            saving: false,
        });
        window.open_dialog(cx, move |dialog, _, _| dialog.w(px(576.)).child(editor.clone()));
    }

    fn delete(&self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let client = self.context.client();
        cx.spawn_in(window, async move |_, cx| {
            if let Err(error) = client.request("scheduledTasks.delete", json!({"id": id})).await {
                let _ = cx.update(|window, cx| window.push_notification(Notification::error(error.to_string()), cx));
            }
        })
        .detach();
    }
}

impl Render for Harness {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity();
        let rows = self.tasks.iter().filter(|task| task["schedule"]["type"] == "webhook").map(|task| {
            let id = task["id"].as_str().unwrap_or_default().to_owned();
            let ours = task["title"] == HARNESS_TITLE;
            let (view, context, task) = (view.clone(), self.context.clone(), task.clone());
            h_flex()
                .gap_2()
                .child(div().flex_1().child(format!("{} ({id})", task["title"].as_str().unwrap_or_default())))
                .child(
                    Button::new(SharedString::from(format!("harness-menu-{id}")))
                        .ghost()
                        .small()
                        .label("Actions")
                        .dropdown_menu(move |menu: PopupMenu, window, _| {
                            let (edit_task, delete_id) = (task.clone(), id.clone());
                            let mut menu = menu.item(PopupMenuItem::new("Edit").on_click(
                                window.listener_for(&view, move |this, _, window, cx| {
                                    this.open_editor(Some(edit_task.clone()), window, cx)
                                }),
                            ));
                            menu = webhook_menu_item(menu, &context, &task);
                            if ours {
                                menu = menu.separator().item(PopupMenuItem::new("Delete").on_click(
                                    window.listener_for(&view, move |this, _, window, cx| {
                                        this.delete(delete_id.clone(), window, cx)
                                    }),
                                ));
                            }
                            menu
                        }),
                )
        });
        v_flex()
            .gap_2()
            .p_4()
            // Clear of the titlebar's drag area, which stays live under the Settings modal.
            .pt(px(80.))
            .child(
                Button::new("harness-new")
                    .outline()
                    .small()
                    .label("New harness webhook task")
                    .on_click(cx.listener(|this, _, window, cx| this.open_editor(None, window, cx))),
            )
            .children(rows)
    }
}

struct Editor {
    context: WebhookContext,
    task: Option<Value>,
    project: Option<(String, String)>,
    replay: Option<Value>,
    fields: Entity<WebhookFields>,
    worktree: Entity<NewWorktreeMode>,
    saving: bool,
}

impl Editor {
    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let schedule = match self.fields.read(cx).schedule(cx) {
            Ok(schedule) => schedule,
            Err(errors) => {
                for error in errors {
                    window.push_notification(Notification::error(error.description).title(error.title), cx);
                }
                return;
            }
        };
        let (Some((project_id, _)), Some(model)) = (&self.project, &self.replay) else {
            window.push_notification(Notification::error("No scratch project or Replay model"), cx);
            return;
        };
        let mut input = match &self.task {
            // Edits keep the task's own strategy and model; the harness never writes a worktree task.
            Some(task) => json!({
                "id": task["id"], "requireExisting": true, "title": task["title"], "prompt": task["prompt"],
                "enabled": task["enabled"], "projectId": task["projectId"], "threadId": task["threadId"],
                "workspaceStrategy": task["workspaceStrategy"], "modelSelection": task["modelSelection"],
                "runtimeMode": task["runtimeMode"], "interactionMode": task["interactionMode"],
            }),
            None => json!({
                "title": HARNESS_TITLE, "prompt": webhooks::DEFAULT_WEBHOOK_PROMPT, "enabled": false,
                "projectId": project_id, "threadId": null, "workspaceStrategy": {"type": "root"},
                "modelSelection": model, "runtimeMode": "full-access", "interactionMode": "default",
            }),
        };
        input["schedule"] = schedule;
        input["creationSource"] = "web".into();
        log::info!("harness upsert: {input}; worktree strategy would be {}", self.worktree.read(cx).strategy());
        let client = self.context.client();
        self.set_saving(true, cx);
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("scheduledTasks.upsert", input).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.set_saving(false, cx);
                match result {
                    Ok(_) => window.close_dialog(cx),
                    Err(error) => window.push_notification(
                        Notification::error(error.to_string()).title("Could not save scheduled task"),
                        cx,
                    ),
                }
            });
        })
        .detach();
    }

    fn set_saving(&mut self, saving: bool, cx: &mut Context<Self>) {
        self.saving = saving;
        self.fields.update(cx, |fields, cx| fields.set_disabled(saving, cx));
        self.worktree.update(cx, |worktree, cx| worktree.set_disabled(saving, cx));
        cx.notify();
    }
}

impl Render for Editor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let strategy = self.worktree.read(cx).strategy().to_string();
        v_flex()
            .id("harness-editor")
            .gap_5()
            .max_h(px(560.))
            .overflow_y_scroll()
            .child(render_new_worktree_mode(&self.worktree))
            .child(div().text_xs().child(format!("workspaceStrategy: {strategy}")))
            .child(render_webhook_fields(&self.fields))
            .child(
                h_flex().justify_end().child(
                    Button::new("harness-save")
                        .primary()
                        .small()
                        .label(if self.task.is_some() { "Save task" } else { "Create task" })
                        .disabled(self.saving)
                        .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
                ),
            )
    }
}
