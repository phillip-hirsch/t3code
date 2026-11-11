//! The desktop app control socket (`DesktopAppActivation.ts` and its broker): `t3 app [path]`
//! asks the running app to open a project, and waits for the project and thread it opened.
//! The address comes from the state directory like `resolveDesktopAppControlAddress`, so the
//! CLI finds this app with `--base-dir` (or `T3CODE_HOME`) set to its T3 home.

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use tokio::io::{AsyncBufReadExt as _, AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _, BufReader};
use tokio::sync::oneshot;

const MAX_REQUEST_BYTES: usize = 64 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// How long a connection may take to send its request line.
const READ_TIMEOUT: Duration = Duration::from_secs(5);

/// `DesktopAppActivationPlatform`: the platform the CLI resolved the path on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    Darwin,
    Linux,
    Win32,
}

impl Platform {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "darwin" => Some(Self::Darwin),
            "linux" => Some(Self::Linux),
            "win32" => Some(Self::Win32),
            _ => None,
        }
    }

    /// `desktopPlatformToEnvironmentOs`.
    pub fn environment_os(self) -> &'static str {
        match self {
            Self::Darwin => "darwin",
            Self::Linux => "linux",
            Self::Win32 => "windows",
        }
    }
}

/// `DesktopAppActivationRequest` (`type: "open-workspace"`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub request_id: String,
    pub workspace_root: String,
    pub platform: Platform,
}

/// `DesktopAppActivationErrorCode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorCode {
    InvalidRequest,
    RendererUnavailable,
    EnvironmentUnavailable,
    PlatformMismatch,
    ProjectCreateFailed,
    ThreadOpenFailed,
    RequestTimeout,
}

impl ErrorCode {
    fn as_str(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid-request",
            Self::RendererUnavailable => "renderer-unavailable",
            Self::EnvironmentUnavailable => "environment-unavailable",
            Self::PlatformMismatch => "platform-mismatch",
            Self::ProjectCreateFailed => "project-create-failed",
            Self::ThreadOpenFailed => "thread-open-failed",
            Self::RequestTimeout => "request-timeout",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Opened { project_id: String, thread_id: String },
    Failed { code: ErrorCode, message: String },
}

/// `DesktopAppActivationResponse`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Response {
    pub request_id: String,
    pub outcome: Outcome,
}

impl Response {
    pub fn failure(request_id: &str, code: ErrorCode, message: impl Into<String>) -> Self {
        Self { request_id: request_id.to_owned(), outcome: Outcome::Failed { code, message: message.into() } }
    }

    fn to_json(&self) -> Value {
        match &self.outcome {
            Outcome::Opened { project_id, thread_id } => json!({
                "version": 1, "requestId": self.request_id, "ok": true, "projectId": project_id, "threadId": thread_id,
            }),
            Outcome::Failed { code, message } => json!({
                "version": 1, "requestId": self.request_id, "ok": false, "code": code.as_str(), "message": message,
            }),
        }
    }

    fn line(&self) -> Vec<u8> {
        let mut line = serde_json::to_vec(&self.to_json()).unwrap_or_default();
        line.push(b'\n');
        line
    }
}

/// A request for the window to handle; dropping it answers that the window went away.
#[derive(Debug)]
pub struct Incoming {
    pub request: Request,
    respond: oneshot::Sender<Response>,
}

impl Incoming {
    /// The command gave up (or timed out) before the window got to it.
    pub fn is_cancelled(&self) -> bool {
        self.respond.is_closed()
    }

    pub fn finish(self, outcome: Outcome) {
        let request_id = self.request.request_id;
        let _ = self.respond.send(Response { request_id, outcome });
    }
}

/// A string the contract's `TrimmedNonEmptyString` accepts.
fn non_blank(value: &Value) -> Option<&str> {
    value.as_str().filter(|value| !value.trim().is_empty())
}

