//! `ProjectFaviconPickerDialog`: search the project's image files and pick one as its icon,
//! with `ProjectFaviconThumbnail` previews and an optional system file dialog.

use std::path::Path;
use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::component::dialog::DialogEntrance;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{Icon, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, FontWeight, InteractiveElement as _, IntoElement, KeyDownEvent, ObjectFit,
    ParentElement as _, PathPromptOptions, Render, ScrollStrategy, SharedString, StatefulInteractiveElement as _,
    Styled as _, StyledImage as _, Subscription, UniformListScrollHandle, Window, div, img, px, uniform_list,
};
use serde_json::json;

use super::icon::Images;
use crate::backend::Endpoint;
use crate::rpc::RpcClient;
use crate::theme;

/// `PROJECT_FILE_PICKER_RESULT_LIMIT`.
const RESULT_LIMIT: usize = 200;
const ROW_HEIGHT: f32 = 44.;

/// `PROJECT_FAVICON_EXTENSIONS`, which the server's `ProjectFaviconPath` also requires.
const FAVICON_EXTENSIONS: [&str; 9] = ["avif", "gif", "icns", "ico", "jpg", "jpeg", "png", "svg", "webp"];

pub(super) type OnPick = Rc<dyn Fn(String, &mut Window, &mut App)>;

/// `canPickExternalProjectFavicon`: on Windows the project must be a Windows path, not WSL.
pub(super) fn can_pick_external(workspace_root: &str) -> bool {
    let bytes = workspace_root.as_bytes();
    !cfg!(windows)
        || (bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && matches!(bytes[2], b'\\' | b'/'))
        || workspace_root.starts_with("\\\\")
}

/// `getLocalFileManagerName`.
fn file_manager_name() -> &'static str {
    if cfg!(windows) {
        "Explorer"
    } else if cfg!(target_os = "macos") {
        "Finder"
    } else {
        "Files"
    }
}

fn is_favicon_path(path: &str) -> bool {
    let extension = path.rsplit('.').next().unwrap_or_default().to_ascii_lowercase();
    path.contains('.') && FAVICON_EXTENSIONS.contains(&extension.as_str())
}

pub(super) struct FaviconPicker {
    cwd: String,
    project_name: SharedString,
    client: Option<RpcClient>,
    endpoint: Option<Endpoint>,
    images: Entity<Images>,
    query: Entity<InputState>,
    /// Image file paths in server rank order.
    entries: Rc<Vec<String>>,
    pending: bool,
    error: Option<SharedString>,
    /// Drops responses to queries typed over.
    generation: u64,
    highlighted: usize,
    scroll: UniformListScrollHandle,
    pick_external: bool,
    picking_external: bool,
    on_pick: OnPick,
    _subscriptions: Vec<Subscription>,
}

impl FaviconPicker {
    pub(super) fn new(
        cwd: String,
        project_name: SharedString,
        client: Option<RpcClient>,
        endpoint: Option<Endpoint>,
        images: Entity<Images>,
        pick_external: bool,
        on_pick: OnPick,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search image files…"));
        query.update(cx, |query, cx| query.focus(window, cx));
        let subscriptions = vec![
            cx.subscribe_in(&query, window, |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Change => this.search(cx),
                InputEvent::PressEnter { .. } => this.pick_highlighted(window, cx),
                _ => {}
            }),
            cx.observe(&images, |_, _, cx| cx.notify()),
        ];
        let mut picker = Self {
            cwd,
            project_name,
            client,
            endpoint,
            images,
            query,
            entries: Rc::default(),
            pending: true,
            error: None,
            generation: 0,
            highlighted: 0,
            scroll: UniformListScrollHandle::new(),
            pick_external,
            picking_external: false,
            on_pick,
            _subscriptions: subscriptions,
        };
        picker.search(cx);
        picker
    }

    /// `useProjectFilePickerQuery(..., { imageOnly: true })`; an empty query lists files too.
    fn search(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            self.error = Some("Not connected.".into());
            return;
        };
        self.generation += 1;
        let generation = self.generation;
        self.pending = true;
        self.highlighted = 0;
        let query = self.query.read(cx).value().trim().to_owned();
        let payload =
            json!({"cwd": self.cwd, "query": query, "limit": RESULT_LIMIT, "kind": "file", "imageOnly": true});
        cx.spawn(async move |this, cx| {
            let result = client.request("projects.searchEntries", payload).await;
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.pending = false;
                match result {
                    Ok(value) => {
                        this.error = None;
                        let entries = value["entries"].as_array().into_iter().flatten();
                        this.entries = Rc::new(
                            entries
                                .filter(|entry| entry["kind"] == "file")
                                .filter_map(|entry| entry["path"].as_str().map(str::to_owned))
                                .take(RESULT_LIMIT)
                                .collect(),
                        );
                    }
                    Err(error) => {
                        this.entries = Rc::default();
                        this.error = Some(error.to_string().into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn pick(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        window.close_dialog(cx);
        (self.on_pick)(path, window, cx);
    }

    fn pick_highlighted(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(path) = self.entries.get(self.highlighted).cloned() {
            self.pick(path, window, cx);
        }
    }

    fn on_key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let last = self.entries.len().saturating_sub(1);
        self.highlighted = match event.keystroke.key.as_str() {
            "down" => (self.highlighted + 1).min(last),
            "up" => self.highlighted.saturating_sub(1),
            _ => return,
        };
        self.scroll.scroll_to_item(self.highlighted, ScrollStrategy::Nearest);
        cx.stop_propagation();
        cx.notify();
    }

    /// The desktop bridge's `pickProjectFavicon`: the system file dialog, for images outside the project.
    fn pick_external(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.picking_external = true;
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose project icon".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let picked = paths.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.picking_external = false;
                cx.notify();
                let path = match picked {
                    Ok(Ok(Some(paths))) => paths.into_iter().next(),
                    Ok(Ok(None)) | Err(_) => None,
                    Ok(Err(error)) => {
                        window.push_notification(format!("Could not open image picker: {error}"), cx);
                        None
                    }
                };
                let Some(path) = path.as_deref().and_then(Path::to_str).map(str::to_owned) else { return };
                if !is_favicon_path(&path) {
                    window.push_notification("Choose an image file (PNG, SVG, ICO, JPEG, GIF, WebP).", cx);
                    return;
                }
                this.pick(path, window, cx);
            });
        })
        .detach();
        cx.notify();
    }

    fn empty_message(&self, cx: &App) -> SharedString {
        if let Some(error) = &self.error {
            return error.clone();
        }
        let searching = !self.query.read(cx).value().trim().is_empty();
        match (self.pending, searching) {
            (true, true) => "Searching project files…",
            (true, false) => "Indexing project files…",
            (false, true) => "No matching image files.",
            (false, false) => "No image files found.",
        }
        .into()
    }

    fn row(&mut self, index: usize, path: &str, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let tokens = *theme::tokens(cx);
        let name = path.rsplit('/').next().unwrap_or(path).to_owned();
        let resource = json!({"_tag": "draft-workspace-file", "cwd": self.cwd, "path": path});
        let (client, endpoint) = (self.client.clone(), self.endpoint.clone());
        let thumbnail = self.images.update(cx, |images, cx| images.get(resource, client, endpoint, cx));
        let highlighted = index == self.highlighted;
        let picked = path.to_owned();
        h_flex()
            .id(SharedString::from(format!("favicon-file-{path}")))
            .w_full()
            .h(px(ROW_HEIGHT))
            .px_2()
            .gap_3()
            .rounded(px(6.))
            .cursor_pointer()
            .when(highlighted, |el| el.bg(tokens.accent))
            .on_mouse_move(cx.listener(move |this, _, _, cx| {
                if this.highlighted != index {
                    this.highlighted = index;
                    cx.notify();
                }
            }))
            .on_click(cx.listener(move |this, _, window, cx| this.pick(picked.clone(), window, cx)))
            .child(
                div().size(px(32.)).flex_none().flex().items_center().justify_center().child(match thumbnail {
                    Some(image) => img(image)
                        .max_w(px(32.))
                        .max_h(px(32.))
                        .rounded(px(2.))
                        .object_fit(ObjectFit::Contain)
                        .into_any_element(),
                    None => Icon::new(IconName::FileImage)
                        .size(px(16.))
                        .text_color(tokens.muted_foreground)
                        .into_any_element(),
                }),
            )
            .child(
                v_flex()
                    .min_w_0()
                    .child(div().truncate().text_sm().child(name))
                    .child(div().truncate().text_xs().text_color(tokens.muted_foreground).child(path.to_owned())),
            )
    }
}

