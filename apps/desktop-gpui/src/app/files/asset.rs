//! Workspace documents and binaries (`FilePreviewPanel` hands them to `AttachmentFilePreview`
//! with origin "File"): the file's bytes are fetched from its asset URL once per workspace
//! mutation, parsed off the UI thread, and shown under a "File › name" row with Save file.
//! The workspace passes no size, so the row shows none.

use std::collections::HashMap;

use gpui_kit::assets::IconName;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{Icon, WindowExt as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, Context, FontWeight, IntoElement, ParentElement as _, StatefulInteractiveElement as _, Styled as _,
    Task, Window, div, px,
};
use serde_json::{Value, json};

use super::logic::{self, FileKind, Office};
use super::{FileKey, archive, chrome, database, document, notebook};
use crate::app::{Target, Workspace};
use crate::theme::Tokens;

/// What a parsed file shows.
pub(super) enum Viewer {
    Archive(archive::Archive),
    Database(database::Database),
    Notebook(notebook::Notebook),
}

/// The parsed bytes, built off the UI thread; `Viewer` adds the UI's entities.
enum Parsed {
    Archive(archive::Listing),
    Database(crate::sqlite::Database),
    Notebook(notebook::NotebookData),
}

enum AssetLoad {
    Loading,
    Ready(Viewer),
    Failed(String),
}

struct AssetState {
    load: AssetLoad,
    mutation: (Option<String>, Option<String>),
    _task: Option<Task<()>>,
}

#[derive(Default)]
pub(super) struct Assets {
    states: HashMap<FileKey, AssetState>,
}

impl Assets {
    /// Drops `key`'s parsed file, so its next draw fetches it again.
    pub(super) fn forget(&mut self, key: &FileKey) {
        self.states.remove(key);
    }

    pub(super) fn viewer_mut(&mut self, key: &FileKey) -> Option<&mut Viewer> {
        match &mut self.states.get_mut(key)?.load {
            AssetLoad::Ready(viewer) => Some(viewer),
            _ => None,
        }
    }
}

/// The byte limit and its message for each viewer, or `None` when the file has no preview here.
fn read_limit(kind: FileKind) -> Option<(u64, &'static str)> {
    match kind {
        // `BINARY_PREVIEW_MAX_BYTES`.
        FileKind::Archive | FileKind::Sqlite => Some((archive::MAX_BYTES, "Preview limited to files under 50 MB.")),
        FileKind::Office(Office::Ipynb) => Some((notebook::MAX_BYTES, "Notebook preview limited to files under 8 MB.")),
        _ => None,
    }
}

fn parse(kind: FileKind, name: &str, bytes: &[u8]) -> Result<Parsed, String> {
    match kind {
        FileKind::Archive => archive::open_archive(name, bytes).map(Parsed::Archive),
        FileKind::Sqlite => crate::sqlite::Database::from_bytes(bytes).map(Parsed::Database),
        FileKind::Office(Office::Ipynb) => notebook::open_notebook(bytes).map(Parsed::Notebook),
        _ => Err("Could not preview this file.".into()),
    }
}

/// One HTTP client for file fetches, so connections are pooled.
pub(super) fn http() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(reqwest::Client::new)
}

/// The response body, or `None` once it passes `limit` bytes, with or without a Content-Length.
pub(super) async fn read_capped(mut response: reqwest::Response, limit: u64) -> reqwest::Result<Option<Vec<u8>>> {
    if response.content_length().is_some_and(|length| length > limit) {
        return Ok(None);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if (body.len() + chunk.len()) as u64 > limit {
            return Ok(None);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(Some(body))
}

/// The file's bytes from a signed asset URL, refused past `limit`.
pub(super) async fn fetch_bounded(url: String, limit: u64, too_large: &'static str) -> Result<Vec<u8>, String> {
    crate::runtime()
        .spawn(async move {
            let failed = "The file could not be loaded. Close it and try again.".to_owned();
            let response = http().get(url).send().await.map_err(|_| failed.clone())?;
            if !response.status().is_success() {
                return Err(failed);
            }
            read_capped(response, limit).await.map_err(|_| failed)?.ok_or_else(|| too_large.to_owned())
        })
        .await
        .map_err(|error| error.to_string())?
}

/// Where a file's bytes are read from.
pub(super) enum Fetch {
    /// Signed by `assets.createUrl` on the file's environment, then read over HTTP.
    Signed { client: crate::rpc::RpcClient, http_base: String, resource: Value },
    /// A draft attachment's bytes, already on this machine.
    Local(bytes::Bytes),
}

/// Why a [`Fetch`] read nothing: the URL could not be signed, or the read failed with a message.
#[derive(Debug, PartialEq)]
pub(super) enum FetchError {
    Unsigned,
    Failed(String),
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsigned => f.write_str("Reconnect to the environment and try again."),
            Self::Failed(message) => f.write_str(message),
        }
    }
}