/// Reads one request line; anything else gets the `invalid-request` answer the CLI shows.
pub fn decode_request(line: &[u8]) -> Result<Request, Response> {
    let Ok(parsed) = serde_json::from_slice::<Value>(line) else {
        return Err(Response::failure(
            "invalid-request",
            ErrorCode::InvalidRequest,
            "The desktop app request is not valid JSON.",
        ));
    };
    let request = (|| {
        if parsed["version"].as_f64() != Some(1.) || parsed["type"] != "open-workspace" {
            return None;
        }
        Some(Request {
            request_id: non_blank(&parsed["requestId"])?.to_owned(),
            workspace_root: non_blank(&parsed["workspaceRoot"])?.to_owned(),
            platform: Platform::parse(parsed["platform"].as_str()?)?,
        })
    })();
    request.ok_or_else(|| {
        let request_id = non_blank(&parsed["requestId"]).unwrap_or("invalid-request");
        Response::failure(request_id, ErrorCode::InvalidRequest, "The desktop app request is invalid.")
    })
}

/// The control socket's address: a named pipe on Windows, else a socket in a private folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Address {
    pub address: String,
    pub directory: Option<String>,
}

fn short_hash(value: &str) -> String {
    crate::hex::lower_hex(&Sha256::digest(value.as_bytes())[..12])
}

/// `resolveDesktopAppControlAddress`; state directories are hashed to fit Unix socket paths.
pub fn address(state_dir: &str, windows: bool, max_path_bytes: usize, temp_dir: &str, user_id: Option<u32>) -> Address {
    let state_hash = short_hash(state_dir);
    if windows {
        return Address { address: format!(r"\\.\pipe\t3code-app-{state_hash}"), directory: None };
    }
    let user_key = user_id.map_or_else(|| state_hash[..12].to_owned(), |id| id.to_string());
    let socket = format!("{state_hash}.sock");
    let mut directory = format!("{}/t3code-{user_key}", temp_dir.trim_end_matches('/'));
    if directory.len() + 1 + socket.len() > max_path_bytes {
        directory = format!("/tmp/t3code-{user_key}");
    }
    Address { address: format!("{directory}/{socket}"), directory: Some(directory) }
}

/// This app's address for `state_dir` (`<T3 home>/userdata`), as `path.resolve` spells it.
fn address_for(state_dir: &Path) -> Address {
    let state_dir = std::path::absolute(state_dir).unwrap_or_else(|_| state_dir.to_path_buf());
    platform_address(&state_dir.to_string_lossy())
}

#[cfg(unix)]
fn platform_address(state_dir: &str) -> Address {
    // `os.tmpdir()`.
    let temp = ["TMPDIR", "TMP", "TEMP"]
        .iter()
        .find_map(|key| std::env::var(key).ok().filter(|value| !value.is_empty()))
        .unwrap_or_else(|| "/tmp".to_owned());
    let max = if cfg!(target_os = "linux") { 107 } else { 103 };
    address(state_dir, false, max, &temp, Some(nix::unistd::getuid().as_raw()))
}

#[cfg(windows)]
fn platform_address(state_dir: &str) -> Address {
    address(state_dir, true, 0, "", None)
}

/// What [`DesktopAppActivationBroker`] keeps: the ids in flight and the way to the window.
#[derive(Clone)]
struct Broker {
    pending: Arc<Mutex<HashSet<String>>>,
    window: async_channel::Sender<Incoming>,
}

impl Broker {
    fn request(&self, request: Request) -> Result<oneshot::Receiver<Response>, Response> {
        let id = request.request_id.clone();
        if !self.pending.lock().unwrap_or_else(PoisonError::into_inner).insert(id.clone()) {
            return Err(Response::failure(&id, ErrorCode::InvalidRequest, "The request id is already in use."));
        }
        let (respond, response) = oneshot::channel();
        if self.window.try_send(Incoming { request, respond }).is_err() {
            self.settle(&id);
            return Err(Response::failure(&id, ErrorCode::RendererUnavailable, "T3 Code is shutting down."));
        }
        Ok(response)
    }

    fn settle(&self, request_id: &str) {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner).remove(request_id);
    }
}

enum Line {
    Complete(Vec<u8>),
    TooLarge,
    Closed,
}

