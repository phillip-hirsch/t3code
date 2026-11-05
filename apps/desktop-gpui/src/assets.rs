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
    ("icons/app/t3-wordmark.svg", include_bytes!("../assets/icons/t3-wordmark.svg")),
];

/// Lucide icons outside gpui-kit's default bundle, or redrawn at the web's 1.75
/// stroke, served at their catalog paths so `gpui_kit::assets::IconName` resolves them.
macro_rules! lucide {
    ($($name:literal),* $(,)?) => {
        &[$((concat!("icons/", $name, ".svg"), include_bytes!(concat!("../assets/icons/lucide/", $name, ".svg")))),*]
    };
}

const LUCIDE_ICONS: &[(&str, &[u8])] = lucide![
    "alarm-clock",
    "alarm-clock-off",
    "bot",
    "chart-no-axes-column",
    "chevron-down",
    "circle-dashed",
    "circle-dot-dashed",
    "cloud",
    "corner-left-up",
    "corner-up-right",
    "folder",
    "folder-git",
    "folder-git-2",
    "folder-plus",
    "git-branch",
    "git-pull-request-arrow",
    "hammer",
    "laptop",
    "list-filter",
    "lock",
    "message-circle-question-mark",
    "monitor",
    "panel-bottom",
    "panel-left",
    "panel-left-close",
    "panel-right",
    "paperclip",
    "pen-line",
    "pin",
    "server",
    "shield",
    "shield-off",
    "shield-question-mark",
    "sparkles",
    "square-menu",
    "square-pen",
    "terminal",
    "user-round",
    "wrench",
    "x",
];

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some((_, bytes)) = APP_ICONS.iter().chain(LUCIDE_ICONS).find(|(name, _)| *name == path) {
            return Ok(Some(Cow::Borrowed(bytes)));
        }
        gpui_kit::assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut names = gpui_kit::assets::Assets.list(path)?;
        names.extend(
            APP_ICONS
                .iter()
                .chain(LUCIDE_ICONS)
                .map(|(name, _)| *name)
                .filter(|name| name.starts_with(path))
                .map(Into::into),
        );
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
