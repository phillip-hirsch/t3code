//! MCP Apps: the app reference a tool's output carries (`@t3tools/shared/mcpApp`) and the host
//! side of the protocol an app's frame speaks (`makeMcpAppHost`, `@t3tools/client-runtime/mcp-apps`).
//!
//! [`Host`] decides every reply without doing I/O: what it posts to the frame collects in its
//! outbox, and work that needs the environment or the reader comes back as an [`Event`] whose
//! [`Ticket`] the owner answers with [`Host::finish`].

use base64::Engine as _;
use serde_json::{Map, Value, json};

use crate::rpc::RpcError;

pub const PROTOCOL_VERSION: &str = "2026-01-26";
pub const OUTPUT_KEY: &str = "t3McpApp";
const RESOURCE_SCHEME: &str = "ui://";
const MIN_HEIGHT: f32 = 80.;
pub const DEFAULT_HEIGHT: f32 = 320.;
pub const MAX_HEIGHT: f32 = 2000.;
pub const MAX_DOWNLOAD_BYTES: usize = 25 * 1024 * 1024;
const MAX_MESSAGE_BYTES: usize = 256 * 1024;
const MAX_PENDING_REQUESTS: usize = 16;
const MAX_DOMAINS: usize = 32;
const MAX_DOMAIN_LENGTH: usize = 256;
const MAX_NAME_LENGTH: usize = 256;
const MAX_RESOURCE_URI_LENGTH: usize = 4096;
const MAX_DOWNLOAD_NAME: usize = 200;

/// `McpAppReference`: the app a tool call produced, read as `readMcpAppReference` validates it.
#[derive(Clone, Debug, PartialEq)]
pub struct AppReference {
    pub attachment_id: String,
    pub server: String,
    pub tool: String,
    pub resource_uri: String,
    /// `readMcpAppCsp`: the declared domain lists that passed validation.
    pub csp: Option<Map<String, Value>>,
    /// `readMcpAppPermissions`: the declared keys, in `PERMISSION_KEYS` order.
    pub permissions: Vec<&'static str>,
    pub prefers_border: Option<bool>,
}

/// JavaScript's `length`: UTF-16 code units.
fn js_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// At most `limit` UTF-16 code units of `text`, cut at a char boundary (`slice(0, limit)`).
fn js_prefix(text: &str, limit: usize) -> &str {
    let mut units = 0;
    for (at, c) in text.char_indices() {
        units += c.len_utf16();
        if units > limit {
            return &text[..at];
        }
    }
    text
}

fn bounded_name(value: &Value) -> Option<String> {
    let text = value.as_str()?;
    (!text.trim().is_empty() && js_len(text) <= MAX_NAME_LENGTH).then(|| text.to_owned())
}

/// `DOMAIN_PATTERN`: a scheme-qualified origin, its host optionally under `*.`, with an optional port.
fn valid_domain(domain: &str) -> bool {
    let lower = domain.to_ascii_lowercase();
    let Some(rest) = ["https://", "http://", "wss://", "ws://"].iter().find_map(|scheme| lower.strip_prefix(scheme))
    else {
        return false;
    };
    let rest = rest.strip_prefix("*.").unwrap_or(rest);
    let host = match rest.rsplit_once(':') {
        Some((host, port)) => {
            if !(1..=5).contains(&port.len()) || !port.bytes().all(|b| b.is_ascii_digit()) {
                return false;
            }
            host
        }
        None => rest,
    };
    host.split('.').all(|label| !label.is_empty() && label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
}

fn read_domains(value: &Value) -> Option<Vec<Value>> {
    let domains: Vec<Value> = value
        .as_array()?
        .iter()
        .filter(|domain| domain.as_str().is_some_and(|d| js_len(d) <= MAX_DOMAIN_LENGTH && valid_domain(d)))
        .take(MAX_DOMAINS)
        .cloned()
        .collect();
    (!domains.is_empty()).then_some(domains)
}

/// `readMcpAppCsp`.
fn read_csp(value: &Value) -> Option<Map<String, Value>> {
    let object = value.as_object()?;
    let mut csp = Map::new();
    for key in ["connectDomains", "resourceDomains", "frameDomains", "baseUriDomains"] {
        if let Some(domains) = object.get(key).and_then(read_domains) {
            csp.insert(key.to_owned(), Value::Array(domains));
        }
    }
    (!csp.is_empty()).then_some(csp)
}

const PERMISSION_KEYS: [&str; 4] = ["camera", "microphone", "geolocation", "clipboardWrite"];

/// `readMcpAppReference`.
pub fn read_reference(value: &Value) -> Option<AppReference> {
    let object = value.as_object()?;
    let resource_uri = object
        .get("resourceUri")
        .and_then(Value::as_str)
        .filter(|uri| js_len(uri) <= MAX_RESOURCE_URI_LENGTH && uri.starts_with(RESOURCE_SCHEME))?;
    let permissions = object.get("permissions").and_then(Value::as_object).map_or_else(Vec::new, |declared| {
        PERMISSION_KEYS.into_iter().filter(|key| declared.get(*key).is_some_and(Value::is_object)).collect()
    });
    Some(AppReference {
        attachment_id: bounded_name(object.get("attachmentId")?)?,
        server: bounded_name(object.get("server")?)?,
        tool: bounded_name(object.get("tool")?)?,
        resource_uri: resource_uri.to_owned(),
        csp: object.get("csp").and_then(read_csp),
        permissions,
        prefers_border: object.get("prefersBorder").and_then(Value::as_bool),
    })
}

/// `claudeMcpToolNamePart`: each UTF-16 unit outside `[a-zA-Z0-9_-]` becomes `_`.
fn claude_name_part(name: &str) -> String {
    let mut part = String::with_capacity(name.len());
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
            part.push(c);
        } else {
            part.extend(std::iter::repeat_n('_', c.len_utf16()));
        }
    }
    part
}

