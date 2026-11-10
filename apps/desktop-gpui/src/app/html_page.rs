//! Pages an agent rendered with `html_render` (`HtmlRenderFrame`, `HtmlRenderDialog`): the page in
//! the timeline at the height it measures, with "Open full screen" into a dialog that zooms it.
//!
//! The page runs in a sandboxed iframe of an embed's own shell page (`HtmlRenderDocument`), whose
//! script forwards the page's `postMessage` bridge over IPC. Links, images and the full screen
//! button need the reader's activation, as the web's frame does.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::base::FocusTrapElement as _;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Context, FocusHandle, Image, InteractiveElement as _, IntoElement as _, KeyDownEvent, Keystroke,
    Modifiers, ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Task, Window, div, px,
};
use serde_json::{Value, json};

use super::Workspace;
use super::embed::{EmbedView, FRAME_CSS, FRAME_SCRIPT, Layer};
use super::image_viewer::{Reference, ViewerImage};
use super::keymap;
use super::mermaid::css_color;
use super::side_chat::{ChatPane, PaneOwner};
use crate::keybindings::{Rule, Shortcut};
use crate::theme::Tokens;
use crate::themes::ThemeColors;
use crate::timeline::{self, PAGE_MAX_HEIGHT, PAGE_MIN_HEIGHT, PageSize, clamp_page_height};

/// `HTML_RENDER_COLUMN_WIDTH`.
const COLUMN_WIDTH: f32 = 728.;
/// `HTML_RENDER_MAX_FIT_HEIGHT`: a page that grows with its frame stops here.
const MAX_FIT_HEIGHT: f32 = 20_000.;
/// `MIN_URL_LIFE_MS`: full screen re-signs a URL that expires sooner.
const MIN_URL_LIFE_MS: f64 = 5. * 60_000.;
/// Pages kept loaded off screen, beside the ones on it.
const KEPT_PAGES: usize = 4;
const ZOOM_STEP: f32 = 0.25;
const ZOOM_RANGE: (f32, f32) = (0.5, 2.);

/// `measuredHeight`: the taller of the measurements around `width`, since a breakpoint between
/// them can pick either.
fn measured_height(heights: &[(u32, f32)], width: f32) -> f32 {
    let Some(last) = heights.len().checked_sub(1) else { return 0. };
    #[expect(clippy::cast_precision_loss, reason = "widths are at most 10 000")]
    let at = |index: usize| heights[index].0 as f32;
    let high = heights.iter().position(|&(measured, _)| measured as f32 >= width).unwrap_or(last);
    let low = if (at(high) - width).abs() < f32::EPSILON { high } else { high.saturating_sub(1) };
    heights[low].1.max(heights[high].1)
}

/// `htmlRenderFrameHeight`: the live page's height once it reports one, else what the server
/// measured at this width, held under the agent's height when that is the smaller.
pub(super) fn frame_height(size: &PageSize, width: f32, content: Option<f32>) -> f32 {
    if let Some(content) = content {
        return content.ceil().clamp(PAGE_MIN_HEIGHT, MAX_FIT_HEIGHT);
    }
    if size.heights.is_empty() {
        return clamp_page_height(size.height);
    }
    let cap = if measured_height(&size.heights, COLUMN_WIDTH) > size.height { size.height } else { PAGE_MAX_HEIGHT };
    clamp_page_height(cap.min(measured_height(&size.heights, width)))
}

/// What a page's shell reports over IPC.
#[derive(Debug, PartialEq)]
enum PageMessage {
    /// The shell (re)loaded and listens for its box.
    Ready,
    /// `readHtmlRenderContentHeight`.
    Height(f32),
    /// `readHtmlRenderLinkRequest`; `external` for a Cmd/Ctrl click.
    Link { url: String, external: bool },
    /// `readHtmlRenderImageRequest`.
    Image { src: String, name: String },
    /// The shell's "Open full screen" button.
    Expand,
    /// `readHtmlRenderKeyDown`: a shortcut the page handed back.
    Key(PageKeyDown),
}

/// A key the page reports pressing, which a page script can forge.
#[derive(Debug, PartialEq)]
struct PageKeyDown {
    key: String,
    code: String,
    meta: bool,
    ctrl: bool,
    shift: bool,
    alt: bool,
    repeat: bool,
}

/// `HTML_RENDER_SHORTCUT_COMMANDS`, beside the thread and model picker jumps: commands that change
/// what the reader sees, never a thread, since the page can report any key.
fn page_shortcut_command(command: &str) -> bool {
    const COMMANDS: [&str; 14] = [
        "commandPalette.toggle",
        "filePicker.toggle",
        "projectSearch.toggle",
        "chat.find",
        "sidebar.toggle",
        "rightPanel.toggle",
        "rightPanel.toggleMaximized",
        "threadPanel.toggle",
        "terminal.toggle",
        "preview.toggle",
        "navigation.back",
        "navigation.forward",
        "thread.previous",
        "thread.next",
    ];
    COMMANDS.contains(&command)
        || keymap::jump_index(command, "thread").is_some()
        || keymap::jump_index(command, "modelPicker").is_some()
}

/// `EVENT_CODE_SHORTCUT_KEYS`.
const EVENT_CODE_KEYS: [(&str, &str); 21] = [
    ("Backquote", "`"),
    ("Backslash", "\\"),
    ("BracketLeft", "["),
    ("BracketRight", "]"),
    ("Comma", ","),
    ("Digit0", "0"),
    ("Digit1", "1"),
    ("Digit2", "2"),
    ("Digit3", "3"),
    ("Digit4", "4"),
    ("Digit5", "5"),
    ("Digit6", "6"),
    ("Digit7", "7"),
    ("Digit8", "8"),
    ("Digit9", "9"),
    ("Equal", "="),
    ("Minus", "-"),
    ("Period", "."),
    ("Quote", "'"),
    ("Semicolon", ";"),
    ("Slash", "/"),
];

/// `normalizeEventKey`.
fn normalize_key(key: &str) -> String {
    let key = key.to_lowercase();
    if key == "esc" { "escape".to_owned() } else { key }
}

/// `resolveEventKeys`: the typed key, a letter's physical key, and `shortcutKeyFromEvent`.
fn event_keys(key: &str, code: &str) -> Vec<String> {
    let typed = normalize_key(key);
    let letter = typed.len() == 1 && typed.as_bytes()[0].is_ascii_lowercase();
    let mut keys = vec![typed.clone()];
    if let Some(physical) = code.strip_prefix("Key").filter(|c| c.len() == 1 && c.as_bytes()[0].is_ascii_uppercase())
        && !letter
    {
        keys.push(physical.to_ascii_lowercase());
    }
    let mapped = EVENT_CODE_KEYS.iter().find(|(name, _)| *name == code).map(|(_, key)| (*key).to_owned());
    keys.push(if letter { typed } else { mapped.unwrap_or(typed) });
    keys
}

/// `HtmlRenderShortcut`: a shortcut with `mod` resolved for this platform.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct PageShortcut {
    key: String,
    meta_key: bool,
    ctrl_key: bool,
    shift_key: bool,
    alt_key: bool,
}

impl PageShortcut {
    fn of(shortcut: &Shortcut, mac: bool) -> Self {
        Self {
            key: shortcut.key.clone(),
            meta_key: shortcut.meta_key || (shortcut.mod_key && mac),
            ctrl_key: shortcut.ctrl_key || (shortcut.mod_key && !mac),
            shift_key: shortcut.shift_key,
            alt_key: shortcut.alt_key,
        }
    }

    /// `matchesKeybindingShortcut` with `mod` already resolved.
    fn matches(&self, key: &PageKeyDown, keys: &[String]) -> bool {
        (self.meta_key, self.ctrl_key, self.shift_key, self.alt_key) == (key.meta, key.ctrl, key.shift, key.alt)
            && keys.contains(&self.key)
    }
}

