//! App icons layered over gpui-kit's Lucide catalog.

use std::borrow::Cow;

use gpui_kit::{AssetSource, Hsla, Result, SharedString, rgb};

const APP_ICONS: &[(&str, &[u8])] = &[
    ("icons/app/claude.svg", include_bytes!("../assets/icons/claude.svg")),
    ("icons/app/openai.svg", include_bytes!("../assets/icons/openai.svg")),
];

/// Lucide icons for the window shell, at the web's 1.75 stroke.
macro_rules! shell_icons {
    ($($name:literal),* $(,)?) => {
        &[$((concat!("icons/shell/", $name, ".svg"), include_bytes!(concat!("../assets/icons/shell/", $name, ".svg")))),*]
    };
}

const SHELL_ICONS: &[(&str, &[u8])] = shell_icons![
    "bot",
    "chevron-down",
    "cloud",
    "corner-left-up",
    "folder",
    "folder-git",
    "folder-git-2",
    "git-branch",
    "laptop",
    "monitor",
    "panel-bottom",
    "panel-left",
    "panel-left-close",
    "panel-right",
    "server",
    "square-menu",
];

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some((_, bytes)) = APP_ICONS.iter().chain(SHELL_ICONS).find(|(name, _)| *name == path) {
            return Ok(Some(Cow::Borrowed(bytes)));
        }
        gpui_kit::assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut names = gpui_kit::assets::Assets.list(path)?;
        names.extend(
            APP_ICONS
                .iter()
                .chain(SHELL_ICONS)
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
        _ => None,
    }
}
