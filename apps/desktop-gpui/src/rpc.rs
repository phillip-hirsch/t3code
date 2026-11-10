//! Effect RPC over WebSocket, JSON serialization (see `.repos/effect/packages/effect/src/rpc`).
//!
//! The socket runs on the shared tokio runtime. Callers get runtime-agnostic
//! futures and channels, so GPUI tasks can await them directly.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow};
use futures::channel::oneshot;
use futures::{SinkExt as _, StreamExt as _};
use serde_json::{Value, json};
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::{Message, Utf8Bytes};

use crate::rpc_permissions::{self, Grants};

const PING_INTERVAL: Duration = Duration::from_secs(5);
const PING_TIMEOUT: Duration = Duration::from_secs(15);
const OPEN_TIMEOUT: Duration = Duration::from_secs(15);
/// Server prefixes every split frame part except the last with U+001E.
const FRAME_PART_PREFIX: char = '\u{1e}';
/// The most a split frame may join to, like the socket's per-message cap.
const MAX_JOINED_FRAME: usize = 256 << 20;
/// How long a guarded request waits for the environment's first grants (`commandPermissions.ts`).
const GRANT_WAIT: Duration = Duration::from_secs(6);

#[derive(Debug, Clone)]
pub enum RpcError {
    /// A typed `Fail` from the handler, e.g. `{"_tag":"OrchestrationV2DispatchCommandError",...}`.
    Failure(Value),
    /// A `Die`/`Defect`, an interrupt, or an unknown cause.
    Defect(String),
    Disconnected(String),
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RpcError::Failure(error) => {
                let message = error.get("message").and_then(Value::as_str);
                let tag = error.get("_tag").and_then(Value::as_str).unwrap_or("Error");
                match message {
                    Some(message) => write!(f, "{tag}: {message}"),
                    None => write!(f, "{tag}"),
                }
            }
            RpcError::Defect(message) => write!(f, "server defect: {message}"),
            RpcError::Disconnected(message) => write!(f, "disconnected: {message}"),
        }
    }
}

impl std::error::Error for RpcError {}

impl RpcError {
    /// What the web shows for a failed request: a typed failure's `message` without its
    /// tag (`squashAtomCommandFailure`), else this error's description.
    pub fn message(&self) -> String {
        match self {
            RpcError::Failure(failure) => {
                failure.get("message").and_then(Value::as_str).map_or_else(|| self.to_string(), str::to_owned)
            }
            _ => self.to_string(),
        }
    }
}

/// One stream element, or the end of the stream.
#[derive(Debug)]
pub enum StreamEvent {
    Item(Value),
    End(Result<(), RpcError>),
}

/// What a stream's channel carries: its events, each chunk's values led by a marker.
enum Delivery {
    /// A chunk's values follow; taking the marker acks the chunk.
    Chunk,
    Event(StreamEvent),
}

enum Pending {
    Unary(oneshot::Sender<Result<Value, RpcError>>),
    Stream(async_channel::Sender<Delivery>),
}

struct Request {
    id: u64,
    tag: String,
    payload: Value,
    pending: Pending,
}

enum Command {
    Request(Request),
    Interrupt { id: u64 },
    Ack { id: u64 },
}

pub struct ConnectOptions {
    /// `ws://host:port/ws` without query.
    pub ws_url: String,
    pub bearer_token: String,
}

/// Cheap to clone; all clones share one socket and its environment's grants.
#[derive(Clone)]
pub struct RpcClient {
    commands: mpsc::UnboundedSender<Command>,
    next_id: Arc<AtomicU64>,
    closed: async_channel::Receiver<String>,
    /// What the socket checks guarded requests against before sending them.
    grants: Arc<watch::Sender<Grants>>,
}

/// Names a client's socket without keeping it open; another connection never equals it.
#[derive(Clone)]
pub struct SocketId(std::sync::Weak<AtomicU64>);

impl PartialEq for SocketId {
    fn eq(&self, other: &Self) -> bool {
        self.0.ptr_eq(&other.0)
    }
}

impl RpcClient {
    pub fn socket_id(&self) -> SocketId {
        SocketId(Arc::downgrade(&self.next_id))
    }

