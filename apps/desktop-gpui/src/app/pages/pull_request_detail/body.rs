//! `PullRequestMarkdown`: a change request body drawn in segments (`splitPullRequestBody`), so a
//! dropped video plays and a before/after media table keeps its cells side by side. Media the host
//! serves only with a credential (`github-media`) loads through the pull request's environment,
//! with its checkout and repository, falling back to the host's URL as the web does.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, FontWeight, Image, IntoElement, ParentElement as _, SharedString, Styled as _, Window, div, px,
};
use serde_json::json;

use super::super::super::Workspace;
use super::super::super::markdown_images::{self, Source, classify, fetch_image, image_destinations};
use super::super::super::markdown_media::{Playable, media_kind};
use super::pr_markdown_view;
use crate::pull_request_body::{self as body_logic, Media, Segment};
use crate::theme::Tokens;

/// A body as the panel split it, kept while its text stays the same.
pub(super) struct Body {
    text: String,
    segments: Vec<Segment>,
    /// The markdown of every segment in order: the images a click pages through.
    gallery: SharedString,
}

/// The views a body's segments keep, under `prefix`: `prefix/index`, and `.row.column/index`
/// inside a table's cells.
fn segment_keys(segments: &[Segment], prefix: &str, keys: &mut HashSet<String>) {
    for (index, segment) in segments.iter().enumerate() {
        let key = format!("{prefix}/{index}");
        if let Segment::Table { rows } = segment {
            for (row, cells) in rows.iter().enumerate() {
                for (column, cell) in cells.iter().enumerate() {
                    segment_keys(&cell.segments, &format!("{key}.{row}.{column}"), keys);
                }
            }
        }
        keys.insert(key);
    }
}

/// How an image attachment draws: as the image markdown would (`![](<url>)`).
fn image_markdown(url: &str) -> String {
    format!("![](<{url}>)")
}

fn gallery_markdown(segments: &[Segment], out: &mut String) {
    for segment in segments {
        match segment {
            Segment::Markdown { text, .. } => {
                let _ = write!(out, "{text}\n\n");
            }
            Segment::Attachment { url, media: Media::Image } => {
                let _ = write!(out, "{}\n\n", image_markdown(url));
            }
            Segment::Attachment { media: Media::Video, .. } => {}
            Segment::Table { rows } => {
                rows.iter().flatten().for_each(|cell| gallery_markdown(&cell.segments, out));
            }
        }
    }
}

impl Workspace {
    /// `PullRequestMarkdown` for `text`, its views kept under `key`. With `tasks`, a ticked
    /// checkbox saves the description.
    pub(super) fn pr_body(
        &mut self,
        key: &str,
        text: &str,
        tasks: bool,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let body = self.pr_body_parts(key, text, cx)?;
        let mut children = Vec::new();
        for (index, segment) in body.segments.iter().enumerate() {
            children.extend(self.render_pr_segment(&format!("{key}/{index}"), segment, tasks, &body.gallery, t, cx));
        }
        Some(v_flex().w_full().min_w_0().gap(px(12.)).children(children).into_any_element())
    }

    /// `key`'s body, split again only when its text changed; the views of segments it no longer
    /// has are dropped.
    fn pr_body_parts(&mut self, key: &str, text: &str, cx: &mut Context<Self>) -> Option<Rc<Body>> {
        let panel = self.pull_requests.panel.as_ref()?;
        if let Some(body) = panel.bodies.get(key).filter(|body| body.text == text) {
            return Some(body.clone());
        }
        let segments = body_logic::split(&self.pr_markdown_source(text));
        let mut gallery = String::new();
        gallery_markdown(&segments, &mut gallery);
        self.load_pr_images(&gallery, cx);
        let mut keys = HashSet::new();
        segment_keys(&segments, key, &mut keys);
        let panel = self.pull_requests.panel.as_mut()?;
        let prefix = format!("{key}/");
        panel.markdown.retain(|kept, _| !kept.starts_with(&prefix) || keys.contains(kept));
        let body = Rc::new(Body { text: text.to_owned(), segments, gallery: gallery.into() });
        panel.bodies.insert(key.to_owned(), body.clone());
        Some(body)
    }