/// `claudeMcpToolName`: `mcp__<server>__<tool>`, with a `claude.ai ` connector's `_` runs
/// collapsed and trimmed.
fn claude_tool_name(server: &str, tool: &str) -> String {
    let mut part = claude_name_part(server);
    if server.starts_with("claude.ai ") {
        let mut collapsed = String::with_capacity(part.len());
        for c in part.chars() {
            if !(c == '_' && collapsed.ends_with('_')) {
                collapsed.push(c);
            }
        }
        part = collapsed.trim_matches('_').to_owned();
    }
    format!("mcp__{part}__{}", claude_name_part(tool))
}

impl AppReference {
    /// `mcpAppFromToolItem`: only a tool named for the app's own server and tool may carry it,
    /// so tools cannot impersonate another app's source.
    pub fn from_tool(tool_name: &str, output: &Value) -> Option<Self> {
        let app = read_reference(output.get(OUTPUT_KEY)?)?;
        (tool_name == format!("{}.{}", app.server, app.tool) || tool_name == claude_tool_name(&app.server, &app.tool))
            .then_some(app)
    }

    /// `mcpAppFileName`.
    pub fn file_name(&self) -> String {
        let mut name = String::new();
        let mut in_run = false;
        for c in self.tool.chars() {
            let kept = c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-');
            if kept {
                name.push(c);
            } else if !in_run {
                name.push('-');
            }
            in_run = !kept;
        }
        let name = js_prefix(&name, 80);
        format!("{}.html", if name.is_empty() { "app" } else { name })
    }

    /// `mcpAppAllowAttribute`.
    pub fn allow_attribute(&self) -> String {
        let features: Vec<&str> =
            self.permissions.iter().map(|key| if *key == "clipboardWrite" { "clipboard-write" } else { key }).collect();
        features.join("; ")
    }

    fn sandbox(&self) -> Value {
        let mut sandbox = Map::new();
        if let Some(csp) = &self.csp {
            sandbox.insert("csp".into(), Value::Object(csp.clone()));
        }
        if !self.permissions.is_empty() {
            let permissions = self.permissions.iter().map(|key| ((*key).to_owned(), json!({}))).collect();
            sandbox.insert("permissions".into(), Value::Object(permissions));
        }
        Value::Object(sandbox)
    }
}

/// `clampMcpAppHeight`.
pub fn clamp_height(height: f64) -> f32 {
    if !height.is_finite() {
        return DEFAULT_HEIGHT;
    }
    #[expect(clippy::cast_possible_truncation, reason = "clamped to a frame height")]
    let height = height.clamp(f64::from(MIN_HEIGHT), f64::from(MAX_HEIGHT)).round() as f32;
    height
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplayMode {
    Inline,
    Fullscreen,
}

impl DisplayMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inline => "inline",
            Self::Fullscreen => "fullscreen",
        }
    }
}

/// What `McpAppFrame`'s `hostContext` reads.
pub struct ContextInput<'a> {
    pub dark: bool,
    /// The page theme's variables (`htmlRenderTheme`), by their T3 names.
    pub theme: &'a [(&'static str, String)],
    pub display_mode: DisplayMode,
    /// The inline box's width.
    pub width: f32,
    /// The frame's size while full screen.
    pub fullscreen: (f32, f32),
    pub locale: &'a str,
    pub time_zone: &'a str,
    pub user_agent: &'a str,
    pub tool_definition: Option<&'a Value>,
}