    /// Opens the socket on `runtime` and resolves once the upgrade succeeds.
    pub async fn connect(runtime: &tokio::runtime::Handle, options: ConnectOptions) -> Result<Self> {
        let (ready_tx, ready_rx) = oneshot::channel();
        let (commands, command_rx) = mpsc::unbounded_channel();
        let (closed_tx, closed) = async_channel::bounded(1);
        let (grants, grants_rx) = watch::channel(Grants::Pending);
        runtime.spawn(async move {
            let socket = match open_socket(&options).await {
                Ok(socket) => {
                    let _ = ready_tx.send(Ok(()));
                    socket
                }
                Err(error) => {
                    let _ = ready_tx.send(Err(error));
                    return;
                }
            };
            let reason = run_socket(socket, command_rx, grants_rx).await;
            log::info!("rpc socket closed: {reason}");
            let _ = closed_tx.try_send(reason);
        });
        ready_rx.await.map_err(|_| anyhow!("rpc runtime stopped"))??;
        Ok(Self { commands, next_id: Arc::new(AtomicU64::new(0)), closed, grants: Arc::new(grants) })
    }

    /// A client whose socket never opened, for state that only asks whether one is held.
    #[cfg(test)]
    pub fn unopened() -> Self {
        let (commands, _) = mpsc::unbounded_channel();
        let (_, closed) = async_channel::bounded(1);
        Self { commands, next_id: Arc::default(), closed, grants: Arc::new(watch::channel(Grants::Pending).0) }
    }

    /// The environment's grants, as its session check answered; guarded requests that wait for
    /// them are sent or refused now.
    pub fn set_grants(&self, grants: Grants) {
        self.grants.send_if_modified(|current| {
            let changed = *current != grants;
            *current = grants;
            changed
        });
    }

    /// Resolves with the reason once the socket closes.
    pub async fn closed(&self) -> String {
        self.closed.recv().await.unwrap_or_else(|_| "closed".into())
    }

    /// Both clients share one socket: a reconnect makes a new one.
    pub fn same_socket(&self, other: &Self) -> bool {
        self.commands.same_channel(&other.commands)
    }

    /// A pull request request may be answered by another trusted machine (see
    /// [`crate::pull_request_routing`]).
    pub async fn request(&self, tag: &str, payload: Value) -> Result<Value, RpcError> {
        if crate::pull_request_routing::intercepts(tag) {
            return crate::pull_request_routing::request(self, tag, payload).await;
        }
        self.request_direct(tag, payload).await
    }

    /// The request on this socket, never routed. A guarded one its environment's grants lack
    /// fails with `EnvironmentAuthorizationError` without being sent.
    pub async fn request_direct(&self, tag: &str, payload: Value) -> Result<Value, RpcError> {
        let (tx, rx) = oneshot::channel();
        self.send(tag, payload, Pending::Unary(tx));
        rx.await.unwrap_or_else(|_| Err(RpcError::Disconnected("socket closed".into())))
    }

    /// Starts a stream. Dropping the returned [`RpcStream`] interrupts it on the server.
    pub fn stream(&self, tag: &str, payload: Value) -> RpcStream {
        // The server sends a stream's next chunk only after its Ack, which goes out once the
        // consumer starts on a chunk: at most that chunk and the next one wait here.
        let (tx, rx) = async_channel::unbounded();
        let id = self.send(tag, payload, Pending::Stream(tx));
        RpcStream { id, events: rx, commands: self.commands.clone() }
    }

    fn send(&self, tag: &str, payload: Value, pending: Pending) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let command = Command::Request(Request { id, tag: tag.to_owned(), payload, pending });
        if let Err(mpsc::error::SendError(Command::Request(request))) = self.commands.send(command) {
            fail_pending(request.pending, RpcError::Disconnected("socket closed".into()));
        }
        id
    }
}

pub struct RpcStream {
    id: u64,
    events: async_channel::Receiver<Delivery>,
    commands: mpsc::UnboundedSender<Command>,
}

impl RpcStream {
    /// `None` after the stream ended and its `End` was delivered.
    pub async fn next(&self) -> Option<StreamEvent> {
        loop {
            if let Some(event) = self.take(self.events.recv().await.ok()?) {
                return Some(event);
            }
        }
    }

