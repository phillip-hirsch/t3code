//! Jupyter notebooks (`NotebookPreview.tsx`, `openNotebook` in `extendedDocumentPreview.ts`):
//! saved cells and outputs only, twenty cells at a time. Code is never executed.

use std::sync::Arc;

use base64::Engine as _;
use gpui_kit::component::input::{Editor, EditorState};
use gpui_kit::component::text::{TextView, TextViewState};
use gpui_kit::component::v_flex;
use gpui_kit::{
    AnyElement, AppContext as _, Context, ElementId, Entity, Image, ImageFormat, InteractiveElement as _, IntoElement,
    ObjectFit, ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, StyledImage as _,
    Window, div, img, px,
};
use serde_json::Value;

use super::chrome;
use crate::app::Workspace;
use crate::app::attachments::format_size;
use crate::theme::Tokens;

pub(super) const MAX_BYTES: u64 = 8 * 1024 * 1024;
const CELL_TEXT_LIMIT: usize = 256_000;
const IMAGE_TEXT_LIMIT: usize = 4 * 1024 * 1024;
const PAGE: usize = 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CellKind {
    Markdown,
    Code,
    Raw,
}

#[derive(Debug, PartialEq)]
pub(super) enum Output {
    Text(String),
    Image(ImageFormat, Vec<u8>),
    /// HTML outputs render in a frame on the web; here their plain-text fallback shows.
    Html(String),
}

#[derive(Debug, PartialEq)]
pub(super) struct Cell {
    pub kind: CellKind,
    pub source: String,
    pub outputs: Vec<Output>,
}

#[derive(Debug, PartialEq)]
pub(super) struct NotebookData {
    pub language: String,
    pub cells: Vec<Cell>,
}

/// `notebookText`: a string, or the strings of an array joined.
fn notebook_text(value: &Value, limit: usize) -> Result<String, String> {
    let text = match value {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts.iter().filter_map(Value::as_str).collect(),
        _ => String::new(),
    };
    if text.encode_utf16().count() > limit {
        return Err("A notebook cell is too large to preview.".into());
    }
    Ok(text)
}

