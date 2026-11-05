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
use super::{FileKey, archive, chrome, document, notebook, office};
use crate::app::{Target, Workspace};
use crate::theme::Tokens;

/// What a parsed file shows.
pub(super) enum Viewer {
    Archive(archive::Archive),
    Notebook(notebook::Notebook),
    Office(office::OfficeDoc),
}

/// The parsed bytes, built off the UI thread; `Viewer` adds the UI's entities.
enum Parsed {
    Archive(archive::Listing),
    Notebook(notebook::NotebookData),
    Office(office::OfficeData),
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
        FileKind::Archive => Some((archive::MAX_BYTES, "Preview limited to files under 50 MB.")),
        FileKind::Office(Office::Ipynb) => Some((notebook::MAX_BYTES, "Notebook preview limited to files under 8 MB.")),
        FileKind::Office(Office::Docx | Office::Xlsx | Office::Pptx) => {
            Some((office::MAX_BYTES, "Preview limited to files under 50 MB."))
        }
        _ => None,
    }
}

fn parse(kind: FileKind, name: &str, bytes: &[u8]) -> Result<Parsed, String> {
    match kind {
        FileKind::Archive => archive::open_archive(name, bytes).map(Parsed::Archive),
        FileKind::Office(Office::Ipynb) => notebook::open_notebook(bytes).map(Parsed::Notebook),
        FileKind::Office(office) => office::open_office(office, bytes).map(Parsed::Office),
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

impl Workspace {
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
        let Some((client, endpoint)) = self.file_connection(key) else { return };
        let resource = self.document_asset(key, false);
        let request_key = key.clone();
        let name = logic::file_name(&key.path).to_owned();
        let task = cx.spawn_in(window, async move |this, cx| {
            let fetched = match document::mint_asset_url(client, endpoint.http_base, resource).await {
                Ok((url, _)) => fetch_bounded(url, limit, too_large).await,
                Err(_) => Err("Reconnect to the environment and try again.".to_owned()),
            };
            let parsed = match fetched {
                Ok(bytes) => {
                    let size = bytes.len() as u64;
                    cx.background_executor().spawn(async move { parse(kind, &name, &bytes).map(|p| (p, size)) }).await
                }
                Err(error) => Err(error),
            };
            let _ = this.update_in(cx, |this, _window, cx| {
                let Some(state) = this.files.assets.states.get_mut(&request_key) else { return };
                state._task = None;
                state.load = match parsed {
                    Ok((Parsed::Archive(listing), size)) => {
                        AssetLoad::Ready(Viewer::Archive(archive::Archive::new(listing, size)))
                    }
                    Ok((Parsed::Notebook(data), size)) => {
                        AssetLoad::Ready(Viewer::Notebook(notebook::Notebook::new(data, size)))
                    }
                    Ok((Parsed::Office(data), _)) => AssetLoad::Ready(Viewer::Office(office::OfficeDoc::new(data))),
                    Err(error) => AssetLoad::Failed(error),
                };
                cx.notify();
            });
        });
        self.files.assets.states.insert(
            key.clone(),
            AssetState { load: AssetLoad::Loading, mutation: self.files.mutation.clone(), _task: Some(task) },
        );
    }

    /// Save file: the file's bytes, written where the user picks.
    fn save_asset_file(&mut self, key: &FileKey, window: &mut Window, cx: &mut Context<Self>) {
        let Some((client, endpoint)) = self.file_connection(key) else { return };
        let resource = self.document_asset(key, true);
        let directory = std::env::home_dir().map(|home| home.join("Downloads")).unwrap_or_default();
        let picked = cx.prompt_for_new_path(&directory, Some(logic::file_name(&key.path)));
        cx.spawn_in(window, async move |_, cx| {
            let Ok(Ok(Some(path))) = picked.await else { return };
            let written = match document::mint_asset_url(client, endpoint.http_base, resource).await {
                Ok((url, _)) => match fetch_bounded(url, u64::MAX, "").await {
                    Ok(bytes) => {
                        let write = async move { std::fs::write(&path, bytes).map_err(|error| error.to_string()) };
                        cx.background_executor().spawn(write).await
                    }
                    Err(error) => Err(error),
                },
                Err(_) => Err("Reconnect to the environment and try again.".to_owned()),
            };
            if let Err(error) = written {
                let _ = cx.update(|window, cx| {
                    window.push_notification(Notification::error(error).title("Could not save file"), cx)
                });
            }
        })
        .detach();
    }

    /// `AttachmentFilePreview` with origin "File": its subheader, then the viewer.
    pub(super) fn render_asset_file(
        &mut self,
        key: &FileKey,
        kind: FileKind,
        width: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tokens = *crate::theme::tokens(cx);
        let mutation = self.files.mutation.clone();
        let stale = self.files.assets.states.get(key).is_none_or(|state| state.mutation != mutation);
        if stale && read_limit(kind).is_some() {
            self.load_asset(key, kind, window, cx);
        }
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
            .child(
                chrome::action("asset-save", IconName::Download, "Save file", &tokens)
                    .on_click(cx.listener(move |this, _, window, cx| this.save_asset_file(&save_key, window, cx))),
            );
        let body = self.render_asset_body(key, kind, width, &tokens, window, cx);
        v_flex().flex_1().min_h_0().overflow_hidden().child(header).child(body).into_any_element()
    }

    fn render_asset_body(
        &mut self,
        key: &FileKey,
        kind: FileKind,
        width: f32,
        tokens: &Tokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if kind == FileKind::Font {
            return self.render_document(key, kind, window, cx);
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
            Some(AssetLoad::Ready(Viewer::Notebook(_))) => self.render_notebook(key, tokens, window, cx),
            Some(AssetLoad::Ready(Viewer::Office(_))) => self.render_office(key, width, tokens, cx),
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
}
