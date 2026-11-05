//! Unsent drafts in the sidebar (`SidebarDraftBlock.tsx`, `lib/discardComposerDraft.ts`): a row per
//! draft thread with content, the "Unsent draft" marker on thread rows, and discarding with undo.

use gpui_kit::assets::IconName;
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::{Icon, h_flex, v_flex};
use gpui_kit::{
    AnyElement, ClipboardItem, Context, FontWeight, Hsla, InteractiveElement, IntoElement, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, prelude::FluentBuilder as _, px,
};

use super::actions::UndoAction;
use super::{ROW_GROUP, icon, project_icon, text_sm, text_xs, tooltip};
use crate::app::composer::ComposerDraft;
use crate::app::{Target, Workspace};
use crate::model::ProjectShell;
use crate::theme::Tokens;

/// What a discard took, for its undo.
pub(super) struct Discarded {
    target: Target,
    prompt: String,
    attachments: Vec<crate::app::attachments::Attachment>,
}

/// A draft's prompt and attachment count (`composerDraftHasUserContent`).
fn content(draft: &ComposerDraft) -> (&str, usize) {
    match draft {
        ComposerDraft::Prompt(prompt, attachments) => (prompt, attachments.len()),
        ComposerDraft::Question(question) => (&question.prompt, 0),
    }
}

fn has_content(draft: &ComposerDraft) -> bool {
    let (prompt, attachments) = content(draft);
    !prompt.trim().is_empty() || attachments > 0
}

/// The row's text: the prompt's first line with context links as their labels, else
/// "N attachments" (`SidebarDraftRow`).
fn preview(prompt: &str, attachments: usize) -> String {
    // Only the first line shows, so chips past it are never looked at.
    let first = prompt.trim_start().split('\n').next().unwrap_or_default();
    let mut text = first.to_owned();
    for (range, chip) in crate::composer_text::chips(first, |_| false).into_iter().rev() {
        if let crate::composer_text::Chip::Context { label, .. } = chip {
            text.replace_range(range, &label);
        }
    }
    let line = text.trim().split('\n').next().unwrap_or_default().to_owned();
    if !line.is_empty() {
        return line;
    }
    format!("{attachments} attachment{}", if attachments == 1 { "" } else { "s" })
}

/// Keeps `order` to the `live` drafts, newest first: a draft joins at the top when it first
/// appears and keeps its place after, like the web's sort by `createdAt`.
pub(super) fn order_drafts(order: &mut Vec<String>, live: &[String]) {
    order.retain(|id| live.contains(id));
    for id in live.iter().rev() {
        if !order.contains(id) {
            order.insert(0, id.clone());
        }
    }
}

/// One draft row: its project, and the preview of a draft that is off screen.
pub(super) struct DraftRow {
    pub(super) project_id: String,
    pub(super) preview: Option<SharedString>,
}

impl Workspace {
    /// The draft rows in sidebar order: the open draft, and the other drafts with content.
    pub(super) fn draft_rows(&mut self) -> Vec<DraftRow> {
        let route = match &self.target {
            Some(Target::Draft { project_id, .. }) => Some(project_id.clone()),
            _ => None,
        };
        let mut previews: Vec<(String, SharedString)> = self
            .composer_drafts
            .iter()
            .filter(|(_, draft)| has_content(draft))
            .filter_map(|(target, draft)| match target {
                Target::Draft { project_id, .. } => {
                    let (prompt, attachments) = content(draft);
                    Some((project_id.clone(), preview(prompt, attachments).into()))
                }
                Target::Thread { .. } => None,
            })
            .collect();
        let live: Vec<String> = route.iter().cloned().chain(previews.iter().map(|(id, _)| id.clone())).collect();
        order_drafts(&mut self.sidebar.draft_order, &live);
        self.sidebar
            .draft_order
            .iter()
            .map(|project_id| {
                let preview = previews.iter().position(|(id, _)| id == project_id).map(|at| previews.remove(at).1);
                DraftRow { project_id: project_id.clone(), preview }
            })
            .collect()
    }