async fn read_request(reader: &mut (impl tokio::io::AsyncBufRead + Unpin)) -> std::io::Result<Line> {
    let mut line = Vec::new();
    reader.take(MAX_REQUEST_BYTES as u64 + 1).read_until(b'\n', &mut line).await?;
    if line.len() > MAX_REQUEST_BYTES {
        return Ok(Line::TooLarge);
    }
    match line.pop() {
        Some(b'\n') => Ok(Line::Complete(line)),
        _ => Ok(Line::Closed),
    }
}

/// Resolves once the command closes its end.
async fn closed(reader: &mut (impl AsyncRead + Unpin)) {
    let mut scratch = [0; 256];
    while matches!(reader.read(&mut scratch).await, Ok(1..)) {}
}

/// One command's connection: its request line, then the window's answer.
async fn serve(stream: impl AsyncRead + AsyncWrite + Unpin, broker: Broker) {
    let (read, mut write) = tokio::io::split(stream);
    let mut reader = BufReader::new(read);
    let response = match tokio::time::timeout(READ_TIMEOUT, read_request(&mut reader)).await {
        Ok(Ok(Line::Complete(line))) => match decode_request(&line) {
            Err(response) => response,
            Ok(request) => {
                let id = request.request_id.clone();
                match broker.request(request) {
                    Err(response) => response,
                    Ok(response) => {
                        let answered = tokio::select! {
                            answered = tokio::time::timeout(REQUEST_TIMEOUT, response) => answered,
                            () = closed(&mut reader) => {
                                // Dropping the receiver tells the window to skip the request.
                                broker.settle(&id);
                                return;
                            }
                        };
                        broker.settle(&id);
                        match answered {
                            Ok(Ok(response)) => response,
                            Ok(Err(_)) => Response::failure(
                                &id,
                                ErrorCode::RendererUnavailable,
                                "The T3 Code window closed before it opened the project.",
                            ),
                            Err(_) => Response::failure(
                                &id,
                                ErrorCode::RequestTimeout,
                                "The desktop app did not finish opening the project in time.",
                            ),
                        }
                    }
                }
            }
        },
        Ok(Ok(Line::TooLarge)) => {
            Response::failure("invalid-request", ErrorCode::InvalidRequest, "The desktop app request is too large.")
        }
        Ok(Ok(Line::Closed) | Err(_)) | Err(_) => return,
    };
    let _ = write.write_all(&response.line()).await;
    let _ = write.shutdown().await;
}

/// The running control socket; quitting removes it if it is still this app's.
#[derive(Debug)]
pub struct ControlServer {
    #[cfg(unix)]
    socket: std::path::PathBuf,
    #[cfg(unix)]
    inode: u64,
}

impl ControlServer {
    pub fn closer(&self) -> impl Fn() + 'static {
        #[cfg(unix)]
        let (socket, inode) = (self.socket.clone(), self.inode);
        move || {
            // Another app may have taken the path over since; its socket stays.
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt as _;
                if std::fs::symlink_metadata(&socket).is_ok_and(|meta| meta.ino() == inode) {
                    let _ = std::fs::remove_file(&socket);
                }
            }
        }
    }
}

/// Starts answering `t3 app` for `state_dir`, handing requests to `window`.
pub fn start(
    state_dir: &Path,
    window: async_channel::Sender<Incoming>,
    runtime: &tokio::runtime::Handle,
) -> Option<ControlServer> {
    let address = address_for(state_dir);
    let broker = Broker { pending: Arc::default(), window };
    match listen(&address, broker, runtime) {
        Ok(server) => Some(server),
        Err(error) => {
            log::warn!("could not start the desktop app control socket at {}: {error}", address.address);
            None
        }
    }
}

