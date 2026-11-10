//! MCP apps in the timeline (`McpAppFrame`): the app's page in a sandboxed iframe of an embed's
//! shell, which forwards the frame's `postMessage` bridge over IPC to the app's [`Host`].
//!
//! Every request an app makes runs against the thread item that produced it, on the environment
//! it was shown from: tool calls and resource reads go through `mcpApps.*` there, messages go
//! to the thread the app sits in, and changes ask the reader first. The shell signs what it
//! forwards with a nonce the cross-origin frame cannot read, so the frame cannot pose as it.

use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AsyncWindowContext, Context, FocusHandle, Hsla, InteractiveElement as _, IntoElement as _,
    KeyDownEvent, MouseButton, ParentElement as _, SharedString, Styled as _, Task, WeakEntity, Window, div, px,
};
use serde_json::{Map, Value, json};

use super::Workspace;
use super::embed::{EmbedView, FRAME_CSS, FRAME_SCRIPT, Layer};
use super::html_page::escape_attribute;
use super::mermaid::css_color;
use crate::mcp_apps::{
    self, AppReference, ContextInput, DisplayMode, Download, Event, Host, Outcome, Request, Ticket, ToolCall,
};
use crate::theme::Tokens;
use crate::timeline;

/// Apps kept loaded off screen, beside the ones on it.
const KEPT_APPS: usize = 4;
/// `TEARDOWN_TIMEOUT_MS`: how long an app gets to answer `ui/resource-teardown`.
const TEARDOWN_TIMEOUT: Duration = Duration::from_secs(2);
/// The full screen header (`h-10`).
const HEADER_HEIGHT: f32 = 40.;
const APP_VERSION: &str = match option_env!("T3CODE_APP_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};
/// `HTML_RENDER_DEFAULT_FONTS`, after the reader's own font.
const DEFAULT_SANS: &str = "-apple-system, BlinkMacSystemFont, \"Segoe UI\", system-ui, sans-serif";
const DEFAULT_MONO: &str = "\"SF Mono\", \"SFMono-Regular\", Menlo, Consolas, \"Liberation Mono\", monospace";

/// What an app's shell reports over IPC.
#[derive(Debug, PartialEq)]
enum ShellMessage {
    /// The shell (re)loaded and listens for its box.
    Ready,
    /// The frame finished loading a document.
    Load,
    /// A message the app posted, and whether the reader was just using the frame.
    App { active: bool, data: Value, bytes: usize },
}

/// Reads a report, dropping any not signed with the shell's `nonce`.
fn shell_message(body: &str, nonce: &str) -> Option<ShellMessage> {
    let mut value: Value = serde_json::from_str(body).ok()?;
    if value["type"] == "ready" {
        return Some(ShellMessage::Ready);
    }
    if value["n"] != nonce {
        return None;
    }
    match value["type"].as_str()? {
        "load" => Some(ShellMessage::Load),
        "app" => {
            Some(ShellMessage::App { active: value["active"] == true, data: value["data"].take(), bytes: body.len() })
        }
        _ => None,
    }
}

/// The shell's bridge: forwards what the frame posts, with whether the frame has focus and the
/// reader's activation, and the frame's loads; `t3Post` delivers to the frame alone.
fn bridge_script(nonce: &str) -> String {
    format!(
        "(()=>{{const n='{nonce}',f=document.getElementById('f');\
         const post=(m)=>{{m.n=n;window.ipc.postMessage(JSON.stringify(m));}};\
         addEventListener('message',(e)=>{{if(e.source!==f.contentWindow)return;\
         const active=document.activeElement===f&&!(navigator.userActivation&&navigator.userActivation.isActive===false);\
         try{{post({{type:'app',active,data:e.data}});}}catch(_){{}}}});\
         f.addEventListener('load',()=>post({{type:'load'}}));\
         window.t3Post=(m)=>{{if(f.contentWindow)f.contentWindow.postMessage(m,'*');}};\
         window.t3Scheme=(s)=>{{f.style.colorScheme=s;document.querySelector('meta[name=color-scheme]').content=s;}};}})();"
    )
}

/// The embed's shell: the app in a frame sandboxed without `allow-same-origin`, which keeps it
/// out of the app's session, with the features it declared.
fn app_shell(url: &str, app: &AppReference, nonce: &str, dark: bool, background: &str) -> String {
    let scheme = if dark { "dark" } else { "light" };
    let src = escape_attribute(url);
    let title = escape_attribute(&format!("{} app", app.server));
    let allow = escape_attribute(&app.allow_attribute());
    let bridge = bridge_script(nonce);
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"color-scheme\" content=\"{scheme}\">\
         <style>{FRAME_CSS}html{{background:{background}}}\
         iframe{{display:block;border:0;width:100%;height:100%;color-scheme:{scheme}}}</style></head>\
         <body><div id=\"t3-embed\"><iframe id=\"f\" src=\"{src}\" title=\"{title}\" sandbox=\"allow-scripts allow-forms\" \
         allow=\"{allow}\"></iframe></div><script>{FRAME_SCRIPT}{bridge}</script></body></html>"
    )
}