    /// `useThreadHasUnsentDraft`, for a thread that is not on screen.
    pub(super) fn has_unsent_draft(&self, thread_id: &str) -> bool {
        // Drafts are few; scanning them saves building a key per sidebar row.
        self.composer_drafts
            .iter()
            .any(|(target, draft)| matches!(target, Target::Thread { id, .. } if id == thread_id) && has_content(draft))
    }

    /// `discardComposerDraft`: clears the draft and offers to undo it.
    pub(super) fn discard_draft(&mut self, target: Target, window: &mut Window, cx: &mut Context<Self>) {
        let (prompt, attachments) = if self.target.as_ref() == Some(&target) {
            let prompt = self.composer.read(cx).value().to_string();
            let attachments = std::mem::take(&mut self.attachments);
            self.set_composer_text("", window, cx);
            (prompt, attachments)
        } else {
            match self.composer_drafts.remove(&target) {
                Some(ComposerDraft::Prompt(prompt, attachments)) => (prompt, attachments),
                Some(ComposerDraft::Question(mut question)) => {
                    let prompt = std::mem::take(&mut question.prompt);
                    self.composer_drafts.insert(target.clone(), ComposerDraft::Question(question));
                    (prompt, Vec::new())
                }
                None => return,
            }
        };
        if prompt.trim().is_empty() && attachments.is_empty() {
            return;
        }
        self.set_undo(UndoAction::Discarded, Vec::new(), Some(Discarded { target, prompt, attachments }), cx);
        cx.notify();
    }

    /// Puts a discarded draft back, unless its target has new content since.
    pub(super) fn restore_discarded(&mut self, discarded: Discarded, window: &mut Window, cx: &mut Context<Self>) {
        let Discarded { target, prompt, attachments } = discarded;
        let fresh = if self.target.as_ref() == Some(&target) {
            !self.composer.read(cx).value().trim().is_empty() || !self.attachments.is_empty()
        } else {
            self.composer_drafts.get(&target).is_some_and(has_content)
        };
        if fresh {
            self.error = Some("Failed to restore draft: The draft has new content.".into());
            cx.notify();
            return;
        }
        if self.target.as_ref() == Some(&target) {
            self.attachments = attachments;
            self.set_composer_text(prompt, window, cx);
        } else {
            match self.composer_drafts.get_mut(&target) {
                Some(ComposerDraft::Question(question)) => question.prompt = prompt,
                _ => {
                    self.composer_drafts.insert(target, ComposerDraft::Prompt(prompt, attachments));
                }
            }
        }
        self.notify_sidebar(cx);
        cx.notify();
    }

