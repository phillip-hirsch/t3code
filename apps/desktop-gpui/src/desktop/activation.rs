//! One app per T3 home, like Electron's `requestSingleInstanceLock`: a second launch hands
//! its arguments to the running app (`second-instance`) and exits. The channel is a named
//! pipe on Windows and a Unix socket elsewhere, both keyed by the T3 home, so copies with
//! different homes stay independent.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use sha2::{Digest as _, Sha256};

const MAX_MESSAGE_BYTES: usize = 64 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub enum Instance {
    /// This process is the app; other launches' arguments arrive on the sender given to [`claim`].
    /// `socket` is the Unix socket it listens on, for [`remove_socket`] at quit.
    Primary { socket: Option<PathBuf> },
    /// The running app took the arguments.
    Forwarded,
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
    if forward(&address, args).is_ok() {
        return Instance::Forwarded;
    }
    match listen(&address, launches, runtime) {
        Ok(()) => Instance::Primary { socket: cfg!(unix).then(|| PathBuf::from(&address)) },
        // Another launch started listening between our attempts.
        Err(_) if forward(&address, args).is_ok() => Instance::Forwarded,
        Err(error) => {
            log::warn!("could not take the single-instance channel {address}: {error}");
            Instance::Primary { socket: None }
        }
    }
}

/// Removes the socket this app listened on, so a clean quit leaves nothing in the temp folder.
pub fn remove_socket(socket: &Path) {
    if let Err(error) = std::fs::remove_file(socket) {
        log::warn!("could not remove the single-instance socket {}: {error}", socket.display());
    }
}

fn address(home: &Path) -> String {
    let key = home.to_string_lossy();
    // Windows paths are case-insensitive.
    let key = if cfg!(windows) { key.to_lowercase() } else { key.into_owned() };
    let digest = Sha256::digest(key.as_bytes());
    let id: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
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
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
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

#[cfg(windows)]
fn listen(
    address: &str,
    sender: async_channel::Sender<Vec<String>>,
    runtime: &tokio::runtime::Handle,
) -> std::io::Result<()> {
    use tokio::net::windows::named_pipe::ServerOptions;
    let _guard = runtime.enter();
    let mut server = ServerOptions::new().first_pipe_instance(true).create(address)?;
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
    Ok(())
}

#[cfg(unix)]
fn forward(address: &str, args: &[String]) -> std::io::Result<()> {
    std::os::unix::net::UnixStream::connect(address)?.write_all(&message(args))
}

#[cfg(unix)]
fn listen(
    address: &str,
    sender: async_channel::Sender<Vec<String>>,
    runtime: &tokio::runtime::Handle,
) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let _guard = runtime.enter();
    if let Some(dir) = Path::new(address).parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Nothing answered on it, so the socket is left over from a crash.
    let _ = std::fs::remove_file(address);
    let listener = tokio::net::UnixListener::bind(address)?;
    std::fs::set_permissions(address, std::fs::Permissions::from_mode(0o600))?;
    runtime.spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(read_one(stream, sender.clone()));
        }
    });
    Ok(())
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
        let Instance::Primary { socket } = claim(&home, &["first".into()], sender, runtime.handle()) else {
            panic!("the first launch is the app");
        };
        let args = vec!["second".to_owned(), "t3code://new?prompt=x".to_owned()];
        let (unused, _) = async_channel::unbounded();
        assert_eq!(claim(&home, &args, unused, runtime.handle()), Instance::Forwarded);
        assert_eq!(runtime.block_on(receiver.recv()).unwrap(), args);
        if let Some(socket) = socket {
            remove_socket(&socket);
            assert!(!socket.exists(), "a clean quit removes the socket");
        }
        let _ = std::fs::remove_dir_all(home);
    }
}