/// `mcpAppStyleVariables`: the theme by the MCP Apps spec's names.
fn style_variables(theme: &[(&'static str, String)]) -> Map<String, Value> {
    const PAIRS: [(&str, &str); 22] = [
        ("--color-background-primary", "--background"),
        ("--color-background-secondary", "--card"),
        ("--color-background-tertiary", "--muted"),
        ("--color-background-inverse", "--foreground"),
        ("--color-background-info", "--info"),
        ("--color-background-danger", "--destructive-surface"),
        ("--color-background-warning", "--warning-surface"),
        ("--color-text-primary", "--foreground"),
        ("--color-text-secondary", "--muted-foreground"),
        ("--color-text-tertiary", "--muted-foreground"),
        ("--color-text-inverse", "--background"),
        ("--color-text-info", "--info-foreground"),
        ("--color-text-danger", "--destructive-foreground"),
        ("--color-text-success", "--success-foreground"),
        ("--color-text-warning", "--warning-foreground"),
        ("--color-border-primary", "--border"),
        ("--color-border-secondary", "--input"),
        ("--color-border-danger", "--destructive"),
        ("--color-ring-primary", "--ring"),
        ("--font-sans", "--font-sans"),
        ("--font-mono", "--font-mono"),
        ("--border-radius-md", "--radius"),
    ];
    let pick = |name: &str| theme.iter().find(|(key, _)| *key == name).map(|(_, value)| value.as_str());
    PAIRS
        .iter()
        .filter_map(|(spec, ours)| Some(((*spec).to_owned(), Value::from(pick(ours).filter(|v| !v.is_empty())?))))
        .collect()
}

/// `McpAppFrame`'s `hostContext`.
pub fn host_context(input: &ContextInput) -> Map<String, Value> {
    let dimensions = match input.display_mode {
        DisplayMode::Fullscreen => json!({"width": input.fullscreen.0, "height": input.fullscreen.1}),
        DisplayMode::Inline => json!({"width": input.width, "maxHeight": MAX_HEIGHT}),
    };
    let mut context = Map::new();
    context.insert("theme".into(), (if input.dark { "dark" } else { "light" }).into());
    context.insert("styles".into(), json!({"variables": style_variables(input.theme)}));
    context.insert("displayMode".into(), input.display_mode.as_str().into());
    context.insert("availableDisplayModes".into(), json!(["inline", "fullscreen"]));
    context.insert("containerDimensions".into(), dimensions);
    context.insert("platform".into(), "desktop".into());
    context.insert("locale".into(), input.locale.into());
    context.insert("timeZone".into(), input.time_zone.into());
    context.insert("userAgent".into(), input.user_agent.into());
    context.insert("deviceCapabilities".into(), json!({"touch": false, "hover": true}));
    context.insert("safeAreaInsets".into(), json!({"top": 0, "right": 0, "bottom": 0, "left": 0}));
    if let Some(tool) = input.tool_definition {
        context.insert("toolInfo".into(), json!({"tool": tool}));
    }
    context
}

/// The call that produced the app, as the thread stored it.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolCall {
    pub arguments: Value,
    pub result: Option<Value>,
}

impl ToolCall {
    /// From a stored `dynamic_tool` item: its input, and its output's `result` once it has one.
    pub fn from_item(item: &Value) -> Option<Self> {
        (item.get("type").and_then(Value::as_str) == Some("dynamic_tool")).then(|| Self {
            arguments: item.get("input").cloned().unwrap_or(Value::Null),
            result: item.pointer("/output/result").filter(|result| result.is_object()).cloned(),
        })
    }
}

/// `normalizeMcpAppToolResult`: providers report absent fields as null, which the SDK rejects.
fn normalize_tool_result(result: &Value) -> Value {
    let mut normalized = Map::new();
    normalized.insert("content".into(), result.get("content").filter(|c| c.is_array()).cloned().unwrap_or(json!([])));
    if let Some(structured) = result.get("structuredContent").filter(|v| v.is_object()) {
        normalized.insert("structuredContent".into(), structured.clone());
    }
    if result.get("isError") == Some(&Value::Bool(true)) {
        normalized.insert("isError".into(), true.into());
    }
    if let Some(meta) = result.get("_meta").filter(|v| v.is_object()) {
        normalized.insert("_meta".into(), meta.clone());
    }
    Value::Object(normalized)
}

/// A file an app asked to save (`McpAppDownload`).
#[derive(Clone, Debug, PartialEq)]
pub enum Download {
    Embedded { name: String, bytes: Vec<u8> },
    Link { name: String, uri: String },
}

impl Download {
    pub fn name(&self) -> &str {
        match self {
            Self::Embedded { name, .. } | Self::Link { name, .. } => name,
        }
    }
}

/// `mcpResourceBytes`: a resource's text, or its base64 blob.
pub fn resource_bytes(content: &Value) -> Option<Vec<u8>> {
    if let Some(text) = content.get("text").and_then(Value::as_str) {
        return Some(text.as_bytes().to_vec());
    }
    let blob = content.get("blob").and_then(Value::as_str)?;
    let lenient = base64::engine::GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        base64::engine::GeneralPurposeConfig::new()
            .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent),
    );
    lenient.decode(blob).ok()
}

/// `downloadName`: the name the app gave, else its URI's last part, made safe for a file name.
fn download_name(uri: Option<&Value>, name: Option<&Value>) -> String {
    let raw = match name.and_then(Value::as_str).filter(|name| !name.trim().is_empty()) {
        Some(name) => name,
        None => uri
            .and_then(Value::as_str)
            .map_or("download", |uri| uri.split(['/', '?', '#']).rfind(|part| !part.is_empty()).unwrap_or("download")),
    };
    let safe: String =
        raw.chars().map(|c| if r#"\/:*?"<>|"#.contains(c) || (c as u32) < 32 { '-' } else { c }).collect();
    let safe = js_prefix(&safe, MAX_DOWNLOAD_NAME);
    if safe.is_empty() { "download".to_owned() } else { safe.to_owned() }
}

/// `readDownloads`: embedded resources and resource links, all valid or none.
fn read_downloads(contents: Option<&Value>) -> Option<Vec<Download>> {
    let entries = contents?.as_array().filter(|entries| !entries.is_empty())?;
    entries
        .iter()
        .map(|entry| match entry.get("type").and_then(Value::as_str) {
            Some("resource") => {
                let resource = entry.get("resource").filter(|r| r.is_object())?;
                Some(Download::Embedded {
                    name: download_name(resource.get("uri"), resource.get("name")),
                    bytes: resource_bytes(resource)?,
                })
            }
            Some("resource_link") => {
                let uri = entry.get("uri").and_then(Value::as_str)?;
                Some(Download::Link { name: download_name(entry.get("uri"), entry.get("name")), uri: uri.to_owned() })
            }
            _ => None,
        })
        .collect()
}

/// Work the host needs done to answer an app's request.
#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    CallTool {
        name: String,
        arguments: Map<String, Value>,
    },
    ReadResource {
        uri: String,
    },
    OpenLink {
        url: String,
    },
    SendMessage {
        text: String,
    },
    /// Replaces what the app tells the agent on its next turn; no content clears it.
    UpdateModelContext {
        content: Option<Value>,
        structured: Option<Value>,
    },
    DisplayMode(DisplayMode),
    Download(Vec<Download>),
}

/// Answers one [`Request`] through [`Host::finish`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ticket(u64);

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Request(Ticket, Request),
    SizeChanged {
        width: Option<f64>,
        height: Option<f64>,
    },
    /// The app asks to be closed.
    RequestTeardown,
    /// The app answered `ui/resource-teardown`; the host is disposed.
    TornDown,
}

