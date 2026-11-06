//! Office Open XML files (`OpenXmlOfficeViewer.tsx` and `office/*`). The web lays the pages out
//! with a WASM engine; here a document shows its text and tables, a workbook its sheets' values
//! and a deck its slides' text, read from the package's XML parts.

use std::collections::HashMap;
use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, ElementId, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, ScrollHandle,
    SharedString, StatefulInteractiveElement as _, Styled as _, div, px, rgb,
};
use roxmltree::{Document, Node};

use super::archive::read_zip;
use super::chrome;
use super::logic::Office;
use super::table::Table;
use crate::app::Workspace;
use crate::theme::Tokens;

/// `OFFICE_PREVIEW_MAX_BYTES`, `OFFICE_PREVIEW_MAX_ROWS`, `OFFICE_PREVIEW_MAX_COLUMNS`.
pub(super) const MAX_BYTES: u64 = 50 * 1024 * 1024;
const MAX_ROWS: usize = 250_000;
const MAX_COLUMNS: usize = 1024;

#[derive(Debug, PartialEq)]
pub(super) enum Block {
    /// A paragraph and its heading level (0 for body text, 1 for a title).
    Paragraph(String, u8),
    Table(Vec<Vec<String>>),
}

#[derive(Debug, PartialEq)]
pub(super) struct Slide {
    pub paragraphs: Vec<String>,
    pub notes: String,
    pub hidden: bool,
}

#[derive(Debug, PartialEq)]
pub(super) enum OfficeData {
    Word(Vec<Block>),
    Sheets(Vec<(String, Vec<Vec<String>>)>),
    Slides(Vec<Slide>),
}

struct Package(HashMap<String, Vec<u8>>);

impl Package {
    fn open(bytes: &[u8]) -> Result<Self, String> {
        let parts = read_zip(bytes, MAX_BYTES)?;
        Ok(Self(parts.into_iter().filter(|p| !p.directory).map(|p| (p.name, p.bytes)).collect()))
    }

    fn text(&self, name: &str) -> Option<&str> {
        std::str::from_utf8(self.0.get(name.trim_start_matches('/'))?).ok()
    }

    /// A part's relationship targets by id, resolved against the part's folder.
    fn relationships(&self, part: &str) -> HashMap<String, String> {
        let (folder, file) = part.rsplit_once('/').unwrap_or(("", part));
        let rels = format!("{}{}_rels/{file}.rels", folder, if folder.is_empty() { "" } else { "/" });
        let Some(xml) = self.text(&rels) else { return HashMap::new() };
        let Ok(doc) = Document::parse(xml) else { return HashMap::new() };
        doc.descendants()
            .filter(|n| n.tag_name().name() == "Relationship")
            .filter_map(|n| Some((n.attribute("Id")?.to_owned(), resolve(folder, n.attribute("Target")?))))
            .collect()
    }
}

/// `target` relative to `folder`, with `..` segments applied.
fn resolve(folder: &str, target: &str) -> String {
    if let Some(absolute) = target.strip_prefix('/') {
        return absolute.to_owned();
    }
    let mut parts: Vec<&str> = folder.split('/').filter(|s| !s.is_empty()).collect();
    for segment in target.split('/') {
        match segment {
            ".." => {
                parts.pop();
            }
            "." | "" => {}
            other => parts.push(other),
        }
    }
    parts.join("/")
}

fn named<'a>(node: Node<'a, 'a>, name: &str) -> impl Iterator<Item = Node<'a, 'a>> {
    let name = name.to_owned();
    node.children().filter(move |n| n.tag_name().name() == name)
}

/// An attribute by local name, whatever its namespace.
fn attr<'a>(node: Node<'a, '_>, name: &str) -> Option<&'a str> {
    node.attributes().find(|a| a.name() == name).map(|a| a.value())
}

/// A part's relationship id (`r:id`), not a plain `id` beside it as on `p:sldId`.
fn rel_id<'a>(node: Node<'a, '_>) -> Option<&'a str> {
    node.attributes().find(|a| a.name() == "id" && a.namespace().is_some()).map(|a| a.value())
}