/// `htmlRenderTheme`'s variables, which `mcpAppStyleVariables` renames for the app.
fn theme_variables(t: &Tokens) -> Vec<(&'static str, String)> {
    let surface = if t.dark { 0.16 } else { 0.08 };
    let font = |name: &str, fallback: &str| {
        if name.is_empty() { fallback.to_owned() } else { format!("\"{name}\", {fallback}") }
    };
    vec![
        ("--background", css_color(t.background)),
        ("--foreground", css_color(t.foreground)),
        ("--muted", css_color(t.muted)),
        ("--muted-foreground", css_color(t.muted_foreground)),
        ("--card", css_color(t.card)),
        ("--border", css_color(t.border)),
        ("--input", css_color(t.input)),
        ("--ring", css_color(t.ring)),
        ("--destructive", css_color(t.error)),
        ("--destructive-foreground", css_color(t.error_foreground)),
        ("--destructive-surface", css_color(t.error.opacity(surface))),
        ("--warning-foreground", css_color(t.warning_foreground)),
        ("--warning-surface", css_color(t.warning.opacity(surface))),
        ("--success-foreground", css_color(t.success_foreground)),
        ("--info", css_color(t.info)),
        ("--info-foreground", css_color(t.info_foreground)),
        ("--radius", "0.625rem".to_owned()),
        ("--font-sans", font(t.fonts.sans, DEFAULT_SANS)),
        ("--font-mono", font(t.fonts.mono, DEFAULT_MONO)),
    ]
}

/// What the theme variables are read from, compared each frame without allocating.
#[derive(Clone, Copy, PartialEq)]
struct ThemeKey {
    dark: bool,
    colors: [Hsla; 15],
    fonts: (&'static str, &'static str),
}

impl ThemeKey {
    fn of(t: &Tokens) -> Self {
        Self {
            dark: t.dark,
            colors: [
                t.background,
                t.foreground,
                t.muted,
                t.muted_foreground,
                t.card,
                t.border,
                t.input,
                t.ring,
                t.error,
                t.error_foreground,
                t.warning,
                t.warning_foreground,
                t.success_foreground,
                t.info,
                t.info_foreground,
            ],
            fonts: (t.fonts.sans, t.fonts.mono),
        }
    }
}

/// What the app's host context was last built from.
#[derive(Clone, Copy, PartialEq)]
struct ContextKey {
    theme: ThemeKey,
    width: f32,
    display: DisplayMode,
    fullscreen: (f32, f32),
    has_tool: bool,
}