/// How a [`Request`] ended.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    /// Its result: a tool's result, `{ mode }` for a display mode, anything for the rest.
    Done(Value),
    /// T3 or the reader turned it down (`McpAppHostRefusal`).
    Refused(String),
    Failed(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reply {
    ToolResult,
    Empty,
    Raw,
    Mode,
    Download,
}

/// `makeMcpAppHost`: one app document's session.
pub struct Host {
    csp_and_permissions: Value,
    host_version: String,
    initialized: bool,
    disposed: bool,
    tool_call: Option<ToolCall>,
    tool_call_sent: bool,
    pending: Vec<(Ticket, Value, Reply)>,
    next_ticket: u64,
    sent_context: Option<Map<String, Value>>,
    app_display_modes: Option<Vec<String>>,
    next_host_request: u64,
    teardown: Option<String>,
    outbox: Vec<Value>,
}

fn is_id(value: &Value) -> bool {
    value.is_string() || value.is_number()
}

impl Host {
    pub fn new(app: &AppReference, host_version: &str) -> Self {
        Self {
            csp_and_permissions: app.sandbox(),
            host_version: host_version.to_owned(),
            initialized: false,
            disposed: false,
            tool_call: None,
            tool_call_sent: false,
            pending: Vec::new(),
            next_ticket: 0,
            sent_context: None,
            app_display_modes: None,
            next_host_request: 0,
            teardown: None,
            outbox: Vec::new(),
        }
    }

    /// The messages to post to the frame, oldest first.
    pub fn take_posts(&mut self) -> Vec<Value> {
        std::mem::take(&mut self.outbox)
    }

    #[cfg(test)]
    pub fn is_disposed(&self) -> bool {
        self.disposed
    }

    /// Whether [`update_host_context`](Self::update_host_context) would send anything.
    pub fn wants_context(&self) -> bool {
        !self.disposed && self.initialized
    }

    pub fn dispose(&mut self) {
        self.disposed = true;
    }

    fn post(&mut self, message: Value) {
        if !self.disposed {
            self.outbox.push(message);
        }
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.post(json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    fn respond(&mut self, id: Value, result: Value) {
        self.post(json!({"jsonrpc": "2.0", "id": id, "result": result}));
    }

    fn fail(&mut self, id: Value, code: i64, message: &str) {
        self.post(json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}));
    }

    fn send_tool_call(&mut self) {
        if !self.initialized || self.tool_call_sent {
            return;
        }
        let Some(call) = self.tool_call.clone() else { return };
        self.tool_call_sent = true;
        let arguments = if call.arguments.is_object() { call.arguments } else { json!({}) };
        self.notify("ui/notifications/tool-input", json!({"arguments": arguments}));
        if let Some(result) = &call.result {
            self.notify("ui/notifications/tool-result", normalize_tool_result(result));
        }
    }

    /// `answer`: a request the owner runs, unless too many are in flight.
    fn answer(&mut self, id: Value, reply: Reply, request: Request) -> Option<Event> {
        if self.pending.len() >= MAX_PENDING_REQUESTS {
            self.fail(id, -32000, "Too many requests in flight.");
            return None;
        }
        let ticket = Ticket(self.next_ticket);
        self.next_ticket += 1;
        self.pending.push((ticket, id, reply));
        Some(Event::Request(ticket, request))
    }

    /// Answers a [`Request`]: the app gets its result or the error it ended with.
    pub fn finish(&mut self, ticket: Ticket, outcome: Outcome) {
        let Some(at) = self.pending.iter().position(|(pending, ..)| *pending == ticket) else { return };
        let (_, id, reply) = self.pending.remove(at);
        match (outcome, reply) {
            (Outcome::Done(result), Reply::ToolResult) => self.respond(id, normalize_tool_result(&result)),
            (Outcome::Done(mode), Reply::Mode) => self.respond(id, json!({"mode": mode})),
            (Outcome::Done(result), Reply::Raw) => self.respond(id, result),
            (Outcome::Done(_), Reply::Empty | Reply::Download) => self.respond(id, json!({})),
            (Outcome::Refused(_), Reply::Download) => self.respond(id, json!({"isError": true})),
            (Outcome::Refused(message) | Outcome::Failed(message), _) => self.fail(id, -32000, &message),
        }
    }

    /// Handles one message the app posted; the caller has already checked it came from this frame.
    /// `bytes` is its encoded size and `context` the host context now.
    pub fn receive(&mut self, data: &Value, bytes: usize, context: impl Fn() -> Map<String, Value>) -> Option<Event> {
        if self.disposed || data.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return None;
        }
        let id = data.get("id").filter(|id| is_id(id)).cloned();
        let method = data.get("method").and_then(Value::as_str);
        if bytes > MAX_MESSAGE_BYTES {
            if let (Some(id), Some(_)) = (id, method) {
                self.fail(id, -32600, "Message too large.");
            }
            return None;
        }
        let Some(method) = method else {
            let answered = data.get("result").is_some() || data.get("error").is_some();
            let ours = id.as_ref().and_then(Value::as_str).zip(self.teardown.as_deref()).is_some_and(|(a, b)| a == b);
            if answered && ours {
                self.teardown = None;
                self.disposed = true;
                return Some(Event::TornDown);
            }
            return None;
        };
        let empty = Map::new();
        let params = data.get("params").and_then(Value::as_object).unwrap_or(&empty);
        match id {
            Some(id) => self.handle_request(id, method, params, &context),
            None => self.handle_notification(method, params, &context),
        }
    }

    fn handle_request(
        &mut self,
        id: Value,
        method: &str,
        params: &Map<String, Value>,
        context: &dyn Fn() -> Map<String, Value>,
    ) -> Option<Event> {
        match method {
            "ui/initialize" => {
                let declared = params.get("appCapabilities").and_then(|caps| caps.get("availableDisplayModes"));
                self.app_display_modes = declared
                    .and_then(Value::as_array)
                    .map(|modes| modes.iter().filter_map(Value::as_str).map(str::to_owned).collect());
                let context = context();
                self.sent_context = Some(context.clone());
                let result = json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "hostInfo": {"name": "t3-code", "version": self.host_version},
                    "hostCapabilities": {
                        "openLinks": {},
                        "serverTools": {},
                        "serverResources": {},
                        "logging": {},
                        "message": {"text": {}},
                        "updateModelContext": {"text": {}, "structuredContent": {}},
                        "downloadFile": {},
                        "sandbox": self.csp_and_permissions,
                    },
                    "hostContext": context,
                });
                self.respond(id, result);
                None
            }
            "ping" => {
                self.respond(id, json!({}));
                None
            }
            "tools/call" => {
                let name = params.get("name").and_then(Value::as_str).filter(|name| !name.trim().is_empty());
                let arguments = match params.get("arguments") {
                    None | Some(Value::Null) => Some(Map::new()),
                    Some(Value::Object(arguments)) => Some(arguments.clone()),
                    Some(_) => None,
                };
                let (Some(name), Some(arguments)) = (name, arguments) else {
                    self.fail(id, -32602, "tools/call needs a tool name and object arguments.");
                    return None;
                };
                self.answer(id, Reply::ToolResult, Request::CallTool { name: name.to_owned(), arguments })
            }
            "resources/read" => {
                let Some(uri) = params.get("uri").and_then(Value::as_str).filter(|uri| !uri.trim().is_empty()) else {
                    self.fail(id, -32602, "resources/read needs a uri.");
                    return None;
                };
                self.answer(id, Reply::Raw, Request::ReadResource { uri: uri.to_owned() })
            }
            "ui/open-link" => {
                let url = params.get("url").and_then(Value::as_str).filter(|url| {
                    let lower = url.get(..8).unwrap_or(url).to_ascii_lowercase();
                    lower.starts_with("http://") || lower.starts_with("https://")
                });
                let Some(url) = url else {
                    self.fail(id, -32602, "Only http(s) links can be opened.");
                    return None;
                };
                self.answer(id, Reply::Empty, Request::OpenLink { url: url.to_owned() })
            }
            "ui/message" => {
                // The SDK sends an array of content blocks; the spec text shows one block.
                let content = params.get("content").unwrap_or(&Value::Null);
                let blocks = content.as_array().map_or_else(|| vec![content], |blocks| blocks.iter().collect());
                let texts: Option<Vec<&str>> = blocks
                    .iter()
                    .map(|block| {
                        (block.get("type").and_then(Value::as_str) == Some("text"))
                            .then(|| block.get("text").and_then(Value::as_str))
                            .flatten()
                    })
                    .collect();
                let user = params.get("role").and_then(Value::as_str) == Some("user");
                let Some(texts) = texts.filter(|texts| user && !texts.is_empty()) else {
                    self.fail(id, -32602, "Only text messages from the user role are supported.");
                    return None;
                };
                let text = texts.join("\n").trim().to_owned();
                if text.is_empty() {
                    self.fail(id, -32602, "ui/message needs non-empty text.");
                    return None;
                }
                self.answer(id, Reply::Empty, Request::SendMessage { text })
            }
            "ui/request-display-mode" => {
                let context = context();
                let current = context.get("displayMode").cloned().unwrap_or_else(|| "inline".into());
                let mode = params.get("mode").and_then(Value::as_str);
                // A mode the host lacks, or the app did not declare, leaves the current one.
                let available = |mode: &str| {
                    context
                        .get("availableDisplayModes")
                        .and_then(Value::as_array)
                        .is_some_and(|modes| modes.iter().any(|m| m == mode))
                };
                let declared =
                    |mode: &str| self.app_display_modes.as_ref().is_none_or(|modes| modes.iter().any(|m| m == mode));
                let wanted = mode.filter(|mode| available(mode) && declared(mode) && current != *mode).and_then(
                    |mode| match mode {
                        "inline" => Some(DisplayMode::Inline),
                        "fullscreen" => Some(DisplayMode::Fullscreen),
                        _ => None,
                    },
                );
                match wanted {
                    Some(mode) => self.answer(id, Reply::Mode, Request::DisplayMode(mode)),
                    None => {
                        self.respond(id, json!({"mode": current}));
                        None
                    }
                }
            }
            "ui/update-model-context" => {
                let content = params.get("content");
                let structured = params.get("structuredContent");
                if content.is_some_and(|c| !c.is_array()) || structured.is_some_and(|s| !s.is_object()) {
                    self.fail(id, -32602, "Model context needs content blocks or an object of structured content.");
                    return None;
                }
                let request =
                    Request::UpdateModelContext { content: content.cloned(), structured: structured.cloned() };
                self.answer(id, Reply::Empty, request)
            }
            "ui/download-file" => {
                let Some(files) = read_downloads(params.get("contents")) else {
                    self.fail(id, -32602, "ui/download-file needs embedded resources or resource links.");
                    return None;
                };
                self.answer(id, Reply::Download, Request::Download(files))
            }
            _ => {
                self.fail(id, -32601, &format!("Method not found: {method}"));
                None
            }
        }
    }

    fn handle_notification(
        &mut self,
        method: &str,
        params: &Map<String, Value>,
        context: &dyn Fn() -> Map<String, Value>,
    ) -> Option<Event> {
        match method {
            "ui/notifications/initialized" if !self.initialized => {
                self.initialized = true;
                self.send_tool_call();
                self.update_host_context(context());
                None
            }
            "ui/notifications/size-changed" => {
                let width = params.get("width").and_then(Value::as_f64);
                let height = params.get("height").and_then(Value::as_f64);
                (width.is_some() || height.is_some()).then_some(Event::SizeChanged { width, height })
            }
            "ui/notifications/request-teardown" => Some(Event::RequestTeardown),
            _ => None,
        }
    }

    /// `setToolCall`: the call goes once the app initializes; a running call's completion later
    /// sends only its result.
    pub fn set_tool_call(&mut self, call: ToolCall) {
        if self.tool_call.is_none() || !self.tool_call_sent {
            if self.tool_call.as_ref().is_none_or(|current| current.result.is_none()) {
                self.tool_call = Some(call);
            }
            self.send_tool_call();
            return;
        }
        let Some(current) = self.tool_call.as_mut() else { return };
        let Some(result) = call.result.filter(|_| current.result.is_none()) else { return };
        let normalized = normalize_tool_result(&result);
        current.result = Some(result);
        self.notify("ui/notifications/tool-result", normalized);
    }

    /// `updateHostContext`: sends the keys that changed since the app last heard, once it has
    /// initialized (the spec forbids messages before that).
    pub fn update_host_context(&mut self, next: Map<String, Value>) {
        if !self.initialized {
            return;
        }
        let Some(sent) = &self.sent_context else { return };
        let changes: Map<String, Value> = next
            .iter()
            .filter(|(key, value)| sent.get(*key) != Some(value))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        self.sent_context = Some(next);
        if !changes.is_empty() {
            self.notify("ui/notifications/host-context-changed", Value::Object(changes));
        }
    }

    /// Asks the app to wrap up before its document goes away. `true`: wait for
    /// [`Event::TornDown`], or dispose after a short timeout; `false`: the host is disposed now,
    /// since an app that never initialized has nothing to save.
    pub fn teardown(&mut self) -> bool {
        if self.disposed || !self.initialized {
            self.disposed = true;
            return false;
        }
        let id = format!("t3-host-{}", self.next_host_request);
        self.next_host_request += 1;
        self.post(json!({"jsonrpc": "2.0", "id": id, "method": "ui/resource-teardown", "params": {}}));
        self.teardown = Some(id);
        true
    }
}

