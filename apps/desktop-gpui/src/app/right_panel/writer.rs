//! One JSON file under userdata, written off the UI thread: a writer thread saves the newest
//! snapshot sent, atomically and in order, so a quick run of saves costs one write.

use std::path::PathBuf;
use std::sync::mpsc::{Sender, channel};

use serde_json::Value;

pub(super) struct Writer {
    name: &'static str,
    path: Option<PathBuf>,
    /// Started by the first save; `None` inside when the thread could not start.
    sender: std::cell::OnceCell<Option<Sender<Value>>>,
}

impl Writer {
    pub(super) fn new(name: &'static str, file: &str) -> Self {
        Self { name, path: crate::settings::userdata_dir().map(|dir| dir.join(file)), sender: Default::default() }
    }

    /// The file's document, or `None` when it is missing or unreadable.
    pub(super) fn read(&self) -> Option<Value> {
        let bytes = std::fs::read(self.path.as_ref()?).ok()?;
        serde_json::from_slice(&bytes).inspect_err(|error| log::warn!("{} unreadable: {error}", self.name)).ok()
    }

    /// Queues `document`; a failed write only loses the latest changes.
    pub(super) fn save(&self, document: Value) {
        let Some(path) = self.path.clone() else { return };
        let name = self.name;
        let sender = self.sender.get_or_init(|| {
            let (sender, receiver) = channel::<Value>();
            let spawned = std::thread::Builder::new().name(name.into()).spawn(move || {
                while let Ok(mut latest) = receiver.recv() {
                    while let Ok(newer) = receiver.try_recv() {
                        latest = newer;
                    }
                    if let Err(error) = crate::settings::write_atomically(&path, &latest) {
                        log::warn!("{name} not saved: {error}");
                    }
                }
            });
            spawned.inspect_err(|error| log::warn!("{name} writer did not start: {error}")).ok().map(|_| sender)
        });
        if let Some(sender) = sender {
            let _ = sender.send(document);
        }
    }
}