impl Fetch {
    /// The same read on `live`, the socket and HTTP origin its environment has now; `None` when
    /// a signed read has none. A draft's bytes need neither.
    pub(super) fn with_connection(self, live: Option<(crate::rpc::RpcClient, String)>) -> Option<Self> {
        match self {
            Self::Local(bytes) => Some(Self::Local(bytes)),
            Self::Signed { resource, .. } => {
                live.map(|(client, http_base)| Self::Signed { client, http_base, resource })
            }
        }
    }

    /// The bytes, refused past `limit` with `too_large`, and the `fileRevision` they were signed at.
    pub(super) async fn read(
        self,
        limit: u64,
        too_large: &'static str,
    ) -> Result<(Vec<u8>, Option<String>), FetchError> {
        match self {
            Self::Local(bytes) if bytes.len() as u64 > limit => Err(FetchError::Failed(too_large.to_owned())),
            Self::Local(bytes) => Ok((bytes.to_vec(), None)),
            Self::Signed { client, http_base, resource } => {
                let (url, revision) =
                    document::mint_asset_url(client, http_base, resource).await.map_err(|_| FetchError::Unsigned)?;
                let bytes = fetch_bounded(url, limit, too_large).await.map_err(FetchError::Failed)?;
                Ok((bytes, revision))
            }
        }
    }

    /// At most the first `limit` bytes (`Range: bytes=0-…`, as `AttachmentFilePreview` asks).
    pub(super) async fn head(self, limit: usize) -> Result<Vec<u8>, FetchError> {
        let url = match self {
            Self::Local(bytes) => return Ok(bytes[..bytes.len().min(limit)].to_vec()),
            Self::Signed { client, http_base, resource } => {
                document::mint_asset_url(client, http_base, resource).await.map_err(|_| FetchError::Unsigned)?.0
            }
        };
        let failed = || FetchError::Failed("The file could not be loaded. Reconnect and try again.".to_owned());
        crate::runtime()
            .spawn(async move {
                let range = format!("bytes=0-{}", limit.saturating_sub(1));
                let mut response = http().get(url).header(reqwest::header::RANGE, range).send().await.ok()?;
                if !response.status().is_success() {
                    return None;
                }
                let mut body = Vec::new();
                while body.len() < limit {
                    let Some(chunk) = response.chunk().await.ok()? else { break };
                    body.extend_from_slice(&chunk[..chunk.len().min(limit - body.len())]);
                }
                Some(body)
            })
            .await
            .ok()
            .flatten()
            .ok_or_else(failed)
    }
}

impl Workspace {
    /// How `key`'s bytes are read: a workspace file through `workspace` (its asset for this read),
    /// an uploaded attachment through its `attachment` asset (`download` for Save file), a
    /// draft's from memory. `None` while the file's environment is not connected.
    pub(super) fn file_fetch(&self, key: &FileKey, workspace: Option<Value>, download: bool) -> Option<Fetch> {
        if let Some(bytes) = self.draft_bytes(key) {
            return Some(Fetch::Local(bytes));
        }
        let resource = if key.in_workspace() { workspace? } else { self.attachment_resource(key, download)? };
        let (client, endpoint) = self.file_connection(key)?;
        Some(Fetch::Signed { client, http_base: endpoint.http_base, resource })
    }

    /// The resource a document reads: a draft's `draft-workspace-file`, else the thread's
    /// `media-file` (an attachment download for files without a viewer).
    fn document_asset(&self, key: &FileKey, attachment: bool) -> Value {
        let absolute = logic::absolute_path(&key.checkout.cwd, &key.path);
        match &self.target {
            Some(Target::Thread { id: thread_id, .. }) => {
                let mut resource = json!({"_tag": "media-file", "threadId": thread_id, "path": absolute});
                if attachment {
                    resource["disposition"] = "attachment".into();
                }
                resource
            }
            _ => json!({"_tag": "draft-workspace-file", "cwd": key.checkout.cwd, "path": absolute}),
        }
    }