    fn render_pr_segment(
        &mut self,
        key: &str,
        segment: &Segment,
        tasks: bool,
        gallery: &SharedString,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        match segment {
            Segment::Markdown { text, source_offset } => {
                self.render_pr_prose(key, text, tasks.then_some(*source_offset), gallery, t, cx)
            }
            Segment::Attachment { url, media: Media::Image } => {
                self.render_pr_prose(key, &image_markdown(url), None, gallery, t, cx)
            }
            Segment::Attachment { url, media: Media::Video } => Some(self.render_pr_body_video(key, url, t, cx)),
            // Each row lays out on its own, its cells in equal columns (`repeat(n, minmax(0, 1fr))`).
            Segment::Table { rows } => {
                let mut table = Vec::with_capacity(rows.len());
                for (row, cells) in rows.iter().enumerate() {
                    let mut columns = Vec::with_capacity(cells.len());
                    for (column, cell) in cells.iter().enumerate() {
                        let mut children = Vec::new();
                        for (index, inner) in cell.segments.iter().enumerate() {
                            let inner_key = format!("{key}.{row}.{column}/{index}");
                            children.extend(self.render_pr_segment(&inner_key, inner, tasks, gallery, t, cx));
                        }
                        columns.push(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap(px(12.))
                                .when(cell.header, |el| el.font_weight(FontWeight::MEDIUM))
                                .children(children),
                        );
                    }
                    table.push(h_flex().w_full().items_start().gap(px(12.)).children(columns));
                }
                Some(v_flex().w_full().gap(px(12.)).children(table).into_any_element())
            }
        }
    }

    /// One prose run. `task_offset` is where it starts in the description, for its checkboxes.
    fn render_pr_prose(
        &mut self,
        key: &str,
        text: &str,
        task_offset: Option<usize>,
        gallery: &SharedString,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let state = self.pr_markdown(key, text, cx)?;
        let images = self.pull_requests.panel.as_ref()?.images.clone();
        let view = cx.entity();
        let (weak, gallery) = (view.downgrade(), gallery.clone());
        let markdown = pr_markdown_view(&state, t, view.clone(), self.pr_code_wraps(&state))
            .image_source(images.resolver())
            .on_image_click(move |url, window, cx| {
                let _ = weak.update(cx, |this, cx| this.open_pr_image(&gallery, url.as_ref(), window, cx));
            });
        let markdown = match task_offset {
            Some(offset) => markdown.on_task_toggle(move |item_start, checked, window, cx| {
                view.update(cx, |this, cx| this.toggle_pr_task(offset + item_start, checked, window, cx));
            }),
            None => markdown,
        };
        Some(markdown.into_any_element())
    }

    /// `PullRequestHostVideo` for host media, else `MediaVideoPlayer` on the URL as written.
    fn render_pr_body_video(&mut self, key: &str, url: &str, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let Some(panel) = self.pull_requests.panel.as_ref() else { return div().into_any_element() };
        let Some(environment) = self.project_environment(&panel.reference.project_id) else {
            return div().into_any_element();
        };
        let target = panel.detail.as_ref().filter(|detail| !detail.workspace_root.is_empty());
        let playable = match (body_logic::host_media(url), target) {
            (Some(media), Some(detail)) => Playable::Asset {
                resource: json!({
                    "_tag": "github-media",
                    "cwd": detail.workspace_root,
                    "url": media.fetch_url,
                    "repository": detail.repository,
                }),
                fallback: media.fetch_url,
                fragment: body_logic::source_fragment(url).to_owned(),
            },
            _ => Playable::Url(url.to_owned()),
        };
        let message = format!("pr:{}:{key}", panel.reference.key()).into();
        self.render_pull_request_video(environment, message, playable, url, t, cx)
    }