    /// An event that already arrived, without waiting. Lets callers apply a
    /// burst of events before re-rendering once.
    pub fn try_next(&self) -> Option<StreamEvent> {
        loop {
            if let Some(event) = self.take(self.events.try_recv().ok()?) {
                return Some(event);
            }
        }
    }

    /// The event a delivery carries; a chunk marker asks the server for the next chunk.
    fn take(&self, delivery: Delivery) -> Option<StreamEvent> {
        match delivery {
            Delivery::Chunk => {
                let _ = self.commands.send(Command::Ack { id: self.id });
                None
            }
            Delivery::Event(event) => Some(event),
        }
    }
}

impl Drop for RpcStream {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Interrupt { id: self.id });
    }
}

/// Whether [`RpcClient::connect`] failed because the server rejected the credential (HTTP 401).
pub fn is_unauthorized(error: &anyhow::Error) -> bool {
    matches!(
        error.downcast_ref::<tokio_tungstenite::tungstenite::Error>(),
        Some(tokio_tungstenite::tungstenite::Error::Http(response)) if response.status() == 401
    )
}

type Socket = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn open_socket(options: &ConnectOptions) -> Result<Socket> {
    let url = format!(
        "{}?orchestrationProtocol=2&frameParts=1&clientSurface=desktop&clientDeviceType=desktop",
        options.ws_url
    );
    let mut request = url.as_str().into_client_request().context("invalid websocket url")?;
    let mut authorization =
        HeaderValue::from_str(&format!("Bearer {}", options.bearer_token)).context("invalid token")?;
    authorization.set_sensitive(true);
    request.headers_mut().insert("Authorization", authorization);
    let config = WebSocketConfig::default().max_message_size(Some(256 << 20)).max_frame_size(Some(256 << 20));
    let connect = tokio_tungstenite::connect_async_with_config(request, Some(config), false);
    let (socket, _) = tokio::time::timeout(OPEN_TIMEOUT, connect)
        .await
        .context("websocket open timed out")?
        .context("websocket upgrade failed")?;
    Ok(socket)
}

/// Guarded requests waiting for the environment's first grants, oldest first.
#[derive(Default)]
struct Held(Vec<(Request, Instant)>);

impl Held {
    fn hold(&mut self, request: Request, now: Instant) {
        self.0.push((request, now + GRANT_WAIT));
    }

    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn deadline(&self) -> Option<Instant> {
        self.0.first().map(|(_, deadline)| *deadline)
    }

    /// Drops a held request whose caller stopped waiting; whether it was held.
    fn interrupt(&mut self, id: u64) -> bool {
        let before = self.0.len();
        self.0.retain(|(request, _)| request.id != id);
        self.0.len() != before
    }

    /// The held requests `grants` decide, and those that waited past their deadline, refused.
    fn decide(&mut self, grants: &Grants, now: Instant) -> Vec<(Request, Result<(), RpcError>)> {
        let mut decided = Vec::new();
        for (request, deadline) in std::mem::take(&mut self.0) {
            match authorize(grants, &request) {
                Some(result) => decided.push((request, result)),
                None if now >= deadline => {
                    let refused = authorize(&Grants::Failed, &request).unwrap_or(Ok(()));
                    decided.push((request, refused));
                }
                None => self.0.push((request, deadline)),
            }
        }
        decided
    }
}

/// `None` while the grants a guarded request needs are still loading.
fn authorize(grants: &Grants, request: &Request) -> Option<Result<(), RpcError>> {
    grants.authorize(&request.tag, &request.payload).map(|result| result.map_err(rpc_permissions::denied))
}

/// The request's frame; it is pending from now on.
fn request_message(request: Request, pending: &mut HashMap<u64, Pending>) -> Value {
    let Request { id, tag, payload, pending: entry } = request;
    pending.insert(id, entry);
    json!({"_tag": "Request", "id": id, "tag": tag, "payload": payload, "headers": []})
}

/// Sends the held requests the grants now allow and fails the ones they refuse.
async fn release(
    held: &mut Held,
    grants: &Grants,
    pending: &mut HashMap<u64, Pending>,
    sink: &mut futures::stream::SplitSink<Socket, Message>,
) -> Result<(), String> {
    let mut messages = Vec::new();
    for (request, result) in held.decide(grants, Instant::now()) {
        match result {
            Ok(()) => messages.push(request_message(request, pending)),
            Err(error) => fail_pending(request.pending, error),
        }
    }
    for message in messages {
        sink.send(Message::text(message.to_string())).await.map_err(|error| format!("send failed: {error}"))?;
    }
    Ok(())
}