    fn load_asset(&mut self, key: &FileKey, kind: FileKind, window: &mut Window, cx: &mut Context<Self>) {
        let Some((limit, too_large)) = read_limit(kind) else { return };
        let Some(fetch) = self.file_fetch(key, Some(self.document_asset(key, false)), false) else { return };
        let request_key = key.clone();
        let name = logic::file_name(&key.path).to_owned();
        let task = cx.spawn_in(window, async move |this, cx| {
            let parsed = match fetch.read(limit, too_large).await {
                Err(FetchError::Unsigned) => Err("Reconnect to the environment and try again.".to_owned()),
                Err(FetchError::Failed(error)) => Err(error),
                Ok((bytes, _)) => {
                    let size = bytes.len() as u64;
                    cx.background_executor().spawn(async move { parse(kind, &name, &bytes).map(|p| (p, size)) }).await
                }
            };
            let _ = this.update_in(cx, |this, _window, cx| {
                let mut first_rows = false;
                let Some(state) = this.files.assets.states.get_mut(&request_key) else { return };
                state._task = None;
                state.load = match parsed {
                    Ok((Parsed::Archive(listing), size)) => {
                        AssetLoad::Ready(Viewer::Archive(archive::Archive::new(listing, size)))
                    }
                    Ok((Parsed::Database(opened), size)) => {
                        first_rows = !opened.tables().is_empty();
                        AssetLoad::Ready(Viewer::Database(database::Database::new(opened, size)))
                    }
                    Ok((Parsed::Notebook(data), size)) => {
                        AssetLoad::Ready(Viewer::Notebook(notebook::Notebook::new(data, size)))
                    }
                    Err(error) => AssetLoad::Failed(error),
                };
                // `BinaryViewer` reads the first table's first page as the database opens.
                if first_rows {
                    this.load_database_rows(&request_key, 0, 0, cx);
                }
                cx.notify();
            });
        });
        self.files.assets.states.insert(
            key.clone(),
            AssetState { load: AssetLoad::Loading, mutation: self.key_mutation(key), _task: Some(task) },
        );
    }

