//! Unsent drafts outlive a restart (`composerDraftStore`'s `persist`): every draft's and
//! thread's prompt, its attachments, the terminal context its chips name and an explicit model
//! or permissions pick, with each draft's identity and the draft each project opens.
//!
//! Kept on this device in `<T3 home>/userdata/composer-drafts/drafts.json`, attachment bytes in
//! `attachments/` beside it (as the prompt stash keeps them), written off the UI thread a moment
//! after the last change and once more on quit. A question card's answers stay in memory as the
//! web's do; the prompt the card set aside is kept.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::Duration;

use bytes::Bytes;
use gpui_kit::{App, Context, Task, Window};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::attachments::Attachment;
use super::composer::{ComposerDraft, ComposerSelection};
use super::draft_sessions::{DraftId, DraftSessions, SavedSession};
use super::{Target, Workspace};
use crate::composer_context::ContextRecords;
use crate::model::ModelSelection;

/// `COMPOSER_PERSIST_DEBOUNCE_MS`.
const SAVE_DEBOUNCE: Duration = Duration::from_millis(300);
const STORE_VERSION: u64 = 1;
const FILE: &str = "drafts.json";
const BLOBS: &str = "attachments";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
enum SavedTarget {
    Thread { environment_id: String, thread_id: String },
    Draft { environment_id: String, project_id: String, draft_id: String },
}

impl SavedTarget {
    fn of(target: &Target) -> Self {
        match target {
            Target::Thread { env, id } => Self::Thread { environment_id: env.to_string(), thread_id: id.clone() },
            Target::Draft { env, project_id, draft } => Self::Draft {
                environment_id: env.to_string(),
                project_id: project_id.clone(),
                draft_id: draft.to_string(),
            },
        }
    }

