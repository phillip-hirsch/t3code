//! App icons layered over gpui-kit's Lucide catalog.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, PoisonError, RwLock};

use gpui_kit::{AssetSource, Hsla, Result, SharedString, rgb};

const APP_ICONS: &[(&str, &[u8])] = &[
    ("icons/app/claude.svg", include_bytes!("../assets/icons/claude.svg")),
    ("icons/app/openai.svg", include_bytes!("../assets/icons/openai.svg")),
    ("icons/app/grok.svg", include_bytes!("../assets/icons/grok.svg")),
    ("icons/app/cursor.svg", include_bytes!("../assets/icons/cursor.svg")),
    ("icons/app/opencode.svg", include_bytes!("../assets/icons/opencode.svg")),
    ("icons/app/acp.svg", include_bytes!("../assets/icons/acp.svg")),
    ("icons/app/muse.svg", include_bytes!("../assets/icons/muse.svg")),
    ("icons/app/pi.svg", include_bytes!("../assets/icons/pi.svg")),
    ("icons/app/fx.svg", include_bytes!("../assets/icons/fx.svg")),
    // Text generator brand marks from Simple Icons (CC0), as the web's `Icons.tsx` draws them.
    ("icons/app/anthropic.svg", include_bytes!("../assets/icons/anthropic.svg")),
    ("icons/app/openrouter.svg", include_bytes!("../assets/icons/openrouter.svg")),
    ("icons/app/ollama.svg", include_bytes!("../assets/icons/ollama.svg")),
    ("icons/app/send.svg", include_bytes!("../assets/icons/send.svg")),
    ("icons/app/t3-wordmark.svg", include_bytes!("../assets/icons/t3-wordmark.svg")),
    ("icons/app/t3.svg", include_bytes!("../assets/icons/t3.svg")),
    ("icons/app/snap-shot.svg", include_bytes!("../assets/icons/snap-shot.svg")),
    ("icons/app/mac-screen-recording.svg", include_bytes!("../assets/icons/mac-screen-recording.svg")),
    ("icons/app/mac-accessibility.svg", include_bytes!("../assets/icons/mac-accessibility.svg")),
    ("icons/app/empty-media-back.svg", include_bytes!("../assets/icons/empty-media-back.svg")),
    ("icons/app/empty-media-back-border.svg", include_bytes!("../assets/icons/empty-media-back-border.svg")),
    ("icons/app/browser.svg", include_bytes!("../assets/icons/browser.svg")),
    ("icons/app/screen-rotation.svg", include_bytes!("../assets/icons/screen-rotation.svg")),
    ("icons/app/viewport-resize-southeast.svg", include_bytes!("../assets/icons/viewport-resize-southeast.svg")),
    ("icons/app/viewport-resize-southwest.svg", include_bytes!("../assets/icons/viewport-resize-southwest.svg")),
    ("icons/app/apple.svg", include_bytes!("../assets/icons/apple.svg")),
    ("icons/app/android.svg", include_bytes!("../assets/icons/android.svg")),
    ("icons/app/linux.svg", include_bytes!("../assets/icons/linux.svg")),
    ("icons/app/mac-mini.svg", include_bytes!("../assets/icons/mac-mini.svg")),
    ("icons/app/mac-studio.svg", include_bytes!("../assets/icons/mac-studio.svg")),
    ("icons/app/onepassword.svg", include_bytes!("../assets/icons/onepassword.svg")),
    ("icons/app/source-control/azure-devops.svg", include_bytes!("../assets/icons/source-control/azure-devops.svg")),
    ("icons/app/source-control/bitbucket.svg", include_bytes!("../assets/icons/source-control/bitbucket.svg")),
    ("icons/app/source-control/cursor-origin.svg", include_bytes!("../assets/icons/source-control/cursor-origin.svg")),
    ("icons/app/source-control/forgejo.svg", include_bytes!("../assets/icons/source-control/forgejo.svg")),
    ("icons/app/source-control/git.svg", include_bytes!("../assets/icons/source-control/git.svg")),
    ("icons/app/source-control/gitcafe.svg", include_bytes!("../assets/icons/source-control/gitcafe.svg")),
    ("icons/app/source-control/github.svg", include_bytes!("../assets/icons/source-control/github.svg")),
    ("icons/app/source-control/gitlab.svg", include_bytes!("../assets/icons/source-control/gitlab.svg")),
    ("icons/app/source-control/jj.svg", include_bytes!("../assets/icons/source-control/jj.svg")),
    // @pierre/diffs' change icons (Apache-2.0), as the web's diff headers draw them.
    ("icons/app/diffs/file-code.svg", include_bytes!("../assets/icons/diffs/file-code.svg")),
    ("icons/app/diffs/symbol-added.svg", include_bytes!("../assets/icons/diffs/symbol-added.svg")),
    ("icons/app/diffs/symbol-deleted.svg", include_bytes!("../assets/icons/diffs/symbol-deleted.svg")),
    ("icons/app/diffs/symbol-modified.svg", include_bytes!("../assets/icons/diffs/symbol-modified.svg")),
    ("icons/app/diffs/symbol-moved.svg", include_bytes!("../assets/icons/diffs/symbol-moved.svg")),
    ("icons/app/pr-branch-mark.svg", include_bytes!("../assets/icons/pr-branch-mark.svg")),
    ("icons/app/vscode.svg", include_bytes!("../assets/icons/vscode.svg")),
    ("icons/app/file-explorer.svg", include_bytes!("../assets/icons/file-explorer.svg")),
    ("icons/app/github.svg", include_bytes!("../assets/icons/github.svg")),
    // Lucide's star with `fill-current`.
    ("icons/app/star-filled.svg", include_bytes!("../assets/icons/star-filled.svg")),
    // Lucide's play with `fill-current`.
    ("icons/app/play-filled.svg", include_bytes!("../assets/icons/play-filled.svg")),
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
    "archive",
    "arrow-up-right",
    "archive-x",
    "file-x",
    "history",
    "picture-in-picture-2",
    "radio-tower",
    "arrow-down-up",
    "arrow-right-left",
    "app-window",
    "blocks",
    "bot",
    "box",
    "brain",
    "building",
    "calendar-clock",
    "calendar-arrow-down",
    "calendar-arrow-up",
    "bug",
    "chart-no-axes-column",
    "chevrons-left-right-ellipsis",
    "check",
    "clock-3",
    "chevron-down",
    "circle-arrow-up",
    "circle-alert",
    "circle-check",
    "chevron-right",
    "chevrons-down-up",
    "chevrons-up-down",
    "circle-dashed",
    "circle-dot",
    "circle-dot-dashed",
    "circle-slash",
    "circle-x",
    "clock",
    "cloud",
    "cloud-download",
    "cloud-upload",
    "code-xml",
    "copy",
    "columns-2",
    "corner-left-up",
    "corner-up-right",
    "download",
    "file-diff",
    "flask-conical",
    "flask-conical-off",
    "file-braces",
    "external-link",
    "eye",
    "eye-off",
    "ellipsis",
    "folder",
    "folder-closed",
    "folder-git",
    "folder-git-2",
    "folder-plus",
    "folder-tree",
    "gauge",
    "git-branch",
    "git-pull-request",
    "git-merge",
    "git-fork",
    "git-branch-plus",
    "git-commit-horizontal",
    "git-pull-request-arrow",
    "git-pull-request-closed",
    "git-pull-request-draft",
    "hammer",
    "key-round",
    "keyboard",
    "info",
    "hash",
    "image",
    "laptop",
    "layers",
    "list-checks",
    "link",
    "link-2",
    "unlink-2",
    "list-filter",
    "list-plus",
    "list-todo",
    "lock",
    "lock-open",
    "maximize-2",
    "mail-open",
    "message-circle",
    "message-circle-question-mark",
    "message-square-text",
    "messages-square",
    "message-square-plus",
    "minimize-2",
    "monitor",
    "package",
    "panel-bottom",
    "panel-left",
    "panel-left-close",
    "panel-right",
    "panels-top-left",
    "paperclip",
    "pencil",
    "pen-line",
    "pilcrow",
    "pin",
    "quote",
    "rotate-ccw",
    "rotate-cw",
    "scan",
    "plug",
    "plug-2",
    "refresh-cw",
    "search",
    "pin-off",
    "rotate-ccw-clock",
    "play",
    "plus",
    "rows-3",
    "server",
    "shield",
    "shield-check",
    "shield-off",
    "shield-question-mark",
    "sliders-horizontal",
    "smartphone",
    "sparkles",
    "square-arrow-out-up-right",
    "square",
    "square-menu",
    "square-pen",
    "square-split-horizontal",
    "square-split-vertical",
    "square-terminal",
    "route",
    "grip-vertical",
    "qr-code",
    "terminal",
    "trash-2",
    "trending-down",
    "trending-up",
    "triangle-alert",
    "user-check",
    "user-lock",
    "trash",
    "text-wrap",
    "text-search",
    "timer",
    "unplug",
    "user-plus",
    "user-round",
    "user-round-x",
    "users",
    "wrench",
    "x",
    "zap",
];