/// Owns the socket until it closes. Returns the close reason. A guarded request is sent only
/// once `grants` allow it.
async fn run_socket(
    socket: Socket,
    mut commands: mpsc::UnboundedReceiver<Command>,
    mut grants: watch::Receiver<Grants>,
) -> String {
    let (mut sink, mut source) = socket.split();
    let mut pending: HashMap<u64, Pending> = HashMap::new();
    let mut held = Held::default();
    let mut frame_parts = String::new();
    let mut ping = tokio::time::interval(PING_INTERVAL);
    let mut last_inbound = Instant::now();

    let reason = 'socket: loop {
        let deadline = held.deadline();
        tokio::select! {
            command = commands.recv() => {
                let Some(command) = command else { break "client dropped".to_owned() };
                let message = match command {
                    Command::Request(request) => {
                        let decision = authorize(&grants.borrow(), &request);
                        match decision {
                            Some(Ok(())) => request_message(request, &mut pending),
                            Some(Err(error)) => { fail_pending(request.pending, error); continue }
                            None => { held.hold(request, Instant::now()); continue }
                        }
                    }
                    Command::Interrupt { id } => {
                        if held.interrupt(id) { continue }
                        // Only interrupt what is still running; finished streams are already gone.
                        if pending.remove(&id).is_none() { continue }
                        json!({"_tag": "Interrupt", "requestId": id})
                    }
                    Command::Ack { id } => {
                        if !pending.contains_key(&id) { continue }
                        json!({"_tag": "Ack", "requestId": id})
                    }
                };
                if let Err(error) = sink.send(Message::text(message.to_string())).await {
                    break format!("send failed: {error}");
                }
            }
            frame = source.next() => {
                let text = match frame {
                    Some(Ok(Message::Text(text))) => text,
                    Some(Ok(Message::Binary(bytes))) => Utf8Bytes::from(String::from_utf8_lossy(&bytes).into_owned()),
                    Some(Ok(Message::Close(frame))) => {
                        break frame.map_or_else(|| "closed by server".into(), |f| format!("closed by server: {} {}", f.code, f.reason))
                    }
                    Some(Ok(_)) => { last_inbound = Instant::now(); continue }
                    Some(Err(error)) => break format!("read failed: {error}"),
                    None => break "socket ended".to_owned(),
                };
                last_inbound = Instant::now();
                let Some(text) = join_frame_part(&mut frame_parts, text.as_str(), MAX_JOINED_FRAME) else { continue };
                let messages = match serde_json::from_str::<Value>(&text) {
                    Ok(Value::Array(items)) => items,
                    Ok(item) => vec![item],
                    Err(error) => { log::warn!("undecodable rpc frame: {error}"); continue }
                };
                let mut acks = Vec::new();
                for message in messages {
                    if let Some(fatal) = handle_server_message(message, &mut pending, &mut acks) {
                        fail_all(&mut pending, RpcError::Defect(fatal.clone()));
                    }
                }
                for id in acks {
                    let ack = json!({"_tag": "Ack", "requestId": id}).to_string();
                    if let Err(error) = sink.send(Message::Text(Utf8Bytes::from(ack))).await {
                        break 'socket format!("send failed: {error}");
                    }
                }
            }
            changed = grants.changed(), if !held.is_empty() => {
                let current = if changed.is_ok() { grants.borrow_and_update().clone() } else { Grants::Failed };
                if let Err(reason) = release(&mut held, &current, &mut pending, &mut sink).await {
                    break reason;
                }
            }
            () = tokio::time::sleep_until(deadline.unwrap_or_else(Instant::now)), if deadline.is_some() => {
                let current = grants.borrow().clone();
                if let Err(reason) = release(&mut held, &current, &mut pending, &mut sink).await {
                    break reason;
                }
            }
            _ = ping.tick() => {
                if last_inbound.elapsed() > PING_TIMEOUT {
                    break format!("server stopped responding for {}s", PING_TIMEOUT.as_secs());
                }
                if let Err(error) = sink.send(Message::text(r#"{"_tag":"Ping"}"#)).await {
                    break format!("send failed: {error}");
                }
            }
        }
    };
    let _ = sink.close().await;
    for (request, _) in held.0 {
        fail_pending(request.pending, RpcError::Disconnected(reason.clone()));
    }
    close(&mut pending, reason)
}

