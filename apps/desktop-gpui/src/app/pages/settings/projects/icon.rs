//! Project icons: `ProjectFavicon`, `ProjectMonogram`, `projectIconOptions.ts` and
//! `projectIconColors.ts`, plus the favicon and thumbnail images they show.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use gpui_kit::assets::IconName;
use gpui_kit::component::{Icon, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, FontWeight, Hsla, Image, ImageFormat, IntoElement, ObjectFit, ParentElement as _, Pixels,
    SharedString, Styled as _, StyledImage as _, div, img, px, rgb,
};
use serde_json::Value;

use crate::backend::Endpoint;
use crate::inbox;
use crate::rpc::RpcClient;
use crate::theme::Tokens;

/// `PROJECT_ICON_COLORS` swatches (Tailwind 500), in `inbox::PROJECT_COLORS` order.
const SWATCHES: [u32; 18] = [
    0x6a7282, 0xfb2c36, 0xff6900, 0xfe9a00, 0xf0b100, 0x7ccf00, 0x00c950, 0x00bc7d, 0x00bba7, 0x00b8db, 0x00a6f4,
    0x2b7fff, 0x615fff, 0x8e51ff, 0xad46ff, 0xe12afb, 0xf6339a, 0xff2056,
];

/// The color names in picker order.
pub(super) fn color_names() -> impl Iterator<Item = &'static str> {
    inbox::PROJECT_COLORS.iter().map(|(name, ..)| *name)
}

fn color_index(name: &str) -> Option<usize> {
    inbox::PROJECT_COLORS.iter().position(|(n, ..)| *n == name)
}

/// `projectIconColorClassName`: Tailwind 600, or 400 in dark mode.
pub(super) fn icon_color(name: &str, tokens: &Tokens) -> Hsla {
    let (_, light, dark) = inbox::PROJECT_COLORS[color_index(name).unwrap_or(0)];
    rgb(if tokens.dark { dark } else { light }).into()
}

pub(super) fn swatch_color(name: &str) -> Hsla {
    rgb(SWATCHES[color_index(name).unwrap_or(0)]).into()
}

/// "gray" → "Gray".
pub(super) fn capitalized(name: &str) -> String {
    let mut chars = name.chars();
    chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

/// `deriveProjectIdentity`: the automatic monogram and its color.
pub(super) fn automatic_identity(title: &str) -> (String, &'static str) {
    (inbox::monogram(title), inbox::PROJECT_COLORS[inbox::color_index(title)].0)
}

/// `POPULAR_PROJECT_ICONS`, shown before a search.
const POPULAR_ICONS: [&str; 24] = [
    "folder-code",
    "code-2",
    "terminal",
    "globe-2",
    "server",
    "database",
    "bot",
    "sparkles",
    "smartphone",
    "monitor",
    "cloud-cog",
    "package",
    "book-open",
    "flask-conical",
    "shield-check",
    "rocket",
    "gamepad-2",
    "music",
    "image",
    "shopping-bag",
    "git-branch",
    "workflow",
    "wrench",
    "layers-3",
];

/// Deprecated Lucide names the web still resolves (lucide-react keeps them as aliases).
const ALIASES: [(&str, &str); 3] = [("code-2", "code-xml"), ("globe-2", "earth"), ("layers-3", "layers")];

/// Every bundled Lucide name (kebab case, as saved in `projectIcon.name`) and its icon.
fn catalog() -> &'static [(String, IconName)] {
    static CATALOG: OnceLock<Vec<(String, IconName)>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        IconName::ALL
            .iter()
            .map(|icon| {
                let path = icon.path();
                let name = path.trim_start_matches("icons/").trim_end_matches(".svg").to_owned();
                (name, *icon)
            })
            .collect()
    })
}

/// `lucideIconByName`.
pub(super) fn lucide_icon(name: &str) -> Option<IconName> {
    let name = ALIASES.iter().find(|(alias, _)| *alias == name).map_or(name, |(_, canonical)| canonical);
    catalog().iter().find(|(n, _)| n == name).map(|(_, icon)| *icon)
}

/// `filterProjectIconNames`: the popular icons, or up to 60 names containing the query.
pub(super) fn filter_icon_names(query: &str) -> Vec<&'static str> {
    let normalized = query.trim().to_lowercase().split_whitespace().collect::<Vec<_>>().join("-");
    if normalized.is_empty() {
        return POPULAR_ICONS.to_vec();
    }
    catalog().iter().map(|(name, _)| name.as_str()).filter(|name| name.contains(&normalized)).take(60).collect()
}

/// "folder-code" → "Folder Code" (`iconLabel`).
pub(super) fn icon_label(name: &str) -> String {
    name.split('-').map(capitalized).collect::<Vec<_>>().join(" ")
}

