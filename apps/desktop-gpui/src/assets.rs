//! App icons layered over gpui-kit's Lucide catalog.

use std::borrow::Cow;

use gpui_kit::{AssetSource, Hsla, Result, SharedString, rgb};

const APP_ICONS: &[(&str, &[u8])] = &[
    ("icons/app/claude.svg", include_bytes!("../assets/icons/claude.svg")),
    ("icons/app/openai.svg", include_bytes!("../assets/icons/openai.svg")),
    ("icons/app/t3-wordmark.svg", include_bytes!("../assets/icons/t3-wordmark.svg")),
];

/// Lucide icons outside gpui-kit's default bundle, served at their catalog
/// paths so `gpui_kit::assets::IconName` resolves them.
const LUCIDE_ICONS: &[(&str, &[u8])] = &[
    ("icons/list-filter.svg", include_bytes!("../assets/icons/lucide/list-filter.svg")),
    ("icons/folder-plus.svg", include_bytes!("../assets/icons/lucide/folder-plus.svg")),
    ("icons/square-pen.svg", include_bytes!("../assets/icons/lucide/square-pen.svg")),
    ("icons/chart-no-axes-column.svg", include_bytes!("../assets/icons/lucide/chart-no-axes-column.svg")),
    ("icons/git-pull-request-arrow.svg", include_bytes!("../assets/icons/lucide/git-pull-request-arrow.svg")),
    ("icons/folder-git-2.svg", include_bytes!("../assets/icons/lucide/folder-git-2.svg")),
    ("icons/circle-dashed.svg", include_bytes!("../assets/icons/lucide/circle-dashed.svg")),
    ("icons/circle-dot-dashed.svg", include_bytes!("../assets/icons/lucide/circle-dot-dashed.svg")),
    ("icons/shield-question-mark.svg", include_bytes!("../assets/icons/lucide/shield-question-mark.svg")),
    (
        "icons/message-circle-question-mark.svg",
        include_bytes!("../assets/icons/lucide/message-circle-question-mark.svg"),
    ),
    ("icons/alarm-clock.svg", include_bytes!("../assets/icons/lucide/alarm-clock.svg")),
    ("icons/alarm-clock-off.svg", include_bytes!("../assets/icons/lucide/alarm-clock-off.svg")),
    ("icons/user-round.svg", include_bytes!("../assets/icons/lucide/user-round.svg")),
    ("icons/pin.svg", include_bytes!("../assets/icons/lucide/pin.svg")),
    ("icons/x.svg", include_bytes!("../assets/icons/lucide/x.svg")),
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
        _ => None,
    }
}