/// What a page hands back (`htmlRenderShortcutsMessage`): the Cmd/Ctrl chords of the allowed
/// commands, and every other binding's chord, which a matching key must not also be.
#[derive(Debug, Default, PartialEq)]
struct PageShortcuts {
    allowed: Vec<PageShortcut>,
    blocked: Vec<PageShortcut>,
}

impl PageShortcuts {
    /// From the primary's rules and the page's environment's (its project scripts).
    fn of(primary: &[Rule], environment: &[Rule], mac: bool) -> Self {
        let blocked = primary
            .iter()
            .chain(environment)
            .filter(|rule| !page_shortcut_command(&rule.command))
            .map(|rule| PageShortcut::of(&rule.shortcut, mac))
            .collect();
        let mut allowed: Vec<PageShortcut> = Vec::new();
        for rule in primary {
            let s = &rule.shortcut;
            let shortcut = PageShortcut::of(s, mac);
            if page_shortcut_command(&rule.command)
                && (s.meta_key || s.ctrl_key || s.mod_key)
                && !allowed.contains(&shortcut)
            {
                allowed.push(shortcut);
            }
        }
        Self { allowed, blocked }
    }

    fn script(&self) -> String {
        let message = json!({
            "jsonrpc": "2.0",
            "method": "t3/notifications/shortcuts",
            "params": {"shortcuts": self.allowed, "blocked": self.blocked},
        });
        format!("window.t3Bridge&&t3Bridge('shortcuts',{});", script_json(&message))
    }

    /// `isShortcut`: the allowed shortcut `key` presses, unless it also presses a blocked one or
    /// names a key no allowed shortcut does, as a forged key/code pair can.
    fn admit(&self, key: &PageKeyDown) -> Option<&PageShortcut> {
        let keys = event_keys(&key.key, &key.code);
        let mut allowed = self.allowed.iter().filter(|shortcut| shortcut.matches(key, &keys)).peekable();
        let first = allowed.peek().copied()?;
        if self.blocked.iter().any(|shortcut| shortcut.matches(key, &keys)) {
            return None;
        }
        let typed = normalize_key(&key.key);
        let named = key.key.chars().count() == 1 || key.key == "Dead" || allowed.any(|s| s.key == typed);
        named.then_some(first)
    }
}

/// The keystroke an admitted key replays as, keyed by the shortcut it pressed.
fn page_keystroke(key: &PageKeyDown, shortcut: &PageShortcut) -> Keystroke {
    let modifiers =
        Modifiers { control: key.ctrl, alt: key.alt, shift: key.shift, platform: key.meta, function: false };
    Keystroke { modifiers, key: shortcut.key.clone(), key_char: None }
}

fn starts_with_ignore_case(text: &str, prefix: &str) -> bool {
    text.get(..prefix.len()).is_some_and(|start| start.eq_ignore_ascii_case(prefix))
}

fn page_message(body: &str) -> Option<PageMessage> {
    let value: Value = serde_json::from_str(body).ok()?;
    match value["type"].as_str()? {
        "ready" => return Some(PageMessage::Ready),
        "expand" => return Some(PageMessage::Expand),
        "page" => {}
        _ => return None,
    }
    let data = &value["data"];
    if data["jsonrpc"] != "2.0" {
        return None;
    }
    let params = &data["params"];
    let method = data["method"].as_str()?;
    #[expect(clippy::cast_possible_truncation, reason = "a page height")]
    if method == "ui/notifications/size-changed" {
        return params["height"].as_f64().filter(|h| h.is_finite() && *h > 0.).map(|h| PageMessage::Height(h as f32));
    }
    // Opens need the reader's activation on the frame: a page can take focus by script.
    if value["active"] != true {
        return None;
    }
    match method {
        "ui/open-link" if data["id"].is_string() || data["id"].is_number() => {
            let url = params["url"].as_str()?;
            (starts_with_ignore_case(url, "http://") || starts_with_ignore_case(url, "https://")).then(|| {
                PageMessage::Link {
                    url: url.to_owned(),
                    external: params["metaKey"] == true || params["ctrlKey"] == true,
                }
            })
        }
        "t3/notifications/open-image" => {
            let src = params["src"].as_str()?;
            let allowed = ["http://", "https://", "data:image/"].iter().any(|p| starts_with_ignore_case(src, p));
            let name: String = params["name"].as_str().unwrap_or_default().trim().chars().take(200).collect();
            allowed.then(|| PageMessage::Image {
                src: src.to_owned(),
                name: if name.is_empty() { "Image".to_owned() } else { name },
            })
        }
        "t3/notifications/keydown" => {
            let flag = |name: &str| params[name] == true;
            Some(PageMessage::Key(PageKeyDown {
                key: params["key"].as_str()?.to_owned(),
                code: params["code"].as_str()?.to_owned(),
                meta: flag("metaKey"),
                ctrl: flag("ctrlKey"),
                shift: flag("shiftKey"),
                alt: flag("altKey"),
                repeat: flag("repeat"),
            }))
        }
        _ => None,
    }
}

/// Forwards the page's bridge (`HtmlRenderDocument`'s message listener): its height always, the
/// rest with the activation it carries, and answers a link request the way the web does.
/// `t3Bridge(kind, message)` keeps the app's latest message of a kind (the theme, the shortcuts)
/// and posts it to the page now and on each load, as the web's frame does on `onLoad`.
const PAGE_SCRIPT: &str = "(()=>{const f=document.getElementById('f'),x=document.getElementById('x');\
const post=(m)=>window.ipc.postMessage(JSON.stringify(m));\
addEventListener('message',(e)=>{if(e.source!==f.contentWindow)return;const d=e.data;\
if(!d||typeof d!=='object'||d.jsonrpc!=='2.0')return;\
const active=document.activeElement===f&&!(navigator.userActivation&&navigator.userActivation.isActive===false);\
if(active&&d.method==='ui/open-link'&&(typeof d.id==='string'||typeof d.id==='number'))\
f.contentWindow.postMessage({jsonrpc:'2.0',id:d.id,result:{}},'*');\
try{post({type:'page',active,data:d});}catch(_){}});\
if(x)x.addEventListener('click',(e)=>{if(e.isTrusted)post({type:'expand'});});\
window.t3Zoom=(z)=>{document.getElementById('t3-embed').style.zoom=z;};\
window.t3Scheme=(s)=>{f.style.colorScheme=s;document.querySelector('meta[name=color-scheme]').content=s;};\
const M={},send=(k)=>{const w=f.contentWindow;if(w&&M[k])w.postMessage(M[k],'*');};\
f.addEventListener('load',()=>Object.keys(M).forEach(send));window.t3Bridge=(k,m)=>{M[k]=m;send(k);};})();";

/// `htmlRenderTheme`: the palette's roles as the page's variables (`--accent` is the brand color,
/// not the hover surface), and the fixed status and chart colors.
fn page_theme(dark: bool, c: &ThemeColors, (sans, mono): (String, String)) -> Value {
    let (fixed, chart) = if dark {
        (
            [("--success-foreground", "#34d399"), ("--info-foreground", "#60a5fa")],
            ["#2dd4bf", "#fbbf24", "#c084fc", "#fb7185", "#a3e635"],
        )
    } else {
        (
            [("--success-foreground", "#047857"), ("--info-foreground", "#1d4ed8")],
            ["#0d9488", "#d97706", "#9333ea", "#e11d48", "#65a30d"],
        )
    };
    let roles = [
        ("--background", c.canvas),
        ("--foreground", c.text),
        ("--muted", c.muted),
        ("--muted-foreground", c.muted_foreground),
        ("--card", c.surface),
        ("--card-foreground", c.text),
        ("--popover", c.surface_overlay),
        ("--popover-foreground", c.text),
        ("--secondary", c.secondary),
        ("--secondary-foreground", c.secondary_foreground),
        ("--border", c.border),
        ("--input", c.input),
        ("--ring", c.focus),
        ("--primary", c.message_action),
        ("--primary-foreground", c.message_action_foreground),
        ("--accent", c.accent),
        ("--accent-foreground", c.accent_foreground),
        ("--accent-surface", c.accent_surface),
        ("--accent-surface-foreground", c.accent_surface_foreground),
        ("--destructive", c.error),
        ("--destructive-foreground", c.error_foreground),
        ("--destructive-surface", c.error_surface),
        ("--warning", c.warning),
        ("--warning-foreground", c.warning_foreground),
        ("--warning-surface", c.warning_surface),
        ("--code-background", c.code_background),
        ("--code-foreground", c.code_foreground),
        ("--chart-1", c.accent),
    ];
    let mut variables: serde_json::Map<String, Value> =
        roles.into_iter().map(|(name, color)| (name.to_owned(), css_color(color).into())).collect();
    let fixed = [("--success", "#10b981"), ("--info", "#3b82f6")].into_iter().chain(fixed);
    variables.extend(fixed.map(|(name, color)| (name.to_owned(), color.into())));
    variables.extend(chart.into_iter().enumerate().map(|(i, color)| (format!("--chart-{}", i + 2), color.into())));
    variables.insert("--radius".into(), "0.625rem".into());
    variables.insert("--font-sans".into(), sans.into());
    variables.insert("--font-mono".into(), mono.into());
    json!({"appearance": if dark { "dark" } else { "light" }, "variables": variables})
}