/// `PROJECT_EMOJIS`.
pub(super) const EMOJIS: [(&str, &str); 30] = [
    ("💻", "Computer"),
    ("🛠️", "Tools"),
    ("🚀", "Rocket"),
    ("🤖", "Robot"),
    ("✨", "Sparkles"),
    ("⚡", "Lightning"),
    ("🌐", "Web"),
    ("📱", "Mobile"),
    ("🖥️", "Desktop"),
    ("⌨️", "Keyboard"),
    ("⚙️", "Gear"),
    ("🗄️", "Database"),
    ("☁️", "Cloud"),
    ("📦", "Package"),
    ("📚", "Books"),
    ("🧪", "Test tube"),
    ("🔒", "Lock"),
    ("🎮", "Game"),
    ("🎵", "Music"),
    ("🎬", "Movie"),
    ("🖼️", "Picture"),
    ("🛍️", "Shopping"),
    ("🔥", "Fire"),
    ("💡", "Idea"),
    ("🧩", "Puzzle"),
    ("📊", "Chart"),
    ("🧠", "Brain"),
    ("🦄", "Unicorn"),
    ("🐙", "Octopus"),
    ("🌱", "Seedling"),
];

/// `\p{Extended_Pictographic}`, by the blocks that hold it.
fn is_pictographic(c: char) -> bool {
    matches!(c as u32,
        0x00A9 | 0x00AE | 0x203C | 0x2049 | 0x2122 | 0x2139 | 0x2194..=0x21AA | 0x231A..=0x23FF | 0x24C2
        | 0x25AA..=0x25FE | 0x2600..=0x27BF | 0x2934 | 0x2935 | 0x2B05..=0x2B55 | 0x3030 | 0x303D | 0x3297
        | 0x3299 | 0x1F000..=0x1F0FF | 0x1F10D..=0x1F10F | 0x1F12F | 0x1F16C..=0x1F171 | 0x1F17E | 0x1F17F
        | 0x1F18E | 0x1F191..=0x1F19A | 0x1F1AD..=0x1F1E5 | 0x1F201..=0x1F20F | 0x1F21A | 0x1F22F
        | 0x1F232..=0x1F23A | 0x1F23C..=0x1F23F | 0x1F249..=0x1F3FA | 0x1F400..=0x1F53D | 0x1F546..=0x1F64F
        | 0x1F680..=0x1F6FF | 0x1F774..=0x1F77F | 0x1F7D5..=0x1F7FF | 0x1F80C..=0x1F80F | 0x1F848..=0x1F84F
        | 0x1F85A..=0x1F85F | 0x1F888..=0x1F88F | 0x1F8AE..=0x1F8FF | 0x1F90C..=0x1F93A | 0x1F93C..=0x1F945
        | 0x1F947..=0x1FAFF | 0x1FC00..=0x1FFFD)
}

fn is_regional_indicator(c: char) -> bool {
    ('\u{1F1E6}'..='\u{1F1FF}').contains(&c)
}

/// `firstEmoji`: the first grapheme of `value` when it is an emoji, flag or keycap.
pub(super) fn first_emoji(value: &str) -> Option<String> {
    use unicode_segmentation::UnicodeSegmentation as _;
    let segment = value.trim().graphemes(true).next()?;
    let chars: Vec<char> = segment.chars().collect();
    let flag = chars.len() == 2 && chars.iter().all(|c| is_regional_indicator(*c));
    let keycap =
        matches!(chars.as_slice(), [c, '\u{20E3}'] | [c, '\u{FE0F}', '\u{20E3}'] if matches!(c, '#' | '*' | '0'..='9'));
    (flag || keycap || chars.iter().copied().any(is_pictographic)).then(|| segment.to_owned())
}

/// `ProjectMonogramText`: up to 32 UTF-16 units, a letter or number first, then letters,
/// numbers, marks and joiners. The server checks the grapheme count.
pub(super) fn is_monogram_text(text: &str) -> bool {
    let is_mark = |c: char| {
        matches!(c as u32, 0x0300..=0x036F | 0x0483..=0x0489 | 0x0591..=0x05BD | 0x0610..=0x061A | 0x064B..=0x065F
            | 0x0900..=0x0903 | 0x093A..=0x094F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF
            | 0xFE00..=0xFE0F | 0xFE20..=0xFE2F)
    };
    let mut chars = text.chars();
    text.encode_utf16().count() <= 32
        && chars.next().is_some_and(char::is_alphanumeric)
        && chars.all(|c| c.is_alphanumeric() || is_mark(c) || c == '\u{200C}' || c == '\u{200D}')
}