    /// Orders the file's drafts, so an unchanged set writes the same document.
    fn key(&self) -> (&str, &str, &str) {
        match self {
            Self::Thread { environment_id, thread_id } => (environment_id, thread_id, ""),
            Self::Draft { environment_id, project_id, draft_id } => (environment_id, project_id, draft_id),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SavedAttachment {
    /// The composer id its prompt's chips name.
    local_id: String,
    kind: String,
    name: String,
    mime_type: String,
}

impl SavedAttachment {
    /// Its bytes' file name; `None` for an id that is not a plain file name.
    fn file_name(&self) -> Option<String> {
        let id = &self.local_id;
        let plain = !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        plain.then(|| format!("{id}.bin"))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SavedDraft {
    target: SavedTarget,
    #[serde(default)]
    prompt: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    attachments: Vec<SavedAttachment>,
    /// The context records its chips name (terminal output, review comments, threads, ...).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    records: Vec<Value>,
    /// A model the user picked (`modelSelectionExplicit`); an inherited one resolves again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    model_selection: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    runtime_mode: Option<String>,
}

#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DraftFile {
    #[serde(default)]
    version: u64,
    #[serde(default, deserialize_with = "crate::model::lossy")]
    drafts: Vec<SavedDraft>,
    #[serde(default, deserialize_with = "crate::model::lossy")]
    sessions: Vec<SavedSession>,
}

/// One target's composer as it stands.
struct Live<'a> {
    target: &'a Target,
    prompt: &'a str,
    attachments: &'a [Attachment],
    records: Option<&'a ContextRecords>,
    selection: Option<&'a ComposerSelection>,
}

/// What a save writes: the document, and the bytes of every attachment it names by file name.
#[derive(Clone)]
struct Write {
    /// Orders every save: an older one never replaces a newer one on disk.
    generation: u64,
    dir: PathBuf,
    document: Value,
    blobs: Vec<(String, Bytes)>,
}

/// The file's contents for `drafts`, keeping what `partializeComposerDraftStoreState` keeps: a
/// draft's entry only while its draft is kept, and an entry only with something in it.
fn snapshot(mut drafts: Vec<Live<'_>>, sessions: &DraftSessions) -> (DraftFile, Vec<(String, Bytes)>) {
    let with_content: HashSet<&DraftId> = (drafts.iter())
        .filter(|live| !live.prompt.trim().is_empty() || !live.attachments.is_empty())
        .filter_map(|live| live.target.draft_id())
        .collect();
    let sessions = sessions.saved(|id| with_content.contains(id));
    let kept: HashSet<&str> = sessions.iter().map(SavedSession::draft_id).collect();
    drafts.retain(|live| live.target.draft_id().is_none_or(|id| kept.contains(id.as_str())));
    let mut blobs = Vec::new();
    let mut saved: Vec<SavedDraft> = (drafts.into_iter())
        .filter_map(|live| {
            let model =
                (live.selection.filter(|s| s.explicit)).and_then(|s| s.model.as_ref()).map(ModelSelection::to_json);
            let runtime_mode = live.selection.and_then(|s| s.runtime_mode.clone());
            let empty = live.prompt.is_empty() && live.attachments.is_empty();
            if empty && model.is_none() && runtime_mode.is_none() {
                return None;
            }
            let attachments = (live.attachments.iter())
                .filter_map(|attachment| {
                    let saved = SavedAttachment {
                        local_id: attachment.local_id().to_owned(),
                        kind: attachment.kind_str().to_owned(),
                        name: attachment.name().to_owned(),
                        mime_type: attachment.mime().to_owned(),
                    };
                    blobs.push((saved.file_name()?, attachment.bytes().clone()));
                    Some(saved)
                })
                .collect();
            Some(SavedDraft {
                target: SavedTarget::of(live.target),
                prompt: live.prompt.to_owned(),
                attachments,
                records: live.records.map(|records| records.referenced(live.prompt)).unwrap_or_default(),
                model_selection: model,
                runtime_mode,
            })
        })
        .collect();
    saved.sort_by(|a, b| a.target.key().cmp(&b.target.key()));
    blobs.sort_by(|a, b| a.0.cmp(&b.0));
    (DraftFile { version: STORE_VERSION, drafts: saved, sessions }, blobs)
}

/// The newest generation on disk, and the last one handed out; a reopened window's workspace
/// carries on from it.
static WRITTEN: Mutex<u64> = Mutex::new(0);
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// Writes the attachment bytes the document needs, then the document, then drops bytes no draft
/// names any more. Skipped when `written` already holds this generation or a newer one.
fn write(write: &Write, written: &Mutex<u64>) -> std::io::Result<()> {
    let mut last = written.lock().unwrap_or_else(PoisonError::into_inner);
    if write.generation <= *last {
        return Ok(());
    }
    let blobs = write.dir.join(BLOBS);
    std::fs::create_dir_all(&blobs)?;
    for (name, bytes) in &write.blobs {
        let path = blobs.join(name);
        if !path.exists() {
            let temp = blobs.join(format!("{name}.tmp"));
            std::fs::write(&temp, bytes)?;
            std::fs::rename(&temp, &path)?;
        }
    }
    crate::settings::write_atomically(&write.dir.join(FILE), &write.document)?;
    *last = write.generation;
    let named: HashSet<&str> = write.blobs.iter().map(|(name, _)| name.as_str()).collect();
    for entry in std::fs::read_dir(&blobs)?.flatten() {
        if entry.file_name().to_str().is_none_or(|name| !named.contains(name)) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    Ok(())
}

/// Hands the save to the writer thread, which writes only the newest it has.
fn write_in_background(save: Write) {
    static WRITER: OnceLock<Option<Sender<Write>>> = OnceLock::new();
    let writer = WRITER.get_or_init(|| {
        let (sender, receiver) = channel::<Write>();
        let spawned = std::thread::Builder::new().name("composer-drafts".into()).spawn(move || {
            while let Ok(mut latest) = receiver.recv() {
                while let Ok(newer) = receiver.try_recv() {
                    latest = newer;
                }
                if let Err(error) = write(&latest, &WRITTEN) {
                    log::warn!("composer drafts not saved: {error}");
                }
            }
        });
        spawned.inspect_err(|error| log::warn!("composer draft writer did not start: {error}")).ok().map(|_| sender)
    });
    if let Some(sender) = writer {
        let _ = sender.send(save);
    }
}

/// The file as read, with the bytes of each attachment found, by local id.
#[derive(Default)]
struct Loaded {
    file: DraftFile,
    blobs: HashMap<String, Vec<u8>>,
    /// Saving would replace a file this build could not read.
    read_only: bool,
}

fn load(dir: &Path) -> Loaded {
    let path = dir.join(FILE);
    let raw = match std::fs::read(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Loaded::default(),
        Err(error) => {
            log::warn!("composer drafts {} could not be read, so drafts are not saved: {error}", path.display());
            return Loaded { read_only: true, ..Loaded::default() };
        }
    };
    let file = match serde_json::from_slice::<DraftFile>(&raw) {
        Ok(file) if file.version <= STORE_VERSION => file,
        Ok(file) => {
            log::warn!("composer drafts are version {}, newer than this build reads; not saving", file.version);
            return Loaded { read_only: true, ..Loaded::default() };
        }
        Err(error) => {
            let aside = dir.join(format!("drafts.corrupt-{}.json", crate::timestamp::now_ms()));
            log::warn!("composer drafts {} are damaged ({error}); moved them to {}", path.display(), aside.display());
            if let Err(error) = std::fs::rename(&path, &aside) {
                log::warn!("could not move the damaged composer drafts aside: {error}");
            }
            return Loaded::default();
        }
    };
    let mut blobs = HashMap::new();
    for attachment in file.drafts.iter().flat_map(|draft| &draft.attachments) {
        let Some(name) = attachment.file_name() else { continue };
        match std::fs::read(dir.join(BLOBS).join(&name)) {
            Ok(bytes) => {
                blobs.insert(attachment.local_id.clone(), bytes);
            }
            Err(error) => log::warn!("draft attachment {} was not restored: {error}", attachment.name),
        }
    }
    Loaded { file, blobs, read_only: false }
}

/// Splits restored targets into those whose environment has no shell yet and those it no longer
/// lists; `listed` is `None` while the environment's snapshot is still on its way.
fn verify(targets: Vec<Target>, listed: impl Fn(&Target) -> Option<bool>) -> (Vec<Target>, Vec<Target>) {
    let mut pending = Vec::new();
    let mut gone = Vec::new();
    for target in targets {
        match listed(&target) {
            None => pending.push(target),
            Some(false) => gone.push(target),
            Some(true) => {}
        }
    }
    (pending, gone)
}

#[derive(Default)]
pub(super) struct DraftStore {
    /// `None` without a home directory, or when the file there could not be read, so a save
    /// never replaces it.
    dir: Option<PathBuf>,
    /// The saved drafts are back; a save before then would write over them.
    restored: bool,
    _load: Option<Task<()>>,
    save: Option<Task<()>>,
    /// The last save handed to the writer, written again on quit.
    latest: Option<Write>,
    /// Restored drafts not yet checked against their environment's shell.
    unverified: Vec<Target>,
}

impl DraftStore {
    pub(super) fn restored(&self) -> bool {
        self.restored
    }
}

impl Workspace {
    /// Reads the saved drafts off the UI thread, brings them back, then lands.
    pub(super) fn load_drafts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let dir = crate::settings::userdata_dir().map(|dir| dir.join("composer-drafts"));
        let task = cx.spawn_in(window, async move |this, cx| {
            let (dir, loaded) = cx
                .background_executor()
                .spawn(async move {
                    let loaded = dir.as_deref().map(load).unwrap_or_default();
                    (dir, loaded)
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.restore_drafts(dir, loaded, cx);
                this.land(window, cx);
            });
        });
        self.draft_store._load = Some(task);
    }

    /// Puts the saved drafts back beside any made meanwhile; a target already holding a draft,
    /// or on screen, keeps its own.
    fn restore_drafts(&mut self, dir: Option<PathBuf>, loaded: Loaded, cx: &mut Context<Self>) {
        let Loaded { file, mut blobs, read_only } = loaded;
        self.draft_store.dir = dir.filter(|_| !read_only);
        self.draft_store.restored = true;
        let restored = self.drafts.restore(file.sessions);
        let mut unverified: Vec<Target> = restored.iter().filter_map(|id| self.drafts.target(id)).collect();
        for saved in file.drafts {
            let target = match &saved.target {
                SavedTarget::Draft { draft_id, .. } => {
                    let id = DraftId::from_saved(draft_id.clone());
                    // Its project is where the draft is now, which the session keeps.
                    match self.drafts.target(&id).filter(|_| restored.contains(&id)) {
                        Some(target) => target,
                        None => continue,
                    }
                }
                SavedTarget::Thread { environment_id, thread_id } => {
                    Target::Thread { env: environment_id.clone().into(), id: thread_id.clone() }
                }
            };
            if self.target.as_ref() == Some(&target) || self.composer_drafts.contains_key(&target) {
                continue;
            }
            let mut records = ContextRecords::default();
            records.extend(saved.records);
            let attachments: Vec<Attachment> = (saved.attachments.into_iter())
                .filter_map(|a| {
                    let bytes = blobs.remove(&a.local_id)?;
                    Some(Attachment::restored_as(a.local_id, &a.kind, a.name, a.mime_type, bytes))
                })
                .collect();
            let model = saved.model_selection.and_then(|value| serde_json::from_value::<ModelSelection>(value).ok());
            if model.is_some() || saved.runtime_mode.is_some() {
                let selection =
                    ComposerSelection { explicit: model.is_some(), model, runtime_mode: saved.runtime_mode };
                self.composer_selections.entry(target.clone()).or_insert(selection);
            }
            if !saved.prompt.is_empty() || !attachments.is_empty() {
                let draft = ComposerDraft::Prompt(saved.prompt, attachments, records);
                self.composer_drafts.insert(target.clone(), draft);
            }
            if matches!(target, Target::Thread { .. }) {
                unverified.push(target);
            }
        }
        self.draft_store.unverified.extend(unverified);
        self.verify_restored_drafts(cx);
        self.notify_sidebar(cx);
        cx.notify();
    }

    /// Drops restored drafts whose project or thread their environment's shell no longer lists,
    /// once that shell arrived; call after each shell batch.
    pub(super) fn verify_restored_drafts(&mut self, cx: &mut Context<Self>) {
        if self.draft_store.unverified.is_empty() {
            return;
        }
        let targets = std::mem::take(&mut self.draft_store.unverified);
        let (pending, gone) = verify(targets, |target| {
            if !self.shell.has_snapshot(target.environment()) {
                return None;
            }
            Some(self.target.as_ref() == Some(target) || self.lists(target))
        });
        self.draft_store.unverified = pending;
        if gone.is_empty() {
            return;
        }
        for target in &gone {
            self.composer_drafts.remove(target);
            self.composer_selections.remove(target);
            if let Some(draft) = target.draft_id() {
                self.drafts.forget(draft);
            }
        }
        self.drafts_changed(cx);
        self.notify_sidebar(cx);
    }

    /// The shell lists the target's thread, or its draft's project. A shell that leaves older
    /// settled threads out can't prove a thread is gone, so its threads' drafts stay.
    fn lists(&self, target: &Target) -> bool {
        match target {
            Target::Thread { env, id } => {
                self.shell.older_settled(env).is_some()
                    || self.shell.threads.iter().any(|t| t.environment_id == *env && t.id == *id)
            }
            Target::Draft { env, project_id, .. } => {
                self.shell.projects.iter().any(|p| p.environment_id == *env && p.id == *project_id)
            }
        }
    }

    /// Something a draft keeps changed: saves them all a moment after the last change.
    pub(super) fn drafts_changed(&mut self, cx: &mut Context<Self>) {
        let store = &self.draft_store;
        if !store.restored || store.dir.is_none() || store.save.is_some() {
            return;
        }
        self.draft_store.save = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_DEBOUNCE).await;
            let _ = this.update(cx, |this, cx| {
                this.draft_store.save = None;
                if let Some(save) = this.next_draft_save(cx) {
                    write_in_background(save);
                }
            });
        }));
    }

    /// Writes the drafts before the app quits or the window goes, waiting for the write.
    pub(super) fn flush_drafts(&mut self, cx: &App) {
        self.draft_store.save = None;
        if !self.draft_store.restored {
            return;
        }
        self.next_draft_save(cx);
        if let Some(save) = &self.draft_store.latest
            && let Err(error) = write(save, &WRITTEN)
        {
            log::warn!("composer drafts not saved before closing: {error}");
        }
    }

    /// The save for the drafts as they stand, unless the last one already holds them.
    fn next_draft_save(&mut self, cx: &App) -> Option<Write> {
        let dir = self.draft_store.dir.clone()?;
        self.stash_draft_workspace();
        let (file, blobs) = self.draft_snapshot(cx);
        let document = serde_json::to_value(&file).inspect_err(|e| log::warn!("composer drafts: {e}")).ok()?;
        if self.draft_store.latest.as_ref().is_some_and(|latest| latest.document == document) {
            return None;
        }
        let generation = GENERATION.fetch_add(1, Ordering::Relaxed) + 1;
        let save = Write { generation, dir, document, blobs };
        self.draft_store.latest = Some(save.clone());
        Some(save)
    }

    /// Every target's composer: the set-aside ones, and the one on screen, whose editor holds a
    /// question's answer or a queued message's edit while the target's own prompt waits.
    fn draft_snapshot(&self, cx: &App) -> (DraftFile, Vec<(String, Bytes)>) {
        let live_selection =
            (self.model_override.is_some() || self.runtime_mode_override.is_some()).then(|| ComposerSelection {
                model: self.model_override.clone(),
                explicit: self.model_explicit,
                runtime_mode: self.runtime_mode_override.clone(),
            });
        let editor;
        let set_aside = self.target.as_ref().and_then(|target| self.set_aside_prompt_ref(target));
        let (live_prompt, live_attachments, live_records): (&str, &[Attachment], Option<&ContextRecords>) =
            match set_aside {
                Some((prompt, attachments, records)) => (prompt, attachments, Some(records)),
                None => {
                    editor = self.composer.read(cx).value();
                    (&*editor, self.attachments.as_slice(), Some(&self.context.records))
                }
            };
        let mut drafts: Vec<Live> = Vec::new();
        if let Some(target) = &self.target {
            drafts.push(Live {
                target,
                prompt: live_prompt,
                attachments: live_attachments,
                records: live_records,
                selection: live_selection.as_ref(),
            });
        }
        let others = self.composer_drafts.keys().chain(self.composer_selections.keys());
        let mut seen: HashSet<&Target> = self.target.iter().collect();
        for target in others {
            if !seen.insert(target) {
                continue;
            }
            let (prompt, attachments, records) = match self.set_aside_prompt_ref(target) {
                Some((prompt, attachments, records)) => (prompt, attachments, Some(records)),
                None => ("", &[][..], None),
            };
            let selection = self.composer_selections.get(target);
            drafts.push(Live { target, prompt, attachments, records, selection });
        }
        snapshot(drafts, &self.drafts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ScopedId;
    use serde_json::json;

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("t3-gpui-composer-drafts-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn image(id: &str) -> Attachment {
        Attachment::restored_as(id.into(), "image", "shot.png".into(), "image/png".into(), vec![1, 2, 3])
    }

    fn live<'a>(target: &'a Target, prompt: &'a str, attachments: &'a [Attachment]) -> Live<'a> {
        Live { target, prompt, attachments, records: None, selection: None }
    }

    fn thread(id: &str) -> Target {
        Target::Thread { env: "local".into(), id: id.into() }
    }

    fn save(dir: &Path, generation: u64, file: &DraftFile, blobs: Vec<(String, Bytes)>, written: &Mutex<u64>) {
        let document = serde_json::to_value(file).unwrap();
        write(&Write { generation, dir: dir.to_owned(), document, blobs }, written).unwrap();
    }

    #[test]
    fn drafts_round_trip_with_their_attachment_bytes_and_identities() {
        let mut sessions = DraftSessions::default();
        let id = sessions.create(ScopedId::new("local", "web"));
        sessions.assign(ScopedId::new("local", "web"), &id);
        let (thread_id, _) = sessions.reserve_thread(&id).unwrap();
        let draft = sessions.target(&id).unwrap();
        let open = thread("t1");
        let attachments = [image("img-1")];
        let selection = ComposerSelection {
            model: Some(ModelSelection { instance_id: "codex".into(), model: "gpt".into(), options: None }),
            explicit: true,
            runtime_mode: Some("full-access".into()),
        };
        let record = json!({"kind": "terminal", "contextId": "terminal_1", "text": "ls"});
        let unreferenced = json!({"kind": "terminal", "contextId": "terminal_2", "text": "pwd"});
        let mut records = ContextRecords::default();
        records.extend([record.clone(), unreferenced]);
        let prompt = format!("fix it {}", crate::composer_text::context_link("terminal", "terminal_1", "ls"));
        let drafts = vec![
            Live { records: Some(&records), ..live(&open, &prompt, &[]) },
            Live { selection: Some(&selection), ..live(&draft, "draft text", &attachments) },
        ];
        let (file, blobs) = snapshot(drafts, &sessions);
        let dir = scratch_dir("round-trip");
        save(&dir, 1, &file, blobs, &Mutex::new(0));

        let loaded = load(&dir);
        assert!(!loaded.read_only);
        assert_eq!(loaded.file, file);
        assert_eq!(loaded.blobs.get("img-1").map(Vec::as_slice), Some(&[1u8, 2, 3][..]));
        let restored = &loaded.file.drafts;
        assert_eq!(restored.len(), 2);
        let (draft_entry, thread_entry) = (&restored[1], &restored[0]);
        assert_eq!(thread_entry.records, [record]);
        assert_eq!(draft_entry.target, SavedTarget::of(&draft));
        assert_eq!(draft_entry.model_selection, Some(json!({"instanceId": "codex", "model": "gpt"})));
        assert_eq!(draft_entry.runtime_mode.as_deref(), Some("full-access"));

        let mut next = DraftSessions::default();
        assert_eq!(next.restore(loaded.file.sessions), std::slice::from_ref(&id));
        assert_eq!(next.of_project(&ScopedId::new("local", "web")), Some(&id));
        assert_eq!(next.thread_id(&id), Some(thread_id.as_str()), "its terminals' thread comes back with it");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn sent_discarded_and_launched_drafts_are_not_kept() {
        let mut sessions = DraftSessions::default();
        let web = ScopedId::new("local", "web");
        let launched = sessions.create(web.clone());
        sessions.assign(web.clone(), &launched);
        sessions.promote(&launched);
        let unmapped_empty = sessions.create(web.clone());
        let unmapped_written = sessions.create(web);
        let [launched, unmapped_empty, unmapped_written] =
            [&launched, &unmapped_empty, &unmapped_written].map(|id| sessions.target(id).unwrap());
        let sent = thread("sent");
        let inherited = ComposerSelection { model: None, explicit: false, runtime_mode: None };
        let drafts = vec![
            live(&launched, "launched text", &[]),
            live(&unmapped_empty, "", &[]),
            live(&unmapped_written, "kept", &[]),
            Live { selection: Some(&inherited), ..live(&sent, "", &[]) },
        ];
        let (file, _) = snapshot(drafts, &sessions);
        let targets: Vec<&SavedTarget> = file.drafts.iter().map(|d| &d.target).collect();
        assert_eq!(targets, [&SavedTarget::of(&unmapped_written)]);
        let ids: Vec<&str> = file.sessions.iter().map(SavedSession::draft_id).collect();
        assert_eq!(ids, [unmapped_written.draft_id().unwrap().as_str()]);
    }

    #[test]
    fn a_save_writes_new_bytes_drops_unnamed_ones_and_never_goes_back_a_generation() {
        let dir = scratch_dir("prune");
        let written = Mutex::new(0);
        let draft = |prompt: &str| DraftFile {
            version: STORE_VERSION,
            drafts: vec![SavedDraft {
                target: SavedTarget::of(&thread("t")),
                prompt: prompt.into(),
                attachments: Vec::new(),
                records: Vec::new(),
                model_selection: None,
                runtime_mode: None,
            }],
            sessions: Vec::new(),
        };
        let blob = |name: &str| (name.to_owned(), Bytes::from_static(b"x"));
        save(&dir, 1, &draft("one"), vec![blob("a.bin"), blob("b.bin")], &written);
        save(&dir, 2, &draft("two"), vec![blob("b.bin")], &written);
        save(&dir, 1, &draft("stale"), vec![blob("a.bin")], &written);
        let mut names: Vec<String> = std::fs::read_dir(dir.join(BLOBS))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["b.bin"]);
        assert_eq!(load(&dir).file.drafts[0].prompt, "two");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_damaged_file_is_moved_aside_and_a_newer_one_is_left_alone() {
        let dir = scratch_dir("damaged");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(FILE), "{\"drafts\": [").unwrap();
        let loaded = load(&dir);
        assert!(!loaded.read_only && loaded.file.drafts.is_empty());
        let aside = std::fs::read_dir(&dir).unwrap().filter_map(Result::ok);
        assert_eq!(aside.filter(|e| e.file_name().to_string_lossy().starts_with("drafts.corrupt-")).count(), 1);

        std::fs::write(dir.join(FILE), json!({"version": STORE_VERSION + 1}).to_string()).unwrap();
        assert!(load(&dir).read_only);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn attachment_ids_that_are_not_plain_names_keep_no_bytes() {
        let attachment = |id: &str| SavedAttachment {
            local_id: id.into(),
            kind: "file".into(),
            name: "a".into(),
            mime_type: "text/plain".into(),
        };
        assert_eq!(attachment("3f2a-b_1").file_name().as_deref(), Some("3f2a-b_1.bin"));
        assert_eq!(attachment("../escape").file_name(), None);
        assert_eq!(attachment("").file_name(), None);
    }

    #[test]
    fn restored_drafts_wait_for_their_shell_then_go_when_it_no_longer_lists_them() {
        let (pending, gone) = verify(vec![thread("kept"), thread("gone"), thread("remote")], |target| match target {
            Target::Thread { id, .. } if id == "remote" => None,
            Target::Thread { id, .. } => Some(id == "kept"),
            Target::Draft { .. } => Some(true),
        });
        assert_eq!(pending, [thread("remote")]);
        assert_eq!(gone, [thread("gone")]);
    }
}
