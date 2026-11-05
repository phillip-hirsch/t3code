//! A draft attachment's preview (`ComposerFilePreviewDialog`, `AttachmentFilePreview.tsx`):
//! its text or image, with copy, save and remove. Rich renderers (markdown, tables,
//! PDFs, office files) stay in the web; other files get its "No preview" notice.

use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::dialog::DialogEntrance;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    ClipboardItem, Context, FontWeight, Image, InteractiveElement as _, IntoElement, ObjectFit, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, StyledImage as _, Window, div, img, px, relative,
    uniform_list,
};

use super::Workspace;

/// `FILE_TEXT_PREVIEW_MAX_BYTES`.
const TEXT_PREVIEW_MAX_BYTES: usize = 1024 * 1024;
const LINE_HEIGHT: f32 = 20.;

/// What the dialog shows.
enum Body {
    /// The lines, split once, and whether the file went past the preview limit.
    Text(Rc<Vec<SharedString>>, bool),
    Image(Arc<Image>),
    None,
}

/// A file previews as text when its first megabyte is UTF-8 without NULs.
fn text_lines(bytes: &[u8]) -> Option<(Vec<SharedString>, bool)> {
    let truncated = bytes.len() > TEXT_PREVIEW_MAX_BYTES;
    let head = &bytes[..bytes.len().min(TEXT_PREVIEW_MAX_BYTES)];
    let text = match std::str::from_utf8(head) {
        Ok(text) => text,
        // A cut through a multi-byte character at the limit.
        Err(error) if truncated && error.error_len().is_none() => {
            std::str::from_utf8(&head[..error.valid_up_to()]).ok()?
        }
        Err(_) => return None,
    };
    if text.contains('\0') {
        return None;
    }
    Some((text.lines().map(|line| SharedString::from(line.replace('\t', "    "))).collect(), truncated))
}