fn close(pending: &mut HashMap<u64, Pending>, reason: String) -> String {
    fail_all(pending, RpcError::Disconnected(reason.clone()));
    reason
}

/// Returns the joined frame once its last part arrives. A frame that would join past
/// `limit` bytes is dropped, so a misbehaving server can't grow the buffer without bound.
fn join_frame_part<'a>(buffer: &mut String, text: &'a str, limit: usize) -> Option<Cow<'a, str>> {
    let (part, last) = match text.strip_prefix(FRAME_PART_PREFIX) {
        Some(part) => (part, false),
        None if buffer.is_empty() => return Some(Cow::Borrowed(text)),
        None => (text, true),
    };
    if buffer.len().saturating_add(part.len()) > limit {
        log::warn!("dropped an rpc frame over {limit} bytes");
        *buffer = String::new();
        return None;
    }
    buffer.push_str(part);
    last.then(|| Cow::Owned(std::mem::take(buffer)))
}

/// Routes one server message. Returns a reason when the whole connection failed.
fn handle_server_message(
    mut message: Value,
    pending: &mut HashMap<u64, Pending>,
    acks: &mut Vec<u64>,
) -> Option<String> {
    let tag = message.get("_tag").and_then(Value::as_str).unwrap_or_default();
    let request_id = message.get("requestId").and_then(request_id);
    match (tag, request_id) {
        ("Chunk", Some(id)) => {
            match pending.get(&id) {
                // The consumer acks as it takes the chunk, so a stalled one holds the server back.
                Some(Pending::Stream(tx)) if tx.try_send(Delivery::Chunk).is_ok() => {
                    if let Value::Array(values) = message["values"].take() {
                        for value in values {
                            let _ = tx.try_send(Delivery::Event(StreamEvent::Item(value)));
                        }
                    }
                }
                // Ack streams already dropped, so the server's writer never stalls.
                _ => acks.push(id),
            }
            None
        }
        ("Exit", Some(id)) => {
            let result = decode_exit(message.get("exit"));
            match pending.remove(&id) {
                Some(Pending::Unary(tx)) => {
                    let _ = tx.send(result);
                }
                Some(Pending::Stream(tx)) => {
                    let _ = tx.try_send(Delivery::Event(StreamEvent::End(result.map(|_| ()))));
                }
                None => {}
            }
            None
        }
        ("Defect", _) => Some(defect_text(message.get("defect"))),
        ("Pong", _) => None,
        _ => {
            log::debug!("ignoring rpc message {tag}");
            None
        }
    }
}

fn request_id(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| value.as_str().and_then(|s| s.parse().ok()))
}

fn decode_exit(exit: Option<&Value>) -> Result<Value, RpcError> {
    let Some(exit) = exit else { return Err(RpcError::Defect("missing exit".into())) };
    match exit.get("_tag").and_then(Value::as_str) {
        Some("Success") => Ok(exit.get("value").cloned().unwrap_or(Value::Null)),
        _ => {
            let causes = exit.get("cause").and_then(Value::as_array).cloned().unwrap_or_default();
            if let Some(fail) = causes.iter().find(|c| c.get("_tag").and_then(Value::as_str) == Some("Fail")) {
                return Err(RpcError::Failure(fail.get("error").cloned().unwrap_or(Value::Null)));
            }
            if let Some(die) = causes.iter().find(|c| c.get("_tag").and_then(Value::as_str) == Some("Die")) {
                return Err(RpcError::Defect(defect_text(die.get("defect"))));
            }
            Err(RpcError::Defect("interrupted".into()))
        }
    }
}

fn defect_text(defect: Option<&Value>) -> String {
    match defect {
        Some(Value::String(text)) => text.clone(),
        Some(value) => value.get("message").and_then(Value::as_str).map_or_else(|| value.to_string(), str::to_owned),
        None => "unknown defect".into(),
    }
}

