//! App icons layered over gpui-kit's Lucide catalog.

use std::borrow::Cow;

use gpui_kit::{AssetSource, Hsla, Result, SharedString, rgb};

const APP_ICONS: &[(&str, &[u8])] = &[
    ("icons/app/claude.svg", include_bytes!("../assets/icons/claude.svg")),
    ("icons/app/openai.svg", include_bytes!("../assets/icons/openai.svg")),
    ("icons/app/grok.svg", include_bytes!("../assets/icons/grok.svg")),
    ("icons/app/cursor.svg", include_bytes!("../assets/icons/cursor.svg")),
    ("icons/app/opencode.svg", include_bytes!("../assets/icons/opencode.svg")),
    ("icons/app/acp.svg", include_bytes!("../assets/icons/acp.svg")),
    ("icons/app/send.svg", include_bytes!("../assets/icons/send.svg")),
    // Lucide icons outside gpui-kit's default set, at the web's 1.75 stroke.
    ("icons/paperclip.svg", include_bytes!("../assets/icons/lucide/paperclip.svg")),
    ("icons/corner-up-right.svg", include_bytes!("../assets/icons/lucide/corner-up-right.svg")),
    ("icons/shield.svg", include_bytes!("../assets/icons/lucide/shield.svg")),
    ("icons/shield-off.svg", include_bytes!("../assets/icons/lucide/shield-off.svg")),
    ("icons/lock.svg", include_bytes!("../assets/icons/lucide/lock.svg")),
    ("icons/pen-line.svg", include_bytes!("../assets/icons/lucide/pen-line.svg")),
    ("icons/sparkles.svg", include_bytes!("../assets/icons/lucide/sparkles.svg")),
    ("icons/square-pen.svg", include_bytes!("../assets/icons/lucide/square-pen.svg")),
    ("icons/terminal.svg", include_bytes!("../assets/icons/lucide/terminal.svg")),
    ("icons/wrench.svg", include_bytes!("../assets/icons/lucide/wrench.svg")),
    ("icons/hammer.svg", include_bytes!("../assets/icons/lucide/hammer.svg")),
    ("icons/x.svg", include_bytes!("../assets/icons/lucide/x.svg")),
];

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some((_, bytes)) = APP_ICONS.iter().find(|(name, _)| *name == path) {
            return Ok(Some(Cow::Borrowed(bytes)));
        }
        gpui_kit::assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut names = gpui_kit::assets::Assets.list(path)?;
        names.extend(APP_ICONS.iter().map(|(name, _)| *name).filter(|name| name.starts_with(path)).map(Into::into));
        Ok(names)
    }
}

/// A provider's logo and its tint, as `PROVIDER_ICON_BY_PROVIDER` in
/// `apps/web/src/components/chat/ProviderInstanceIcon.tsx`. `None` tint means
/// the current foreground. Unknown drivers have no logo yet.
pub fn provider_icon(driver: &str) -> Option<(&'static str, Option<Hsla>)> {
    match driver {
        "claudeAgent" => Some(("icons/app/claude.svg", Some(rgb(0xd97757).into()))),
        "codex" => Some(("icons/app/openai.svg", None)),
        "grok" => Some(("icons/app/grok.svg", None)),
        "cursor" => Some(("icons/app/cursor.svg", None)),
        "opencode" => Some(("icons/app/opencode.svg", None)),
        "acpRegistry" => Some(("icons/app/acp.svg", None)),
        _ => None,
    }
}