/// `ProjectMonogram` at `size`: the text on a 14% tint of its color, corners at 25%.
/// The web stretches glyphs to a text length of 12/16 (6/16 for one glyph); GPUI cannot,
/// so the glyphs spread across that width instead.
pub(super) fn monogram(text: &str, color: &str, size: f32, tokens: &Tokens) -> AnyElement {
    let color = icon_color(color, tokens);
    let glyphs: Vec<SharedString> = {
        use unicode_segmentation::UnicodeSegmentation as _;
        text.graphemes(true).map(|g| SharedString::from(g.to_owned())).collect()
    };
    let span = size * if glyphs.len() > 1 { 12. } else { 6. } / 16.;
    div()
        .size(px(size))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(size / 4.))
        .bg(color.opacity(0.14))
        .text_color(color)
        .font_family(tokens.fonts.mono)
        .font_weight(FontWeight::BOLD)
        .text_size(px(size * 8.25 / 16.))
        .line_height(px(size))
        .child(h_flex().w(px(span)).justify_between().when(glyphs.len() < 2, |el| el.justify_center()).children(glyphs))
        .into_any_element()
}

/// `ProjectFavicon` at `size`: the custom icon, else the project's favicon, else its monogram.
pub(super) fn project_icon(
    icon: Option<&Value>,
    title: &str,
    favicon: Option<Arc<Image>>,
    size: f32,
    tokens: &Tokens,
) -> AnyElement {
    let icon = icon.filter(|icon| !icon.is_null());
    let text = |key: &str| icon.and_then(|icon| icon[key].as_str());
    match text("kind") {
        Some("monogram") => monogram(text("text").unwrap_or_default(), text("color").unwrap_or("gray"), size, tokens),
        Some("emoji") => div()
            .size(px(size))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(size * 0.8))
            .line_height(px(size))
            .child(SharedString::from(text("emoji").unwrap_or_default().to_owned()))
            .into_any_element(),
        Some("lucide") => {
            let name = lucide_icon(text("name").unwrap_or_default()).unwrap_or(IconName::FolderCode);
            Icon::new(name)
                .size(px(size))
                .text_color(icon_color(text("color").unwrap_or("gray"), tokens))
                .into_any_element()
        }
        _ => match favicon {
            Some(image) => img(image)
                .size(px(size))
                .flex_none()
                .rounded(px(size / 4.))
                .object_fit(ObjectFit::Contain)
                .into_any_element(),
            None => {
                let (letters, color) = automatic_identity(title);
                monogram(&letters, color, size, tokens)
            }
        },
    }
}

/// `ProjectIconOverride` in words, for the icon row's description.
pub(super) fn describe(icon: Option<&Value>, favicon_path: Option<&str>) -> String {
    let icon = icon.filter(|icon| !icon.is_null());
    let text = |key: &str| icon.and_then(|icon| icon[key].as_str()).unwrap_or_default();
    match text("kind") {
        "lucide" => format!("{} · {}", text("name"), text("color")),
        "monogram" => format!("{} · {}", text("text"), text("color")),
        "emoji" => text("emoji").to_owned(),
        _ => favicon_path.unwrap_or("Automatic").to_owned(),
    }
}

enum Slot {
    Loading,
    Ready(Arc<Image>),
    Missing,
}

/// Images behind `assets.createUrl` (project favicons, workspace files), fetched once each.
/// The web defers thumbnails until they scroll into view; callers here ask only for what
/// they draw, which the virtual list already limits.
#[derive(Default)]
pub(super) struct Images {
    slots: HashMap<String, Slot>,
}

impl Images {
    /// The image for `resource`, starting its fetch on first use. `None` while loading or
    /// when it has no usable image (a missing favicon, an undecodable format).
    pub(super) fn get(
        &mut self,
        resource: Value,
        client: Option<RpcClient>,
        endpoint: Option<Endpoint>,
        cx: &mut Context<Self>,
    ) -> Option<Arc<Image>> {
        let key = resource.to_string();
        match self.slots.get(&key) {
            Some(Slot::Ready(image)) => return Some(image.clone()),
            Some(_) => return None,
            None => {}
        }
        let (Some(client), Some(endpoint)) = (client, endpoint) else { return None };
        self.slots.insert(key.clone(), Slot::Loading);
        cx.spawn(async move |this, cx| {
            let image = fetch(&client, endpoint, resource).await;
            if let Err(error) = &image {
                log::debug!("project image unavailable: {error:#}");
            }
            let _ = this.update(cx, |this, cx| {
                this.slots.insert(key, image.map_or(Slot::Missing, Slot::Ready));
                cx.notify();
            });
        })
        .detach();
        None
    }
}

/// The `project-favicon` resource for a project (`projectFaviconUrlAtom`).
pub(super) fn favicon_resource(workspace_root: &str, favicon_path: Option<&str>) -> Value {
    let mut resource = serde_json::json!({"_tag": "project-favicon", "cwd": workspace_root});
    if let Some(path) = favicon_path {
        resource["path"] = path.into();
    }
    resource
}