/// A Word paragraph's text: runs' text, tabs and breaks.
fn word_text(paragraph: Node) -> String {
    let mut text = String::new();
    for node in paragraph.descendants() {
        match node.tag_name().name() {
            "t" => text.push_str(node.text().unwrap_or_default()),
            "tab" if node.parent().is_some_and(|p| p.tag_name().name() == "r") => text.push('\t'),
            "br" | "cr" => text.push('\n'),
            _ => {}
        }
    }
    text
}

fn heading_level(paragraph: Node) -> u8 {
    let style = paragraph
        .children()
        .find(|n| n.tag_name().name() == "pPr")
        .and_then(|ppr| named(ppr, "pStyle").next())
        .and_then(|s| attr(s, "val"))
        .unwrap_or_default()
        .to_lowercase();
    if style == "title" {
        return 1;
    }
    style.strip_prefix("heading").and_then(|n| n.parse::<u8>().ok()).map_or(0, |n| (n + 1).min(7))
}

fn open_word(package: &Package) -> Result<OfficeData, String> {
    let xml = package.text("word/document.xml").ok_or("This document has no readable body.")?;
    let doc = Document::parse(xml).map_err(|_| "Could not render this Office document.".to_owned())?;
    let body =
        doc.descendants().find(|n| n.tag_name().name() == "body").ok_or("This document has no readable body.")?;
    let mut blocks = Vec::new();
    for node in body.children() {
        match node.tag_name().name() {
            "p" => blocks.push(Block::Paragraph(word_text(node), heading_level(node))),
            "tbl" => {
                let rows = named(node, "tr")
                    .map(|row| {
                        named(row, "tc")
                            .map(|cell| named(cell, "p").map(word_text).collect::<Vec<_>>().join("\n"))
                            .collect()
                    })
                    .collect();
                blocks.push(Block::Table(rows));
            }
            _ => {}
        }
    }
    Ok(OfficeData::Word(blocks))
}

/// A cell reference's zero-based column (`B3` is 1).
fn column_of(reference: &str) -> Option<usize> {
    let letters: String = reference.chars().take_while(char::is_ascii_alphabetic).collect();
    if letters.is_empty() {
        return None;
    }
    Some(letters.to_ascii_uppercase().bytes().fold(0usize, |n, b| n * 26 + (b - b'A' + 1) as usize) - 1)
}

fn row_of(reference: &str) -> Option<usize> {
    reference.trim_start_matches(|c: char| c.is_ascii_alphabetic()).parse::<usize>().ok()?.checked_sub(1)
}

fn open_workbook(package: &Package) -> Result<OfficeData, String> {
    let shared: Vec<String> = package
        .text("xl/sharedStrings.xml")
        .and_then(|xml| Document::parse(xml).ok())
        .map(|doc| {
            doc.root_element()
                .children()
                .filter(|n| n.tag_name().name() == "si")
                .map(|si| si.descendants().filter(|n| n.tag_name().name() == "t").filter_map(|t| t.text()).collect())
                .collect()
        })
        .unwrap_or_default();
    let xml = package.text("xl/workbook.xml").ok_or("This workbook has no sheets.")?;
    let doc = Document::parse(xml).map_err(|_| "Could not render this Office document.".to_owned())?;
    let targets = package.relationships("xl/workbook.xml");
    let mut sheets = Vec::new();
    for sheet in doc.descendants().filter(|n| n.tag_name().name() == "sheet") {
        let name = attr(sheet, "name").unwrap_or("Sheet").to_owned();
        let Some(target) = rel_id(sheet).and_then(|id| targets.get(id)) else { continue };
        let Some(sheet_xml) = package.text(target) else { continue };
        let Ok(sheet_doc) = Document::parse(sheet_xml) else { continue };
        let mut rows: Vec<Vec<String>> = Vec::new();
        for (index, row) in sheet_doc.descendants().filter(|n| n.tag_name().name() == "row").enumerate() {
            let row_index = attr(row, "r").and_then(|r| r.parse::<usize>().ok()).map_or(index, |r| r.saturating_sub(1));
            if row_index >= MAX_ROWS {
                break;
            }
            for (position, cell) in named(row, "c").enumerate() {
                let reference = attr(cell, "r").unwrap_or_default();
                let column = column_of(reference).unwrap_or(position);
                let row_index = row_of(reference).unwrap_or(row_index);
                if column >= MAX_COLUMNS || row_index >= MAX_ROWS {
                    continue;
                }
                let value = named(cell, "v").next().and_then(|v| v.text()).unwrap_or_default();
                let text = match attr(cell, "t") {
                    Some("s") => value.parse::<usize>().ok().and_then(|i| shared.get(i).cloned()).unwrap_or_default(),
                    Some("inlineStr") => {
                        cell.descendants().filter(|n| n.tag_name().name() == "t").filter_map(|t| t.text()).collect()
                    }
                    Some("b") => if value == "1" { "TRUE" } else { "FALSE" }.to_owned(),
                    _ => value.to_owned(),
                };
                if text.is_empty() {
                    continue;
                }
                if rows.len() <= row_index {
                    rows.resize_with(row_index + 1, Vec::new);
                }
                let row = &mut rows[row_index];
                if row.len() <= column {
                    row.resize(column + 1, String::new());
                }
                row[column] = text;
            }
        }
        sheets.push((name, rows));
    }
    if sheets.is_empty() {
        return Err("This workbook has no sheets.".into());
    }
    Ok(OfficeData::Sheets(sheets))
}

