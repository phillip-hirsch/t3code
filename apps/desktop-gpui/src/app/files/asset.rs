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
use super::{archive, chrome, document, notebook, office};
use crate::app::{Target, Workspace};
use crate::theme::Tokens;

type Key = (String, String);

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
    states: HashMap<Key, AssetState>,
}

impl Assets {
    pub(super) fn viewer_mut(&mut self, key: &Key) -> Option<&mut Viewer> {
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

/// The file's bytes from a signed asset URL, refused past `limit`.
pub(super) async fn fetch_bounded(url: String, limit: u64, too_large: &'static str) -> Result<Vec<u8>, String> {
    crate::runtime()
        .spawn(async move {
            let failed = "The file could not be loaded. Close it and try again.".to_owned();
            let mut response = reqwest::Client::new().get(url).send().await.map_err(|_| failed.clone())?;
            if !response.status().is_success() {
                return Err(failed);
            }
            if response.content_length().is_some_and(|length| length > limit) {
                return Err(too_large.to_owned());
            }
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|_| failed.clone())? {
                bytes.extend_from_slice(&chunk);
                if bytes.len() as u64 > limit {
                    return Err(too_large.to_owned());
                }
            }
            Ok(bytes)
        })
        .await
        .map_err(|error| error.to_string())?
}

impl Workspace {
    /// The resource a document reads: a draft's `draft-workspace-file`, else the thread's
    /// `media-file` (an attachment download for files without a viewer).
    fn document_asset(&self, key: &Key, attachment: bool) -> Value {
        let absolute = logic::absolute_path(&key.0, &key.1);
        match &self.target {
            Some(Target::Thread(thread_id)) => {
                let mut resource = json!({"_tag": "media-file", "threadId": thread_id, "path": absolute});
                if attachment {
                    resource["disposition"] = "attachment".into();
                }
                resource
            }
            _ => json!({"_tag": "draft-workspace-file", "cwd": key.0, "path": absolute}),
        }
    }

    fn load_asset(&mut self, key: &Key, kind: FileKind, window: &mut Window, cx: &mut Context<Self>) {
        let Some((limit, too_large)) = read_limit(kind) else { return };
        let (Some(client), Some(endpoint)) = (self.client(), self.endpoint.clone()) else { return };
        let resource = self.document_asset(key, false);
        let request_key = key.clone();
        let name = logic::file_name(&key.1).to_owned();
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
    fn save_asset_file(&mut self, key: &Key, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(client), Some(endpoint)) = (self.client(), self.endpoint.clone()) else { return };
        let resource = self.document_asset(key, true);
        let directory = std::env::home_dir().map(|home| home.join("Downloads")).unwrap_or_default();
        let picked = cx.prompt_for_new_path(&directory, Some(logic::file_name(&key.1)));
        cx.spawn_in(window, async move |_, cx| {
            let Ok(Ok(Some(path))) = picked.await else { return };
            let written = match document::mint_asset_url(client, endpoint.http_base, resource).await {
                Ok((url, _)) => match fetch_bounded(url, u64::MAX, "").await {
                    Ok(bytes) => std::fs::write(&path, bytes).map_err(|error| error.to_string()),
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
        key: &Key,
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
        let name = logic::file_name(&key.1).to_owned();
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
                            .child(name.clone()),
                    ),
            )
            .child(
                chrome::action("asset-save", IconName::Download, "Save file", false, &tokens)
                    .on_click(cx.listener(move |this, _, window, cx| this.save_asset_file(&save_key, window, cx))),
            );
        let body = self.render_asset_body(key, kind, &name, width, &tokens, window, cx);
        v_flex().flex_1().min_h_0().overflow_hidden().child(header).child(body).into_any_element()
    }

    fn render_asset_body(
        &mut self,
        key: &Key,
        kind: FileKind,
        name: &str,
        width: f32,
        tokens: &Tokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if kind == FileKind::Font {
            return self.render_document(key, kind, window, cx);
        }
        if read_limit(kind).is_none() {
            let extension = name.rsplit_once('.').map(|(_, e)| e).filter(|e| !e.is_empty()).unwrap_or("this format");
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