    /// The draft row's right-click menu (`buildDraftActionMenuItems`).
    fn draft_menu(
        &self,
        project: Option<&ProjectShell>,
        project_id: &str,
        cx: &mut Context<Self>,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        let view = cx.weak_entity();
        let path = project.map(|p| p.workspace_root.clone());
        let has_project = project.is_some();
        let draft = self.draft_target(project_id);
        move |menu, window, cx| {
            let path = path.clone();
            let mut menu =
                menu.min_w(px(200.)).submenu_with_icon(
                    Some(Icon::new(IconName::Copy)),
                    "Copy",
                    window,
                    cx,
                    move |menu, _, _| match &path {
                        Some(path) => {
                            let path = path.clone();
                            menu.item(PopupMenuItem::new("Path").icon(Icon::new(IconName::Folder)).on_click(
                                move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(path.clone())),
                            ))
                        }
                        None => menu,
                    },
                );
            if has_project {
                let view = view.clone();
                menu = menu.item(PopupMenuItem::new("Project settings").icon(Icon::new(IconName::Settings)).on_click(
                    move |_, _, cx| {
                        let _ = view.update(cx, |this, cx| this.open_settings("projects", cx));
                    },
                ));
            }
            let (view, target) = (view.clone(), draft.clone());
            menu.separator().item(PopupMenuItem::new("Discard draft").icon(Icon::new(IconName::Trash)).on_click(
                move |_, window, cx| {
                    let _ = view.update(cx, |this, cx| {
                        if let Some(target) = target.clone() {
                            this.discard_draft(target, window, cx)
                        }
                    });
                },
            ))
        }
    }

    /// A draft row (`SidebarDraftRow`): the open draft reads "New thread", another one its preview.
    pub(super) fn render_draft(
        &self,
        row: &DraftRow,
        project: Option<&ProjectShell>,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let sb = tokens.sidebar;
        let active = row.preview.is_none();
        let project_id = row.project_id.clone();
        let target = self.draft_target(&project_id);
        let discard = div()
            .id(SharedString::from(format!("draft-discard-{project_id}")))
            .flex()
            .items_center()
            .px(px(4.))
            .rounded(px(6.))
            .cursor_pointer()
            .invisible()
            .group_hover(ROW_GROUP, |s| s.visible())
            .text_color(tokens.muted_foreground)
            .hover(|s| s.text_color(tokens.foreground))
            .tooltip(tooltip("Discard draft"))
            .on_click(cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                if let Some(target) = target.clone() {
                    this.discard_draft(target, window, cx)
                };
            }))
            .child(Icon::new(IconName::X).size(px(12.)));
        let open_id = project_id.clone();
        div()
            .py(px(2.))
            .child(
                div()
                    .id(SharedString::from(format!("draft-{project_id}")))
                    .group(ROW_GROUP)
                    .rounded(px(6.))
                    .cursor_pointer()
                    .map(|el| if active { el.bg(sb.row_active) } else { draft_surface(el, tokens) })
                    .on_click(cx.listener(move |this, _, window, cx| this.start_draft(open_id.clone(), window, cx)))
                    .child(
                        v_flex()
                            .h(px(78.))
                            .px(px(10.))
                            .py(px(8.))
                            .child(
                                h_flex()
                                    .h(px(20.))
                                    .gap(px(6.))
                                    .child(icon(IconName::SquarePen, 12., tokens.warning_foreground))
                                    .children(project.map(|p| project_icon(p, tokens, false, 16., self.favicon(p))))
                                    .child(
                                        text_xs(div())
                                            .min_w_0()
                                            .flex_1()
                                            .truncate()
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(sb.secondary_label)
                                            .children(project.map(|p| p.title.clone())),
                                    )
                                    .child(h_flex().ml_auto().h(px(20.)).min_w(px(20.)).justify_end().child(discard)),
                            )
                            .child(
                                text_sm(div())
                                    .mt(px(2.))
                                    .truncate()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(sb.foreground.opacity(0.9))
                                    .child(row.preview.clone().unwrap_or_else(|| "New thread".into())),
                            ),
                    )
                    .context_menu(self.draft_menu(project, &row.project_id, cx)),
            )
            .into_any_element()
    }
}

/// `draftSurfaceClassName`: a warning tint that deepens on hover.
pub(super) fn draft_surface<E: gpui_kit::Styled + InteractiveElement>(el: E, tokens: &Tokens) -> E {
    let warning: Hsla = tokens.warning;
    el.bg(warning.opacity(0.04)).hover(move |s| s.bg(warning.opacity(0.08)))
}

/// The "Unsent draft" pen before a row's title (`draftIndicator`).
pub(super) fn unsent_draft_marker(thread_id: &str, tokens: &Tokens) -> AnyElement {
    div()
        .id(SharedString::from(format!("unsent-draft-{thread_id}")))
        .flex_none()
        .tooltip(tooltip("Unsent draft"))
        .child(icon(IconName::SquarePen, 12., tokens.warning_foreground))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_preview_is_the_first_line_or_the_attachment_count() {
        assert_eq!(preview("  Fix the build\nthen the tests", 2), "Fix the build");
        assert_eq!(preview("   ", 1), "1 attachment");
        assert_eq!(preview("", 3), "3 attachments");
    }

    #[test]
    fn drafts_join_at_the_top_and_keep_their_place() {
        let mut order = Vec::new();
        order_drafts(&mut order, &["a".into()]);
        order_drafts(&mut order, &["a".into(), "b".into()]);
        assert_eq!(order, ["b", "a"]);
        order_drafts(&mut order, &["a".into(), "b".into()]);
        assert_eq!(order, ["b", "a"]);
        order_drafts(&mut order, &["a".into()]);
        assert_eq!(order, ["a"]);
    }
}