/// A slide part's paragraphs, each the text of its runs.
fn slide_paragraphs(doc: &Document) -> Vec<String> {
    doc.descendants()
        .filter(|n| n.tag_name().name() == "p" && n.tag_name().namespace().is_some_and(|ns| ns.contains("drawingml")))
        .map(|p| p.descendants().filter(|n| n.tag_name().name() == "t").filter_map(|t| t.text()).collect::<String>())
        .filter(|text| !text.trim().is_empty())
        .collect()
}

fn open_deck(package: &Package) -> Result<OfficeData, String> {
    let xml = package.text("ppt/presentation.xml").ok_or("This presentation has no slides.")?;
    let doc = Document::parse(xml).map_err(|_| "Could not render this Office document.".to_owned())?;
    let targets = package.relationships("ppt/presentation.xml");
    let mut slides = Vec::new();
    for id in doc.descendants().filter(|n| n.tag_name().name() == "sldId") {
        let Some(target) = rel_id(id).and_then(|id| targets.get(id)) else { continue };
        let Some(slide_xml) = package.text(target) else { continue };
        let Ok(slide) = Document::parse(slide_xml) else { continue };
        let notes = package
            .relationships(target)
            .values()
            .find(|t| t.contains("notesSlide"))
            .and_then(|t| package.text(t))
            .and_then(|xml| Document::parse(xml).ok().map(|doc| slide_paragraphs(&doc).join("\n")))
            .unwrap_or_default();
        slides.push(Slide {
            paragraphs: slide_paragraphs(&slide),
            notes,
            hidden: slide.root_element().attribute("show") == Some("0"),
        });
    }
    if slides.is_empty() {
        return Err("This presentation has no slides.".into());
    }
    Ok(OfficeData::Slides(slides))
}

pub(super) fn open_office(kind: Office, bytes: &[u8]) -> Result<OfficeData, String> {
    if bytes.len() as u64 > MAX_BYTES {
        return Err("Preview limited to files under 50 MB.".into());
    }
    let package = Package::open(bytes)?;
    match kind {
        Office::Docx => open_word(&package),
        Office::Xlsx => open_workbook(&package),
        _ => open_deck(&package),
    }
}

pub(super) struct OfficeDoc {
    data: OfficeData,
    /// The sheet or slide shown.
    page: usize,
    tables: HashMap<usize, Rc<Table>>,
    scroll: ScrollHandle,
}

impl OfficeDoc {
    pub(super) fn new(data: OfficeData) -> Self {
        Self { data, page: 0, tables: HashMap::new(), scroll: ScrollHandle::new() }
    }
}

const PAPER: u32 = 0xffffff;
const INK: u32 = 0x1f1f1f;