/// `encodeURIComponent`.
fn encode_uri_component(text: &str) -> String {
    let mut encoded = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

/// `value` as a script literal that can't close the `<script>` it sits in.
fn script_json(value: &Value) -> String {
    value.to_string().replace('<', "\\u003c")
}

/// A page's theme as the bridge sends it: `htmlRenderThemeFragment` on the first load, then
/// `htmlRenderThemeMessage` whenever it changes. Without a palette it is empty, leaving the page
/// its own default theme.
#[derive(Debug, Default)]
struct PageTheme {
    fragment: String,
    script: String,
}

impl PageTheme {
    fn new(theme: &Value) -> Self {
        let message = json!({
            "jsonrpc": "2.0",
            "method": "ui/notifications/host-context-changed",
            "params": {"theme": theme["appearance"], "styles": {"variables": theme["variables"]}},
        });
        Self {
            fragment: format!("#t3-theme={}", encode_uri_component(&theme.to_string())),
            script: format!("window.t3Bridge&&t3Bridge('theme',{});", script_json(&message)),
        }
    }
}

/// Lucide's `Maximize2`.
const EXPAND_ICON: &str = "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"14\" height=\"14\" viewBox=\"0 0 24 24\" \
fill=\"none\" stroke=\"currentColor\" stroke-width=\"2\" stroke-linecap=\"round\" stroke-linejoin=\"round\">\
<polyline points=\"15 3 21 3 21 9\"/><polyline points=\"9 21 3 21 3 15\"/><line x1=\"21\" x2=\"14\" y1=\"3\" y2=\"10\"/>\
<line x1=\"3\" x2=\"10\" y1=\"21\" y2=\"14\"/></svg>";

pub(super) fn escape_attribute(value: &str) -> String {
    value.replace('&', "&amp;").replace('"', "&quot;").replace('<', "&lt;")
}

/// The colours the shell draws itself in: the page's background and the glass button's.
struct ShellLook {
    dark: bool,
    background: String,
    glass: String,
    border: String,
    icon: String,
    hover_border: String,
    ring: String,
}

impl ShellLook {
    fn of(t: &Tokens) -> Self {
        Self {
            dark: t.dark,
            background: css_color(t.background),
            glass: css_color(t.background.opacity(t.glass)),
            border: css_color(t.border.opacity(0.6)),
            icon: css_color(t.muted_foreground),
            hover_border: css_color(t.border),
            ring: css_color(t.ring),
        }
    }
}

/// The embed's shell: the page in its sandboxed frame (`sandbox="allow-scripts allow-forms"`, never
/// `allow-same-origin`, so it stays out of the app's session), and inline, the hover-revealed
/// "Open full screen" button over its top-right corner. Full screen scrolls when zoomed past 100%.
/// The page loads with the theme's fragment, and `bridge` scripts seed what the shell posts it.
fn page_shell(url: &str, title: &str, look: &ShellLook, inline: bool, theme: &PageTheme, bridge: &str) -> String {
    let scheme = if look.dark { "dark" } else { "light" };
    let src = escape_attribute(&format!("{}{}", url.split('#').next().unwrap_or(url), theme.fragment));
    let bridge = format!("{}{bridge}", theme.script);
    let title = escape_attribute(title);
    let ShellLook { background, glass, border, icon, hover_border, ring, .. } = look;
    let (button, overflow) = if inline {
        (
            format!(
                "<button id=\"x\" type=\"button\" aria-label=\"Open full screen\" title=\"Open full screen\">{EXPAND_ICON}</button>"
            ),
            "",
        )
    } else {
        (String::new(), "html,body{overflow:auto}")
    };
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"color-scheme\" content=\"{scheme}\">\
         <style>{FRAME_CSS}{overflow}html{{background:{background}}}\
         iframe{{display:block;border:0;width:100%;height:100%;color-scheme:{scheme}}}\
         #x{{position:absolute;top:8px;right:8px;width:24px;height:24px;padding:0;display:flex;align-items:center;\
         justify-content:center;border-radius:9999px;border:1px solid {border};background:{glass};color:{icon};\
         -webkit-backdrop-filter:blur(12px) saturate(1.5);backdrop-filter:blur(12px) saturate(1.5);\
         box-shadow:0 1px 2px rgba(0,0,0,.05);cursor:pointer;opacity:0;transition:opacity .15s}}\
         body:hover #x,#x:focus-visible{{opacity:1}}#x:hover{{border-color:{hover_border}}}\
         #x:focus-visible{{outline:2px solid {ring};outline-offset:1px}}</style></head>\
         <body><div id=\"t3-embed\"><iframe id=\"f\" src=\"{src}\" title=\"{title}\" sandbox=\"allow-scripts allow-forms\" \
         allow=\"fullscreen\"></iframe>{button}</div><script>{FRAME_SCRIPT}{PAGE_SCRIPT}{bridge}</script></body></html>"
    )
}

/// A page by the pane and thread showing it and its attachment: a fork that inherits the
/// attachment shows its own page, as the web mounts one per timeline.
#[derive(Clone, PartialEq, Eq, Hash)]
struct PageKey {
    owner: PaneOwner,
    attachment: SharedString,
}

struct Page {
    embed: Option<EmbedView>,
    /// The URL the frame loaded and when it expires (ms since the epoch); the frame keeps it.
    url: Option<(String, f64)>,
    failed: bool,
    /// The live page's height, once it reports one.
    content_height: Option<f32>,
    dark: bool,
    /// The theme the page was last sent.
    theme: Option<Rc<PageTheme>>,
    /// The shortcuts the page was last sent.
    shortcuts: Option<ShownShortcuts>,
    /// The frame this page last drew in.
    used: u64,
    _task: Option<Task<()>>,
    _messages: Option<Task<()>>,
}

/// A page's [`PageShortcuts`] and the keymaps they were resolved from.
struct ShownShortcuts {
    keymap: Rc<[Rule]>,
    environment: Option<Rc<[Rule]>>,
    list: PageShortcuts,
}

impl ShownShortcuts {
    fn from(&self, keymap: &Rc<[Rule]>, environment: Option<&Rc<[Rule]>>) -> bool {
        Rc::ptr_eq(&self.keymap, keymap)
            && match (&self.environment, environment) {
                (Some(shown), Some(now)) => Rc::ptr_eq(shown, now),
                (shown, now) => shown.is_none() && now.is_none(),
            }
    }
}

struct Dialog {
    /// Its pane leaving the thread closes it, as unmounting the row does.
    key: PageKey,
    title: SharedString,
    embed: Option<EmbedView>,
    zoom: f32,
    dark: bool,
    theme: Option<Rc<PageTheme>>,
    focus: FocusHandle,
    close_focus: FocusHandle,
    restore: Option<FocusHandle>,
    _task: Option<Task<()>>,
    _messages: Option<Task<()>>,
}