/// `htmlRenderTheme`'s variables, by their T3 names.
type ThemeVariables = Rc<[(&'static str, String)]>;

/// An app by environment and attachment.
type AppKey = (SharedString, SharedString);

/// The thread item that produced an app: every request it makes runs against this.
#[derive(Clone)]
struct Scope {
    environment: SharedString,
    thread_id: String,
    item_id: String,
    /// The thread the app is shown in, which its messages go to.
    conversation: String,
    server: String,
}

impl Scope {
    fn target(&self) -> Map<String, Value> {
        let mut target = Map::new();
        target.insert("threadId".into(), self.thread_id.clone().into());
        target.insert("itemId".into(), self.item_id.clone().into());
        target
    }
}

/// One document the frame loaded: replaced when the app is closed and shown again.
struct Document {
    generation: u64,
    host: Host,
    nonce: String,
    failed: bool,
    navigated_away: bool,
    loads: u32,
    embed: Option<EmbedView>,
    /// Waits out an app's teardown.
    closing: Option<Task<()>>,
    _task: Option<Task<()>>,
    _messages: Option<Task<()>>,
}

struct AppFrame {
    scope: Scope,
    reference: Rc<AppReference>,
    /// The item revision the stored call was read at.
    revision: SharedString,
    tool_call: Option<ToolCall>,
    tool_definition: Option<Value>,
    /// `None` once the app was closed.
    document: Option<Document>,
    height: f32,
    display: DisplayMode,
    width: f32,
    /// The thread was waiting on the reader last frame.
    awaiting: bool,
    dark: bool,
    context: Option<ContextKey>,
    /// The frame this app last drew in.
    used: u64,
    _detail: Option<Task<()>>,
    _tool_info: Option<Task<()>>,
}

#[derive(Default)]
pub(super) struct McpApps {
    apps: HashMap<AppKey, AppFrame>,
    fullscreen: Option<AppKey>,
    next_generation: u64,
    /// The full screen layer's, so its controls take the keyboard once clicked.
    focus: Option<FocusHandle>,
    theme: Option<(ThemeKey, ThemeVariables)>,
}

fn downloads_dir() -> std::path::PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(|home| std::path::PathBuf::from(home).join("Downloads"))
        .unwrap_or_default()
}

/// Asks the reader in the confirm dialog, with the app back inline so it stays in view.
async fn ask(
    this: &WeakEntity<Workspace>,
    key: &AppKey,
    message: String,
    cx: &mut AsyncWindowContext,
) -> Result<(), String> {
    const UNASKABLE: &str = "T3 could not ask for approval here.";
    let answer = this
        .update_in(cx, |this, window, cx| {
            this.set_app_display(key, DisplayMode::Inline, cx);
            super::confirm::confirm(&message, false, window, cx)
        })
        .map_err(|_| UNASKABLE.to_owned())?;
    match answer.await {
        Ok(true) => Ok(()),
        Ok(false) => Err("Declined by the user.".to_owned()),
        Err(_) => Err(UNASKABLE.to_owned()),
    }
}

impl Workspace {
    /// `McpAppFrame` in its row: the app at the height it reports, nothing while its URL is
    /// minted, and why it stopped once it did. Full screen leaves the row's height in place.
    pub(super) fn render_mcp_app(
        &mut self,
        embed: &Rc<timeline::Embed>,
        width: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(reference) = embed.app.clone() else { return div().into_any_element() };
        let Some(environment) = self.active_environment().cloned() else { return div().into_any_element() };
        let key = (environment, embed.attachment_id.clone());
        if !self.mcp_apps.apps.contains_key(&key) && !self.start_mcp_app(key.clone(), embed, reference, window, cx) {
            return div().into_any_element();
        }
        if self.mcp_apps.apps.get(&key).is_some_and(|app| app.revision != embed.revision) {
            self.fetch_app_tool_call(&key, embed.revision.clone(), cx);
        }
        let frame = self.embeds.frame();
        let t = *crate::theme::tokens(cx);
        let Some(app) = self.mcp_apps.apps.get_mut(&key) else { return div().into_any_element() };
        app.used = frame;
        app.width = width.round();
        let server = app.scope.server.clone();
        let Some(document) = &app.document else {
            let show = cx.listener(move |this, _, window, cx| this.show_mcp_app(&key, window, cx));
            return h_flex()
                .w_full()
                .justify_between()
                .gap(px(8.))
                .rounded(px(8.))
                .border_1()
                .border_color(t.border)
                .px(px(12.))
                .py(px(8.))
                .text_xs()
                .text_color(t.muted_foreground)
                .child(format!("The {server} app was closed"))
                .child(
                    Button::new(SharedString::from(format!("mcp-app-show:{}", embed.attachment_id)))
                        .ghost()
                        .xsmall()
                        .label("Show app")
                        .on_click(show),
                )
                .into_any_element();
        };
        let notice = |text: String| {
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_xs()
                .text_color(t.muted_foreground)
                .child(text)
                .into_any_element()
        };
        let fullscreen = app.display == DisplayMode::Fullscreen;
        let body = match (&document.embed, document.navigated_away, document.failed) {
            (_, true, _) => notice(format!("The {server} app left its page and was stopped")),
            (Some(_), _, _) if fullscreen => div().into_any_element(),
            (Some(view), ..) => view.frame().into_any_element(),
            (None, _, true) => notice(format!("Unable to load the {server} app")),
            (None, ..) => div().into_any_element(),
        };
        let border = app.reference.prefers_border == Some(true) && !fullscreen;
        div()
            .w_full()
            .h(px(app.height))
            .child(
                div()
                    .relative()
                    .size_full()
                    .overflow_hidden()
                    .when(border, |el| el.rounded(px(8.)).border_1().border_color(t.border))
                    .child(body),
            )
            .into_any_element()
    }

    /// The thread whose rows show the app `attachment_id`: the open thread's, else the side chat's.
    fn app_conversation(&self, attachment_id: &SharedString) -> Option<String> {
        let shows = |rows: &[timeline::Row]| {
            rows.iter().any(|row| matches!(row, timeline::Row::Embed(e) if e.attachment_id == *attachment_id))
        };
        if shows(&self.rows[..]) {
            return self.thread.as_ref().map(|thread| thread.id.clone());
        }
        if self.side_chat.view.as_ref().is_some_and(|view| shows(&view.rows[..])) {
            return self.side_chat_thread_id().map(str::to_owned);
        }
        None
    }

    fn start_mcp_app(
        &mut self,
        key: AppKey,
        embed: &timeline::Embed,
        reference: Rc<AppReference>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let (Some((thread_id, item_id)), Some(conversation)) =
            (self.source_of(&embed.id), self.app_conversation(&embed.attachment_id))
        else {
            return false;
        };
        let scope =
            Scope { environment: key.0.clone(), thread_id, item_id, conversation, server: reference.server.clone() };
        let app = AppFrame {
            scope,
            reference,
            revision: SharedString::default(),
            tool_call: None,
            tool_definition: None,
            document: None,
            height: mcp_apps::DEFAULT_HEIGHT,
            display: DisplayMode::Inline,
            width: 0.,
            awaiting: false,
            dark: crate::theme::tokens(cx).dark,
            context: None,
            used: self.embeds.frame(),
            _detail: None,
            _tool_info: None,
        };
        self.mcp_apps.apps.insert(key.clone(), app);
        self.fetch_app_tool_call(&key, embed.revision.clone(), cx);
        self.fetch_app_tool_definition(&key, cx);
        self.open_app_document(&key, window, cx);
        true
    }

    /// `useTurnItemDetail`: the stored call (its arguments, and its result once it settles),
    /// read again whenever the item's revision moves.
    fn fetch_app_tool_call(&mut self, key: &AppKey, revision: SharedString, cx: &mut Context<Self>) {
        let client = self.app_client(key);
        let Some(app) = self.mcp_apps.apps.get_mut(key) else { return };
        app.revision = revision.clone();
        let Some(client) = client else { return };
        let mut payload = app.scope.target();
        payload.insert("revision".into(), revision.to_string().into());
        let key = key.clone();
        app._detail = Some(cx.spawn(async move |this, cx| {
            let response = client.request("orchestration.getTurnItem", Value::Object(payload)).await;
            let call = match response {
                Ok(response) => ToolCall::from_item(&response["item"]),
                Err(error) => {
                    log::debug!("orchestration.getTurnItem for an MCP app: {error}");
                    None
                }
            };
            let Some(call) = call else { return };
            let _ = this.update(cx, |this, cx| {
                let Some(app) = this.mcp_apps.apps.get_mut(&key).filter(|app| app.revision == revision) else {
                    return;
                };
                app.tool_call = Some(call.clone());
                if let Some(document) = app.document.as_mut() {
                    document.host.set_tool_call(call);
                    flush(document, cx);
                }
            });
        }));
    }

    /// `toolInfoQuery`: the tool definition the app reads from its host context.
    fn fetch_app_tool_definition(&mut self, key: &AppKey, cx: &mut Context<Self>) {
        let client = self.app_client(key);
        let Some(app) = self.mcp_apps.apps.get_mut(key) else { return };
        let Some(client) = client else { return };
        let mut payload = app.scope.target();
        payload.insert("name".into(), app.reference.tool.clone().into());
        let key = key.clone();
        app._tool_info = Some(cx.spawn(async move |this, cx| {
            let Ok(info) = client.request("mcpApps.toolInfo", Value::Object(payload)).await else { return };
            let Some(tool) = info.get("tool").filter(|tool| !tool.is_null()).cloned() else { return };
            let _ = this.update(cx, |this, cx| {
                if let Some(app) = this.mcp_apps.apps.get_mut(&key) {
                    app.tool_definition = Some(tool);
                    cx.notify();
                }
            });
        }));
    }

    /// The socket of the environment the app was shown from.
    fn app_client(&self, key: &AppKey) -> Option<crate::rpc::RpcClient> {
        self.env_client(&self.mcp_apps.apps.get(key)?.scope.environment)
    }

    /// Mints the app's URL (`assets.createUrl`, `renderIntent: "mcp-app"`) and loads it in a new
    /// document with its own host.
    fn open_app_document(&mut self, key: &AppKey, window: &mut Window, cx: &mut Context<Self>) {
        self.mcp_apps.next_generation += 1;
        let generation = self.mcp_apps.next_generation;
        let base = self.env_endpoint(&key.0).map(|endpoint| endpoint.http_base.clone());
        let client = self.env_client(&key.0);
        let Some(app) = self.mcp_apps.apps.get_mut(key) else { return };
        let mut host = Host::new(&app.reference, APP_VERSION);
        if let Some(call) = app.tool_call.clone() {
            host.set_tool_call(call);
        }
        app.document = Some(Document {
            generation,
            host,
            nonce: uuid::Uuid::new_v4().simple().to_string(),
            failed: false,
            navigated_away: false,
            loads: 0,
            embed: None,
            closing: None,
            _task: None,
            _messages: None,
        });
        let (Some(client), Some(base)) = (client, base) else {
            self.app_document_failed(key, generation, cx);
            return;
        };
        let resource = json!({
            "_tag": "attachment",
            "attachmentId": app.reference.attachment_id,
            "fileName": app.reference.file_name(),
            "mimeType": "text/html",
            "disposition": "inline",
            "renderIntent": "mcp-app",
        });
        let request = key.clone();
        let task = cx.spawn_in(window, async move |this, cx| {
            let minted = client.request("assets.createUrl", json!({"resource": resource})).await;
            let url = match minted {
                Ok(minted) => minted["relativeUrl"].as_str().map(|url| format!("{base}{url}")),
                Err(error) => {
                    log::debug!("assets.createUrl for an MCP app: {error}");
                    None
                }
            };
            let _ = this.update_in(cx, |this, window, cx| match url {
                Some(url) => this.build_app_view(&request, generation, &url, window, cx),
                None => this.app_document_failed(&request, generation, cx),
            });
        });
        if let Some(document) = self.app_document(key, generation) {
            document._task = Some(task);
        }
    }

    fn app_document(&mut self, key: &AppKey, generation: u64) -> Option<&mut Document> {
        self.mcp_apps.apps.get_mut(key)?.document.as_mut().filter(|document| document.generation == generation)
    }

    fn app_document_failed(&mut self, key: &AppKey, generation: u64, cx: &mut Context<Self>) {
        if let Some(document) = self.app_document(key, generation) {
            document.failed = true;
            document.host.dispose();
            document._task = None;
            cx.notify();
        }
    }

    /// Builds the shell's view in a task of its own: see [`build_view`](super::preview::build_view).
    fn build_app_view(
        &mut self,
        key: &AppKey,
        generation: u64,
        url: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let t = *crate::theme::tokens(cx);
        let Some(app) = self.mcp_apps.apps.get(key) else { return };
        let Some(document) = app.document.as_ref().filter(|document| document.generation == generation) else {
            return;
        };
        let nonce = document.nonce.clone();
        let shell = app_shell(url, &app.reference, &nonce, t.dark, &css_color(t.background));
        let (sender, reports) = async_channel::unbounded::<String>();
        let origin = key.clone();
        let messages = cx.spawn_in(window, async move |this, cx| {
            while let Ok(body) = reports.recv().await {
                let Some(message) = shell_message(&body, &nonce) else { continue };
                let key = origin.clone();
                if this
                    .update_in(cx, |this, window, cx| this.on_app_message(&key, generation, message, window, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        let rgba = gpui_kit::Rgba::from(t.background);
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
        let request = key.clone();
        let dark = t.dark;
        let task = cx.spawn_in(window, async move |this, cx| {
            let _ = this.update_in(cx, |this, window, cx| {
                if this.app_document(&request, generation).is_none() {
                    return;
                }
                let built = super::preview::build_view(builder, window, cx);
                match built {
                    Ok(view) => {
                        let _ = view.set_theme(if dark { wry::Theme::Dark } else { wry::Theme::Light });
                        let embed = this.embeds.adopt(view, Layer::Page, cx);
                        if let Some(app) = this.mcp_apps.apps.get_mut(&request) {
                            app.dark = dark;
                            if app.display == DisplayMode::Fullscreen {
                                embed.set_layer(Layer::Dialog);
                            }
                        }
                        if let Some(document) = this.app_document(&request, generation) {
                            document.embed = Some(embed);
                            document._task = None;
                        }
                    }
                    Err(error) => {
                        log::warn!("MCP app: {error}");
                        this.app_document_failed(&request, generation, cx);
                    }
                }
                cx.notify();
            });
        });
        if let Some(document) = self.app_document(key, generation) {
            document._task = Some(task);
            document._messages = Some(messages);
        }
    }

    /// The host context the app sees now (`hostContext`).
    fn app_context(&mut self, key: &AppKey, window: &Window, cx: &Context<Self>) -> Option<Map<String, Value>> {
        let theme = self.app_theme(cx);
        let fullscreen = fullscreen_size(window);
        let app = self.mcp_apps.apps.get(key)?;
        let zone = iana_time_zone::get_timezone().unwrap_or_else(|_| "UTC".into());
        let user_agent = format!("t3-code/{APP_VERSION}");
        Some(mcp_apps::host_context(&ContextInput {
            dark: crate::theme::tokens(cx).dark,
            theme: &theme,
            display_mode: app.display,
            width: app.width,
            fullscreen,
            locale: crate::timestamp::locale_tag(),
            time_zone: &zone,
            user_agent: &user_agent,
            tool_definition: app.tool_definition.as_ref(),
        }))
    }

    /// The theme's variables, rebuilt only when the theme changes.
    fn app_theme(&mut self, cx: &Context<Self>) -> ThemeVariables {
        let tokens = crate::theme::tokens(cx);
        let key = ThemeKey::of(tokens);
        match &self.mcp_apps.theme {
            Some((current, variables)) if *current == key => variables.clone(),
            _ => {
                let variables: ThemeVariables = theme_variables(tokens).into();
                self.mcp_apps.theme = Some((key, variables.clone()));
                variables
            }
        }
    }

    fn on_app_message(
        &mut self,
        key: &AppKey,
        generation: u64,
        message: ShellMessage,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match message {
            ShellMessage::Ready => {
                if let Some(embed) = self.app_document(key, generation).and_then(|d| d.embed.as_ref()) {
                    embed.retell();
                }
                cx.notify();
            }
            // Navigation keeps the frame's window, so a second document could pose as the app.
            ShellMessage::Load => {
                let Some(document) = self.app_document(key, generation) else { return };
                document.loads += 1;
                if document.loads > 1 {
                    document.host.dispose();
                    document.navigated_away = true;
                    document.embed = None;
                    self.set_app_display(key, DisplayMode::Inline, cx);
                    cx.notify();
                }
            }
            ShellMessage::App { active, data, bytes } => {
                if self.app_document(key, generation).is_none() {
                    return;
                }
                let context = self.app_context(key, window, cx).unwrap_or_default();
                let Some(document) = self.app_document(key, generation) else { return };
                let event = document.host.receive(&data, bytes, || context.clone());
                flush(document, cx);
                if let Some(event) = event {
                    self.on_app_event(key, generation, event, active, window, cx);
                }
            }
        }
    }

    fn on_app_event(
        &mut self,
        key: &AppKey,
        generation: u64,
        event: Event,
        active: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            Event::SizeChanged { height, .. } => {
                let Some(app) = self.mcp_apps.apps.get_mut(key) else { return };
                // Full screen reports the viewport's height, not the inline one.
                let Some(height) = height.filter(|_| app.display == DisplayMode::Inline) else { return };
                let height = mcp_apps::clamp_height(height);
                if (app.height - height).abs() >= 0.5 {
                    app.height = height;
                    self.remeasure_page_row(&key.1, cx);
                }
            }
            Event::RequestTeardown => {
                if self.mcp_apps.apps.get(key).is_some_and(|app| app.display == DisplayMode::Fullscreen) {
                    self.set_app_display(key, DisplayMode::Inline, cx);
                    return;
                }
                self.close_mcp_app(key, generation, cx);
            }
            Event::TornDown => self.app_closed(key, generation, cx),
            Event::Request(ticket, request) => {
                self.run_app_request(key, generation, ticket, request, active, window, cx);
            }
        }
    }

    /// `teardown`, then the closed card: at once for an app with nothing to save, else once it
    /// answers or the timeout passes.
    fn close_mcp_app(&mut self, key: &AppKey, generation: u64, cx: &mut Context<Self>) {
        let Some(document) = self.app_document(key, generation) else { return };
        if !document.host.teardown() {
            self.app_closed(key, generation, cx);
            return;
        }
        flush(document, cx);
        let key = key.clone();
        document.closing = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(TEARDOWN_TIMEOUT).await;
            let _ = this.update(cx, |this, cx| this.app_closed(&key, generation, cx));
        }));
    }

    fn app_closed(&mut self, key: &AppKey, generation: u64, cx: &mut Context<Self>) {
        if self.app_document(key, generation).is_none() {
            return;
        }
        if let Some(app) = self.mcp_apps.apps.get_mut(key) {
            app.document = None;
            app.context = None;
        }
        self.set_app_display(key, DisplayMode::Inline, cx);
        self.remeasure_page_row(&key.1, cx);
    }

    /// "Show app": a fresh document with a fresh URL and host.
    fn show_mcp_app(&mut self, key: &AppKey, window: &mut Window, cx: &mut Context<Self>) {
        if self.mcp_apps.apps.get(key).is_some_and(|app| app.document.is_none()) {
            self.open_app_document(key, window, cx);
            self.remeasure_page_row(&key.1, cx);
        }
    }

    fn finish_app_request(
        &mut self,
        key: &AppKey,
        generation: u64,
        ticket: Ticket,
        outcome: Outcome,
        cx: &mut Context<Self>,
    ) {
        if let Some(document) = self.app_document(key, generation) {
            document.host.finish(ticket, outcome);
            flush(document, cx);
        }
    }

    /// Runs one request against the item and environment that own the app.
    #[expect(clippy::too_many_arguments, reason = "the request and the document it answers")]
    fn run_app_request(
        &mut self,
        key: &AppKey,
        generation: u64,
        ticket: Ticket,
        request: Request,
        active: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(app) = self.mcp_apps.apps.get(key) else { return };
        let scope = app.scope.clone();
        let current = app.display;
        match request {
            Request::OpenLink { url } => {
                let outcome = if active {
                    cx.open_url(&url);
                    Outcome::Done(json!({}))
                } else {
                    Outcome::Refused("Links open only from a click in the app.".into())
                };
                self.finish_app_request(key, generation, ticket, outcome, cx);
                return;
            }
            Request::DisplayMode(mode) => {
                let allowed = mode == DisplayMode::Inline
                    || (active
                        && !app.awaiting
                        && !window.has_active_dialog(cx)
                        && self.mcp_apps.fullscreen.as_ref().is_none_or(|open| open == key));
                let mode = if allowed { mode } else { current };
                self.set_app_display(key, mode, cx);
                self.finish_app_request(key, generation, ticket, Outcome::Done(mode.as_str().into()), cx);
                return;
            }
            _ => {}
        }
        let send_allowed = self.thread.as_ref().is_some_and(|thread| thread.id == scope.conversation)
            || self.side_chat_thread_id() == Some(scope.conversation.as_str());
        let can_send = send_allowed && self.can_operate(&scope.environment);
        let Some(client) = self.env_client(&scope.environment) else {
            self.finish_app_request(key, generation, ticket, Outcome::Failed("Request failed.".into()), cx);
            return;
        };
        let key = key.clone();
        cx.spawn_in(window, async move |this, cx| {
            let outcome = run_request(&this, &key, &scope, &client, request, can_send, cx).await;
            let _ = this.update(cx, |this, cx| this.finish_app_request(&key, generation, ticket, outcome, cx));
        })
        .detach();
    }

    /// Moves the app in or out of full screen; full screen takes it out of the layout.
    fn set_app_display(&mut self, key: &AppKey, mode: DisplayMode, cx: &mut Context<Self>) {
        let Some(app) = self.mcp_apps.apps.get_mut(key) else { return };
        if app.display == mode {
            return;
        }
        app.display = mode;
        let layer = if mode == DisplayMode::Fullscreen { Layer::Dialog } else { Layer::Page };
        if let Some(embed) = app.document.as_ref().and_then(|document| document.embed.as_ref()) {
            embed.set_layer(layer);
        }
        match mode {
            DisplayMode::Fullscreen => self.mcp_apps.fullscreen = Some(key.clone()),
            DisplayMode::Inline => {
                if self.mcp_apps.fullscreen.as_ref() == Some(key) {
                    self.mcp_apps.fullscreen = None;
                }
            }
        }
        cx.notify();
    }

    pub(super) fn mcp_app_fullscreen(&self) -> bool {
        self.mcp_apps.fullscreen.is_some()
    }

    /// Each frame, before anything draws: apps follow the theme and their box, step out of full
    /// screen when the thread waits on the reader or leaves the screen, and long unseen ones close.
    pub(super) fn sync_mcp_apps(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.mcp_apps.apps.is_empty() {
            return;
        }
        let main_thread = self.thread.as_ref().map(|thread| thread.id.clone());
        let side_thread = self.side_chat_thread_id().map(str::to_owned);
        let awaiting_in = |projection: Option<&crate::model::ThreadProjection>| {
            projection.is_some_and(|p| !p.pending_approvals().is_empty() || !p.pending_user_inputs().is_empty())
        };
        let main_awaiting = awaiting_in(self.open_projection());
        let side_awaiting = awaiting_in(self.side_chat_projection());
        let tokens = *crate::theme::tokens(cx);
        let theme = ThemeKey::of(&tokens);
        let fullscreen = fullscreen_size(window);
        let mut step_aside = Vec::new();
        let mut contexts = Vec::new();
        for (key, app) in &mut self.mcp_apps.apps {
            let on_screen = main_thread.as_ref() == Some(&app.scope.conversation)
                || side_thread.as_ref() == Some(&app.scope.conversation);
            let awaiting =
                if main_thread.as_ref() == Some(&app.scope.conversation) { main_awaiting } else { side_awaiting };
            if app.display == DisplayMode::Fullscreen && (!on_screen || (awaiting && !app.awaiting)) {
                step_aside.push(key.clone());
            }
            app.awaiting = awaiting;
            let Some(document) = app.document.as_mut() else { continue };
            if let Some(embed) = document.embed.as_ref().filter(|_| app.dark != tokens.dark) {
                app.dark = tokens.dark;
                let _ = embed.webview(cx).set_theme(if tokens.dark { wry::Theme::Dark } else { wry::Theme::Light });
                let scheme = if tokens.dark {
                    "window.t3Scheme&&t3Scheme('dark')"
                } else {
                    "window.t3Scheme&&t3Scheme('light')"
                };
                let _ = embed.raw(cx).evaluate_script(scheme);
            }
            let next = ContextKey {
                theme,
                width: app.width,
                display: app.display,
                fullscreen,
                has_tool: app.tool_definition.is_some(),
            };
            if document.host.wants_context() && app.context != Some(next) {
                app.context = Some(next);
                contexts.push(key.clone());
            }
        }
        for key in step_aside {
            self.set_app_display(&key, DisplayMode::Inline, cx);
        }
        for key in contexts {
            let Some(context) = self.app_context(&key, window, cx) else { continue };
            if let Some(document) = self.mcp_apps.apps.get_mut(&key).and_then(|app| app.document.as_mut()) {
                document.host.update_host_context(context);
                flush(document, cx);
            }
        }
        let apps = &mut self.mcp_apps;
        if apps.apps.len() > KEPT_APPS {
            let last = self.embeds.frame();
            let mut used: Vec<u64> = apps.apps.values().map(|app| app.used).collect();
            used.sort_unstable_by(|a, b| b.cmp(a));
            let oldest_kept = used[KEPT_APPS - 1];
            let fullscreen = apps.fullscreen.clone();
            apps.apps
                .retain(|key, app| app.used >= oldest_kept || app.used + 1 >= last || fullscreen.as_ref() == Some(key));
        }
    }

    fn mcp_app_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key == "escape"
            && let Some(key) = self.mcp_apps.fullscreen.clone()
        {
            cx.stop_propagation();
            self.set_app_display(&key, DisplayMode::Inline, cx);
        }
    }

    /// The full screen app over the window under the title bar: its server's name and "Exit full
    /// screen" over the page.
    pub(super) fn render_mcp_app_fullscreen(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let key = self.mcp_apps.fullscreen.clone()?;
        let app = self.mcp_apps.apps.get(&key)?;
        let embed = app.document.as_ref()?.embed.as_ref()?;
        let t = *crate::theme::tokens(cx);
        let titlebar = f32::from(super::titlebar::TITLEBAR_HEIGHT);
        let focus = self.mcp_apps.focus.get_or_insert_with(|| cx.focus_handle()).clone();
        let exit = {
            let key = key.clone();
            cx.listener(move |this, _, _, cx| this.set_app_display(&key, DisplayMode::Inline, cx))
        };
        Some(
            v_flex()
                .id("mcp-app-fullscreen")
                .absolute()
                .inset_0()
                .pt(px(titlebar))
                .occlude()
                .bg(t.background)
                .track_focus(&focus)
                .on_mouse_down(MouseButton::Left, move |_, window, cx| focus.focus(window, cx))
                .on_key_down(cx.listener(Self::mcp_app_key_down))
                .child(
                    h_flex()
                        .h(px(HEADER_HEIGHT))
                        .flex_none()
                        .justify_between()
                        .px(px(12.))
                        .border_b_1()
                        .border_color(t.border)
                        .text_sm()
                        .child(div().min_w_0().truncate().child(app.scope.server.clone()))
                        .child(
                            Button::new("mcp-app-exit-fullscreen")
                                .ghost()
                                .small()
                                .icon(Icon::new(IconName::Minimize2))
                                .tooltip("Exit full screen")
                                .on_click(exit),
                        ),
                )
                .child(div().flex_1().min_h_0().w_full().child(embed.frame()))
                .into_any_element(),
        )
    }
}

/// The frame's size while full screen: the window under the title bar and the header.
fn fullscreen_size(window: &Window) -> (f32, f32) {
    let viewport = window.viewport_size();
    let titlebar = f32::from(super::titlebar::TITLEBAR_HEIGHT);
    (f32::from(viewport.width), (f32::from(viewport.height) - titlebar - HEADER_HEIGHT).max(0.))
}

/// Posts what the host queued to the frame.
fn flush(document: &mut Document, cx: &gpui_kit::App) {
    let posts = document.host.take_posts();
    let Some(embed) = &document.embed else { return };
    for post in posts {
        let _ = embed.raw(cx).evaluate_script(&format!("window.t3Post&&t3Post({post})"));
    }
}

/// The work behind a request, on the environment that owns the app.
async fn run_request(
    this: &WeakEntity<Workspace>,
    key: &AppKey,
    scope: &Scope,
    client: &crate::rpc::RpcClient,
    request: Request,
    can_send: bool,
    cx: &mut AsyncWindowContext,
) -> Outcome {
    let server = &scope.server;
    let call = |method: &'static str, extra: Map<String, Value>| {
        let mut payload = scope.target();
        payload.extend(extra);
        let client = client.clone();
        async move { client.request(method, Value::Object(payload)).await.map_err(|e| mcp_apps::failure_message(&e)) }
    };
    match request {
        Request::CallTool { name, arguments } => {
            let mut extra = Map::new();
            extra.insert("name".into(), name.clone().into());
            let info = match call("mcpApps.toolInfo", extra).await {
                Ok(info) => info,
                Err(message) => return Outcome::Failed(message),
            };
            if info["callable"] != true {
                return Outcome::Refused("This app cannot call that tool.".into());
            }
            if info["readOnly"] != true {
                let title = info["title"].as_str().unwrap_or(&name);
                let arguments_text = serde_json::to_string_pretty(&arguments).unwrap_or_default();
                if let Err(refusal) =
                    ask(this, key, format!("Allow {server} to run {title}?\n{arguments_text}"), cx).await
                {
                    return Outcome::Refused(refusal);
                }
            }
            let mut extra = Map::new();
            extra.insert("name".into(), name.into());
            extra.insert("arguments".into(), Value::Object(arguments));
            call("mcpApps.callTool", extra).await.map_or_else(Outcome::Failed, Outcome::Done)
        }
        Request::ReadResource { uri } => {
            let mut extra = Map::new();
            extra.insert("uri".into(), uri.into());
            call("mcpApps.readResource", extra).await.map_or_else(Outcome::Failed, Outcome::Done)
        }
        Request::UpdateModelContext { content, structured } => {
            let mut extra = Map::new();
            if let Some(content) = content {
                extra.insert("content".into(), content);
            }
            if let Some(structured) = structured {
                extra.insert("structuredContent".into(), structured);
            }
            extra.insert("conversationThreadId".into(), scope.conversation.clone().into());
            call("mcpApps.updateModelContext", extra).await.map_or_else(Outcome::Failed, Outcome::Done)
        }
        Request::SendMessage { text } => {
            if !can_send {
                return Outcome::Refused("Messages are not available here.".into());
            }
            if let Err(refusal) = ask(this, key, format!("Send this message from {server}?\n{text}"), cx).await {
                return Outcome::Refused(refusal);
            }
            // `startThreadTurn` with `dispatchMode: "queue"`: after whatever the thread is running.
            let command = json!({
                "type": "message.dispatch",
                "commandId": super::new_id(),
                "createdBy": "user",
                "creationSource": "web",
                "threadId": scope.conversation,
                "messageId": super::new_id(),
                "text": text,
                "attachments": [],
                "dispatchMode": {"type": "queue_after_active"},
            });
            match client.request("orchestration.dispatchCommand", command).await {
                Ok(_) => Outcome::Done(json!({})),
                Err(error) => Outcome::Failed(match &error {
                    crate::rpc::RpcError::Failure(failure) => failure["message"]
                        .as_str()
                        .filter(|m| !m.trim().is_empty())
                        .unwrap_or("Could not send the app's message.")
                        .to_owned(),
                    _ => "Could not send the app's message.".to_owned(),
                }),
            }
        }
        Request::Download(files) => {
            let names: Vec<&str> = files.iter().map(Download::name).collect();
            if let Err(refusal) = ask(this, key, format!("Save {} from {server}?", names.join(", ")), cx).await {
                return Outcome::Refused(refusal);
            }
            let mut saved = Vec::with_capacity(files.len());
            for file in files {
                let (name, bytes) = match file {
                    Download::Embedded { name, bytes } => (name, Some(bytes)),
                    Download::Link { name, uri } => {
                        let mut extra = Map::new();
                        extra.insert("uri".into(), uri.into());
                        let read = match call("mcpApps.readResource", extra).await {
                            Ok(read) => read,
                            Err(message) => return Outcome::Failed(message),
                        };
                        (name, read.pointer("/contents/0").and_then(mcp_apps::resource_bytes))
                    }
                };
                let Some(bytes) = bytes else { return Outcome::Refused(format!("{name} has no contents.")) };
                if bytes.len() > mcp_apps::MAX_DOWNLOAD_BYTES {
                    return Outcome::Refused(format!("{name} is too large to save."));
                }
                saved.push((name, bytes));
            }
            let directory = downloads_dir();
            for (name, bytes) in saved {
                let Ok(picked) = cx.update(|_, cx| cx.prompt_for_new_path(&directory, Some(&name))) else {
                    return Outcome::Failed("Request failed.".into());
                };
                let Ok(Ok(Some(path))) = picked.await else { continue };
                let write = async move { std::fs::write(path, bytes) };
                if let Err(error) = cx.background_executor().spawn(write).await {
                    return Outcome::Failed(format!("Could not save {name}: {error}"));
                }
            }
            Outcome::Done(json!({}))
        }
        Request::OpenLink { .. } | Request::DisplayMode(_) => Outcome::Failed("Request failed.".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> AppReference {
        mcp_apps::read_reference(&json!({
            "attachmentId": "a", "server": "Maps <x>", "tool": "show", "resourceUri": "ui://m",
            "permissions": {"geolocation": {}},
        }))
        .unwrap()
    }

    #[test]
    fn the_shell_sandboxes_the_app_and_signs_what_it_forwards() {
        let shell = app_shell("http://h/api/a?x=1&y=\"2\"", &app(), "abc123", true, "#000");
        assert!(shell.contains("src=\"http://h/api/a?x=1&amp;y=&quot;2&quot;\" title=\"Maps &lt;x> app\""));
        assert!(shell.contains("sandbox=\"allow-scripts allow-forms\"") && !shell.contains("allow-same-origin"));
        assert!(shell.contains("allow=\"geolocation\"") && shell.contains("const n='abc123'"));
    }

    #[test]
    fn reports_need_the_shells_nonce() {
        let report = |value: Value| shell_message(&value.to_string(), "n1");
        assert_eq!(report(json!({"type": "ready"})), Some(ShellMessage::Ready));
        assert_eq!(report(json!({"type": "load", "n": "n1"})), Some(ShellMessage::Load));
        assert_eq!(report(json!({"type": "load"})), None, "the frame can reach the IPC, not the nonce");
        assert_eq!(report(json!({"type": "app", "n": "n2", "active": true, "data": {}})), None);
        let body = json!({"type": "app", "n": "n1", "active": true, "data": {"jsonrpc": "2.0"}}).to_string();
        assert_eq!(
            shell_message(&body, "n1"),
            Some(ShellMessage::App { active: true, data: json!({"jsonrpc": "2.0"}), bytes: body.len() })
        );
        assert_eq!(shell_message("nope", "n1"), None);
    }
}