async fn fetch(client: &RpcClient, endpoint: Endpoint, resource: Value) -> anyhow::Result<Arc<Image>> {
    let result = client.request("assets.createUrl", serde_json::json!({"resource": resource})).await?;
    let relative = result["relativeUrl"].as_str().unwrap_or_default().to_owned();
    // `isProjectFaviconFallbackUrl`: the server's stand-in for "no favicon".
    let path = relative.split(['?', '#']).next().unwrap_or_default();
    anyhow::ensure!(!path.ends_with("/project-favicon-missing"), "no favicon");
    let url = if relative.starts_with("http") {
        relative.clone()
    } else {
        format!("{}/{}", endpoint.http_base, relative.trim_start_matches('/'))
    };
    let response = crate::runtime()
        .spawn(async move {
            let response = reqwest::Client::new().get(url).bearer_auth(&endpoint.bearer_token).send().await?;
            let response = response.error_for_status()?;
            let mime = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .map(|value| value.split(';').next().unwrap_or_default().trim().to_owned());
            anyhow::Ok((mime, response.bytes().await?.to_vec()))
        })
        .await??;
    let (mime, bytes) = response;
    let extension = path.rsplit('.').next().unwrap_or_default().to_lowercase();
    let format = mime.as_deref().and_then(ImageFormat::from_mime_type).or(match extension.as_str() {
        "png" => Some(ImageFormat::Png),
        "jpg" | "jpeg" => Some(ImageFormat::Jpeg),
        "webp" => Some(ImageFormat::Webp),
        "gif" => Some(ImageFormat::Gif),
        "svg" => Some(ImageFormat::Svg),
        "ico" => Some(ImageFormat::Ico),
        _ => None,
    });
    let format = format.ok_or_else(|| anyhow::anyhow!("unsupported image type {mime:?}"))?;
    Ok(Arc::new(Image::from_bytes(format, bytes)))
}

/// Pixels at which the panel shows the project's icon (`size-6`).
pub(super) const PANEL_ICON: Pixels = px(24.);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lucide_names_resolve_with_their_aliases() {
        assert_eq!(lucide_icon("folder-code"), Some(IconName::FolderCode));
        assert_eq!(lucide_icon("code-2"), Some(IconName::CodeXml));
        assert_eq!(lucide_icon("not-an-icon"), None);
        assert!(POPULAR_ICONS.iter().all(|name| lucide_icon(name).is_some()), "every popular icon is bundled");
    }

    #[test]
    fn icon_search_matches_the_web_filter() {
        assert_eq!(filter_icon_names("  ").len(), 24);
        let results = filter_icon_names("Folder Code");
        assert!(results.contains(&"folder-code") && results.iter().all(|name| name.contains("folder-code")));
        assert!(filter_icon_names("a").len() <= 60);
        assert!(filter_icon_names("zzzz-nothing").is_empty());
        assert_eq!(icon_label("folder-code"), "Folder Code");
    }

    #[test]
    fn first_emoji_takes_one_pictographic_grapheme() {
        assert_eq!(first_emoji(" 🚀 launch").as_deref(), Some("🚀"));
        assert_eq!(first_emoji("👩‍💻").as_deref(), Some("👩‍💻"));
        assert_eq!(first_emoji("🇯🇵").as_deref(), Some("🇯🇵"));
        assert_eq!(first_emoji("1️⃣").as_deref(), Some("1️⃣"));
        assert_eq!(first_emoji("🛠️").as_deref(), Some("🛠️"));
        assert_eq!(first_emoji("abc"), None);
        assert_eq!(first_emoji(""), None);
    }

    #[test]
    fn monogram_text_follows_the_contract_pattern() {
        assert!(is_monogram_text("T3"));
        assert!(is_monogram_text("É"));
        assert!(!is_monogram_text(""));
        assert!(!is_monogram_text("-A"));
        assert!(!is_monogram_text("A B"));
        assert!(!is_monogram_text(&"A".repeat(33)));
    }

    #[test]
    fn descriptions_and_favicon_resources_match_the_web() {
        let icon = serde_json::json!({"kind": "lucide", "name": "rocket", "color": "blue"});
        assert_eq!(describe(Some(&icon), None), "rocket · blue");
        assert_eq!(describe(None, Some("public/logo.svg")), "public/logo.svg");
        assert_eq!(describe(Some(&Value::Null), None), "Automatic");
        assert_eq!(favicon_resource("/a", None), serde_json::json!({"_tag": "project-favicon", "cwd": "/a"}));
        assert_eq!(capitalized("emerald"), "Emerald");
        assert_eq!(automatic_identity("examples"), ("ES".to_owned(), "blue"));
    }
}