impl Workspace {
    /// Opens a composer attachment's preview.
    pub(super) fn open_attachment_preview(&mut self, local_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(attachment) = self.attachments.iter().find(|a| a.local_id() == local_id) else { return };
        let (name, bytes) = (SharedString::from(attachment.name().to_owned()), attachment.bytes().clone());
        let size = super::attachments::format_size(bytes.len() as u64);
        let body = match (attachment.image(), text_lines(&bytes)) {
            (Some(image), _) => Body::Image(image),
            (None, Some((lines, truncated))) => Body::Text(Rc::new(lines), truncated),
            (None, None) => Body::None,
        };
        let extension = attachment.name().rsplit_once('.').map(|(_, e)| e.to_owned()).filter(|e| !e.is_empty());
        let view = cx.weak_entity();
        let local_id = local_id.to_owned();
        let body = Rc::new(body);
        window.open_dialog(cx, move |dialog, window, cx| {
            let t = *crate::theme::tokens(cx);
            let height = (f32::from(window.viewport_size().height) * 0.85).min(832.);
            let action = |id: &'static str, icon: Lucide, label: &'static str| {
                div()
                    .id(id)
                    .size(px(28.))
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .rounded(px(8.))
                    .cursor_pointer()
                    .text_color(t.muted_foreground)
                    .hover(|s| s.bg(t.accent).text_color(t.foreground))
                    .child(Icon::new(icon).size(px(16.)))
                    .tooltip(move |window, cx| Tooltip::new(label).build(window, cx))
            };
            let copy = matches!(*body, Body::Text(..)).then(|| {
                let bytes = bytes.clone();
                action("preview-copy", Lucide::Copy, "Copy contents").on_click(move |_, window, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(String::from_utf8_lossy(&bytes).into_owned()));
                    window.push_notification(Notification::success("").title("Copied to clipboard"), cx);
                })
            });
            let save = {
                let (bytes, name) = (bytes.clone(), name.clone());
                action("preview-save", Lucide::Download, "Save file").on_click(move |_, window, cx| {
                    let downloads = std::env::var_os("USERPROFILE")
                        .or_else(|| std::env::var_os("HOME"))
                        .map(|home| std::path::PathBuf::from(home).join("Downloads"))
                        .unwrap_or_default();
                    let picked = cx.prompt_for_new_path(&downloads, Some(&name));
                    let bytes = bytes.clone();
                    window
                        .spawn(cx, async move |cx| {
                            let Ok(Ok(Some(path))) = picked.await else { return };
                            if let Err(error) = std::fs::write(&path, bytes.as_slice()) {
                                let _ = cx.update(|window, cx| {
                                    let toast = Notification::error(error.to_string()).title("Could not save file");
                                    window.push_notification(toast, cx);
                                });
                            }
                        })
                        .detach();
                })
            };
            let remove = {
                let (view, local_id) = (view.clone(), local_id.clone());
                action("preview-remove", Lucide::Trash, "Remove attachment").on_click(move |_, window, cx| {
                    window.close_dialog(cx);
                    let _ = view.update(cx, |this, cx| this.remove_attachment(&local_id, window, cx));
                })
            };
            let close = action("preview-close", Lucide::X, "Close").on_click(|_, window, cx| window.close_dialog(cx));
            let header = h_flex()
                .h(px(40.))
                .flex_none()
                .gap(px(8.))
                .px(px(12.))
                .border_b_1()
                .border_color(t.border.opacity(0.6))
                .child(
                    h_flex()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .child(div().flex_none().px(px(2.)).text_color(t.muted_foreground).child("Draft"))
                        .child(
                            Icon::new(Lucide::ChevronRight)
                                .size(px(14.))
                                .mx(px(4.))
                                .text_color(t.muted_foreground.opacity(0.6)),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .px(px(2.))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(t.foreground)
                                .child(name.clone()),
                        )
                        .child(div().flex_none().ml(px(8.)).text_color(t.muted_foreground).child(size.clone())),
                )
                .children(copy)
                .child(save)
                .child(remove)
                .child(close);
            let content = match &*body {
                Body::Text(lines, truncated) => {
                    let lines = lines.clone();
                    v_flex()
                        .flex_1()
                        .min_h_0()
                        .child(
                            uniform_list("attachment-preview-lines", lines.len(), move |range, _, _| {
                                range
                                    .map(|index| {
                                        div()
                                            .h(px(LINE_HEIGHT))
                                            .px(px(16.))
                                            .whitespace_nowrap()
                                            .overflow_hidden()
                                            .child(lines[index].clone())
                                    })
                                    .collect::<Vec<_>>()
                            })
                            .flex_1()
                            .py(px(8.))
                            .font_family(t.fonts.mono)
                            .text_xs()
                            .line_height(px(LINE_HEIGHT))
                            .text_color(t.foreground),
                        )
                        .when(*truncated, |el| {
                            el.child(
                                div()
                                    .px(px(16.))
                                    .py(px(8.))
                                    .border_t_1()
                                    .border_color(t.border.opacity(0.6))
                                    .text_xs()
                                    .text_color(t.muted_foreground)
                                    .child("Showing the first 1 MB. Save the file to see all of it."),
                            )
                        })
                        .into_any_element()
                }
                Body::Image(image) => div()
                    .flex_1()
                    .min_h_0()
                    .p(px(16.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(img(image.clone()).max_w(relative(1.)).max_h(relative(1.)).object_fit(ObjectFit::Contain))
                    .into_any_element(),
                Body::None => v_flex()
                    .flex_1()
                    .min_h_0()
                    .items_center()
                    .justify_center()
                    .gap(px(4.))
                    .px(px(24.))
                    .child(div().text_sm().font_weight(FontWeight::MEDIUM).child("No preview for this file"))
                    .child(div().max_w(px(384.)).text_xs().text_color(t.muted_foreground).child(format!(
                        "Save it to open in an app that supports {} files.",
                        extension.as_deref().unwrap_or("this format")
                    )))
                    .into_any_element(),
            };
            dialog
                .entrance(DialogEntrance::None)
                .close_button(false)
                .w(px(896.))
                .p_0()
                .child(v_flex().h(px(height)).overflow_hidden().bg(t.background).child(header).child(content))
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_previews_need_utf8_without_nuls() {
        let (lines, truncated) = text_lines(b"a\tb\r\nc").unwrap();
        assert_eq!(lines, [SharedString::from("a    b"), SharedString::from("c")]);
        assert!(!truncated);
        assert!(text_lines(b"PK\x03\x04\0\0").is_none());
        assert!(text_lines(&[0xff, 0xfe]).is_none());
        let mut big = "é".repeat(TEXT_PREVIEW_MAX_BYTES / 2).into_bytes();
        big.insert(0, b'x');
        let (lines, truncated) = text_lines(&big).unwrap();
        assert!(truncated && lines[0].len() < TEXT_PREVIEW_MAX_BYTES, "a cut character is dropped");
    }
}