/// What a page's theme is built from: the palette, whether it is dark, and the fonts.
type ThemeStamp = (bool, ThemeColors, &'static str, &'static str);

#[derive(Default)]
pub(super) struct HtmlPages {
    pages: HashMap<PageKey, Page>,
    dialog: Option<Dialog>,
    /// The theme pages show now, rebuilt only when what it is built from changes.
    theme: Option<(ThemeStamp, Rc<PageTheme>)>,
}

impl HtmlPages {
    /// Drops `environment`'s pages that could not load, so they mint again when next drawn.
    fn forget_failed(&mut self, environment: &str) {
        self.pages.retain(|key, page| !(page.failed && page.embed.is_none() && key.owner.env == environment));
    }
}

/// Where a shell's report came from.
#[derive(Clone)]
enum Origin {
    Inline(PageKey),
    Dialog(PageKey),
}

fn now_ms() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0., |d| d.as_secs_f64() * 1000.)
}

/// `assets.createUrl` for the page (`HtmlRenderFrame`'s resource): its URL and expiry.
async fn mint_page_url(
    client: crate::rpc::RpcClient,
    base: String,
    attachment_id: SharedString,
    file_name: String,
) -> Option<(String, f64)> {
    let resource = json!({
        "_tag": "attachment",
        "attachmentId": attachment_id,
        "fileName": file_name,
        "mimeType": "text/html",
        "disposition": "inline",
    });
    match client.request("assets.createUrl", json!({"resource": resource})).await {
        Ok(minted) => {
            let url = format!("{base}{}", minted["relativeUrl"].as_str()?);
            Some((url, minted["expiresAt"].as_f64().unwrap_or(0.)))
        }
        Err(error) => {
            log::debug!("assets.createUrl for a rendered page: {error}");
            None
        }
    }
}

fn page_row(row: &timeline::Row, attachment_id: &SharedString) -> bool {
    matches!(row, timeline::Row::Embed(embed) if embed.attachment_id == *attachment_id)
}

impl Workspace {
    /// `useHtmlRenderTheme`: the active palette and fonts as the page theme.
    fn page_theme_now(&mut self, cx: &App) -> Rc<PageTheme> {
        let tokens = crate::theme::tokens(cx);
        let Some((dark, colors)) = crate::theme::palette(cx) else { return Rc::default() };
        let stamp = (dark, *colors, tokens.fonts.sans, tokens.fonts.mono);
        match &self.html_pages.theme {
            Some((current, theme)) if *current == stamp => theme.clone(),
            _ => {
                let theme = Rc::new(PageTheme::new(&page_theme(dark, colors, super::mcp_app::font_stacks(tokens))));
                self.html_pages.theme = Some((stamp, theme.clone()));
                theme
            }
        }
    }

    /// The shortcuts an inline page hands back, from the primary's keymap and the page's environment's.
    fn page_shortcuts_now(&self, key: &PageKey) -> ShownShortcuts {
        let environment = self.env_info(&key.owner.env).map(|info| info.keybindings.clone());
        let list = PageShortcuts::of(&self.keymap, environment.as_deref().unwrap_or_default(), keymap::MAC);
        ShownShortcuts { keymap: self.keymap.clone(), environment, list }
    }

    /// A shortcut a focused inline page handed back (`HtmlRenderDocument` replays it on the frame).
    /// The page can post any key, so it runs only as an allowed chord no other binding claims, and
    /// resolves as a key pressed with nothing in the app focused.
    fn html_page_key(&mut self, page: &PageKey, key: &PageKeyDown, window: &mut Window, cx: &mut Context<Self>) {
        let Some(shown) = self.html_pages.pages.get(page).and_then(|page| page.shortcuts.as_ref()) else { return };
        let Some(shortcut) = shown.list.admit(key) else { return };
        let keystroke = page_keystroke(key, shortcut);
        let context = keymap::ShortcutContext {
            composer_focus: false,
            composer_requires_modifier: false,
            ..self.shortcut_context(&[], window, cx)
        };
        let keymap = self.keymap.clone();
        let Some(command) = keymap::resolve(&keymap, &keystroke, &context) else { return };
        let focused = window.focused(cx);
        if self.run_command(command, key.repeat, &context, window, cx)
            && window.focused(cx) != focused
            && let Some(embed) = self.html_pages.pages.get(page).and_then(|page| page.embed.as_ref())
        {
            // The command moved focus (the palette, find), which the native page still holds.
            let _ = embed.raw(cx).focus_parent();
        }
    }

    fn page_key(&self, pane: ChatPane, embed: &timeline::Embed) -> Option<PageKey> {
        Some(PageKey { owner: self.pane_owner(pane)?, attachment: embed.attachment_id.clone() })
    }

    fn pane_page_embed(&self, key: &PageKey) -> Option<Rc<timeline::Embed>> {
        self.pane_rows(key.owner.pane).iter().find_map(|row| match row {
            timeline::Row::Embed(embed) if embed.attachment_id == key.attachment => Some(embed.clone()),
            _ => None,
        })
    }

    /// `HtmlRenderTimelineRow`: the page at its frame height, nothing while its URL is minted, and
    /// "Unable to load …" when it can't be.
    pub(super) fn render_html_page(
        &mut self,
        embed: &Rc<timeline::Embed>,
        pane: ChatPane,
        size: &PageSize,
        width: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = *crate::theme::tokens(cx);
        let Some(key) = self.page_key(pane, embed) else { return div().into_any_element() };
        let frame = self.embeds.frame();
        if !self.html_pages.pages.contains_key(&key) {
            self.start_html_page(key.clone(), embed, window, cx);
        }
        let Some(page) = self.html_pages.pages.get_mut(&key) else { return div().into_any_element() };
        page.used = frame;
        let height = frame_height(size, width, page.content_height);
        let body = match (&page.embed, page.failed) {
            (Some(view), _) => view.frame().into_any_element(),
            (None, true) => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_xs()
                .text_color(t.muted_foreground)
                .child(format!("Unable to load {}", embed.title))
                .into_any_element(),
            (None, false) => div().into_any_element(),
        };
        div().w_full().h(px(height)).relative().child(body).into_any_element()
    }

    fn start_html_page(&mut self, key: PageKey, embed: &timeline::Embed, window: &mut Window, cx: &mut Context<Self>) {
        let look = ShellLook::of(crate::theme::tokens(cx));
        let page = Page {
            embed: None,
            url: None,
            failed: false,
            content_height: None,
            dark: look.dark,
            theme: None,
            shortcuts: None,
            used: self.embeds.frame(),
            _task: None,
            _messages: None,
        };
        self.html_pages.pages.insert(key.clone(), page);
        let base = self.env_endpoint(&key.owner.env).map(|endpoint| endpoint.http_base.clone());
        let (Some(client), Some(base)) = (self.env_client(&key.owner.env), base) else {
            self.html_page_failed(&key, cx);
            return;
        };
        let (attachment, file_name, title) = (embed.attachment_id.clone(), embed.file_name(), embed.title.to_string());
        let request = key.clone();
        let task = cx.spawn_in(window, async move |this, cx| {
            let minted = mint_page_url(client, base, attachment, file_name).await;
            let _ = this.update_in(cx, |this, window, cx| match minted {
                Some((url, expires)) => {
                    let theme = this.page_theme_now(cx);
                    let shortcuts = this.page_shortcuts_now(&request);
                    let Some(page) = this.html_pages.pages.get_mut(&request) else { return };
                    page.url = Some((url.clone(), expires));
                    page.theme = Some(theme.clone());
                    let shell = page_shell(&url, &title, &look, true, &theme, &shortcuts.list.script());
                    page.shortcuts = Some(shortcuts);
                    this.build_page_view(Origin::Inline(request), shell, look.dark, window, cx);
                }
                None => this.html_page_failed(&request, cx),
            });
        });
        if let Some(page) = self.html_pages.pages.get_mut(&key) {
            page._task = Some(task);
        }
    }