/// `commandFailure`: what an app hears when an environment request fails.
pub fn failure_message(error: &RpcError) -> String {
    if let RpcError::Failure(failure) = error {
        if failure.get("_tag").and_then(Value::as_str) == Some("McpAppRequestError") {
            let message = match failure.get("reason").and_then(Value::as_str) {
                Some("not-an-app") => "This tool call has no MCP app.",
                Some("provider-unsupported") => "This thread's provider cannot run MCP app requests.",
                Some("session-stopped") => {
                    "The app's thread is not running. Send a message in the thread that created it to use the app again."
                }
                Some("tool-not-callable") => "This app cannot call that tool.",
                Some("unsupported-content") => "Only text and structured content are supported.",
                _ => "The app's MCP server request failed.",
            };
            return message.to_owned();
        }
        if let Some(message) = failure.get("message").and_then(Value::as_str).filter(|m| !m.trim().is_empty()) {
            return message.to_owned();
        }
    }
    "Request failed.".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> AppReference {
        read_reference(&json!({
            "attachmentId": "a1", "server": "maps", "tool": "show map", "resourceUri": "ui://maps/show",
            "csp": {"connectDomains": ["https://api.maps.dev", "javascript:alert(1)", "https://*.tiles.dev:8443"],
                "frameDomains": ["self"]},
            "permissions": {"clipboardWrite": {}, "camera": {}, "microphone": true},
            "prefersBorder": true,
        }))
        .unwrap()
    }

    fn context() -> Map<String, Value> {
        let theme = [("--background", "#000".to_owned()), ("--foreground", "#fff".to_owned())];
        host_context(&ContextInput {
            dark: true,
            theme: &theme,
            display_mode: DisplayMode::Inline,
            width: 700.,
            fullscreen: (1200., 800.),
            locale: "en-US",
            time_zone: "UTC",
            user_agent: "t3-code/1.0.0",
            tool_definition: None,
        })
    }

    fn rpc(id: u64, method: &str, params: Value) -> Value {
        json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
    }

    fn note(method: &str, params: Value) -> Value {
        json!({"jsonrpc": "2.0", "method": method, "params": params})
    }

    fn send(host: &mut Host, message: &Value) -> Option<Event> {
        host.receive(message, message.to_string().len(), context)
    }

    fn initialized() -> Host {
        let mut host = Host::new(&app(), "1.0.0");
        send(&mut host, &rpc(0, "ui/initialize", json!({"protocolVersion": "2026-01-26"})));
        send(&mut host, &note("ui/notifications/initialized", json!({})));
        host.take_posts();
        host
    }

    #[test]
    fn references_validate_like_read_mcp_app_reference() {
        let app = app();
        assert_eq!(
            app.csp,
            Some(
                json!({"connectDomains": ["https://api.maps.dev", "https://*.tiles.dev:8443"]})
                    .as_object()
                    .unwrap()
                    .clone()
            )
        );
        assert_eq!(app.permissions, ["camera", "clipboardWrite"]);
        assert_eq!(app.allow_attribute(), "camera; clipboard-write");
        assert_eq!(app.file_name(), "show-map.html");
        let reference = |extra: Value| {
            let mut value = json!({"attachmentId": "a", "server": "s", "tool": "t", "resourceUri": "ui://s/t"});
            value.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            read_reference(&value)
        };
        assert!(reference(json!({})).is_some());
        assert_eq!(reference(json!({"resourceUri": "https://s/t"})), None, "only ui:// resources");
        assert_eq!(reference(json!({"server": "  "})), None);
        assert_eq!(reference(json!({"tool": "x".repeat(257)})), None);
        for (domain, valid) in [
            ("wss://live.dev", true),
            ("HTTPS://A-B.dev:1", true),
            ("https://*.dev", true),
            ("https://dev:123456", false),
            ("https://a..dev", false),
            ("https://a.dev/path", false),
            ("ftp://a.dev", false),
            ("https://*", false),
        ] {
            assert_eq!(valid_domain(domain), valid, "{domain}");
        }
    }

    #[test]
    fn only_the_apps_own_tool_may_carry_it() {
        let output = json!({"t3McpApp": {"attachmentId": "a", "server": "claude.ai Maps & Co", "tool": "show.map",
            "resourceUri": "ui://m"}});
        assert!(AppReference::from_tool("claude.ai Maps & Co.show.map", &output).is_some());
        assert!(AppReference::from_tool("mcp__claude_ai_Maps_Co__show_map", &output).is_some());
        assert!(AppReference::from_tool("mcp__other__show_map", &output).is_none());
        assert_eq!(claude_tool_name("my server", "café"), "mcp__my_server__caf_");
        assert_eq!(claude_tool_name("x", "a😀"), "mcp__x__a__");
    }

    #[test]
    fn initialize_answers_with_capabilities_and_context() {
        let mut host = Host::new(&app(), "1.2.3");
        assert_eq!(send(&mut host, &rpc(7, "ui/initialize", json!({"protocolVersion": "1999"}))), None);
        let posts = host.take_posts();
        let result = &posts[0]["result"];
        assert_eq!(posts[0]["id"], 7);
        assert_eq!(result["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(result["hostInfo"], json!({"name": "t3-code", "version": "1.2.3"}));
        assert_eq!(result["hostCapabilities"]["sandbox"]["permissions"], json!({"camera": {}, "clipboardWrite": {}}));
        assert_eq!(result["hostContext"]["containerDimensions"], json!({"width": 700.0, "maxHeight": 2000.0}));
        assert_eq!(result["hostContext"]["styles"]["variables"]["--color-text-inverse"], "#000");
        assert_eq!(result["hostContext"]["platform"], "desktop");
        // Nothing reaches the app before it says it initialized.
        host.update_host_context(Map::new());
        host.set_tool_call(ToolCall { arguments: json!({"q": 1}), result: None });
        assert!(host.take_posts().is_empty());
        send(&mut host, &note("ui/notifications/initialized", json!({})));
        assert_eq!(host.take_posts(), [note("ui/notifications/tool-input", json!({"arguments": {"q": 1}}))]);
    }

    #[test]
    fn the_tool_call_arrives_once_and_its_result_when_it_completes() {
        let mut host = initialized();
        let running = ToolCall { arguments: json!("not an object"), result: None };
        host.set_tool_call(running.clone());
        assert_eq!(host.take_posts(), [note("ui/notifications/tool-input", json!({"arguments": {}}))]);
        host.set_tool_call(running);
        assert!(host.take_posts().is_empty());
        let done = json!({"content": [{"type": "text", "text": "ok"}], "_meta": null, "structuredContent": null});
        host.set_tool_call(ToolCall { arguments: json!({"other": true}), result: Some(done) });
        assert_eq!(
            host.take_posts(),
            [note("ui/notifications/tool-result", json!({"content": [{"type": "text", "text": "ok"}]}))]
        );
        // A settled call is not replaced.
        host.set_tool_call(ToolCall { arguments: json!({}), result: Some(json!({"content": []})) });
        assert!(host.take_posts().is_empty());
    }

    #[test]
    fn context_changes_send_only_what_changed() {
        let mut host = initialized();
        let mut next = context();
        host.update_host_context(next.clone());
        assert!(host.take_posts().is_empty());
        next.insert("theme".into(), "light".into());
        host.update_host_context(next);
        assert_eq!(host.take_posts(), [note("ui/notifications/host-context-changed", json!({"theme": "light"}))]);
    }

    #[test]
    fn requests_become_work_and_their_outcomes_replies() {
        let mut host = initialized();
        let Some(Event::Request(call, Request::CallTool { name, arguments })) =
            send(&mut host, &rpc(1, "tools/call", json!({"name": "zoom", "arguments": null})))
        else {
            panic!("a tool call");
        };
        assert_eq!((name.as_str(), arguments.len()), ("zoom", 0));
        let Some(Event::Request(download, Request::Download(files))) = send(
            &mut host,
            &rpc(
                2,
                "ui/download-file",
                json!({"contents": [
                    {"type": "resource", "resource": {"uri": "file:///a/b%20c.txt", "blob": "aGk"}},
                    {"type": "resource_link", "uri": "ui://x/r?y", "name": "a:b"},
                ]}),
            ),
        ) else {
            panic!("a download");
        };
        assert_eq!(
            files,
            [
                Download::Embedded { name: "b%20c.txt".into(), bytes: b"hi".to_vec() },
                Download::Link { name: "a-b".into(), uri: "ui://x/r?y".into() },
            ]
        );
        host.finish(download, Outcome::Refused("Declined by the user.".into()));
        host.finish(call, Outcome::Done(json!({"content": null, "isError": false})));
        host.finish(call, Outcome::Done(json!({})));
        assert_eq!(
            host.take_posts(),
            [
                json!({"jsonrpc": "2.0", "id": 2, "result": {"isError": true}}),
                json!({"jsonrpc": "2.0", "id": 1, "result": {"content": []}}),
            ]
        );
        let Some(Event::Request(message, Request::SendMessage { text })) = send(
            &mut host,
            &rpc(
                3,
                "ui/message",
                json!({"role": "user", "content": [{"type": "text", "text": " a"}, {"type": "text", "text": "b "}]}),
            ),
        ) else {
            panic!("a message");
        };
        assert_eq!(text, "a\nb");
        host.finish(message, Outcome::Failed("Declined by the user.".into()));
        assert_eq!(
            host.take_posts(),
            [json!({"jsonrpc": "2.0", "id": 3, "error": {"code": -32000, "message": "Declined by the user."}})]
        );
    }

    #[test]
    fn malformed_requests_fail_without_work() {
        let mut host = initialized();
        let error = |host: &mut Host, message: Value| {
            assert_eq!(send(host, &message), None);
            let posts = host.take_posts();
            (posts[0]["error"]["code"].as_i64().unwrap(), posts[0]["error"]["message"].as_str().unwrap().to_owned())
        };
        assert_eq!(error(&mut host, rpc(1, "tools/call", json!({"name": " "}))).0, -32602);
        assert_eq!(error(&mut host, rpc(1, "tools/call", json!({"name": "x", "arguments": [1]}))).0, -32602);
        assert_eq!(error(&mut host, rpc(1, "ui/open-link", json!({"url": "javascript:alert(1)"}))).0, -32602);
        assert_eq!(
            error(
                &mut host,
                rpc(1, "ui/message", json!({"role": "assistant", "content": {"type": "text", "text": "x"}}))
            )
            .0,
            -32602
        );
        assert_eq!(
            error(&mut host, rpc(1, "ui/message", json!({"role": "user", "content": {"type": "text", "text": "  "}})))
                .1,
            "ui/message needs non-empty text."
        );
        assert_eq!(error(&mut host, rpc(1, "ui/update-model-context", json!({"content": null}))).0, -32602);
        assert_eq!(
            error(&mut host, rpc(1, "ui/download-file", json!({"contents": [{"type": "resource", "resource": {}}]}))).0,
            -32602
        );
        assert_eq!(error(&mut host, rpc(1, "nope", json!({}))).1, "Method not found: nope");
        let large = rpc(1, "ping", json!({"pad": "x".repeat(MAX_MESSAGE_BYTES)}));
        assert_eq!(error(&mut host, large), (-32600, "Message too large.".into()));
        // Not JSON-RPC, or a notification it doesn't know: ignored.
        assert_eq!(send(&mut host, &json!({"id": 1, "method": "ping"})), None);
        assert_eq!(send(&mut host, &note("ui/notifications/unknown", json!({}))), None);
        assert!(host.take_posts().is_empty());
        for id in 0..MAX_PENDING_REQUESTS as u64 {
            assert!(send(&mut host, &rpc(id, "resources/read", json!({"uri": "ui://x"}))).is_some());
        }
        assert_eq!(
            error(&mut host, rpc(99, "resources/read", json!({"uri": "ui://x"}))).1,
            "Too many requests in flight."
        );
    }

    #[test]
    fn display_modes_change_only_when_both_sides_allow_them() {
        let mut host = Host::new(&app(), "1");
        send(&mut host, &rpc(0, "ui/initialize", json!({"appCapabilities": {"availableDisplayModes": ["inline"]}})));
        send(&mut host, &note("ui/notifications/initialized", json!({})));
        host.take_posts();
        assert_eq!(send(&mut host, &rpc(1, "ui/request-display-mode", json!({"mode": "fullscreen"}))), None);
        assert_eq!(host.take_posts(), [json!({"jsonrpc": "2.0", "id": 1, "result": {"mode": "inline"}})]);
        let mut host = initialized();
        assert_eq!(send(&mut host, &rpc(1, "ui/request-display-mode", json!({"mode": "pip"}))), None);
        host.take_posts();
        let Some(Event::Request(ticket, Request::DisplayMode(DisplayMode::Fullscreen))) =
            send(&mut host, &rpc(2, "ui/request-display-mode", json!({"mode": "fullscreen"})))
        else {
            panic!("a display mode request");
        };
        host.finish(ticket, Outcome::Done("inline".into()));
        assert_eq!(host.take_posts(), [json!({"jsonrpc": "2.0", "id": 2, "result": {"mode": "inline"}})]);
    }

    #[test]
    fn lifecycle_notifications_and_teardown() {
        let mut fresh = Host::new(&app(), "1");
        assert!(!fresh.teardown(), "an app that never initialized closes at once");
        assert!(fresh.is_disposed());
        let mut host = initialized();
        assert_eq!(
            send(&mut host, &note("ui/notifications/size-changed", json!({"height": 412.6}))),
            Some(Event::SizeChanged { width: None, height: Some(412.6) })
        );
        assert_eq!(send(&mut host, &note("ui/notifications/size-changed", json!({"height": "1"}))), None);
        assert_eq!(
            send(&mut host, &note("ui/notifications/request-teardown", json!({}))),
            Some(Event::RequestTeardown)
        );
        assert!(host.teardown());
        assert_eq!(
            host.take_posts(),
            [json!({"jsonrpc": "2.0", "id": "t3-host-0", "method": "ui/resource-teardown", "params": {}})]
        );
        // Only the answer to its own request ends it.
        assert_eq!(send(&mut host, &json!({"jsonrpc": "2.0", "id": "t3-host-9", "result": {}})), None);
        assert_eq!(send(&mut host, &json!({"jsonrpc": "2.0", "id": "t3-host-0", "result": {}})), Some(Event::TornDown));
        assert!(host.is_disposed());
        assert_eq!(send(&mut host, &rpc(1, "ping", json!({}))), None);
        assert!(host.take_posts().is_empty());
    }

    #[test]
    fn heights_and_failures_read_like_the_web() {
        assert_eq!(clamp_height(f64::NAN), 320.);
        assert_eq!(clamp_height(10.), 80.);
        assert_eq!(clamp_height(412.5), 413.);
        assert_eq!(clamp_height(1e9), 2000.);
        let failure = |value: Value| failure_message(&RpcError::Failure(value));
        assert_eq!(
            failure(json!({"_tag": "McpAppRequestError", "threadId": "t", "reason": "session-stopped"})),
            "The app's thread is not running. Send a message in the thread that created it to use the app again."
        );
        assert_eq!(failure(json!({"_tag": "X", "message": "Boom"})), "Boom");
        assert_eq!(failure(json!({"_tag": "X", "message": " "})), "Request failed.");
        assert_eq!(failure_message(&RpcError::Disconnected("gone".into())), "Request failed.");
    }
}