impl Render for FaviconPicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = *theme::tokens(cx);
        let entries = self.entries.clone();
        let hint = |label: &'static str, key: &'static str| {
            h_flex()
                .gap_1p5()
                .text_xs()
                .text_color(tokens.muted_foreground)
                .child(label)
                .child(div().px_1().rounded(px(4.)).border_1().border_color(tokens.border).child(key))
        };
        v_flex()
            .id("favicon-picker")
            .h(px(480.))
            .capture_key_down(cx.listener(Self::on_key))
            .child(Input::new(&self.query).appearance(false).px_1().pb_2())
            .child(div().h_px().bg(tokens.border))
            .child(if entries.is_empty() {
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_sm()
                    .text_color(tokens.muted_foreground)
                    .child(self.empty_message(cx))
                    .into_any_element()
            } else {
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .pt_2()
                    .child(
                        div()
                            .px_2()
                            .pb_1()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(tokens.muted_foreground)
                            .child(self.project_name.clone()),
                    )
                    .child(
                        uniform_list(
                            "favicon-files",
                            entries.len(),
                            cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                                range.map(|index| this.row(index, &entries[index], cx)).collect::<Vec<_>>()
                            }),
                        )
                        .track_scroll(&self.scroll)
                        .flex_1(),
                    )
                    .into_any_element()
            })
            .child(div().h_px().bg(tokens.border))
            .child(
                h_flex()
                    .pt_2()
                    .gap_4()
                    .when(self.pick_external, |el| {
                        el.child(
                            div()
                                .id("favicon-pick-external")
                                .text_xs()
                                .font_weight(FontWeight::MEDIUM)
                                .cursor_pointer()
                                .when(self.picking_external, |el| el.opacity(0.5))
                                .hover(|s| s.text_color(tokens.foreground))
                                .text_color(tokens.muted_foreground)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    if !this.picking_external {
                                        this.pick_external(window, cx);
                                    }
                                }))
                                .child(format!("Open in {}", file_manager_name())),
                        )
                    })
                    .child(div().flex_1())
                    .child(hint("Select icon", "↵"))
                    .child(hint("Close", "Esc")),
            )
    }
}

/// Opens the picker as a command-style dialog: no title, the search field on top.
pub(super) fn open_picker(window: &mut Window, cx: &mut App, picker: Entity<FaviconPicker>) {
    window.open_dialog(cx, move |dialog, _, _| {
        dialog.entrance(DialogEntrance::None).w(px(576.)).close_button(false).child(picker.clone())
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_paths_must_be_images() {
        assert!(is_favicon_path("C:\\art\\logo.SVG"));
        assert!(!is_favicon_path("C:\\art\\notes.txt"));
        assert!(!is_favicon_path("png"));
        assert!(can_pick_external("C:\\src\\app"));
        assert_eq!(can_pick_external("/home/me/app"), !cfg!(windows));
    }
}