#[cfg(windows)]
fn listen(address: &Address, broker: Broker, runtime: &tokio::runtime::Handle) -> std::io::Result<ControlServer> {
    use tokio::net::windows::named_pipe::ServerOptions;
    let _guard = runtime.enter();
    let mut server = ServerOptions::new().first_pipe_instance(true).create(&address.address)?;
    let address = address.address.clone();
    runtime.spawn(async move {
        loop {
            let connected = server.connect().await;
            let previous = std::mem::replace(
                &mut server,
                match ServerOptions::new().create(&address) {
                    Ok(next) => next,
                    Err(error) => {
                        log::warn!("desktop app control socket closed: {error}");
                        return;
                    }
                },
            );
            if connected.is_ok() {
                tokio::spawn(serve(previous, broker.clone()));
            }
        }
    });
    Ok(ControlServer {})
}

/// `prepareUnixDirectory`: a private folder this user owns.
#[cfg(unix)]
fn prepare_directory(directory: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _};
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(directory)?;
    let meta = std::fs::symlink_metadata(directory)?;
    if !meta.is_dir() {
        return Err(std::io::Error::other(format!("{} is not a directory.", directory.display())));
    }
    if meta.uid() != nix::unistd::getuid().as_raw() {
        return Err(std::io::Error::other(format!("{} is owned by another user.", directory.display())));
    }
    std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))
}