    /// The image viewer over the body's images, at the one clicked.
    fn open_pr_image(&mut self, gallery: &str, url: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(images) = self.pull_requests.panel.as_ref().map(|panel| panel.images.clone()) else { return };
        if let Some((items, index)) = markdown_images::gallery(&images, None, gallery, url) {
            self.open_image_viewer(items, index, window, cx);
        }
    }

    /// Starts loading the images `markdown` shows that the panel has not seen: host media through
    /// `github-media` on the pull request's environment (`ChatMarkdownAssetImage`), and the host's
    /// URL when that fails; any other web image as written.
    fn load_pr_images(&mut self, markdown: &str, cx: &mut Context<Self>) {
        let Some(panel) = self.pull_requests.panel.as_ref() else { return };
        let images = panel.images.clone();
        let target = panel
            .detail
            .as_ref()
            .filter(|detail| !detail.workspace_root.is_empty())
            .map(|detail| (detail.workspace_root.clone(), detail.repository.clone()));
        let client = self.project_client(&panel.reference.project_id);
        let base = self.project_endpoint(&panel.reference.project_id).map(|endpoint| endpoint.http_base.clone());
        let connection = client.zip(base);
        for destination in image_destinations(markdown) {
            if media_kind(&destination).is_some() || !images.claim(&destination) {
                continue;
            }
            let source = classify(&destination, None);
            let host = match &source {
                Source::Direct(url) => body_logic::host_media(url).zip(target.clone()).zip(connection.clone()),
                _ => None,
            };
            let images = images.clone();
            cx.spawn(async move |this, cx| {
                let image = match (source, host) {
                    (Source::Data(format, bytes), _) => Some(Arc::new(Image::from_bytes(format, bytes))),
                    (_, Some(((media, (cwd, repository)), (client, base)))) => {
                        let resource =
                            json!({"_tag": "github-media", "cwd": cwd, "url": media.fetch_url, "repository": repository});
                        let signed = match client.request("assets.createUrl", json!({"resource": resource})).await {
                            Ok(signed) => signed["relativeUrl"].as_str().map(|relative| format!("{base}{relative}")),
                            Err(error) => {
                                log::debug!("assets.createUrl for a pull request image: {error}");
                                None
                            }
                        };
                        match signed {
                            Some(url) => match fetch_image(url).await {
                                Some(image) => Some(image),
                                None => fetch_image(media.fetch_url).await,
                            },
                            None => fetch_image(media.fetch_url).await,
                        }
                    }
                    (Source::Direct(url), None) => fetch_image(url).await,
                    _ => None,
                };
                let Some(image) = image else { return };
                images.store(destination, image);
                let _ = this.update(cx, |this, cx| {
                    if this.pull_requests.panel.as_ref().is_some_and(|panel| panel.images.same(&images)) {
                        this.remeasure_pr_panel(cx);
                    }
                });
            })
            .detach();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bodys_views_and_gallery_follow_its_segments() {
        let video = "https://example.com/a.mp4";
        let body = format!(
            "Intro ![one](a.png)\n\nhttps://example.com/shot.png\n\n<table>\n<tr><td>\n\n![two](b.png)\n\n</td><td>\n\n{video}\n\n</td></tr>\n</table>"
        );
        let segments = body_logic::split(&body);
        let mut keys = HashSet::new();
        segment_keys(&segments, "c:1", &mut keys);
        let mut keys: Vec<_> = keys.into_iter().collect();
        keys.sort();
        assert_eq!(keys, ["c:1/0", "c:1/1", "c:1/2", "c:1/2.0.0/0", "c:1/2.0.1/0"]);
        let mut gallery = String::new();
        gallery_markdown(&segments, &mut gallery);
        let images: Vec<String> = image_destinations(&gallery);
        assert_eq!(images, ["a.png", "https://example.com/shot.png", "b.png"], "videos are not images");
    }
}
