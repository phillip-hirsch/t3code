//! One app per T3 home, like Electron's `requestSingleInstanceLock`: a second launch hands
//! its arguments to the running app (`second-instance`) and exits. The channel is a named
//! pipe on Windows and a Unix socket elsewhere, both keyed by the T3 home, so copies with
//! different homes stay independent.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use sha2::{Digest as _, Sha256};

const MAX_MESSAGE_BYTES: usize = 64 * 1024;
/// How long a launch waits for a running app that has not answered yet (starting or busy).
const OWNER_WAIT: Duration = Duration::from_secs(3);
const OWNER_POLL: Duration = Duration::from_millis(50);

#[derive(Debug)]
pub enum Instance {
    /// This process is the app; other launches' arguments arrive on the sender given to [`claim`].
    /// `channel` is what it listens on; keep it until the process exits.
    Primary { channel: Option<Channel> },
    /// The running app took the arguments.
    Forwarded,
}

/// The channel the app owns. On Unix the socket is a file, so a lock held for the app's life
/// tells a running app's socket from one a crash left behind. Like Electron's single-instance
/// lock it must last until the process exits (on Windows the pipe is the lock and already does),
/// so a launch while this app quits waits up to [`OWNER_WAIT`] for it. That covers a normal quit;
/// one stalled on a stuck server or a keyring prompt can outlast it, and the launch then starts.
#[derive(Debug)]
pub struct Channel {
    #[cfg(unix)]
    socket: PathBuf,
    #[cfg(unix)]
    _lock: std::fs::File,
}

impl Channel {
    /// What quitting runs: removes the socket, so a clean quit leaves nothing in the temp folder.
    /// The next launch takes over once the channel is dropped.
    pub fn closer(&self) -> impl Fn() + 'static {
        #[cfg(unix)]
        let socket = self.socket.clone();
        move || {
            #[cfg(unix)]
            if let Err(error) = std::fs::remove_file(&socket) {
                log::warn!("could not remove the single-instance socket {}: {error}", socket.display());
            }
        }
    }
}

/// Forwards `args` to a running app for `home`, or starts listening as the app, sending later
/// launches' arguments to `launches`.
pub fn claim(
    home: &Path,
    args: &[String],
    launches: async_channel::Sender<Vec<String>>,
    runtime: &tokio::runtime::Handle,
) -> Instance {
    let address = address(home);
    let deadline = Instant::now() + OWNER_WAIT;
    loop {
        if forward(&address, args).is_ok() {
            return Instance::Forwarded;
        }
        match listen(&address, launches.clone(), runtime) {
            Ok(Some(channel)) => return Instance::Primary { channel: Some(channel) },
            // A running app owns the channel but did not answer: it is starting, busy or quitting.
            Ok(None) if Instant::now() < deadline => std::thread::sleep(OWNER_POLL),
            Ok(None) => {
                log::warn!("the app running for this T3 home did not answer on {address}");
                return Instance::Primary { channel: None };
            }
            Err(error) => {
                log::warn!("could not take the single-instance channel {address}: {error}");
                return Instance::Primary { channel: None };
            }
        }
    }
}

fn address(home: &Path) -> String {
    let key = home.to_string_lossy();
    // Windows paths are case-insensitive.
    let key = if cfg!(windows) { key.to_lowercase() } else { key.into_owned() };
    let digest = Sha256::digest(key.as_bytes());
    let id = crate::hex::lower_hex(&digest[..8]);
    if cfg!(windows) {
        format!(r"\\.\pipe\t3code-gpui-{id}")
    } else {
        socket_dir().join(format!("t3code-gpui-{id}.sock")).to_string_lossy().into_owned()
    }
}

/// A Unix socket path must fit in 104 bytes on macOS (108 on Linux), so like Chromium's
/// singleton socket it lives in the user's short runtime directory, not under the T3 home.
fn socket_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|_| cfg!(target_os = "linux"))
        .map_or_else(std::env::temp_dir, PathBuf::from)
}

fn message(args: &[String]) -> Vec<u8> {
    let mut line = serde_json::to_vec(args).unwrap_or_else(|_| b"[]".to_vec());
    line.push(b'\n');
    line
}

/// Reads one launch's arguments; anything malformed or oversized is dropped.
pub fn decode(line: &[u8]) -> Option<Vec<String>> {
    (line.len() <= MAX_MESSAGE_BYTES).then(|| serde_json::from_slice(line).ok()).flatten()
}

#[cfg(windows)]
fn forward(address: &str, args: &[String]) -> std::io::Result<()> {
    let mut pipe = std::fs::OpenOptions::new().write(true).open(address)?;
    // The launch the user just made may hand the foreground to the running app.
    let _ = winsafe::AllowSetForegroundWindow(None);
    pipe.write_all(&message(args))
}

/// Listens as the app; `None` when a running app owns the channel.
#[cfg(windows)]
fn listen(
    address: &str,
    sender: async_channel::Sender<Vec<String>>,
    runtime: &tokio::runtime::Handle,
) -> std::io::Result<Option<Channel>> {
    use tokio::net::windows::named_pipe::ServerOptions;
    let _guard = runtime.enter();
    // The pipe lasts while its owner runs, so a first instance is refused only then.
    let mut server = match ServerOptions::new().first_pipe_instance(true).create(address) {
        Ok(server) => server,
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => return Ok(None),
        Err(error) => return Err(error),
    };
    let address = address.to_owned();
    runtime.spawn(async move {
        loop {
            // A client that left before we accepted it fails the connect; its instance is spent either way.
            let connected = server.connect().await;
            let previous = std::mem::replace(
                &mut server,
                match ServerOptions::new().create(&address) {
                    Ok(next) => next,
                    Err(error) => {
                        log::warn!("single-instance channel closed: {error}");
                        return;
                    }
                },
            );
            if connected.is_ok() {
                tokio::spawn(read_one(previous, sender.clone()));
            }
        }
    });
    Ok(Some(Channel {}))
}

