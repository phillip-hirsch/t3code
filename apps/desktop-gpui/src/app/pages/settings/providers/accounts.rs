//! An instance's account and usage (`ProviderAuthenticationSection.tsx` and friends). Owned by
//! the provider accounts port.

use gpui_kit::{AnyElement, Context, Window};

use crate::app::Workspace;

impl Workspace {
    /// The account rows for one instance's Setup section, or nothing.
    pub(super) fn render_provider_account(
        &mut self,
        _instance_id: &str,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<AnyElement> {
        None
    }
}
