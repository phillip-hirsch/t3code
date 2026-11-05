//! Effect RPC over WebSocket, JSON serialization (see `.repos/effect/packages/effect/src/rpc`).
//!
//! The socket runs on the shared tokio runtime. Callers get runtime-agnostic
//! futures and channels, so GPUI tasks can await them directly.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow};
use futures::channel::oneshot;
use futures::{SinkExt as _, StreamExt as _};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::{Message, Utf8Bytes};

const PING_INTERVAL: Duration = Duration::from_secs(5);
const PING_TIMEOUT: Duration = Duration::from_secs(15);
const OPEN_TIMEOUT: Duration = Duration::from_secs(15);
/// Server prefixes every split frame part except the last with U+001E.
const FRAME_PART_PREFIX: char = '\u{1e}';

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

/// One stream element, or the end of the stream.
#[derive(Debug)]
pub enum StreamEvent {
    Item(Value),
    End(Result<(), RpcError>),
}

enum Pending {
    Unary(oneshot::Sender<Result<Value, RpcError>>),
    Stream(async_channel::Sender<StreamEvent>),
}

enum Command {
    Request { id: u64, tag: String, payload: Value, pending: Pending },
    Interrupt { id: u64 },
}

pub struct ConnectOptions {
    /// `ws://host:port/ws` without query.
    pub ws_url: String,
    pub bearer_token: String,
}

/// Cheap to clone; all clones share one socket.
#[derive(Clone)]
pub struct RpcClient {
    commands: mpsc::UnboundedSender<Command>,
    next_id: Arc<AtomicU64>,
    closed: async_channel::Receiver<String>,
}

impl RpcClient {
    /// Opens the socket on `runtime` and resolves once the upgrade succeeds.
    pub async fn connect(runtime: &tokio::runtime::Handle, options: ConnectOptions) -> Result<Self> {
        let (ready_tx, ready_rx) = oneshot::channel();
        let (commands, command_rx) = mpsc::unbounded_channel();
        let (closed_tx, closed) = async_channel::bounded(1);
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
            let reason = run_socket(socket, command_rx).await;
            log::info!("rpc socket closed: {reason}");
            let _ = closed_tx.try_send(reason);
        });
        ready_rx.await.map_err(|_| anyhow!("rpc runtime stopped"))??;
        Ok(Self { commands, next_id: Arc::new(AtomicU64::new(0)), closed })
    }

    /// Resolves with the reason once the socket closes.
    pub async fn closed(&self) -> String {
        self.closed.recv().await.unwrap_or_else(|_| "closed".into())
    }

    pub async fn request(&self, tag: &str, payload: Value) -> Result<Value, RpcError> {
        let (tx, rx) = oneshot::channel();
        self.send(tag, payload, Pending::Unary(tx));
        rx.await.unwrap_or_else(|_| Err(RpcError::Disconnected("socket closed".into())))
    }

    /// Starts a stream. Dropping the returned [`RpcStream`] interrupts it on the server.
    pub fn stream(&self, tag: &str, payload: Value) -> RpcStream {
        // Unbounded on our side: the server already waits for our Ack per chunk.
        let (tx, rx) = async_channel::unbounded();
        let id = self.send(tag, payload, Pending::Stream(tx));
        RpcStream { id, events: rx, commands: self.commands.clone() }
    }

    fn send(&self, tag: &str, payload: Value, pending: Pending) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let command = Command::Request { id, tag: tag.to_owned(), payload, pending };
        if let Err(mpsc::error::SendError(Command::Request { pending, .. })) = self.commands.send(command) {
            fail_pending(pending, RpcError::Disconnected("socket closed".into()));
        }
        id
    }
}

pub struct RpcStream {
    id: u64,
    events: async_channel::Receiver<StreamEvent>,
    commands: mpsc::UnboundedSender<Command>,
}

impl RpcStream {
    /// `None` after the stream ended and its `End` was delivered.
    pub async fn next(&self) -> Option<StreamEvent> {
        self.events.recv().await.ok()
    }