fn word_block(block: &Block, index: usize) -> AnyElement {
    match block {
        Block::Paragraph(text, level) => {
            let size = match level {
                1 => 26.,
                2 => 20.,
                3 => 17.,
                4 => 15.,
                _ => 14.,
            };
            div()
                .id(ElementId::NamedInteger("word-block".into(), index as u64))
                .min_h(px(size * 1.4))
                .text_size(px(size))
                .when(*level > 0, |el| el.font_weight(FontWeight::SEMIBOLD).mt(px(8.)))
                .line_height(gpui_kit::relative(1.45))
                .child(SharedString::from(text.clone()))
                .into_any_element()
        }
        Block::Table(rows) => v_flex()
            .id(ElementId::NamedInteger("word-block".into(), index as u64))
            .my(px(6.))
            .border_1()
            .border_color(rgb(0xbfbfbf))
            .text_size(px(13.))
            .children(rows.iter().map(|row| {
                h_flex().children(row.iter().map(|cell| {
                    div()
                        .flex_1()
                        .min_w_0()
                        .px(px(8.))
                        .py(px(4.))
                        .border_1()
                        .border_color(rgb(0xbfbfbf))
                        .child(SharedString::from(cell.clone()))
                }))
            }))
            .into_any_element(),
    }
}

impl Workspace {
    fn office_mut(&mut self, key: &super::FileKey) -> Option<&mut OfficeDoc> {
        match self.files.assets.viewer_mut(key)? {
            super::asset::Viewer::Office(doc) => Some(doc),
            _ => None,
        }
    }