/// Images fetched at runtime (e.g. ACP Registry icons), served under `remote/…` so `svg()` can tint them.
/// A panic while it is held can't leave it half-updated (one insert), so poisoning is ignored
/// rather than failing every icon load after it.
static REMOTE: RwLock<Option<HashMap<String, Arc<[u8]>>>> = RwLock::new(None);

pub fn put_remote(path: &str, bytes: Arc<[u8]>) {
    REMOTE.write().unwrap_or_else(PoisonError::into_inner).get_or_insert_default().insert(path.to_owned(), bytes);
}

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some((_, bytes)) =
            APP_ICONS.iter().chain(LUCIDE_ICONS).chain(crate::file_icons::SVGS).find(|(name, _)| *name == path)
        {
            return Ok(Some(Cow::Borrowed(bytes)));
        }
        if let Some(bytes) = REMOTE.read().unwrap_or_else(PoisonError::into_inner).as_ref().and_then(|r| r.get(path)) {
            return Ok(Some(Cow::Owned(bytes.to_vec())));
        }
        // The full catalog: a project icon can be any Lucide icon (`ProjectIconPickerDialog`).
        gpui_kit::assets::AllAssets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut names = gpui_kit::assets::AllAssets.list(path)?;
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

/// A provider's logo and its tint, as `PROVIDER_ICON_BY_PROVIDER` and the provider packages'
/// `icon` in `apps/web/src/components/chat/ProviderInstanceIcon.tsx`. `None` tint means
/// the current foreground. Unknown drivers have no logo yet. Muse's blue gradient draws in
/// its flat fill.
pub fn provider_icon(driver: &str) -> Option<(&'static str, Option<Hsla>)> {
    match driver {
        "claudeAgent" => Some(("icons/app/claude.svg", Some(rgb(0xd97757).into()))),
        "codex" => Some(("icons/app/openai.svg", None)),
        "grok" => Some(("icons/app/grok.svg", None)),
        "cursor" => Some(("icons/app/cursor.svg", None)),
        "opencode" => Some(("icons/app/opencode.svg", None)),
        "acpRegistry" => Some(("icons/app/acp.svg", None)),
        "muse" => Some(("icons/app/muse.svg", Some(rgb(0x0081fb).into()))),
        "pi" => Some(("icons/app/pi.svg", None)),
        "fx" => Some(("icons/app/fx.svg", None)),
        // Text generators, by the service they reach (`textGeneratorBrand`).
        "openai" => Some(("icons/app/openai.svg", None)),
        "anthropic" => Some(("icons/app/anthropic.svg", None)),
        "openrouter" => Some(("icons/app/openrouter.svg", None)),
        "ollama" => Some(("icons/app/ollama.svg", None)),
        _ => None,
    }
}
