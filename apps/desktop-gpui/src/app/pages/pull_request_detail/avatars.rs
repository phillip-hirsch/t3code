//! `PullRequestActorAvatar`: people's pictures from their host, read once per address and kept
//! for the session. Until one arrives, or if it cannot be read, the login's initial stands in.

use std::sync::{Arc, Mutex, OnceLock};

use gpui_kit::{Image, ImageFormat, img};

use super::*;

/// Pictures by address; `None` marks one being read or that could not be.
fn cache() -> &'static Mutex<HashMap<String, Option<Arc<Image>>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<Arc<Image>>>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// The picture for `actor`, once read.
pub(super) fn avatar_image(actor: Option<&Actor>) -> Option<Arc<Image>> {
    let url = actor?.avatar_url.as_deref()?;
    cache().lock().ok()?.get(url).cloned().flatten()
}

/// The round picture, or `fallback` while there is none.
pub(super) fn avatar_or(actor: Option<&Actor>, size: f32, fallback: gpui_kit::Div) -> gpui_kit::Div {
    match avatar_image(actor) {
        Some(image) => div()
            .size(px(size))
            .flex_none()
            .rounded_full()
            .overflow_hidden()
            .child(img(image).size(px(size)).rounded_full()),
        None => fallback,
    }
}

async fn read(url: String) -> Option<Arc<Image>> {
    let bytes = crate::runtime()
        .spawn(async move {
            let response = reqwest::Client::new().get(url).timeout(Duration::from_secs(20)).send().await.ok()?;
            let response = response.error_for_status().ok()?;
            let mime = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .map(|value| value.split(';').next().unwrap_or_default().trim().to_owned());
            Some((mime, response.bytes().await.ok()?.to_vec()))
        })
        .await
        .ok()??;
    let (mime, bytes) = bytes;
    let format = mime.as_deref().and_then(ImageFormat::from_mime_type).unwrap_or(ImageFormat::Png);
    Some(Arc::new(Image::from_bytes(format, bytes)))
}

impl Workspace {
    /// Reads the pictures of everyone the shown pull request names that are not read yet.
    pub(super) fn read_pr_avatars(&mut self, cx: &mut Context<Self>) {
        let Some(panel) = self.pull_requests.panel.as_ref() else { return };
        let mut actors: Vec<&Actor> = Vec::new();
        if let Some(detail) = &panel.detail {
            actors.extend(detail.author.iter());
            actors.extend(detail.reviewers.iter());
        }
        if let Some(activity) = &panel.activity {
            actors.extend(activity.comments.iter().filter_map(|c| c.author.as_ref()));
            actors.extend(
                activity.review_threads.iter().flat_map(|t| t.comments.iter().filter_map(|c| c.author.as_ref())),
            );
            actors.extend(activity.commits.iter().flat_map(|c| c.authors.iter()));
            actors.extend(activity.reviewers.iter().flatten());
        }
        let wanted: Vec<String> = {
            let Ok(mut held) = cache().lock() else { return };
            let mut wanted = Vec::new();
            for url in actors.iter().filter_map(|a| a.avatar_url.as_deref()) {
                if !held.contains_key(url) {
                    held.insert(url.to_owned(), None);
                    wanted.push(url.to_owned());
                }
            }
            wanted
        };
        for url in wanted {
            cx.spawn(async move |this, cx| {
                let image = read(url.clone()).await;
                if let Some(image) = image {
                    if let Ok(mut held) = cache().lock() {
                        held.insert(url, Some(image));
                    }
                    let _ = this.update(cx, |_, cx| cx.notify());
                }
            })
            .detach();
        }
    }
}
