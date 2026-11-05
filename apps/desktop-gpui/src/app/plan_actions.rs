//! The proposed plan card's actions menu (`ProposedPlanCard.tsx`): copy to clipboard,
//! download as markdown, and save into the thread's workspace (`projects.writeFile`).

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::{DialogEntrance, DialogFooter};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, WindowExt as _, v_flex};
use gpui_kit::{
    App, AppContext as _, ClipboardItem, Context, ElementId, Focusable as _, FontWeight, IntoElement,
    ParentElement as _, SharedString, Styled as _, WeakEntity, Window, div, px,
};
use serde_json::json;

use super::Workspace;
use crate::proposed_plan;

const COPIED_FOR: Duration = Duration::from_secs(2);

/// The card's `…` button and its menu.
pub(super) fn plan_actions_button(
    this: &WeakEntity<Workspace>,
    id: &SharedString,
    markdown: &SharedString,
    cx: &App,
) -> impl IntoElement {
    let copy_id: SharedString = format!("plan-copy:{id}").into();
    let workspace = this.upgrade();
    let copied = workspace.as_ref().is_some_and(|w| w.read(cx).copy_feedback.is(&copy_id));
    let workspace_root = workspace.and_then(|w| w.read(cx).composer_cwd());
    let (this, markdown) = (this.clone(), markdown.clone());
    Button::new(ElementId::Name(format!("plan-actions:{id}").into()))
        .outline()
        .xsmall()
        .icon(Icon::new(Lucide::Ellipsis).size(px(16.)))
        .tooltip("Plan actions")
        .dropdown_menu(move |menu, _, _| {
            let (copy, download, save) = (this.clone(), this.clone(), this.clone());
            let (copy_md, download_md, save_md) = (markdown.clone(), markdown.clone(), markdown.clone());
            let copy_id = copy_id.clone();
            let root = workspace_root.clone();
            menu.item(PopupMenuItem::new(if copied { "Copied!" } else { "Copy to clipboard" }).on_click(
                move |_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(proposed_plan::export(&copy_md)));
                    let _ = copy.update(cx, |w, cx| w.copied(copy_id.clone(), COPIED_FOR, cx));
                },
            ))
            .item(PopupMenuItem::new("Download as markdown").on_click(move |_, window, cx| {
                let _ = download.update(cx, |w, cx| w.download_plan(&download_md, window, cx));
            }))
            .item(PopupMenuItem::new("Save to workspace").disabled(root.is_none()).on_click(
                move |_, window, cx| {
                    let Some(root) = root.clone() else { return };
                    let _ = save.update(cx, |w, cx| w.open_save_plan_dialog(save_md.to_string(), root, window, cx));
                },
            ))
        })
}

fn toast_error(window: &mut Window, cx: &mut App, title: &'static str, message: String) {
    window.push_notification(Notification::error(message).title(title), cx);
}

impl Workspace {
    /// `downloadPlanAsTextFile`: a save dialog in Downloads, named after the plan.
    fn download_plan(&mut self, markdown: &str, window: &mut Window, cx: &mut Context<Self>) {
        let filename = proposed_plan::filename(markdown);
        let contents = proposed_plan::export(markdown);
        let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"));
        let downloads = home.map(|home| std::path::PathBuf::from(home).join("Downloads")).unwrap_or_default();
        let picked = cx.prompt_for_new_path(&downloads, Some(&filename));
        cx.spawn_in(window, async move |_, cx| {
            let Ok(Ok(Some(path))) = picked.await else { return };
            if let Err(error) = std::fs::write(&path, contents) {
                let _ = cx.update(|window, cx| toast_error(window, cx, "Could not download plan", error.to_string()));
            }
        })
        .detach();
    }

    /// `SavePlanToWorkspaceForm`: a path relative to the workspace, then `projects.writeFile`.
    fn open_save_plan_dialog(&mut self, markdown: String, root: String, window: &mut Window, cx: &mut Context<Self>) {
        let filename = proposed_plan::filename(&markdown);
        let contents: Rc<str> = proposed_plan::export(&markdown).into();
        let path = cx.new(|cx| InputState::new(window, cx).default_value(filename.clone()).placeholder(filename));
        let saving = Rc::new(Cell::new(false));
        let focus = Cell::new(true);
        let view = cx.weak_entity();
        let description: SharedString = format!("Enter a path relative to {root}.").into();
        window.open_dialog(cx, move |dialog, window, cx| {
            if focus.replace(false) {
                path.focus_handle(cx).focus(window, cx);
            }
            let t = crate::theme::tokens(cx);
            let busy = saving.get();
            let save = {
                let (path, saving, view, root, contents) =
                    (path.clone(), saving.clone(), view.clone(), root.clone(), contents.clone());
                move |_: &gpui_kit::ClickEvent, window: &mut Window, cx: &mut App| {
                    let relative = path.read(cx).value().trim().to_owned();
                    if relative.is_empty() {
                        window.push_notification(Notification::warning("").title("Enter a workspace path"), cx);
                        return;
                    }
                    saving.set(true);
                    window.refresh();
                    let (root, contents, saving) = (root.clone(), contents.clone(), saving.clone());
                    let _ = view.update(cx, |w, cx| w.save_plan(root, relative, contents, saving, window, cx));
                }
            };
            dialog
                .entrance(DialogEntrance::None)
                .w(px(576.))
                .overlay_closable(!busy)
                .keyboard(!busy)
                .title("Save plan to workspace")
                .child(div().text_sm().text_color(t.muted_foreground).child(description.clone()))
                .child(
                    v_flex()
                        .mt(px(16.))
                        .gap(px(6.))
                        .child(
                            div()
                                .text_xs()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(t.foreground)
                                .child("Workspace path"),
                        )
                        .child(Input::new(&path).disabled(busy)),
                )
                .footer(
                    DialogFooter::new()
                        .child(
                            Button::new("plan-save-cancel")
                                .outline()
                                .small()
                                .label("Cancel")
                                .disabled(busy)
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            Button::new("plan-save")
                                .primary()
                                .small()
                                .label(if busy { "Saving..." } else { "Save" })
                                .disabled(busy)
                                .on_click(save),
                        ),
                )
        });
    }

    fn save_plan(
        &mut self,
        cwd: String,
        relative_path: String,
        contents: Rc<str>,
        saving: Rc<Cell<bool>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.client() else {
            saving.set(false);
            return;
        };
        let input = json!({ "cwd": cwd, "relativePath": relative_path, "contents": &*contents });
        cx.spawn_in(window, async move |_, cx| {
            let result = client.request("projects.writeFile", input).await;
            let _ = cx.update(|window, cx| {
                saving.set(false);
                match result {
                    Ok(value) => {
                        window.close_dialog(cx);
                        let saved = value["relativePath"].as_str().unwrap_or(&relative_path).to_owned();
                        window.push_notification(Notification::success(saved).title("Plan saved to workspace"), cx);
                    }
                    Err(error) => {
                        window.refresh();
                        toast_error(window, cx, "Could not save plan", error.to_string());
                    }
                }
            });
        })
        .detach();
    }
}