#[cfg(unix)]
fn forward(address: &str, args: &[String]) -> std::io::Result<()> {
    std::os::unix::net::UnixStream::connect(address)?.write_all(&message(args))
}

/// Listens as the app; `None` when a running app owns the channel.
#[cfg(unix)]
fn listen(
    address: &str,
    sender: async_channel::Sender<Vec<String>>,
    runtime: &tokio::runtime::Handle,
) -> std::io::Result<Option<Channel>> {
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
    let _guard = runtime.enter();
    if let Some(dir) = Path::new(address).parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Kept on disk: removing it could let two launches lock different files.
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(format!("{address}.lock"))?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => return Ok(None),
        Err(std::fs::TryLockError::Error(error)) => return Err(error),
    }
    // No app holds the lock, so a socket here is left over from a crash.
    let _ = std::fs::remove_file(address);
    let listener = tokio::net::UnixListener::bind(address)?;
    std::fs::set_permissions(address, std::fs::Permissions::from_mode(0o600))?;
    runtime.spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(read_one(stream, sender.clone()));
        }
    });
    Ok(Some(Channel { socket: PathBuf::from(address), _lock: lock }))
}

async fn read_one(stream: impl tokio::io::AsyncRead + Unpin, sender: async_channel::Sender<Vec<String>>) {
    use tokio::io::AsyncBufReadExt as _;
    let mut line = Vec::new();
    let mut reader = tokio::io::BufReader::new(tokio::io::AsyncReadExt::take(stream, MAX_MESSAGE_BYTES as u64 + 1));
    if reader.read_until(b'\n', &mut line).await.is_ok()
        && let Some(args) = decode(line.trim_ascii_end())
    {
        let _ = sender.send(args).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_arguments_round_trip_and_bad_messages_drop() {
        let args = vec!["t3-desktop.exe".to_owned(), "t3code://new?prompt=a b".to_owned()];
        assert_eq!(decode(message(&args).trim_ascii_end()), Some(args));
        assert_eq!(decode(b"{\"argv\":1}"), None);
        assert_eq!(decode(&vec![b' '; MAX_MESSAGE_BYTES + 1]), None);
    }

    #[test]
    fn each_home_gets_its_own_channel() {
        let a = address(Path::new("C:/Users/me/.t3-gpui"));
        assert_ne!(a, address(Path::new("C:/Users/me/scratch/home")));
        if cfg!(windows) {
            assert_eq!(a, address(Path::new("c:/users/ME/.t3-gpui")));
            assert!(a.starts_with(r"\\.\pipe\t3code-gpui-"));
        } else {
            let deep = address(&Path::new("/Users/someone").join("nested/".repeat(20)));
            assert!(deep.len() < 104, "{deep} fits a macOS socket path");
        }
    }

    /// A second launch reaches the first through the real channel.
    #[test]
    fn a_second_launch_hands_its_arguments_to_the_first() {
        let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
        let home = std::env::temp_dir().join(format!("t3-activation-test-{}", std::process::id()));
        let (sender, receiver) = async_channel::unbounded();
        let Instance::Primary { channel: Some(channel) } = claim(&home, &["first".into()], sender, runtime.handle())
        else {
            panic!("the first launch is the app");
        };
        let args = vec!["second".to_owned(), "t3code://new?prompt=x".to_owned()];
        let (unused, _) = async_channel::unbounded();
        assert!(matches!(claim(&home, &args, unused, runtime.handle()), Instance::Forwarded));
        assert_eq!(runtime.block_on(receiver.recv()).unwrap(), args);
        #[cfg(unix)]
        let socket = channel.socket.clone();
        channel.closer()();
        #[cfg(unix)]
        {
            assert!(!socket.exists(), "a clean quit removes the socket");
            let (unused, _) = async_channel::unbounded();
            let quitting = listen(&address(&home), unused.clone(), runtime.handle()).unwrap();
            assert!(quitting.is_none(), "the lock lasts until the quitting app exits");
            drop(channel);
            let next = listen(&address(&home), unused, runtime.handle()).unwrap();
            next.expect("the next launch takes over once the app is gone").closer()();
        }
        let _ = std::fs::remove_dir_all(home);
    }

    /// A crash leaves the socket file but releases the lock, so the next launch takes over.
    #[cfg(unix)]
    #[test]
    fn a_crashed_apps_socket_is_replaced() {
        let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
        let home = std::env::temp_dir().join(format!("t3-activation-crash-test-{}", std::process::id()));
        let address = address(&home);
        drop(std::os::unix::net::UnixListener::bind(&address).unwrap());
        assert!(Path::new(&address).exists(), "the dead app's socket is left");
        let (sender, receiver) = async_channel::unbounded();
        let Instance::Primary { channel: Some(channel) } = claim(&home, &[], sender, runtime.handle()) else {
            panic!("the launch after a crash is the app");
        };
        let (unused, _) = async_channel::unbounded();
        assert!(matches!(claim(&home, &["again".into()], unused, runtime.handle()), Instance::Forwarded));
        assert_eq!(runtime.block_on(receiver.recv()).unwrap(), ["again"]);
        channel.closer()();
        let _ = std::fs::remove_file(format!("{address}.lock"));
    }
}