/// `openNotebook`.
pub(super) fn open_notebook(bytes: &[u8]) -> Result<NotebookData, String> {
    if bytes.len() as u64 > MAX_BYTES {
        return Err("Notebook preview limited to files under 8 MB.".into());
    }
    let unsupported = || "Preview supports version 4 notebooks with up to 500 cells.".to_owned();
    let text = std::str::from_utf8(bytes).map_err(|_| "Could not read this file.".to_owned())?;
    let notebook: Value = serde_json::from_str(text).map_err(|_| "Could not read this file.".to_owned())?;
    let cells = notebook["cells"].as_array().filter(|cells| cells.len() <= 500).ok_or_else(unsupported)?;
    if notebook["nbformat"] != 4 {
        return Err(unsupported());
    }
    let language: String = match &notebook["metadata"]["language_info"]["name"] {
        Value::Null => "text".into(),
        Value::String(name) => name.clone(),
        other => other.to_string(),
    }
    .chars()
    .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '#' | '-'))
    .take(40)
    .collect();
    let cells = cells
        .iter()
        .map(|cell| {
            let outputs = cell["outputs"].as_array().map(Vec::as_slice).unwrap_or_default();
            if outputs.len() > 100 {
                return Err("A notebook cell has too many outputs to preview.".to_owned());
            }
            let mut parsed = Vec::new();
            for output in outputs {
                let data = &output["data"];
                let image = [
                    ("image/png", ImageFormat::Png),
                    ("image/jpeg", ImageFormat::Jpeg),
                    ("image/gif", ImageFormat::Gif),
                    ("image/webp", ImageFormat::Webp),
                ]
                .into_iter()
                .find_map(|(mime, format)| {
                    let base64 = notebook_text(&data[mime], IMAGE_TEXT_LIMIT).ok()?;
                    let base64: String = base64.chars().filter(|c| !c.is_whitespace()).collect();
                    let bytes = base64::engine::general_purpose::STANDARD.decode(base64).ok()?;
                    (!bytes.is_empty()).then_some(Output::Image(format, bytes))
                });
                if let Some(image) = image {
                    parsed.push(image);
                    continue;
                }
                let html = notebook_text(&data["text/html"], CELL_TEXT_LIMIT)?;
                let plain = match &output["text"] {
                    Value::Null => match &data["text/plain"] {
                        Value::Null => &output["traceback"],
                        plain => plain,
                    },
                    text => text,
                };
                let plain = notebook_text(plain, CELL_TEXT_LIMIT)?;
                let output = if html.trim().is_empty() { Output::Text(plain) } else { Output::Html(plain) };
                if !matches!(&output, Output::Text(text) if text.trim().is_empty()) {
                    parsed.push(output);
                }
            }
            Ok(Cell {
                kind: match cell["cell_type"].as_str() {
                    Some("markdown") => CellKind::Markdown,
                    Some("code") => CellKind::Code,
                    _ => CellKind::Raw,
                },
                source: notebook_text(&cell["source"], CELL_TEXT_LIMIT)?,
                outputs: parsed,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(NotebookData { language, cells })
}

/// Re-parses after each pass: dropping a link's brackets can join text into new syntax.
const INERT_PASSES: usize = 4;

/// The renderer's own syntax (`MarkdownParseContext::parse_options`): GFM with math.
fn markdown_options() -> markdown::ParseOptions {
    let mut options = markdown::ParseOptions::gfm();
    options.constructs.math_text = true;
    options.constructs.math_flow = true;
    options
}

/// Backslash-escapes every ASCII punctuation mark, so the text parses as nothing but text.
fn escape_markdown(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_ascii_punctuation() {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// The source edits that make `node` inert, in document order.
fn inert_edits(node: &markdown::mdast::Node, edits: &mut Vec<(std::ops::Range<usize>, String)>) {
    use markdown::mdast::Node;
    let range = node.position().map(|p| p.start.offset..p.end.offset);
    let children = node.children().map(Vec::as_slice).unwrap_or_default();
    match (node, range) {
        (Node::Html(_) | Node::Definition(_), Some(range)) => edits.push((range, String::new())),
        (Node::Image(image), Some(range)) => edits.push((range, escape_markdown(&image.alt))),
        (Node::ImageReference(image), Some(range)) => edits.push((range, escape_markdown(&image.alt))),
        (Node::Link(_) | Node::LinkReference(_), Some(range)) => {
            let inner = children
                .first()
                .zip(children.last())
                .and_then(|(first, last)| Some(first.position()?.start.offset..last.position()?.end.offset));
            match inner.filter(|_| !children.iter().all(|child| matches!(child, Node::Text(_)))) {
                // Formatted labels keep their markup; only the brackets and destination go.
                Some(inner) => {
                    edits.push((range.start..inner.start, String::new()));
                    for child in children {
                        inert_edits(child, edits);
                    }
                    edits.push((inner.end..range.end, String::new()));
                }
                // Plain labels, autolinks and bare URLs become escaped text, which no longer links.
                None => edits.push((range, escape_markdown(&node.to_string()))),
            }
        }
        _ => children.iter().for_each(|child| inert_edits(child, edits)),
    }
}

/// `inertNotebookMarkdown`: HTML and link definitions go, images become their alt text and
/// links their label, so a saved notebook loads and links no URLs. Text that still parses
/// as any of them after a few passes shows escaped.
pub(super) fn inert_markdown(source: &str) -> String {
    let options = markdown_options();
    let mut text = source.to_owned();
    for _ in 0..INERT_PASSES {
        let Ok(root) = markdown::to_mdast(&text, &options) else { break };
        let mut edits = Vec::new();
        inert_edits(&root, &mut edits);
        if edits.is_empty() {
            return text;
        }
        let mut out = String::with_capacity(text.len());
        let mut at = 0;
        for (range, replacement) in edits {
            out.push_str(&text[at..range.start]);
            out.push_str(&replacement);
            at = range.end;
        }
        out.push_str(&text[at..]);
        text = out;
    }
    escape_markdown(source)
}

/// `{ python: "py", javascript: "js", typescript: "ts", julia: "jl" }`, as the highlighter's language.
fn cell_language(language: &str) -> &'static str {
    match language {
        "python" => "python",
        "javascript" => "javascript",
        "typescript" => "typescript",
        "rust" => "rust",
        "go" => "go",
        "bash" => "bash",
        _ => "text",
    }
}

#[derive(Default)]
struct CellViews {
    markdown: Option<Entity<TextViewState>>,
    editor: Option<Entity<EditorState>>,
    images: Vec<Option<Arc<Image>>>,
}

pub(super) struct Notebook {
    data: NotebookData,
    size: u64,
    visible: usize,
    views: Vec<CellViews>,
}

impl Notebook {
    pub(super) fn new(data: NotebookData, size: u64) -> Self {
        Self { data, size, visible: PAGE, views: Vec::new() }
    }
}

impl Workspace {
    pub(super) fn render_notebook(
        &mut self,
        key: &super::FileKey,
        tokens: &Tokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(super::asset::Viewer::Notebook(notebook)) = self.files.assets.viewer_mut(key) else {
            return div().into_any_element();
        };
        let shown = notebook.visible.min(notebook.data.cells.len());
        // Views for the cells shown, made once.
        while notebook.views.len() < shown {
            let cell = &notebook.data.cells[notebook.views.len()];
            let mut views = CellViews::default();
            match cell.kind {
                CellKind::Markdown => {
                    let source = inert_markdown(&cell.source);
                    views.markdown = Some(cx.new(|cx| TextViewState::markdown(&source, cx)));
                }
                _ if !cell.source.trim().is_empty() => {
                    let language =
                        if cell.kind == CellKind::Code { cell_language(&notebook.data.language) } else { "text" };
                    let source = cell.source.clone();
                    views.editor =
                        Some(cx.new(|cx| EditorState::new(window, cx).language(language).default_value(source)));
                }
                _ => {}
            }
            views.images = cell
                .outputs
                .iter()
                .map(|output| match output {
                    Output::Image(format, bytes) => Some(Arc::new(Image::from_bytes(*format, bytes.clone()))),
                    _ => None,
                })
                .collect();
            notebook.views.push(views);
        }
        let border = tokens.border.opacity(0.6);
        let muted = tokens.muted_foreground;
        let language = notebook.data.language.clone();
        let mut cells = Vec::with_capacity(shown);
        for (index, (cell, views)) in notebook.data.cells.iter().zip(&notebook.views).take(shown).enumerate() {
            let label = match cell.kind {
                CellKind::Code => language.clone(),
                CellKind::Markdown => "markdown".into(),
                CellKind::Raw => "raw".into(),
            };
            let mut section =
                v_flex().min_w_0().overflow_hidden().rounded(px(6.)).border_1().border_color(border).child(
                    div()
                        .px(px(12.))
                        .py(px(6.))
                        .border_b_1()
                        .border_color(border)
                        .bg(tokens.muted.opacity(0.3))
                        .text_xs()
                        .text_color(muted)
                        .child(format!("{label} · {}", index + 1)),
                );
            if let Some(markdown) = &views.markdown {
                section = section.child(div().px(px(16.)).py(px(12.)).child(TextView::new(markdown)));
            }
            if let Some(editor) = &views.editor {
                let lines = cell.source.lines().count().max(1);
                let height = (lines as f32 * 20. + 16.).min(360.);
                section = section.child(
                    div().h(px(height)).child(
                        Editor::new(editor)
                            .appearance(false)
                            .bordered(false)
                            .readonly(true)
                            .h_full()
                            .font_family(tokens.fonts.mono),
                    ),
                );
            }
            for (output, image) in cell.outputs.iter().zip(&views.images) {
                let body: AnyElement = match (output, image) {
                    (_, Some(image)) => div()
                        .p(px(16.))
                        .child(img(image.clone()).max_w_full().max_h(px(384.)).object_fit(ObjectFit::Contain))
                        .into_any_element(),
                    (Output::Text(text) | Output::Html(text), _) => div()
                        .p(px(16.))
                        .text_xs()
                        .font_family(tokens.fonts.mono)
                        .child(SharedString::from(text.clone()))
                        .into_any_element(),
                    _ => div().into_any_element(),
                };
                section = section.child(div().border_t_1().border_color(border).child(body));
            }
            cells.push(section);
        }
        let remaining = notebook.data.cells.len() - shown;
        let empty = notebook.data.cells.is_empty();
        let summary = format!(
            "{} cells · {} · {}",
            super::logic::grouped(notebook.data.cells.len()),
            language,
            if notebook.size == 0 { "0 bytes".to_owned() } else { format_size(notebook.size) }
        );
        let more_key = key.clone();
        v_flex()
            .flex_1()
            .min_h_0()
            .child(
                div()
                    .flex_none()
                    .px(px(12.))
                    .py(px(8.))
                    .border_b_1()
                    .border_color(border)
                    .text_xs()
                    .text_color(muted)
                    .child(summary),
            )
            .child(chrome::notice("Saved outputs only. Code is never executed.", tokens))
            .child(
                div()
                    .id(ElementId::Name(format!("notebook:{}", key.path).into()))
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(
                        v_flex()
                            .w_full()
                            .max_w(px(896.))
                            .mx_auto()
                            .gap(px(20.))
                            .px(px(20.))
                            .py(px(24.))
                            .children(cells)
                            .children(empty.then(|| div().text_sm().text_color(muted).child("This notebook is empty.")))
                            .children((remaining > 0).then(|| {
                                div()
                                    .id("notebook-more")
                                    .flex_none()
                                    .self_start()
                                    .px(px(10.))
                                    .py(px(4.))
                                    .rounded(px(6.))
                                    .border_1()
                                    .border_color(tokens.input)
                                    .text_xs()
                                    .cursor_pointer()
                                    .hover(|s| s.bg(tokens.accent))
                                    .child(format!("Show more cells ({remaining} remaining)"))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if let Some(super::asset::Viewer::Notebook(notebook)) =
                                            this.files.assets.viewer_mut(&more_key)
                                        {
                                            notebook.visible += PAGE;
                                        }
                                        cx.notify();
                                    }))
                            })),
                    ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notebooks_keep_saved_cells_and_outputs() {
        let notebook = serde_json::json!({
            "nbformat": 4,
            "metadata": {"language_info": {"name": "python 3!"}},
            "cells": [
                {"cell_type": "markdown", "source": ["# Title\n", "text"]},
                {"cell_type": "code", "source": "print(1)", "outputs": [
                    {"output_type": "stream", "text": ["1\n"]},
                    {"output_type": "display_data", "data": {"text/html": "<b>x</b>", "text/plain": "x"}},
                    {"output_type": "display_data", "data": {"image/png": "iVBORw0K\nGgo="}},
                    {"output_type": "stream", "text": "  "}
                ]},
                {"cell_type": "raw", "source": "r"}
            ]
        });
        let data = open_notebook(notebook.to_string().as_bytes()).unwrap();
        assert_eq!(data.language, "python3");
        assert_eq!(data.cells[0].source, "# Title\ntext");
        assert_eq!(data.cells[1].kind, CellKind::Code);
        assert_eq!(data.cells[1].outputs.len(), 3, "blank outputs are dropped");
        assert_eq!(data.cells[1].outputs[0], Output::Text("1\n".into()));
        assert_eq!(data.cells[1].outputs[1], Output::Html("x".into()));
        assert!(matches!(data.cells[1].outputs[2], Output::Image(ImageFormat::Png, _)));
        assert_eq!(data.cells[2].kind, CellKind::Raw);
        let v3 = serde_json::json!({"nbformat": 3, "cells": []});
        assert_eq!(
            open_notebook(v3.to_string().as_bytes()).err().unwrap(),
            "Preview supports version 4 notebooks with up to 500 cells."
        );
    }

    /// Every node that loads or links a URL, in `text` as the renderer parses it.
    fn loaders(text: &str) -> Vec<String> {
        fn walk(node: &markdown::mdast::Node, found: &mut Vec<String>) {
            use markdown::mdast::Node;
            if matches!(
                node,
                Node::Html(_)
                    | Node::Definition(_)
                    | Node::Image(_)
                    | Node::ImageReference(_)
                    | Node::Link(_)
                    | Node::LinkReference(_)
            ) {
                found.push(format!("{node:?}"));
            }
            node.children().into_iter().flatten().for_each(|child| walk(child, found));
        }
        let mut found = Vec::new();
        walk(&markdown::to_mdast(text, &markdown_options()).expect("markdown parses"), &mut found);
        found
    }

    #[test]
    fn notebook_markdown_loads_no_urls() {
        assert_eq!(inert_markdown("a ![plot](http://x/p.png) [docs](https://d) b"), "a plot docs b");
        assert_eq!(inert_markdown("[x] task\n[y](z)"), "[x] task\ny");
        assert_eq!(inert_markdown("[x] done [y](z)"), "[x] done y");
        assert_eq!(inert_markdown("é ![α](u) **[b *c*](u)**"), "é α **b *c***", "offsets are bytes");
        assert_eq!(inert_markdown("x\n\n[r]: http://x/a.png\n\n![alt][r] [see][r]"), "x\n\n\n\nalt see");
        assert_eq!(inert_markdown("a <img src=\"http://x/a.png\"> <b>b</b>"), "a  b");
        for source in [
            "<http://x/a> and https://x/b and www.x.com and me@x.com",
            "![a [b](http://x)](http://x/i.png)",
            "[<](x)img src=http://x/i.png>",
            "| a | ![t](http://x/i.png) |\n|---|---|\n| <img src=x> | [l](u) |",
            "> [r]: http://x/a.png\n> ![r]",
            "<div>\n<img src=\"http://x/a.png\">\n</div>",
        ] {
            let inert = inert_markdown(source);
            assert_eq!(loaders(&inert), Vec::<String>::new(), "{source:?} became {inert:?}");
        }
    }
}