fn fail_pending(pending: Pending, error: RpcError) {
    match pending {
        Pending::Unary(tx) => {
            let _ = tx.send(Err(error));
        }
        Pending::Stream(tx) => {
            let _ = tx.try_send(Delivery::Event(StreamEvent::End(Err(error))));
        }
    }
}

fn fail_all(pending: &mut HashMap<u64, Pending>, error: RpcError) {
    for (_, entry) in pending.drain() {
        fail_pending(entry, error.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_split_frames() {
        let mut buffer = String::new();
        assert_eq!(join_frame_part(&mut buffer, "\u{1e}{\"a\":", 64), None);
        assert_eq!(join_frame_part(&mut buffer, "\u{1e}1", 64), None);
        assert_eq!(join_frame_part(&mut buffer, "}", 64).as_deref(), Some("{\"a\":1}"));
        assert_eq!(join_frame_part(&mut buffer, "{}", 64).as_deref(), Some("{}"));
    }

    #[test]
    fn split_frames_stop_growing_at_the_limit() {
        let mut buffer = String::new();
        assert_eq!(join_frame_part(&mut buffer, "\u{1e}12345", 8), None);
        assert_eq!(join_frame_part(&mut buffer, "\u{1e}6789", 8), None);
        assert!(buffer.is_empty(), "the oversized frame is dropped");
        assert_eq!(join_frame_part(&mut buffer, "{}", 8).as_deref(), Some("{}"));
    }

    #[test]
    fn a_401_upgrade_reads_as_unauthorized() {
        use tokio_tungstenite::tungstenite::{Error, http::Response};
        let rejected = |status: u16| {
            let response = Response::builder().status(status).body(None).unwrap();
            Err::<(), _>(Error::Http(Box::new(response))).context("websocket upgrade failed").unwrap_err()
        };
        assert!(is_unauthorized(&rejected(401)));
        assert!(!is_unauthorized(&rejected(426)));
        assert!(!is_unauthorized(&anyhow!("websocket open timed out")));
    }

    #[test]
    fn held_requests_are_refused_once_the_first_grants_never_came() {
        let mut held = Held::default();
        let now = Instant::now();
        let (tx, _rx) = oneshot::channel();
        held.hold(Request { id: 1, tag: "vcs.init".into(), payload: Value::Null, pending: Pending::Unary(tx) }, now);
        assert!(held.decide(&Grants::Pending, now + GRANT_WAIT - Duration::from_millis(1)).is_empty());
        let decided = held.decide(&Grants::Pending, now + GRANT_WAIT);
        let [(_, Err(RpcError::Failure(error)))] = &decided[..] else { panic!("expected a refusal") };
        assert_eq!(error["requiredPermission"], "source-control:write");
        assert!(held.is_empty());
    }

    /// A server that answers each request with its tag, and reports each tag it received.
    async fn echo_server() -> (String, mpsc::UnboundedReceiver<String>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ws_url = format!("ws://{}/ws", listener.local_addr().unwrap());
        let (received, tags) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            while let Some(Ok(Message::Text(text))) = socket.next().await {
                let message: Value = serde_json::from_str(&text).unwrap();
                if message["_tag"] != "Request" {
                    continue;
                }
                let tag = message["tag"].as_str().unwrap().to_owned();
                let exit =
                    json!({"_tag": "Exit", "requestId": message["id"], "exit": {"_tag": "Success", "value": tag}});
                received.send(tag).unwrap();
                socket.send(Message::text(exit.to_string())).await.unwrap();
            }
        });
        (ws_url, tags)
    }

    fn required_permission(result: Result<Value, RpcError>) -> String {
        match result {
            Err(RpcError::Failure(error)) if error["_tag"] == "EnvironmentAuthorizationError" => {
                error["requiredPermission"].as_str().unwrap().to_owned()
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn guarded_requests_go_out_only_when_the_environments_grants_allow_them() {
        use crate::rpc_permissions::{ORCHESTRATION_OPERATE, SOURCE_CONTROL_WRITE};
        let (ws_url, mut received) = echo_server().await;
        let options = ConnectOptions { ws_url, bearer_token: "token".into() };
        let client = RpcClient::connect(&tokio::runtime::Handle::current(), options).await.unwrap();
        let mut sent = move || std::iter::from_fn(|| received.try_recv().ok()).collect::<Vec<_>>();

        // Before the first grants arrive a guarded request waits; others go out.
        let mut run_now = Box::pin(client.request_direct("scheduledTasks.runNow", json!({"id": "a"})));
        assert!(futures::poll!(&mut run_now).is_pending());
        let dropped =
            client.stream("git.runStackedAction", json!({"actionId": "x", "cwd": "/repo", "action": "commit"}));
        drop(dropped);
        assert_eq!(client.request_direct("server.getConfig", json!({})).await.unwrap(), "server.getConfig");
        assert_eq!(sent(), ["server.getConfig"]);
        client.set_grants(Grants::ready(|scope| scope == ORCHESTRATION_OPERATE));
        assert_eq!(run_now.await.unwrap(), "scheduledTasks.runNow");

        // Refused without reaching the server, unary and streamed alike.
        client.set_grants(Grants::ready(|scope| scope == SOURCE_CONTROL_WRITE));
        assert_eq!(
            required_permission(client.request_direct("scheduledTasks.delete", json!({"id": "a"})).await),
            "orchestration:operate"
        );
        let create_pr = json!({"actionId": "y", "cwd": "/repo", "action": "create_pr", "threadId": "t"});
        let stream = client.stream("git.runStackedAction", create_pr);
        let Some(StreamEvent::End(result)) = stream.next().await else { panic!("expected the stream to end") };
        assert_eq!(required_permission(result.map(|()| Value::Null)), "orchestration:read");
        assert_eq!(client.request_direct("vcs.init", json!({"cwd": "/repo"})).await.unwrap(), "vcs.init");
        client.set_grants(Grants::Failed);
        assert_eq!(
            required_permission(client.request_direct("vcs.init", json!({"cwd": "/repo"})).await),
            "source-control:write"
        );
        assert_eq!(client.request_direct("server.getConfig", json!({})).await.unwrap(), "server.getConfig");
        assert_eq!(
            sent(),
            ["scheduledTasks.runNow", "vcs.init", "server.getConfig"],
            "the dropped stream never went out"
        );
    }

    #[test]
    fn decodes_typed_failures_before_defects() {
        let exit = json!({"_tag":"Failure","cause":[{"_tag":"Die","defect":"x"},{"_tag":"Fail","error":{"_tag":"E","message":"bad"}}]});
        let Err(RpcError::Failure(error)) = decode_exit(Some(&exit)) else { panic!("expected failure") };
        assert_eq!(error["message"], "bad");
    }

    #[test]
    fn chunks_ack_when_the_consumer_takes_them_and_exit_ends_stream() {
        let mut pending = HashMap::new();
        let (tx, events) = async_channel::unbounded();
        pending.insert(7, Pending::Stream(tx));
        let (commands, mut sent) = mpsc::unbounded_channel();
        let stream = RpcStream { id: 7, events, commands };
        let mut acks = Vec::new();
        handle_server_message(json!({"_tag":"Chunk","requestId":7,"values":[1,2]}), &mut pending, &mut acks);
        handle_server_message(json!({"_tag":"Chunk","requestId":"9","values":[3]}), &mut pending, &mut acks);
        assert_eq!(acks, vec![9], "only a chunk nobody reads is acked on arrival");
        assert!(sent.try_recv().is_err(), "an untaken chunk is not acked");

        assert!(matches!(stream.try_next(), Some(StreamEvent::Item(v)) if v == 1));
        assert!(matches!(sent.try_recv(), Ok(Command::Ack { id: 7 })), "taking the chunk acks it");
        assert!(matches!(stream.try_next(), Some(StreamEvent::Item(v)) if v == 2));
        assert!(sent.try_recv().is_err(), "once per chunk");

        handle_server_message(
            json!({"_tag":"Exit","requestId":7,"exit":{"_tag":"Success","value":null}}),
            &mut pending,
            &mut acks,
        );
        assert!(matches!(stream.try_next(), Some(StreamEvent::End(Ok(())))));
        assert!(pending.is_empty());
        drop(stream);
        assert!(matches!(sent.try_recv(), Ok(Command::Interrupt { id: 7 })));
    }
}
