//! Add provider (`AddProviderInstanceDialog.tsx`). Owned by the add-provider port.

use gpui_kit::{Context, Window};

use crate::app::Workspace;

impl Workspace {
    /// Opens the Add provider dialog.
    pub(super) fn open_add_provider_dialog(&mut self, _: &mut Window, _: &mut Context<Self>) {}
}
