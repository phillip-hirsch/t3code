//! App icons layered over gpui-kit's Lucide catalog.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

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
    ("icons/app/t3.svg", include_bytes!("../assets/icons/t3.svg")),
    ("icons/app/browser.svg", include_bytes!("../assets/icons/browser.svg")),
    ("icons/app/apple.svg", include_bytes!("../assets/icons/apple.svg")),
    ("icons/app/android.svg", include_bytes!("../assets/icons/android.svg")),
    ("icons/app/linux.svg", include_bytes!("../assets/icons/linux.svg")),
    ("icons/app/mac-mini.svg", include_bytes!("../assets/icons/mac-mini.svg")),
    ("icons/app/mac-studio.svg", include_bytes!("../assets/icons/mac-studio.svg")),
    ("icons/app/onepassword.svg", include_bytes!("../assets/icons/onepassword.svg")),
    ("icons/app/source-control/azure-devops.svg", include_bytes!("../assets/icons/source-control/azure-devops.svg")),
    ("icons/app/source-control/bitbucket.svg", include_bytes!("../assets/icons/source-control/bitbucket.svg")),
    ("icons/app/source-control/forgejo.svg", include_bytes!("../assets/icons/source-control/forgejo.svg")),
    ("icons/app/source-control/git.svg", include_bytes!("../assets/icons/source-control/git.svg")),
    ("icons/app/source-control/gitcafe.svg", include_bytes!("../assets/icons/source-control/gitcafe.svg")),
    ("icons/app/source-control/github.svg", include_bytes!("../assets/icons/source-control/github.svg")),
    ("icons/app/source-control/gitlab.svg", include_bytes!("../assets/icons/source-control/gitlab.svg")),
    ("icons/app/source-control/jj.svg", include_bytes!("../assets/icons/source-control/jj.svg")),
    ("icons/app/pr-branch-mark.svg", include_bytes!("../assets/icons/pr-branch-mark.svg")),
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
    "archive-x",
    "arrow-down-up",
    "app-window",
    "bot",
    "brain",
    "building",
    "calendar-clock",
    "calendar-arrow-down",
    "calendar-arrow-up",
    "bug",
    "chart-no-axes-column",
    "chevrons-left-right-ellipsis",
    "chevron-down",
    "circle-arrow-up",
    "circle-alert",
    "circle-check",
    "circle-dashed",
    "circle-dot",
    "circle-dot-dashed",
    "circle-slash",
    "circle-x",
    "clock",
    "cloud",
    "code-xml",
    "copy",
    "corner-left-up",
    "corner-up-right",
    "download",
    "file-diff",
    "flask-conical",
    "file-braces",
    "external-link",
    "eye",
    "eye-off",
    "folder",
    "folder-git",
    "folder-git-2",
    "folder-plus",
    "folder-tree",
    "gauge",
    "git-branch",
    "git-pull-request",
    "git-merge",
    "git-fork",
    "git-pull-request-arrow",
    "git-pull-request-closed",
    "git-pull-request-draft",
    "hammer",
    "key-round",
    "keyboard",
    "info",
    "hash",
    "laptop",
    "layers",
    "list-checks",
    "link",
    "list-filter",
    "list-todo",
    "lock",
    "lock-open",
    "maximize-2",
    "mail-open",
    "message-circle",
    "message-circle-question-mark",
    "message-square-text",
    "messages-square",
    "minimize-2",
    "monitor",
    "panel-bottom",
    "panel-left",
    "panel-left-close",
    "panel-right",
    "panels-top-left",
    "paperclip",
    "pencil",
    "pen-line",
    "pencil",
    "pin",
    "rotate-ccw",
    "scan",
    "plug-2",
    "refresh-cw",
    "search",
    "pin-off",
    "server",
    "shield",
    "shield-check",
    "shield-off",
    "shield-question-mark",
    "smartphone",
    "sparkles",
    "square-menu",
    "square-pen",
    "terminal",
    "trash-2",
    "trending-down",
    "trending-up",
    "triangle-alert",
    "user-check",
    "user-lock",
    "trash",
    "text-wrap",
    "user-round",
    "user-round-x",
    "users",
    "wrench",
    "x",
    "zap",
];

/// Images fetched at runtime (e.g. ACP Registry icons), served under `remote/…` so `svg()` can tint them.
static REMOTE: Mutex<Option<HashMap<String, Arc<[u8]>>>> = Mutex::new(None);

pub fn put_remote(path: &str, bytes: Arc<[u8]>) {
    REMOTE.lock().unwrap().get_or_insert_default().insert(path.to_owned(), bytes);
}

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some((_, bytes)) = APP_ICONS.iter().chain(LUCIDE_ICONS).find(|(name, _)| *name == path) {
            return Ok(Some(Cow::Borrowed(bytes)));
        }
        if let Some(bytes) = REMOTE.lock().unwrap().as_ref().and_then(|remote| remote.get(path)) {
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
