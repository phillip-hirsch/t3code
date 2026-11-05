//! App icons layered over gpui-kit's Lucide catalog.

use std::borrow::Cow;

use gpui_kit::{AssetSource, Hsla, Result, SharedString, rgb};

const APP_ICONS: &[(&str, &[u8])] = &[
    ("icons/app/claude.svg", include_bytes!("../assets/icons/claude.svg")),
    ("icons/app/openai.svg", include_bytes!("../assets/icons/openai.svg")),
    ("icons/app/t3.svg", include_bytes!("../assets/icons/t3.svg")),
    ("icons/app/browser.svg", include_bytes!("../assets/icons/browser.svg")),
    // Lucide icons the timeline uses beyond gpui-kit's default set.
    ("icons/terminal.svg", include_bytes!("../assets/icons/terminal.svg")),
    ("icons/square-pen.svg", include_bytes!("../assets/icons/square-pen.svg")),
    ("icons/smartphone.svg", include_bytes!("../assets/icons/smartphone.svg")),
    ("icons/git-pull-request-arrow.svg", include_bytes!("../assets/icons/git-pull-request-arrow.svg")),
    ("icons/wrench.svg", include_bytes!("../assets/icons/wrench.svg")),
    ("icons/hammer.svg", include_bytes!("../assets/icons/hammer.svg")),
    ("icons/brain.svg", include_bytes!("../assets/icons/brain.svg")),
    ("icons/zap.svg", include_bytes!("../assets/icons/zap.svg")),
    ("icons/message-circle.svg", include_bytes!("../assets/icons/message-circle.svg")),
    ("icons/list-todo.svg", include_bytes!("../assets/icons/list-todo.svg")),
    ("icons/code-xml.svg", include_bytes!("../assets/icons/code-xml.svg")),
    ("icons/minimize-2.svg", include_bytes!("../assets/icons/minimize-2.svg")),
    ("icons/shield.svg", include_bytes!("../assets/icons/shield.svg")),
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
        _ => None,
    }
}