    pub(super) fn render_office(
        &mut self,
        key: &super::FileKey,
        width: f32,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(doc) = self.office_mut(key) else { return div().into_any_element() };
        let border = tokens.border.opacity(0.6);
        let details = |text: String| {
            div()
                .flex_none()
                .h(px(28.))
                .mb(px(12.))
                .px(px(12.))
                .flex()
                .items_center()
                .text_xs()
                .text_color(tokens.muted_foreground)
                .child(text)
        };
        match &doc.data {
            OfficeData::Word(blocks) => {
                let blocks: Vec<AnyElement> = blocks.iter().enumerate().map(|(i, b)| word_block(b, i)).collect();
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .child(chrome::notice(
                        "Text-only preview. Save it or open it in another app to see the layout.",
                        tokens,
                    ))
                    .child(
                        div()
                            .id(ElementId::Name(format!("word:{}", key.path).into()))
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .bg(tokens.muted.opacity(0.4))
                            .p(px((width * 0.04).clamp(8., 20.)))
                            .child(
                                v_flex()
                                    .w_full()
                                    .max_w(px(816.))
                                    .mx_auto()
                                    .p(px((width * 0.07).clamp(16., 56.)))
                                    .gap(px(6.))
                                    .bg(rgb(PAPER))
                                    .text_color(rgb(INK))
                                    .children(blocks),
                            ),
                    )
                    .into_any_element()
            }
            OfficeData::Sheets(sheets) => {
                let page = doc.page.min(sheets.len() - 1);
                let names: Vec<String> = sheets.iter().map(|(name, _)| name.clone()).collect();
                let rows = &sheets[page].1;
                let table = doc.tables.entry(page).or_insert_with(|| Rc::new(Table::grid(rows))).clone();
                let count = sheets.len();
                let id = format!("{}#{page}", key.path);
                let grid = self.render_table(&id, table, width, tokens, cx);
                let tabs = h_flex()
                    .id("sheet-tabs")
                    .flex_none()
                    .h(px(36.))
                    .gap(px(4.))
                    .px(px(8.))
                    .overflow_x_scroll()
                    .border_t_1()
                    .border_color(tokens.border)
                    .children(names.into_iter().enumerate().map(|(index, name)| {
                        let selected = index == page;
                        let tab_key = key.clone();
                        div()
                            .id(ElementId::NamedInteger("sheet-tab".into(), index as u64))
                            .flex_none()
                            .h(px(28.))
                            .px(px(10.))
                            .flex()
                            .items_center()
                            .rounded(px(6.))
                            .text_xs()
                            .cursor_pointer()
                            .text_color(if selected { tokens.foreground } else { tokens.muted_foreground })
                            .when(selected, |el| el.bg(tokens.accent))
                            .hover(|s| s.bg(tokens.accent))
                            .child(name)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(doc) = this.office_mut(&tab_key) {
                                    doc.page = index;
                                }
                                cx.notify();
                            }))
                    }));
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .child(details(format!("{count} sheets")))
                    .child(grid)
                    .child(tabs)
                    .into_any_element()
            }
            OfficeData::Slides(slides) => {
                let count = slides.len();
                let page = doc.page.min(count - 1);
                let scroll = doc.scroll.clone();
                let card_width = (width - 48.).clamp(160., 960.);
                let cards: Vec<AnyElement> = slides
                    .iter()
                    .enumerate()
                    .map(|(index, slide)| {
                        v_flex()
                            .id(ElementId::NamedInteger("slide".into(), index as u64))
                            .flex_none()
                            .gap(px(8.))
                            .child(
                                v_flex()
                                    .relative()
                                    .w(px(card_width))
                                    .h(px(card_width * 9. / 16.))
                                    .overflow_hidden()
                                    .p(px(card_width * 0.06))
                                    .gap(px(card_width * 0.015))
                                    .bg(rgb(PAPER))
                                    .text_color(rgb(INK))
                                    .border_1()
                                    .border_color(border)
                                    .children(slide.paragraphs.iter().enumerate().map(|(i, text)| {
                                        div()
                                            .text_size(px(if i == 0 { card_width * 0.04 } else { card_width * 0.022 }))
                                            .when(i == 0, |el| el.font_weight(FontWeight::SEMIBOLD))
                                            .child(SharedString::from(text.clone()))
                                    }))
                                    .when(slide.hidden, |el| {
                                        el.child(div().absolute().inset_0().bg(gpui_kit::white().opacity(0.5)))
                                    }),
                            )
                            .when(slide.hidden || !slide.notes.is_empty(), |el| {
                                el.child(
                                    v_flex()
                                        .gap(px(4.))
                                        .w(px(card_width))
                                        .text_xs()
                                        .text_color(tokens.muted_foreground)
                                        .when(slide.hidden, |el| el.child("Hidden slide"))
                                        .when(!slide.notes.is_empty(), |el| {
                                            el.child(SharedString::from(slide.notes.clone()))
                                        }),
                                )
                            })
                            .into_any_element()
                    })
                    .collect();
                let step = |delta: isize, id: &'static str, icon: IconName, label: &'static str, enabled: bool| {
                    let step_key = key.clone();
                    let scroll = scroll.clone();
                    chrome::action(id, icon, label, tokens)
                        .when(!enabled, |el| el.opacity(0.5).cursor_default())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let Some(doc) = this.office_mut(&step_key) else { return };
                            let next = doc.page.saturating_add_signed(delta).min(count - 1);
                            doc.page = next;
                            scroll.scroll_to_item(next);
                            cx.notify();
                        }))
                };
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .child(
                        h_flex()
                            .flex_none()
                            .h(px(28.))
                            .mb(px(12.))
                            .gap(px(8.))
                            .px(px(12.))
                            .child(step(-1, "slide-previous", IconName::ChevronLeft, "Previous slide", page > 0))
                            .child(
                                div().flex_1().text_center().text_xs().child(format!("Slide {} of {count}", page + 1)),
                            )
                            .child(step(1, "slide-next", IconName::ChevronRight, "Next slide", page + 1 < count)),
                    )
                    .child(chrome::notice(
                        "Text-only preview. Save it or open it in another app to see the slides.",
                        tokens,
                    ))
                    .child(
                        div()
                            .id(ElementId::Name(format!("slides:{}", key.path).into()))
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .track_scroll(&scroll)
                            .child(v_flex().items_center().gap(px(24.)).p(px(24.)).children(cards)),
                    )
                    .into_any_element()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package(parts: &[(&str, &str)]) -> Vec<u8> {
        let mut out = std::io::Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(&mut out);
        for (name, body) in parts {
            zip.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            std::io::Write::write_all(&mut zip, body.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
        out.into_inner()
    }

    const W: &str = "xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"";

    #[test]
    fn word_documents_keep_paragraphs_headings_and_tables() {
        let body = format!(
            "<w:document {W}><w:body>\
             <w:p><w:pPr><w:pStyle w:val=\"Heading1\"/></w:pPr><w:r><w:t>Intro</w:t></w:r></w:p>\
             <w:p><w:r><w:t>a</w:t><w:tab/><w:t>b</w:t></w:r></w:p>\
             <w:tbl><w:tr><w:tc><w:p><w:r><w:t>x</w:t></w:r></w:p></w:tc><w:tc><w:p/></w:tc></w:tr></w:tbl>\
             </w:body></w:document>"
        );
        let data = open_office(Office::Docx, &package(&[("word/document.xml", &body)])).unwrap();
        assert_eq!(
            data,
            OfficeData::Word(vec![
                Block::Paragraph("Intro".into(), 2),
                Block::Paragraph("a\tb".into(), 0),
                Block::Table(vec![vec!["x".into(), String::new()]]),
            ])
        );
    }

    #[test]
    fn workbooks_place_cells_by_reference() {
        let rels = "<Relationships><Relationship Id=\"rId1\" Target=\"worksheets/sheet1.xml\"/></Relationships>";
        let workbook = "<workbook xmlns:r=\"r\"><sheets><sheet name=\"Data\" r:id=\"rId1\"/></sheets></workbook>";
        let strings = "<sst><si><t>name</t></si><si><r><t>A</t></r><r><t>da</t></r></si></sst>";
        let sheet = "<worksheet><sheetData>\
             <row r=\"1\"><c r=\"A1\" t=\"s\"><v>0</v></c><c r=\"C1\"><v>3.5</v></c></row>\
             <row r=\"3\"><c r=\"B3\" t=\"s\"><v>1</v></c><c r=\"C3\" t=\"b\"><v>1</v></c></row>\
             </sheetData></worksheet>";
        let bytes = package(&[
            ("xl/workbook.xml", workbook),
            ("xl/_rels/workbook.xml.rels", rels),
            ("xl/sharedStrings.xml", strings),
            ("xl/worksheets/sheet1.xml", sheet),
        ]);
        let OfficeData::Sheets(sheets) = open_office(Office::Xlsx, &bytes).unwrap() else { panic!() };
        assert_eq!(sheets[0].0, "Data");
        assert_eq!(
            sheets[0].1,
            vec![
                vec!["name".to_owned(), String::new(), "3.5".into()],
                vec![],
                vec![String::new(), "Ada".into(), "TRUE".into()]
            ]
        );
        assert_eq!(column_of("AA10"), Some(26));
        assert_eq!(resolve("ppt/slides", "../notesSlides/n1.xml"), "ppt/notesSlides/n1.xml");
    }

    #[test]
    fn decks_list_slide_text_and_notes() {
        let a = "xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\"";
        let presentation = "<p:presentation xmlns:p=\"p\" xmlns:r=\"r\"><p:sldIdLst><p:sldId id=\"256\" r:id=\"rId2\"/></p:sldIdLst></p:presentation>";
        let rels = "<Relationships><Relationship Id=\"rId2\" Target=\"slides/slide1.xml\"/></Relationships>";
        let slide = format!("<p:sld xmlns:p=\"p\" {a} show=\"0\"><a:p><a:r><a:t>Title</a:t></a:r></a:p><a:p/></p:sld>");
        let slide_rels =
            "<Relationships><Relationship Id=\"rId1\" Target=\"../notesSlides/notesSlide1.xml\"/></Relationships>";
        let notes = format!("<p:notes xmlns:p=\"p\" {a}><a:p><a:r><a:t>Say hi</a:t></a:r></a:p></p:notes>");
        let bytes = package(&[
            ("ppt/presentation.xml", presentation),
            ("ppt/_rels/presentation.xml.rels", rels),
            ("ppt/slides/slide1.xml", &slide),
            ("ppt/slides/_rels/slide1.xml.rels", slide_rels),
            ("ppt/notesSlides/notesSlide1.xml", &notes),
        ]);
        let data = open_office(Office::Pptx, &bytes).unwrap();
        assert_eq!(
            data,
            OfficeData::Slides(vec![Slide { paragraphs: vec!["Title".into()], notes: "Say hi".into(), hidden: true }])
        );
    }
}