    /// `environment` (re)connected: the pages, apps and chat images that could not load there load
    /// again, as the web's asset URL queries run again once their connection settles.
    pub(super) fn retry_failed_embeds(&mut self, environment: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.html_pages.forget_failed(environment);
        self.retry_failed_mcp_apps(environment, window, cx);
        self.retry_failed_images(environment, cx);
        cx.notify();
    }

    fn html_page_failed(&mut self, key: &PageKey, cx: &mut Context<Self>) {
        if let Some(page) = self.html_pages.pages.get_mut(key) {
            page.failed = true;
            page._task = None;
            cx.notify();
        }
    }

    /// Builds a shell's view in a task of its own: see [`build_view`](super::preview::build_view).
    fn build_page_view(
        &mut self,
        origin: Origin,
        shell: String,
        dark: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (sender, reports) = async_channel::unbounded::<String>();
        let report_origin = origin.clone();
        let messages = cx.spawn_in(window, async move |this, cx| {
            while let Ok(body) = reports.recv().await {
                let Some(message) = page_message(&body) else { continue };
                let origin = report_origin.clone();
                if this.update_in(cx, |this, window, cx| this.on_page_message(origin, message, window, cx)).is_err() {
                    break;
                }
            }
        });
        let background = crate::theme::tokens(cx).background;
        let rgba = gpui_kit::Rgba::from(background);
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped to 0..=255")]
        let byte = |v: f32| (v.clamp(0., 1.) * 255.).round() as u8;
        let builder = wry::WebViewBuilder::new()
            .with_devtools(true)
            .with_focused(false)
            .with_background_color((byte(rgba.r), byte(rgba.g), byte(rgba.b), 255))
            .with_ipc_handler(move |request| {
                let _ = sender.try_send(request.into_body());
            })
            .with_new_window_req_handler(|_, _| wry::NewWindowResponse::Deny)
            .with_html(shell);
        let built_origin = origin.clone();
        let task = cx.spawn_in(window, async move |this, cx| {
            let origin = built_origin;
            let _ = this.update_in(cx, |this, window, cx| {
                let wanted = match &origin {
                    Origin::Inline(key) => this.html_pages.pages.contains_key(key),
                    Origin::Dialog(key) => this.html_pages.dialog.as_ref().is_some_and(|d| d.key == *key),
                };
                if !wanted {
                    return;
                }
                let built = super::preview::build_view(builder, window, cx);
                let layer = if matches!(origin, Origin::Dialog(_)) { Layer::Dialog } else { Layer::Page };
                let embed = built.map(|view| {
                    let _ = view.set_theme(if dark { wry::Theme::Dark } else { wry::Theme::Light });
                    this.embeds.adopt(view, layer, cx)
                });
                match origin {
                    Origin::Inline(key) => match embed {
                        Ok(embed) => {
                            if let Some(page) = this.html_pages.pages.get_mut(&key) {
                                page.embed = Some(embed);
                                page._task = None;
                            }
                        }
                        Err(error) => {
                            log::warn!("rendered page: {error}");
                            this.html_page_failed(&key, cx);
                        }
                    },
                    Origin::Dialog(_) => match embed {
                        Ok(embed) => {
                            if let Some(dialog) = this.html_pages.dialog.as_mut() {
                                dialog.embed = Some(embed);
                                dialog._task = None;
                            }
                        }
                        Err(error) => {
                            log::warn!("rendered page: {error}");
                            this.close_html_dialog(window, cx);
                            this.page_unavailable(window, cx);
                        }
                    },
                }
                cx.notify();
            });
        });
        match &origin {
            Origin::Inline(key) => {
                if let Some(page) = self.html_pages.pages.get_mut(key) {
                    page._task = Some(task);
                    page._messages = Some(messages);
                }
            }
            Origin::Dialog(_) => {
                if let Some(dialog) = self.html_pages.dialog.as_mut() {
                    dialog._task = Some(task);
                    dialog._messages = Some(messages);
                }
            }
        }
    }

    fn on_page_message(&mut self, origin: Origin, message: PageMessage, window: &mut Window, cx: &mut Context<Self>) {
        let embed = match &origin {
            Origin::Inline(key) => self.html_pages.pages.get(key).and_then(|page| page.embed.as_ref()),
            Origin::Dialog(key) => {
                self.html_pages.dialog.as_ref().filter(|d| d.key == *key).and_then(|d| d.embed.as_ref())
            }
        };
        match message {
            PageMessage::Ready => {
                if let Some(embed) = embed {
                    embed.retell();
                }
                if let Origin::Dialog(_) = origin
                    && let Some(dialog) = &self.html_pages.dialog
                    && let Some(embed) = &dialog.embed
                {
                    let _ = embed.raw(cx).evaluate_script(&format!("window.t3Zoom&&t3Zoom({})", dialog.zoom));
                }
                cx.notify();
            }
            PageMessage::Height(height) => {
                let Origin::Inline(key) = origin else { return };
                let Some(page) = self.html_pages.pages.get_mut(&key) else { return };
                if page.content_height.is_some_and(|h| (h - height).abs() < 0.5) {
                    return;
                }
                page.content_height = Some(height);
                self.remeasure_page_row(&key.attachment, cx);
            }
            PageMessage::Link { url, external } => {
                if external {
                    cx.open_url(&url);
                } else if !self.open_change_request_link(&url, cx) {
                    let in_app = crate::preview::link_opens_in_app(
                        &url,
                        false,
                        &self.client_settings.values,
                        self.browser_available(),
                    );
                    match self.preview_thread().filter(|_| in_app) {
                        Some(thread) => self.request_preview_tab(thread, Some(url), true, window, cx),
                        None => cx.open_url(&url),
                    }
                }
            }
            PageMessage::Image { src, name } => self.open_page_image(&origin, src, name, window, cx),
            PageMessage::Expand => {
                if let Origin::Inline(key) = origin {
                    self.open_html_dialog(key, window, cx);
                }
            }
            // Full screen keeps its keys: the dialog draws over what the commands would open.
            PageMessage::Key(key) => {
                if let Origin::Inline(page) = origin {
                    self.html_page_key(&page, &key, window, cx);
                }
            }
        }
    }

    /// Re-lays out the timeline row of the page `attachment_id`, whose height changed.
    pub(super) fn remeasure_page_row(&mut self, attachment_id: &SharedString, cx: &mut Context<Self>) {
        let is_page = |row: &timeline::Row| page_row(row, attachment_id);
        if let Some(index) = self.rows.iter().position(is_page) {
            self.scroller.update(cx, |scroller, cx| scroller.remeasure_items(index..index + 1, cx));
        }
        if let Some(side) = &self.side_chat.view
            && let Some(index) = side.rows.iter().position(is_page)
        {
            side.scroller.update(cx, |scroller, cx| scroller.remeasure_items(index..index + 1, cx));
        }
        cx.notify();
    }

    /// An image the page asked to expand (`ExpandedImageDialog`): a published image's data URL, or
    /// a web image, loaded and opened in the image viewer while the page's pane still shows its
    /// thread (the viewer is per thread).
    fn open_page_image(
        &mut self,
        origin: &Origin,
        src: String,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (Origin::Inline(key) | Origin::Dialog(key)) = origin;
        let owner = key.owner.clone();
        cx.spawn_in(window, async move |this, cx| {
            let (image, reference) = if src.get(..5).is_some_and(|s| s.eq_ignore_ascii_case("data:")) {
                let image = super::markdown_images::data_url(&src)
                    .map(|(format, bytes)| Arc::new(Image::from_bytes(format, bytes)));
                (image, None)
            } else {
                let image = super::markdown_images::fetch_image(src.clone()).await;
                (image, Some(Reference::Url(src)))
            };
            let _ = this.update_in(cx, |this, window, cx| match image {
                _ if !this.owner_shown(&owner) => {}
                Some(image) => {
                    let item = ViewerImage { image, name: name.into(), reference };
                    this.open_owned_image_viewer(Some(owner), vec![item], 0, window, cx);
                }
                None => window.push_notification(Notification::error("The image could not be loaded."), cx),
            });
        })
        .detach();
    }

