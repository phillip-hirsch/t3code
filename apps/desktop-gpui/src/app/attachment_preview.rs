//! A draft attachment's preview (`ComposerFilePreviewDialog`): `AttachmentFilePreview` with origin
//! "Draft", Remove from draft and Close, in a dialog. The file surfaces' viewers show it by kind
//! (`files::attachment`), reading the draft's bytes; its media and rendered page play from its
//! upload.

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::dialog::DialogEntrance;
use gpui_kit::component::v_flex;
use gpui_kit::{Context, ParentElement as _, Styled as _, Window, px};

use super::Workspace;

/// `max-w-4xl` and `h-[min(85vh,52rem)]`.
const DIALOG_WIDTH: f32 = 896.;
const DIALOG_MAX_HEIGHT: f32 = 832.;

impl Workspace {
    /// Opens a composer attachment's preview.
    pub(super) fn open_attachment_preview(&mut self, local_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.draft_preview_key(local_id) else { return };
        let view = cx.entity();
        window.open_dialog(cx, move |dialog, window, cx| {
            let background = crate::theme::tokens(cx).background;
            let viewport = window.viewport_size();
            let height = (f32::from(viewport.height) * 0.85).min(DIALOG_MAX_HEIGHT);
            // The dialog keeps a margin on each side of a narrow window.
            let width = (f32::from(viewport.width) - 32.).min(DIALOG_WIDTH);
            let body = view.update(cx, |this, cx| this.render_draft_preview(&key, width, window, cx));
            let (closed, closed_key) = (view.clone(), key.clone());
            dialog
                .entrance(DialogEntrance::None)
                .close_button(false)
                .w(px(DIALOG_WIDTH))
                .p_0()
                .on_close(move |_, _, cx| closed.update(cx, |this, cx| this.close_draft_preview(&closed_key, cx)))
                .child(v_flex().h(px(height)).overflow_hidden().bg(background).child(body))
        });
    }
}