/// Binds a staging path and renames it into place, taking the address over as Electron does;
/// the CLI refuses a folder others can read.
#[cfg(unix)]
fn listen(address: &Address, broker: Broker, runtime: &tokio::runtime::Handle) -> std::io::Result<ControlServer> {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    let _guard = runtime.enter();
    let directory = Path::new(address.directory.as_deref().unwrap_or("/tmp"));
    prepare_directory(directory)?;
    let staging = directory.join(format!("{}.tmp", &uuid::Uuid::new_v4().simple().to_string()[..12]));
    let bound = (|| {
        let listener = tokio::net::UnixListener::bind(&staging)?;
        std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o600))?;
        let inode = std::fs::symlink_metadata(&staging)?.ino();
        std::fs::rename(&staging, &address.address)?;
        Ok::<_, std::io::Error>((listener, inode))
    })();
    let (listener, inode) = bound.inspect_err(|_| {
        let _ = std::fs::remove_file(&staging);
    })?;
    runtime.spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(serve(stream, broker.clone()));
        }
    });
    Ok(ControlServer { socket: address.address.clone().into(), inode })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_decode_like_the_contract_and_bad_ones_get_invalid_request() {
        let request = br#"{"version":1,"requestId":"r1","type":"open-workspace","workspaceRoot":"/a b","platform":"win32","extra":1}"#;
        assert_eq!(
            decode_request(request),
            Ok(Request { request_id: "r1".into(), workspace_root: "/a b".into(), platform: Platform::Win32 })
        );
        assert_eq!(Platform::Win32.environment_os(), "windows");
        let invalid = |line: &[u8]| match decode_request(line) {
            Err(Response { request_id, outcome: Outcome::Failed { code: ErrorCode::InvalidRequest, message } }) => {
                (request_id, message)
            }
            other => panic!("{other:?}"),
        };
        assert_eq!(invalid(b"{nope").1, "The desktop app request is not valid JSON.");
        let wrong_platform =
            br#"{"version":1,"requestId":"r2","type":"open-workspace","workspaceRoot":"/a","platform":"aix"}"#;
        assert_eq!(invalid(wrong_platform), ("r2".into(), "The desktop app request is invalid.".into()));
        let blank_root =
            br#"{"version":1,"requestId":"r3","type":"open-workspace","workspaceRoot":"  ","platform":"linux"}"#;
        assert_eq!(invalid(blank_root).0, "r3");
        let old = br#"{"version":2,"requestId":" ","type":"open-workspace","workspaceRoot":"/a","platform":"linux"}"#;
        assert_eq!(invalid(old).0, "invalid-request", "a blank id is not echoed");
    }

    #[test]
    fn responses_encode_like_the_contract() {
        let opened = Response {
            request_id: "r".into(),
            outcome: Outcome::Opened { project_id: "p".into(), thread_id: "t".into() },
        };
        assert_eq!(
            opened.to_json(),
            json!({"version": 1, "requestId": "r", "ok": true, "projectId": "p", "threadId": "t"})
        );
        let failed = Response::failure("r", ErrorCode::PlatformMismatch, "m");
        assert_eq!(
            failed.to_json(),
            json!({"version": 1, "requestId": "r", "ok": false, "code": "platform-mismatch", "message": "m"})
        );
        assert!(failed.line().ends_with(b"\n"));
    }

    #[test]
    fn addresses_match_resolve_desktop_app_control_address() {
        let long_state = format!("/home/user/{}userdata", "long/".repeat(40));
        let other = address("/home/user/.t3/other/userdata", false, 107, "/tmp", Some(1000));
        for (max, temp, directory) in [
            (107, "/tmp".to_owned(), "/tmp/t3code-1000".to_owned()),
            (107, "/var/tmp/custom/".to_owned(), "/var/tmp/custom/t3code-1000".to_owned()),
            (107, format!("/tmp/{}", "x".repeat(120)), "/tmp/t3code-1000".to_owned()),
            (103, format!("/tmp/{}", "é".repeat(30)), "/tmp/t3code-1000".to_owned()),
            (103, format!("/tmp/{}", "x".repeat(56)), format!("/tmp/{}/t3code-1000", "x".repeat(56))),
        ] {
            let first = address(&long_state, false, max, &temp, Some(1000));
            assert!(first.address.len() <= max, "{}", first.address);
            assert_eq!(first.directory.as_deref(), Some(directory.as_str()));
            assert_ne!(first.address, other.address);
        }
        let pipe = address(r"C:\Users\user\.t3\userdata", true, 0, "", None);
        assert_eq!(pipe.directory, None);
        let hash = pipe.address.strip_prefix(r"\\.\pipe\t3code-app-").unwrap();
        assert!(hash.len() == 24 && hash.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
        // sha256("abc") starts ba7816bf8f01cfea414140de.
        assert_eq!(short_hash("abc"), "ba7816bf8f01cfea414140de");
        assert_eq!(address("abc", false, 107, "/tmp", None).directory.as_deref(), Some("/tmp/t3code-ba7816bf8f01"));
    }

    /// A command reaches the window through the real socket and gets its answer; a second
    /// command reusing an id in flight is refused.
    #[cfg(unix)]
    #[test]
    fn a_command_gets_the_windows_answer_over_the_socket() {
        use std::io::{BufRead as _, Write as _};
        let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
        let root = std::env::temp_dir().join(format!("t3-app-control-test-{}", std::process::id()));
        let address = Address {
            address: root.join("s.sock").to_string_lossy().into_owned(),
            directory: Some(root.to_string_lossy().into_owned()),
        };
        let (window, requests) = async_channel::unbounded();
        let broker = Broker { pending: Arc::default(), window };
        let server = listen(&address, broker, runtime.handle()).unwrap();
        let send = |line: &str| {
            let mut stream = std::os::unix::net::UnixStream::connect(&address.address).unwrap();
            stream.write_all(line.as_bytes()).unwrap();
            stream
        };
        let request =
            r#"{"version":1,"requestId":"r","type":"open-workspace","workspaceRoot":"/w","platform":"linux"}"#;
        let first = send(&format!("{request}\n"));
        let incoming = runtime.block_on(requests.recv()).unwrap();
        assert_eq!(incoming.request.workspace_root, "/w");
        let mut duplicate = std::io::BufReader::new(send(&format!("{request}\n")));
        let mut answer = String::new();
        duplicate.read_line(&mut answer).unwrap();
        assert!(answer.contains("The request id is already in use."), "{answer}");
        incoming.finish(Outcome::Opened { project_id: "p".into(), thread_id: "t".into() });
        let mut answer = String::new();
        std::io::BufReader::new(first).read_line(&mut answer).unwrap();
        let answer: Value = serde_json::from_str(&answer).unwrap();
        assert_eq!(answer, json!({"version": 1, "requestId": "r", "ok": true, "projectId": "p", "threadId": "t"}));
        server.closer()();
        assert!(!Path::new(&address.address).exists(), "quitting removes the socket");
        let _ = std::fs::remove_dir(&root);
    }
}