    fn page_unavailable(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.push_notification(
            Notification::error("Reconnect to the environment and try again.").title("Page unavailable"),
            cx,
        );
    }

    pub(super) fn html_dialog_open(&self) -> bool {
        self.html_pages.dialog.is_some()
    }

    /// "Open full screen": the page in a dialog, from the frame's URL while it has five minutes
    /// left, else a fresh one.
    fn open_html_dialog(&mut self, key: PageKey, window: &mut Window, cx: &mut Context<Self>) {
        if self.html_pages.dialog.is_some() {
            return;
        }
        let Some(embed) = self.pane_page_embed(&key) else { return };
        let look = ShellLook::of(crate::theme::tokens(cx));
        let restore = window.focused(cx);
        let close_focus = cx.focus_handle().tab_index(0).tab_stop(true);
        close_focus.focus(window, cx);
        self.html_pages.dialog = Some(Dialog {
            key: key.clone(),
            title: embed.title.clone(),
            embed: None,
            zoom: 1.,
            dark: look.dark,
            theme: None,
            focus: cx.focus_handle(),
            close_focus,
            restore,
            _task: None,
            _messages: None,
        });
        let fresh = self
            .html_pages
            .pages
            .get(&key)
            .and_then(|page| page.url.clone())
            .filter(|(_, expires)| expires - now_ms() > MIN_URL_LIFE_MS);
        let title = embed.title.to_string();
        if let Some((url, _)) = fresh {
            let shell = self.dialog_shell(&url, &title, &look, cx);
            self.build_page_view(Origin::Dialog(key), shell, look.dark, window, cx);
            cx.notify();
            return;
        }
        let base = self.env_endpoint(&key.owner.env).map(|endpoint| endpoint.http_base.clone());
        let (Some(client), Some(base)) = (self.env_client(&key.owner.env), base) else {
            self.close_html_dialog(window, cx);
            self.page_unavailable(window, cx);
            return;
        };
        let (attachment, file_name) = (embed.attachment_id.clone(), embed.file_name());
        let task = cx.spawn_in(window, async move |this, cx| {
            let minted = mint_page_url(client, base, attachment, file_name).await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.html_pages.dialog.as_ref().is_none_or(|dialog| dialog.key != key) {
                    return;
                }
                match minted {
                    Some((url, _)) => {
                        let shell = this.dialog_shell(&url, &title, &look, cx);
                        this.build_page_view(Origin::Dialog(key), shell, look.dark, window, cx);
                    }
                    None => {
                        this.close_html_dialog(window, cx);
                        this.page_unavailable(window, cx);
                    }
                }
            });
        });
        if let Some(dialog) = self.html_pages.dialog.as_mut() {
            dialog._task = Some(task);
        }
        cx.notify();
    }

    /// The full screen shell for `url`, in the theme the dialog now records.
    fn dialog_shell(&mut self, url: &str, title: &str, look: &ShellLook, cx: &App) -> String {
        let theme = self.page_theme_now(cx);
        if let Some(dialog) = self.html_pages.dialog.as_mut() {
            dialog.theme = Some(theme.clone());
        }
        page_shell(url, title, look, false, &theme, "")
    }

    fn close_html_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = self.html_pages.dialog.take() else { return };
        if let Some(restore) = dialog.restore {
            restore.focus(window, cx);
        }
        cx.notify();
    }

    fn zoom_html_dialog(&mut self, zoom: Option<f32>, cx: &mut Context<Self>) {
        let Some(dialog) = self.html_pages.dialog.as_mut() else { return };
        let zoom = zoom.map_or(1., |zoom| zoom.clamp(ZOOM_RANGE.0, ZOOM_RANGE.1));
        dialog.zoom = zoom;
        if let Some(embed) = &dialog.embed {
            let _ = embed.raw(cx).evaluate_script(&format!("window.t3Zoom&&t3Zoom({zoom})"));
        }
        cx.notify();
    }

    /// Each frame, before anything draws: pages whose pane left their thread close, as unmounting
    /// does; the rest follow the theme, pages long off screen close, and the dialog closes with the
    /// thread it opened from.
    pub(super) fn sync_html_pages(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let left: Vec<PageKey> =
            self.html_pages.pages.keys().filter(|key| !self.owner_shown(&key.owner)).cloned().collect();
        for key in left {
            self.html_pages.pages.remove(&key);
        }
        if self.html_pages.pages.is_empty() && self.html_pages.dialog.is_none() {
            return;
        }
        let dark = crate::theme::tokens(cx).dark;
        let scheme = if dark { "window.t3Scheme&&t3Scheme('dark')" } else { "window.t3Scheme&&t3Scheme('light')" };
        let theme = if dark { wry::Theme::Dark } else { wry::Theme::Light };
        let page_theme = self.page_theme_now(cx);
        let pages = &mut self.html_pages;
        let views =
            pages.pages.values_mut().map(|page| (&mut page.dark, &mut page.theme, page.embed.as_ref())).chain(
                pages.dialog.as_mut().map(|dialog| (&mut dialog.dark, &mut dialog.theme, dialog.embed.as_ref())),
            );
        for (shown_dark, shown_theme, embed) in views {
            let Some(embed) = embed else { continue };
            if *shown_dark != dark {
                *shown_dark = dark;
                let _ = embed.webview(cx).set_theme(theme);
                let _ = embed.raw(cx).evaluate_script(scheme);
            }
            // `htmlRenderThemeMessage` on a theme change, as the web's frame posts it.
            if shown_theme.as_ref().is_none_or(|shown| !Rc::ptr_eq(shown, &page_theme)) {
                *shown_theme = Some(page_theme.clone());
                let _ = embed.raw(cx).evaluate_script(&page_theme.script);
            }
        }
        let keymap = &self.keymap;
        let stale: Vec<PageKey> = self
            .html_pages
            .pages
            .iter()
            .filter(|(key, page)| {
                let environment = self.env_info(&key.owner.env).map(|info| &info.keybindings);
                page.embed.is_some() && page.shortcuts.as_ref().is_none_or(|shown| !shown.from(keymap, environment))
            })
            .map(|(key, _)| key.clone())
            .collect();
        for key in stale {
            let next = self.page_shortcuts_now(&key);
            let Some(page) = self.html_pages.pages.get_mut(&key) else { continue };
            if let Some(embed) = &page.embed
                && page.shortcuts.as_ref().is_none_or(|shown| shown.list != next.list)
            {
                let _ = embed.raw(cx).evaluate_script(&next.list.script());
            }
            page.shortcuts = Some(next);
        }
        let pages = &mut self.html_pages;
        if pages.pages.len() > KEPT_PAGES {
            let last = self.embeds.frame();
            let mut used: Vec<u64> = pages.pages.values().map(|page| page.used).collect();
            used.sort_unstable_by(|a, b| b.cmp(a));
            let oldest_kept = used[KEPT_PAGES - 1];
            pages.pages.retain(|_, page| page.used >= oldest_kept || page.used + 1 >= last);
        }
        let owner_left = self.html_pages.dialog.as_ref().is_some_and(|dialog| !self.owner_shown(&dialog.key.owner));
        if owner_left {
            self.close_html_dialog(window, cx);
        }
    }

    fn html_dialog_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key == "escape" {
            cx.stop_propagation();
            self.close_html_dialog(window, cx);
        }
    }

    /// `HtmlRenderDialog` over the window: the page in a 92%-wide box, its title and the zoom
    /// controls under it, and Close above its corner.
    pub(super) fn render_html_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let dialog = self.html_pages.dialog.as_ref()?;
        let t = *crate::theme::tokens(cx);
        let white = gpui_kit::white();
        let viewport = window.viewport_size();
        let titlebar = f32::from(super::titlebar::TITLEBAR_HEIGHT);
        let (width, height) = (f32::from(viewport.width), f32::from(viewport.height));
        let box_height = (height * 0.86).min(height - 160.).max(120.);
        let zoom = dialog.zoom;
        let body = match &dialog.embed {
            Some(embed) => embed.frame().into_any_element(),
            None => div().into_any_element(),
        };
        let control = |id: &'static str, label: &'static str, disabled: bool| {
            h_flex()
                .id(id)
                .h(px(28.))
                .min_w(px(28.))
                .justify_center()
                .rounded(px(8.))
                .text_color(white.opacity(0.9))
                .border_1()
                .border_color(gpui_kit::transparent_black())
                .when(!disabled, |el| {
                    el.tab_index(0)
                        .cursor_pointer()
                        .hover(move |style| style.bg(white.opacity(0.1)).text_color(white))
                        .focus_visible(move |style| style.border_color(white))
                })
                .when(disabled, |el| el.opacity(0.5))
                .tooltip(move |window, cx| Tooltip::new(label).build(window, cx))
        };
        let step =
            |id: &'static str, icon: IconName, label: &'static str, delta: f32, limit: f32, cx: &mut Context<Self>| {
                let disabled = (zoom - limit).abs() < f32::EPSILON;
                control(id, label, disabled).child(Icon::new(icon).size(px(16.))).when(!disabled, |el| {
                    el.on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.zoom_html_dialog(Some(zoom + delta), cx);
                    }))
                })
            };
        let zoom_out = step("html-dialog-zoom-out", IconName::Minus, "Zoom out", -ZOOM_STEP, ZOOM_RANGE.0, cx);
        let zoom_in = step("html-dialog-zoom-in", IconName::Plus, "Zoom in", ZOOM_STEP, ZOOM_RANGE.1, cx);
        #[expect(clippy::cast_possible_truncation, reason = "a percentage from 50 to 200")]
        let percent = (zoom * 100.).round() as i32;
        let reset = control("html-dialog-zoom-reset", "Reset zoom", false)
            .px(px(10.))
            .text_sm()
            .child(format!("{percent}%"))
            .on_click(cx.listener(|this, _, _, cx| {
                cx.stop_propagation();
                this.zoom_html_dialog(None, cx);
            }));
        let close = div()
            .id("html-dialog-close")
            .track_focus(&dialog.close_focus)
            .absolute()
            .right_0()
            .top(px(-40.))
            .size(px(28.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(8.))
            .cursor_pointer()
            .bg(gpui_kit::black().opacity(0.65))
            .text_color(white)
            .border_1()
            .border_color(white.opacity(0.2))
            .shadow_sm()
            .hover(|style| style.bg(gpui_kit::black().opacity(0.8)))
            .focus_visible(move |style| style.border_color(white))
            .child(Icon::new(IconName::X).size(px(16.)))
            .on_click(cx.listener(|this, _, window, cx| {
                cx.stop_propagation();
                this.close_html_dialog(window, cx);
            }));
        let content = v_flex()
            .id("html-dialog-content")
            .relative()
            .w(px(width * 0.92))
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(close)
            .child(
                div()
                    .h(px(box_height))
                    .w_full()
                    .p(px(16.))
                    .rounded(px(8.))
                    .bg(t.background)
                    .border_1()
                    .border_color(t.border.opacity(0.7))
                    .shadow_2xl()
                    .child(body),
            )
            .child(
                h_flex()
                    .mt(px(8.))
                    .min_w_0()
                    .justify_center()
                    .gap(px(12.))
                    .text_xs()
                    .text_color(white.opacity(0.8))
                    .child(div().min_w_0().truncate().child(dialog.title.clone()))
                    .child(h_flex().flex_none().gap(px(4.)).child(zoom_out).child(reset).child(zoom_in)),
            )
            .focus_trap("html-dialog-trap", &dialog.focus);
        Some(
            div()
                .id("html-dialog")
                .absolute()
                .inset_0()
                .occlude()
                .bg(gpui_kit::black().opacity(0.75))
                .flex()
                .items_center()
                .justify_center()
                .px(px(16.))
                .pb(px(24.))
                .pt(px(titlebar + 24.))
                .on_key_down(cx.listener(Self::html_dialog_key_down))
                .on_click(cx.listener(|this, _, window, cx| this.close_html_dialog(window, cx)))
                .child(content)
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(key: &str, code: &str, ctrl: bool, shift: bool) -> PageKeyDown {
        PageKeyDown { key: key.into(), code: code.into(), meta: false, ctrl, shift, alt: false, repeat: false }
    }

    fn chord(key: &str, shift: bool) -> PageShortcut {
        PageShortcut { key: key.into(), meta_key: false, ctrl_key: true, shift_key: shift, alt_key: false }
    }

    #[test]
    fn pages_hand_back_only_allowed_chords_no_other_binding_claims() {
        let rule = |key: &str, command: &str| crate::keybindings::compile(key, command, None).unwrap();
        let primary = [
            rule("mod+k", "commandPalette.toggle"),
            rule("mod+f", "chat.find"),
            rule("mod+shift+k", "commandPalette.toggle"),
            rule("mod+k", "commandPalette.toggle"),
            rule("f6", "sidebar.toggle"),
            rule("mod+2", "thread.jump.2"),
            rule("mod+n", "chat.new"),
            rule("mod+j", "terminal.toggle"),
            rule("mod+j", "composer.focus"),
        ];
        let environment = [rule("mod+shift+f", "script.build.run")];
        let shortcuts = PageShortcuts::of(&primary, &environment, false);
        assert_eq!(
            shortcuts.allowed,
            [chord("k", false), chord("f", false), chord("k", true), chord("2", false), chord("j", false)],
            "Cmd/Ctrl chords of the allowed commands, once each; F6 alone stays with the page"
        );
        assert_eq!(shortcuts.blocked, [chord("n", false), chord("j", false), chord("f", true)]);
        let mac = PageShortcuts::of(&primary[..1], &[], true);
        assert_eq!(mac.allowed, [PageShortcut { meta_key: true, ctrl_key: false, ..chord("k", false) }]);

        assert_eq!(shortcuts.admit(&press("k", "KeyK", true, false)), Some(&chord("k", false)));
        assert_eq!(shortcuts.admit(&press("K", "KeyK", true, true)), Some(&chord("k", true)));
        assert_eq!(shortcuts.admit(&press("л", "KeyK", true, false)), Some(&chord("k", false)), "by its physical key");
        assert_eq!(shortcuts.admit(&press("@", "Digit2", true, false)), Some(&chord("2", false)));
        assert_eq!(shortcuts.admit(&press("Enter", "KeyK", true, false)), None, "a forged named key");
        assert_eq!(shortcuts.admit(&press("k", "KeyK", false, false)), None, "the modifiers must match");
        assert_eq!(shortcuts.admit(&press("j", "KeyJ", true, false)), None, "another binding claims it");
        assert_eq!(shortcuts.admit(&press("n", "KeyN", true, false)), None);

        // An admitted key replays through the keymap as the command it is bound to.
        let defaults = PageShortcuts::of(crate::keybindings::defaults(), &[], keymap::MAC);
        let context = keymap::ShortcutContext::default();
        let replay = |key: PageKeyDown| {
            let key = PageKeyDown { meta: keymap::MAC && key.ctrl, ctrl: !keymap::MAC && key.ctrl, ..key };
            let shortcut = defaults.admit(&key).unwrap();
            keymap::resolve(crate::keybindings::defaults(), &page_keystroke(&key, shortcut), &context)
        };
        assert_eq!(replay(press("[", "BracketLeft", true, false)), Some("navigation.back"));
        assert_eq!(replay(press("B", "KeyB", true, true)), Some("threadPanel.toggle"));

        let script = shortcuts.script();
        let message = script.strip_prefix("window.t3Bridge&&t3Bridge('shortcuts',").and_then(|s| s.strip_suffix(");"));
        let message: Value = serde_json::from_str(message.unwrap()).unwrap();
        assert_eq!(message["method"], "t3/notifications/shortcuts");
        assert_eq!(
            message["params"]["shortcuts"][0],
            json!({"key": "k", "metaKey": false, "ctrlKey": true, "shiftKey": false, "altKey": false})
        );
        assert_eq!(message["params"]["blocked"].as_array().map(Vec::len), Some(3));
    }

    #[test]
    fn a_reconnect_forgets_only_that_environments_failed_pages() {
        let page = |failed| Page {
            embed: None,
            url: None,
            failed,
            content_height: None,
            dark: false,
            theme: None,
            shortcuts: None,
            used: 0,
            _task: None,
            _messages: None,
        };
        let key = |env: &str, attachment: &str| PageKey {
            owner: PaneOwner { pane: ChatPane::Main, env: env.to_owned().into(), thread: "t".into() },
            attachment: attachment.to_owned().into(),
        };
        let mut pages = HtmlPages::default();
        pages.pages.insert(key("a", "failed"), page(true));
        pages.pages.insert(key("a", "loading"), page(false));
        pages.pages.insert(key("b", "failed"), page(true));
        pages.forget_failed("a");
        let mut left: Vec<_> = pages.pages.keys().map(|k| format!("{}/{}", k.owner.env, k.attachment)).collect();
        left.sort();
        assert_eq!(left, ["a/loading", "b/failed"]);
    }

    #[test]
    fn frames_fit_the_page_like_html_render_frame_height() {
        let plain = PageSize { height: 640., heights: vec![] };
        assert_eq!(frame_height(&plain, 728., None), 640.);
        // The live page wins, within 80 and the fit cap.
        assert_eq!(frame_height(&plain, 728., Some(912.2)), 913.);
        assert_eq!(frame_height(&plain, 728., Some(10.)), 80.);
        assert_eq!(frame_height(&plain, 728., Some(1e9)), 20_000.);
        let measured = PageSize { height: 900., heights: vec![(320, 1400.), (640, 1000.), (728, 800.), (1000, 700.)] };
        // Between two measurements the taller one counts; an exact width uses its own.
        assert_eq!(frame_height(&measured, 700., None), 1000.);
        assert_eq!(frame_height(&measured, 728., None), 800.);
        // Past the widest, the two widest still count.
        assert_eq!(frame_height(&measured, 2000., None), 800.);
        assert_eq!(frame_height(&measured, 100., None), 1400.);
        // An agent height under the column's measurement caps the narrower measurements.
        let capped = PageSize { height: 600., heights: vec![(320, 1400.), (728, 800.)] };
        assert_eq!(frame_height(&capped, 320., None), 600.);
    }

    #[test]
    fn shells_forward_the_bridge_with_its_activation() {
        let page = |active: bool, data: Value| {
            page_message(&json!({"type": "page", "active": active, "data": data}).to_string())
        };
        let rpc = |method: &str, params: Value| json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
        assert_eq!(
            page(false, rpc("ui/notifications/size-changed", json!({"height": 420.5}))),
            Some(PageMessage::Height(420.5))
        );
        assert_eq!(page(true, rpc("ui/notifications/size-changed", json!({"height": 0}))), None);
        let link = rpc("ui/open-link", json!({"url": "HTTPS://x.dev/a", "ctrlKey": true}));
        assert_eq!(page(false, link.clone()), None, "a page can't open links without the reader");
        assert_eq!(page(true, link), Some(PageMessage::Link { url: "HTTPS://x.dev/a".into(), external: true }));
        assert_eq!(page(true, rpc("ui/open-link", json!({"url": "javascript:alert(1)"}))), None);
        assert_eq!(
            page(true, json!({"jsonrpc": "2.0", "method": "ui/open-link", "params": {"url": "https://x.dev"}})),
            None,
            "a link request needs its id"
        );
        assert_eq!(
            page(true, rpc("t3/notifications/open-image", json!({"src": "data:image/png;base64,AA", "name": "  "}))),
            Some(PageMessage::Image { src: "data:image/png;base64,AA".into(), name: "Image".into() })
        );
        assert_eq!(page(true, rpc("t3/notifications/open-image", json!({"src": "file:///etc/passwd"}))), None);
        let key = rpc("t3/notifications/keydown", json!({"key": "k", "code": "KeyK", "ctrlKey": true, "repeat": true}));
        assert_eq!(page(false, key.clone()), None, "a page can't press keys without the reader");
        assert_eq!(
            page(true, key),
            Some(PageMessage::Key(PageKeyDown { repeat: true, ..press("k", "KeyK", true, false) }))
        );
        assert_eq!(page(true, rpc("t3/notifications/keydown", json!({"key": "k"}))), None, "a key needs its code");
        assert_eq!(page(true, json!({"method": "ui/notifications/size-changed", "params": {"height": 9}})), None);
        assert_eq!(page_message(r#"{"type":"ready"}"#), Some(PageMessage::Ready));
        assert_eq!(page_message(r#"{"type":"expand"}"#), Some(PageMessage::Expand));
        assert_eq!(page_message("nope"), None);
    }

    fn look() -> ShellLook {
        ShellLook {
            dark: true,
            background: "#000".into(),
            glass: "#111".into(),
            border: "#222".into(),
            icon: "#333".into(),
            hover_border: "#444".into(),
            ring: "#555".into(),
        }
    }

    #[test]
    fn the_shell_sandboxes_the_page_with_the_apps_theme_fragment() {
        let theme = PageTheme { fragment: "#t3-theme=%7B%7D".into(), script: "t3Bridge('theme',{});".into() };
        let inline = page_shell("http://h/a?x=1&y=\"2\"#t3-theme=x", "A <b>", &look(), true, &theme, "");
        assert!(inline.contains("src=\"http://h/a?x=1&amp;y=&quot;2&quot;#t3-theme=%7B%7D\" title=\"A &lt;b>\""));
        assert!(inline.contains("sandbox=\"allow-scripts allow-forms\"") && !inline.contains("allow-same-origin"));
        assert!(inline.contains("aria-label=\"Open full screen\"") && inline.contains("content=\"dark\""));
        assert!(inline.contains("t3Bridge('theme',{});</script>"));
        let full = page_shell("http://h/a", "A", &look(), false, &PageTheme::default(), "");
        assert!(!full.contains("id=\"x\"") && full.contains("html,body{overflow:auto}"));
    }

    #[test]
    fn pages_take_the_palette_like_html_render_theme() {
        let colors = ThemeColors::from_roles(crate::themes::standard_roles(true), None).unwrap();
        let fonts = ("\"Inter\", sans-serif".to_owned(), "monospace".to_owned());
        let theme = page_theme(true, &colors, fonts);
        let variables = &theme["variables"];
        assert_eq!(theme["appearance"], "dark");
        assert_eq!(variables["--background"], "#000000", "the standard dark canvas");
        assert_eq!(variables["--primary"], css_color(colors.message_action).as_str());
        assert_eq!(variables["--accent"], css_color(colors.accent).as_str(), "the brand, not the hover surface");
        assert_eq!(variables["--chart-1"], variables["--accent"]);
        assert_eq!(variables["--accent-surface"], css_color(colors.accent_surface).as_str());
        assert_eq!((&variables["--success"], &variables["--chart-6"]), (&json!("#10b981"), &json!("#a3e635")));
        assert_eq!(variables["--font-sans"], "\"Inter\", sans-serif");
        assert_eq!(variables.as_object().map(serde_json::Map::len), Some(40));

        let page = PageTheme::new(&json!({"appearance": "light", "variables": {"--font-sans": "A </script> & b"}}));
        assert_eq!(
            page.fragment,
            "#t3-theme=%7B%22appearance%22%3A%22light%22%2C%22variables%22%3A%7B%22--font-sans%22%3A%22A%20%3C%2Fscript%3E%20%26%20b%22%7D%7D"
        );
        let message = page.script.strip_prefix("window.t3Bridge&&t3Bridge('theme',").and_then(|s| s.strip_suffix(");"));
        let message = message.unwrap();
        assert!(!message.contains('<'), "the script can't be closed early");
        assert_eq!(
            serde_json::from_str::<Value>(message).unwrap(),
            json!({
                "jsonrpc": "2.0",
                "method": "ui/notifications/host-context-changed",
                "params": {"theme": "light", "styles": {"variables": {"--font-sans": "A </script> & b"}}},
            })
        );
    }
}