    /// Save file: the file's bytes, written where the user picks. A failure shows `failure`;
    /// `saved`, when given, confirms the write.
    pub(super) fn save_asset_file(
        &mut self,
        key: &FileKey,
        failure: String,
        saved: Option<&'static str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(fetch) = self.file_fetch(key, Some(self.document_asset(key, true)), true) else {
            let error = Notification::error("Reconnect to the environment and try again.").title(failure);
            return window.push_notification(error, cx);
        };
        let directory = std::env::home_dir().map(|home| home.join("Downloads")).unwrap_or_default();
        let name = match &key.origin {
            super::Origin::Attachment { name, .. } => name.to_string(),
            super::Origin::Draft { .. } | super::Origin::Workspace => {
                self.draft_name(key).unwrap_or_else(|| logic::file_name(&key.path).to_owned())
            }
        };
        let picked = cx.prompt_for_new_path(&directory, Some(&name));
        let (saving, env) = (key.clone(), self.file_environment(key));
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(path))) = picked.await else { return };
            let fetch = this.update(cx, |this, cx| {
                // Looked up after the dialog: a reconnect while it was open replaced the socket.
                let live = env.as_deref().and_then(|env| this.env_connection(env));
                let fetch = fetch.with_connection(live.map(|(client, endpoint)| (client, endpoint.http_base)))?;
                this.files.saving.insert(saving.clone());
                cx.notify();
                Some(fetch)
            });
            let read = match fetch {
                Ok(Some(fetch)) => fetch.read(u64::MAX, "").await,
                _ => Err(FetchError::Unsigned),
            };
            let written = match read {
                Ok((bytes, _)) => {
                    let write = async move { std::fs::write(&path, bytes).map_err(|error| error.to_string()) };
                    cx.background_executor().spawn(write).await
                }
                Err(FetchError::Failed(error)) => Err(error),
                Err(FetchError::Unsigned) => Err("Reconnect to the environment and try again.".to_owned()),
            };
            let _ = this.update(cx, |this, cx| {
                this.files.saving.remove(&saving);
                cx.notify();
            });
            let _ = cx.update(|window, cx| match written {
                Err(error) => window.push_notification(Notification::error(error).title(failure), cx),
                Ok(()) => {
                    if let Some(saved) = saved {
                        window.push_notification(Notification::success(saved), cx);
                    }
                }
            });
        })
        .detach();
    }

    /// `AttachmentFilePreview` with origin "File": its subheader, then the viewer.
    pub(super) fn render_asset_file(
        &mut self,
        key: &FileKey,
        kind: FileKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tokens = *crate::theme::tokens(cx);
        let name = logic::file_name(&key.path).to_owned();
        let save_key = key.clone();
        let header = h_flex()
            .flex_none()
            .h(px(28.))
            .mb(px(12.))
            .gap(px(8.))
            .px(px(12.))
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .text_xs()
                    .child(div().flex_none().px(px(2.)).text_color(tokens.muted_foreground).child("File"))
                    .child(
                        Icon::new(IconName::ChevronRight)
                            .size(px(14.))
                            .mx(px(4.))
                            .flex_none()
                            .text_color(tokens.muted_foreground.opacity(0.6)),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .px(px(2.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(tokens.foreground)
                            .child(name),
                    ),
            )
            .child(chrome::action("asset-save", IconName::Download, "Save file", &tokens).on_click(cx.listener(
                move |this, _, window, cx| {
                    this.save_asset_file(&save_key, "Could not save file".into(), None, window, cx);
                },
            )));
        let body = self.render_asset_body(key, kind, &tokens, window, cx);
        v_flex().flex_1().min_h_0().overflow_hidden().child(header).child(body).into_any_element()
    }

    pub(super) fn render_asset_body(
        &mut self,
        key: &FileKey,
        kind: FileKind,
        tokens: &Tokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if kind == FileKind::Font || matches!(kind, FileKind::Office(office) if office != Office::Ipynb) {
            return self.render_document(key, kind, window, cx);
        }
        let mutation = self.key_mutation(key);
        let stale = self.files.assets.states.get(key).is_none_or(|state| state.mutation != mutation);
        if stale && read_limit(kind).is_some() {
            self.load_asset(key, kind, window, cx);
        }
        if read_limit(kind).is_none() {
            let extension = logic::file_name(&key.path)
                .rsplit_once('.')
                .map(|(_, e)| e)
                .filter(|e| !e.is_empty())
                .unwrap_or("this format");
            return chrome::no_preview(format!("Save it to open in an app that supports {extension} files."), tokens);
        }
        match self.files.assets.states.get(key).map(|s| &s.load) {
            None | Some(AssetLoad::Loading) => chrome::loading(tokens),
            Some(AssetLoad::Failed(message)) if matches!(kind, FileKind::Office(_)) => {
                chrome::no_preview(format!("{message} Save it or open it in another app."), tokens)
            }
            Some(AssetLoad::Failed(message)) => {
                let retry = key.clone();
                let button =
                    chrome::retry_button("asset-retry", tokens).on_click(cx.listener(move |this, _, _, cx| {
                        this.files.assets.states.remove(&retry);
                        cx.notify();
                    }));
                chrome::failure(message.clone(), Some(button), tokens)
            }
            Some(AssetLoad::Ready(Viewer::Archive(_))) => self.render_archive_listing(key, tokens, cx),
            Some(AssetLoad::Ready(Viewer::Database(_))) => self.render_database(key, tokens, cx),
            Some(AssetLoad::Ready(Viewer::Notebook(_))) => self.render_notebook(key, tokens, window, cx),
        }
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    use super::*;

    /// A chunked body (no Content-Length) of three 4-byte chunks, read with each limit.
    async fn read_chunked(limit: u64) -> Option<Vec<u8>> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut request = [0; 1024];
            let _ = socket.read(&mut request).await;
            let body = "4\r\naaaa\r\n4\r\nbbbb\r\n4\r\ncccc\r\n0\r\n\r\n";
            let response = format!("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{body}");
            let _ = socket.write_all(response.as_bytes()).await;
        });
        let response = reqwest::get(format!("http://{address}/")).await.expect("request");
        assert_eq!(response.content_length(), None, "the server sends no length");
        let body = read_capped(response, limit).await.expect("read");
        server.await.expect("server");
        body
    }

    #[test]
    fn reads_without_a_content_length_stop_at_the_limit() {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime");
        assert_eq!(runtime.block_on(read_chunked(12)).as_deref(), Some(&b"aaaabbbbcccc"[..]));
        assert_eq!(runtime.block_on(read_chunked(11)), None);
    }

    #[test]
    fn a_save_reads_on_the_socket_its_environment_has_after_the_dialog() {
        let resource = json!({"_tag": "attachment", "attachmentId": "a"});
        let opened = || Fetch::Signed {
            client: crate::rpc::RpcClient::unopened(),
            http_base: "http://old".into(),
            resource: resource.clone(),
        };
        // The environment reconnected while the Save dialog was open.
        let reconnected = crate::rpc::RpcClient::unopened();
        let Some(Fetch::Signed { client, http_base, resource: read }) =
            opened().with_connection(Some((reconnected.clone(), "http://new".into())))
        else {
            panic!("a connected environment reads the file");
        };
        assert!(client.same_socket(&reconnected));
        assert_eq!((http_base.as_str(), &read), ("http://new", &resource));
        assert!(opened().with_connection(None).is_none(), "a disconnected environment reads nothing");
        let draft = Fetch::Local(bytes::Bytes::from_static(b"draft")).with_connection(None);
        assert!(matches!(draft, Some(Fetch::Local(_))), "a draft's bytes need no socket");
    }
}