    /// An event that already arrived, without waiting. Lets callers apply a
    /// burst of events before re-rendering once.
    pub fn try_next(&self) -> Option<StreamEvent> {
        self.events.try_recv().ok()
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
    request.headers_mut().insert(
        "Authorization",
        HeaderValue::from_str(&format!("Bearer {}", options.bearer_token)).context("invalid token")?,
    );
    let config = WebSocketConfig::default().max_message_size(Some(256 << 20)).max_frame_size(Some(256 << 20));
    let connect = tokio_tungstenite::connect_async_with_config(request, Some(config), false);
    let (socket, _) = tokio::time::timeout(OPEN_TIMEOUT, connect)
        .await
        .context("websocket open timed out")?
        .context("websocket upgrade failed")?;
    Ok(socket)
}

/// Owns the socket until it closes. Returns the close reason.
async fn run_socket(socket: Socket, mut commands: mpsc::UnboundedReceiver<Command>) -> String {
    let (mut sink, mut source) = socket.split();
    let mut pending: HashMap<u64, Pending> = HashMap::new();
    let mut frame_parts = String::new();
    let mut ping = tokio::time::interval(PING_INTERVAL);
    let mut last_inbound = Instant::now();

    let reason = loop {
        tokio::select! {
            command = commands.recv() => {
                let Some(command) = command else { break "client dropped".to_owned() };
                let message = match command {
                    Command::Request { id, tag, payload, pending: entry } => {
                        pending.insert(id, entry);
                        json!({"_tag": "Request", "id": id, "tag": tag, "payload": payload, "headers": []})
                    }
                    Command::Interrupt { id } => {
                        // Only interrupt what is still running; finished streams are already gone.
                        if pending.remove(&id).is_none() { continue }
                        json!({"_tag": "Interrupt", "requestId": id})
                    }
                };
                if let Err(error) = sink.send(Message::text(message.to_string())).await {
                    break format!("send failed: {error}");
                }
            }
            frame = source.next() => {
                let text = match frame {
                    Some(Ok(Message::Text(text))) => text.to_string(),
                    Some(Ok(Message::Binary(bytes))) => String::from_utf8_lossy(&bytes).into_owned(),
                    Some(Ok(Message::Close(frame))) => {
                        break frame.map(|f| format!("closed by server: {} {}", f.code, f.reason)).unwrap_or_else(|| "closed by server".into())
                    }
                    Some(Ok(_)) => { last_inbound = Instant::now(); continue }
                    Some(Err(error)) => break format!("read failed: {error}"),
                    None => break "socket ended".to_owned(),
                };
                last_inbound = Instant::now();
                let Some(text) = join_frame_part(&mut frame_parts, text) else { continue };
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
                        return close(&mut pending, format!("send failed: {error}"));
                    }
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
    close(&mut pending, reason)
}

fn close(pending: &mut HashMap<u64, Pending>, reason: String) -> String {
    fail_all(pending, RpcError::Disconnected(reason.clone()));
    reason
}

/// Returns the joined frame once its last part arrives.
fn join_frame_part(buffer: &mut String, text: String) -> Option<String> {
    match text.strip_prefix(FRAME_PART_PREFIX) {
        Some(part) => {
            buffer.push_str(part);
            None
        }
        None if buffer.is_empty() => Some(text),
        None => {
            buffer.push_str(&text);
            Some(std::mem::take(buffer))
        }
    }
}

/// Routes one server message. Returns a reason when the whole connection failed.
fn handle_server_message(message: Value, pending: &mut HashMap<u64, Pending>, acks: &mut Vec<u64>) -> Option<String> {
    let tag = message.get("_tag").and_then(Value::as_str).unwrap_or_default();
    let request_id = message.get("requestId").and_then(request_id);
    match (tag, request_id) {
        ("Chunk", Some(id)) => {
            // Ack even for streams we already dropped, so the server's writer never stalls.
            acks.push(id);
            if let Some(Pending::Stream(tx)) = pending.get(&id) {
                if let Some(Value::Array(values)) = message.get("values") {
                    for value in values {
                        let _ = tx.try_send(StreamEvent::Item(value.clone()));
                    }
                }
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
                    let _ = tx.try_send(StreamEvent::End(result.map(|_| ())));
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
        Some(value) => {
            value.get("message").and_then(Value::as_str).map(str::to_owned).unwrap_or_else(|| value.to_string())
        }
        None => "unknown defect".into(),
    }
}

fn fail_pending(pending: Pending, error: RpcError) {
    match pending {
        Pending::Unary(tx) => {
            let _ = tx.send(Err(error));
        }
        Pending::Stream(tx) => {
            let _ = tx.try_send(StreamEvent::End(Err(error)));
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
        assert_eq!(join_frame_part(&mut buffer, "\u{1e}{\"a\":".into()), None);
        assert_eq!(join_frame_part(&mut buffer, "\u{1e}1".into()), None);
        assert_eq!(join_frame_part(&mut buffer, "}".into()).as_deref(), Some("{\"a\":1}"));
        assert_eq!(join_frame_part(&mut buffer, "{}".into()).as_deref(), Some("{}"));
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
    fn decodes_typed_failures_before_defects() {
        let exit = json!({"_tag":"Failure","cause":[{"_tag":"Die","defect":"x"},{"_tag":"Fail","error":{"_tag":"E","message":"bad"}}]});
        let Err(RpcError::Failure(error)) = decode_exit(Some(&exit)) else { panic!("expected failure") };
        assert_eq!(error["message"], "bad");
    }

    #[test]
    fn chunks_are_acked_and_delivered_and_exit_ends_stream() {
        let mut pending = HashMap::new();
        let (tx, rx) = async_channel::unbounded();
        pending.insert(7, Pending::Stream(tx));
        let mut acks = Vec::new();
        handle_server_message(json!({"_tag":"Chunk","requestId":7,"values":[1,2]}), &mut pending, &mut acks);
        handle_server_message(json!({"_tag":"Chunk","requestId":"9","values":[3]}), &mut pending, &mut acks);
        handle_server_message(
            json!({"_tag":"Exit","requestId":7,"exit":{"_tag":"Success","value":null}}),
            &mut pending,
            &mut acks,
        );
        assert_eq!(acks, vec![7, 9]);
        assert!(matches!(rx.try_recv(), Ok(StreamEvent::Item(v)) if v == 1));
        assert!(matches!(rx.try_recv(), Ok(StreamEvent::Item(v)) if v == 2));
        assert!(matches!(rx.try_recv(), Ok(StreamEvent::End(Ok(())))));
        assert!(pending.is_empty());
    }
}
