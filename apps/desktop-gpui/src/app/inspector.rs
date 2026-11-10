//! What an expanded tool entry shows (`V2ItemInspector.tsx`, `CommandOutputPanel.tsx`):
//! a command with its streamed output, a tool's input and output, a file change, a
//! search's results, or a request's questions. Output is loaded only while its entry is open.

use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;

use gpui_kit::StyledImage as _;
use gpui_kit::assets::IconName;
use gpui_kit::base::SelectableText;
use gpui_kit::component::button::Button;
use gpui_kit::component::{Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, Context, ElementId, FontWeight, HighlightStyle, InteractiveElement as _, IntoElement,
    ParentElement as _, ScrollHandle, SharedString, StatefulInteractiveElement as _, Styled as _, Task, WeakEntity,
    div, prelude::FluentBuilder as _, px, relative,
};
use serde_json::{Value, json};

use super::Workspace;
use super::markdown_images::Source::WorkspaceFile;
use super::side_chat::ChatPane;
use crate::file_links::workspace_display_path;
use crate::model::TurnItem;
use crate::rpc::StreamEvent;
use crate::terminal_text::TerminalText;
use crate::theme::{FONT_MONO, Tokens};
use crate::timeline::{Row, WorkRow, WorkRowKind};
use crate::work_log::Entry;

/// Output of the open entries, by entry id.
#[derive(Default)]
pub(super) struct Details {
    loads: HashMap<SharedString, Detail>,
    /// Open tools' input and output as text, formatted once per item change, not per frame.
    tools: HashMap<SharedString, ToolText>,
}

/// What a tool entry shows, formatted from `item` (the streamed or the fetched one).
struct ToolText {
    item: Rc<TurnItem>,
    input: Option<ToolInput>,
    output: Option<SharedString>,
    /// A task list's steps, one per line with its status mark.
    steps: Option<SharedString>,
    /// A request's questions, each followed by its options.
    questions: Option<SharedString>,
}

enum ToolInput {
    /// `key value` lines, with each key's range for its muted highlight.
    Lines {
        text: SharedString,
        keys: Vec<Range<usize>>,
    },
    Text(SharedString),
}

impl ToolText {
    fn new(item: Rc<TurnItem>) -> Self {
        let input = item.raw.get("input").and_then(|input| match arg_lines(input) {
            Some(lines) => {
                let (mut text, mut keys) = (String::new(), Vec::with_capacity(lines.len()));
                for (key, value) in lines {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    keys.push(text.len()..text.len() + key.len());
                    text.push_str(&key);
                    text.push(' ');
                    text.push_str(&value);
                }
                Some(ToolInput::Lines { text: text.into(), keys })
            }
            None => format_value(input.get("summary").unwrap_or(input)).map(|text| ToolInput::Text(text.into())),
        });
        let output = item.raw.get("output").and_then(format_value).map(Into::into);
        let steps = todo_steps(&item).map(Into::into);
        let questions = questions_text(&item).map(Into::into);
        Self { item, input, output, steps, questions }
    }
}

enum Detail {
    /// `orchestration.subscribeCommandOutput`, folded as a terminal shows it.
    Output {
        output: TerminalText,
        /// `output` as one string, rebuilt when frames land.
        text: SharedString,
        loaded: bool,
        running: bool,
        error: Option<SharedString>,
        scroll: ScrollHandle,
        _task: Task<()>,
    },
    /// `orchestration.getTurnItem` for a tool whose input or output the stream left out,
    /// refetched once the item settles (`revision`).
    Item { revision: SharedString, fetched: Fetch, _task: Task<()> },
}

enum Fetch {
    Loading,
    Loaded(Option<Rc<TurnItem>>),
    Failed(SharedString),
}

/// The stream leaves this tool's output or input out (`turnItemNeedsDetailFetch`).
fn needs_fetch(item: &TurnItem, streams_output: bool) -> bool {
    let omitted = item.bool("outputOmitted");
    match item.item_type.as_str() {
        "command_execution" => omitted && !streams_output,
        "dynamic_tool" => omitted || item.raw.pointer("/input/truncated").and_then(Value::as_bool) == Some(true),
        _ => false,
    }
}

/// `"live"` until the item settles, then its `updatedAt` (`turnItemDetailRevision`).
pub(super) fn revision(item: &TurnItem) -> SharedString {
    match item.status.as_str() {
        "idle" | "pending" | "running" | "waiting" => "live".into(),
        _ => item.str("updatedAt").unwrap_or_default().to_owned().into(),
    }
}

/// Whether an entry opens onto the inspector (`turnItemHasDetail`, for the types it covers).
pub(super) fn has_inspector(entry: &Entry) -> bool {
    let item = &entry.item.0;
    let listed = |key: &str| item.raw.get(key).and_then(Value::as_array).is_some_and(|list| !list.is_empty());
    match item.item_type.as_str() {
        "command_execution" | "file_change" => true,
        "todo_list" => listed("steps"),
        "user_input_request" => listed("questions"),
        "file_search" => listed("results") || item.str("pattern").is_some_and(|p| !p.trim().is_empty()),
        "web_search" => listed("results") || listed("patterns"),
        "dynamic_tool" => {
            item.raw.get("input").is_some_and(|input| !is_empty_value(input))
                || item.raw.get("output").is_some_and(|output| !is_empty_value(output))
                || item.bool("outputOmitted")
        }
        _ => false,
    }
}

fn is_empty_value(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(text) => text.trim().is_empty(),
        Value::Object(map) => map.is_empty(),
        Value::Array(items) => items.is_empty(),
        _ => false,
    }
}

/// The view key an open entry's images load under in `pane`, for its row there to remeasure.
fn thought_view_key(pane: ChatPane, entry_id: &str) -> SharedString {
    super::chat::view_key(pane, &super::chat::thought_key(entry_id))
}

fn open_entries(rows: &[Row]) -> impl Iterator<Item = &Rc<Entry>> {
    rows.iter().flat_map(|row| -> Vec<&Rc<Entry>> {
        match row {
            Row::Work(WorkRow { kind: WorkRowKind::Single { entry, expanded: true, .. }, .. }) => vec![entry],
            Row::Work(WorkRow { kind: WorkRowKind::Details { entries }, .. }) => {
                entries.iter().filter(|detail| detail.expanded).map(|detail| &detail.entry).collect()
            }
            _ => Vec::new(),
        }
    })
}

impl Workspace {
    /// Starts loading the output of entries that just opened and drops it for closed ones.
    pub(super) fn sync_details(&mut self, cx: &mut Context<Self>) {
        let streams_output = self.server_info().capability("commandOutputStreaming");
        let shown = self.pane_open_entries();
        let open: Vec<Rc<Entry>> =
            shown.iter().filter(|(_, entry)| has_inspector(entry)).map(|(_, e)| e.clone()).collect();
        self.details.loads.retain(|id, _| open.iter().any(|entry| entry.id == *id));
        self.details.tools.retain(|id, _| open.iter().any(|entry| entry.id == *id));
        for (pane, entry) in shown.iter().filter(|(_, entry)| has_inspector(entry)) {
            self.details.format_tool(entry);
            self.load_tool_images(*pane, entry, cx);
        }
        let viewed: Vec<(SharedString, String)> = shown
            .iter()
            .filter_map(|(pane, entry)| Some((thought_view_key(*pane, &entry.id), self.viewed_image(entry)?)))
            .collect();
        for (key, path) in viewed {
            self.load_images(&key, vec![path], cx);
        }
        for entry in open {
            let item = entry.item.0.clone();
            let streams = item.item_type == "command_execution" && streams_output;
            match self.details.loads.get(&entry.id) {
                Some(Detail::Output { .. }) => continue,
                Some(Detail::Item { revision: current, .. }) if *current == revision(&item) => continue,
                _ => {}
            }
            let Some(client) = self.client() else { return };
            let Some((thread_id, item_id)) = self.source_of(&item.id) else { continue };
            let payload = json!({"threadId": thread_id, "itemId": item_id});
            let id = entry.id.clone();
            let detail = if streams {
                let stream = client.stream("orchestration.subscribeCommandOutput", payload);
                let _task = cx.spawn(async move |this, cx| {
                    while let Some(first) = stream.next().await {
                        let mut events = vec![first];
                        events.extend(std::iter::from_fn(|| stream.try_next()));
                        let alive = this.update(cx, |this, cx| {
                            if let Some(Detail::Output { output, text, loaded, running, error, scroll, .. }) =
                                this.details.loads.get_mut(&id)
                            {
                                let following = scroll.offset().y.abs() + px(8.) >= scroll.max_offset().y;
                                for event in events {
                                    match event {
                                        StreamEvent::Item(frame) => apply_frame(output, running, &frame),
                                        StreamEvent::End(Err(failure)) => *error = Some(failure.to_string().into()),
                                        StreamEvent::End(Ok(())) => *running = false,
                                    }
                                }
                                *text = output.text_content();
                                *loaded = true;
                                if following {
                                    scroll.scroll_to_bottom();
                                }
                                cx.notify();
                            }
                        });
                        if alive.is_err() {
                            break;
                        }
                    }
                });
                Detail::Output {
                    output: TerminalText::default(),
                    text: SharedString::default(),
                    loaded: false,
                    running: true,
                    error: None,
                    scroll: ScrollHandle::new(),
                    _task,
                }
            } else if needs_fetch(&item, streams_output) {
                let _task = cx.spawn(async move |this, cx| {
                    let fetched = match client.request("orchestration.getTurnItem", payload).await {
                        Ok(response) => match response.get("item").filter(|item| !item.is_null()) {
                            Some(item) => match serde_json::from_value::<TurnItem>(item.clone()) {
                                Ok(item) => Fetch::Loaded(Some(Rc::new(item))),
                                Err(error) => Fetch::Failed(error.to_string().into()),
                            },
                            None => Fetch::Loaded(None),
                        },
                        Err(error) => Fetch::Failed(error.to_string().into()),
                    };
                    let _ = this.update(cx, |this, cx| {
                        if let Some(Detail::Item { fetched: slot, .. }) = this.details.loads.get_mut(&id) {
                            *slot = fetched;
                            let shown = this.pane_open_entries();
                            for (pane, entry) in shown.into_iter().filter(|(_, entry)| entry.id == id) {
                                this.details.format_tool(&entry);
                                this.load_tool_images(pane, &entry, cx);
                            }
                            cx.notify();
                        }
                    });
                });
                Detail::Item { revision: revision(&item), fetched: Fetch::Loading, _task }
            } else {
                continue;
            };
            self.details.loads.insert(entry.id.clone(), detail);
        }
    }

    /// Starts loading the images a tool's output holds (`turnItemOutputImages`).
    fn load_tool_images(&mut self, pane: ChatPane, entry: &Entry, cx: &mut Context<Self>) {
        let item = self.details.shown_item(entry);
        let assets: Vec<(String, Value)> = (0..tool_output_image_count(&item))
            .map(|index| {
                let resource = json!({
                    "_tag": "tool-output-image", "threadId": item.thread_id, "itemId": item.id, "index": index,
                });
                (tool_image_destination(&item, index), resource)
            })
            .collect();
        if !assets.is_empty() {
            self.load_asset_images(&thought_view_key(pane, &entry.id), assets, cx);
        }
    }

    /// The workspace image a tool viewed, as its row previews it (`workEntryViewedImagePath`,
    /// `resolveViewedImageAsset`): only a file on the thread's host.
    pub(super) fn viewed_image(&self, entry: &Entry) -> Option<String> {
        let path = viewed_image_path(entry)?;
        matches!(self.target, Some(super::Target::Thread { .. })).then_some(())?;
        let workspace_file =
            matches!(super::markdown_images::classify(path, self.git_cwd().as_deref()), WorkspaceFile(_));
        workspace_file.then(|| path.to_owned())
    }

    /// The open entries of the timeline and of the side chat on screen, which loads its own.
    fn pane_open_entries(&self) -> Vec<(ChatPane, Rc<Entry>)> {
        let side = self.side_chat.view.as_ref().map(|view| view.rows.clone());
        let mut open: Vec<_> = open_entries(&self.rows).map(|entry| (ChatPane::Main, entry.clone())).collect();
        open.extend(side.iter().flat_map(|rows| open_entries(rows)).map(|entry| (ChatPane::Side, entry.clone())));
        open
    }

    /// The thread and item an item was projected from, for the item RPCs.
    pub(super) fn source_of(&self, item_id: &str) -> Option<(String, String)> {
        let main = self.thread.as_ref().and_then(|thread| thread.state.projection.as_ref());
        let rows = main.into_iter().chain(self.side_chat_projection()).flat_map(|p| &p.visible_turn_items);
        let row = rows.into_iter().find(|row| row.item.id == item_id)?;
        Some((row.source_thread_id.clone(), row.source_item_id.clone()))
    }
}

impl Details {
    /// The item an entry shows: the fetched one once it loads, else the streamed one.
    fn shown_item(&self, entry: &Entry) -> Rc<TurnItem> {
        match self.loads.get(&entry.id) {
            Some(Detail::Item { fetched: Fetch::Loaded(Some(item)), .. })
                if item.item_type == entry.item.0.item_type =>
            {
                item.clone()
            }
            _ => entry.item.0.clone(),
        }
    }

    /// Formats a tool entry's text unless it is current for the item it shows.
    fn format_tool(&mut self, entry: &Entry) {
        let item = self.shown_item(entry);
        if !self.tools.get(&entry.id).is_some_and(|text| Rc::ptr_eq(&text.item, &item)) {
            self.tools.insert(entry.id.clone(), ToolText::new(item));
        }
    }
}

/// `workEntryViewedImagePath`: the image a tool viewed, else a read's detail naming one.
fn viewed_image_path(entry: &Entry) -> Option<&str> {
    let image = |path: &str| !path.contains(['\r', '\n']) && super::files::is_image(path);
    let viewed = entry.item.0.raw.get("viewedImagePath").and_then(Value::as_str).map(str::trim);
    if let Some(path) = viewed.filter(|path| image(path)) {
        return Some(path);
    }
    let detail = entry.detail.as_deref().map(str::trim);
    detail.filter(|detail| entry.action == crate::work_log::Action::Read && image(detail))
}

/// `reduceCommandOutputFrame`: a `replace` starts over, an `append` extends.
fn apply_frame(output: &mut TerminalText, running: &mut bool, frame: &Value) {
    if frame.get("kind").and_then(Value::as_str) == Some("replace") {
        output.clear();
    }
    output.truncated |= frame.get("truncated").and_then(Value::as_bool) == Some(true);
    output.append(frame.get("text").and_then(Value::as_str).unwrap_or_default());
    *running = frame.get("running").and_then(Value::as_bool).unwrap_or(false);
}

/// `formatToolValue`: text blocks joined, anything else as indented JSON.
fn format_value(value: &Value) -> Option<String> {
    if is_empty_value(value) {
        return None;
    }
    if let Value::String(text) = value {
        return Some(text.clone());
    }
    let blocks: Option<Vec<&str>> = value.as_array().map(|items| {
        items
            .iter()
            .filter(|item| item.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|item| item.get("text").and_then(Value::as_str))
            .collect()
    });
    match blocks {
        Some(blocks) if !blocks.is_empty() => Some(blocks.join("\n\n")),
        _ => serde_json::to_string_pretty(value).ok(),
    }
}

/// A task list's steps as the inspector lists them: done, running or waiting, then the text.
fn todo_steps(item: &TurnItem) -> Option<String> {
    if item.item_type != "todo_list" {
        return None;
    }
    let steps = item.raw.get("steps")?.as_array()?;
    let lines: Vec<String> = steps
        .iter()
        .map(|step| {
            let mark = match step.get("status").and_then(Value::as_str) {
                Some("completed") => '✓',
                Some("running") => '◉',
                _ => '○',
            };
            format!("{mark} {}", step.get("text").and_then(Value::as_str).unwrap_or_default())
        })
        .collect();
    (!lines.is_empty()).then(|| lines.join("\n"))
}

/// A request's questions as the inspector lists them: each question, then a `•` line per option.
fn questions_text(item: &TurnItem) -> Option<String> {
    if item.item_type != "user_input_request" {
        return None;
    }
    let text = |value: &Value, key: &str| value.get(key).and_then(Value::as_str).unwrap_or_default().to_owned();
    let questions: Vec<String> = item
        .raw
        .get("questions")?
        .as_array()?
        .iter()
        .map(|question| {
            let options = question.get("options").and_then(Value::as_array).into_iter().flatten();
            std::iter::once(text(question, "question"))
                .chain(options.map(|option| format!("• {}", text(option, "label"))))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .collect();
    (!questions.is_empty()).then(|| questions.join("\n\n"))
}

/// The most images one tool output shows (`MAX_TOOL_OUTPUT_IMAGES`).
const MAX_TOOL_OUTPUT_IMAGES: usize = 8;

/// `readToolOutputImage`: a base64 raster block a provider could send a turn.
fn is_tool_output_image(block: &Value) -> bool {
    if block.get("type").and_then(Value::as_str) != Some("image") {
        return false;
    }
    let source = block.get("source").filter(|source| source.is_object());
    let mime = match source {
        Some(source) if source.get("type").and_then(Value::as_str) != Some("base64") => return false,
        Some(source) => source.get("media_type"),
        None => block.get("mimeType"),
    };
    mime.and_then(Value::as_str).is_some_and(|mime| {
        matches!(mime.to_ascii_lowercase().as_str(), "image/gif" | "image/jpeg" | "image/png" | "image/webp")
    })
}

/// `turnItemOutputImages`: how many images a tool's output holds, in asset index order.
fn tool_output_image_count(item: &TurnItem) -> usize {
    if item.item_type != "dynamic_tool" || item.bool("outputOmitted") {
        return 0;
    }
    let Some(output) = item.raw.get("output") else { return 0 };
    let blocks: Vec<&Value> = match output {
        Value::Array(blocks) => blocks.iter().collect(),
        Value::Object(record) => match record.get("content") {
            Some(Value::Array(blocks)) => blocks.iter().collect(),
            _ => vec![output],
        },
        _ => vec![output],
    };
    blocks.into_iter().filter(|block| is_tool_output_image(block)).take(MAX_TOOL_OUTPUT_IMAGES).count()
}

/// The image cache's key for a tool output image.
fn tool_image_destination(item: &TurnItem, index: usize) -> String {
    format!("tool-output-image:{}:{}:{index}", item.thread_id, item.id)
}

/// `toolCallLines`: `key value`, strings raw and everything else as JSON.
fn arg_lines(input: &Value) -> Option<Vec<(String, String)>> {
    let object =
        input.as_object().filter(|object| !object.contains_key("summary") || !object.contains_key("truncated"))?;
    Some(
        object
            .iter()
            .map(|(key, value)| {
                let value = match value {
                    Value::String(text) if !text.is_empty() => text.clone(),
                    other => other.to_string(),
                };
                (key.clone(), value)
            })
            .collect(),
    )
}

fn mono() -> gpui_kit::Div {
    div().font_family(FONT_MONO).text_size(px(13.)).line_height(relative(1.625))
}

fn note(text: impl Into<SharedString>, color: gpui_kit::Hsla) -> gpui_kit::Div {
    div().text_xs().italic().text_color(color).child(text.into())
}

/// The inspector body for an open entry; `None` for types the fallback text covers.
pub(super) fn inspector(entry: &Entry, this: &WeakEntity<Workspace>, t: &Tokens, cx: &App) -> Option<AnyElement> {
    if !has_inspector(entry) {
        return None;
    }
    let workspace = this.upgrade()?;
    let workspace = workspace.read(cx);
    let item = workspace.details.shown_item(entry);
    let root = workspace.git_cwd();
    let body = match item.item_type.as_str() {
        "command_execution" | "dynamic_tool" => tool_body(entry, &item, workspace, t),
        "file_change" => {
            let scheme = super::git::DiffColorScheme::from_settings(&workspace.client_settings.values);
            // `onOpenToolDiff`, on a server thread only.
            let open_diff = matches!(workspace.target, Some(super::Target::Thread { .. }))
                .then(|| workspace.source_of(&entry.item.0.id))
                .flatten()
                .map(|source| open_diff_button(&entry.id, source, this, t));
            file_change(&entry.id, &item, root.as_deref(), scheme, open_diff, t)
        }
        "file_search" | "web_search" => search(entry, &item, root.as_deref(), this, t),
        _ => {
            let text = workspace.details.tools.get(&entry.id).filter(|text| Rc::ptr_eq(&text.item, &item));
            let text = match text {
                Some(text) => text.steps.clone().or_else(|| text.questions.clone()),
                None => todo_steps(&item).or_else(|| questions_text(&item)).map(Into::into),
            };
            v_flex().children(text.map(|text| structured(&format!("text:{}", entry.id), text, t)))
        }
    };
    Some(body.into_any_element())
}

/// `ToolCallBody`: a tool's output images, its command or arguments, its output, and a failed exit.
fn tool_body(entry: &Entry, item: &Rc<TurnItem>, workspace: &Workspace, t: &Tokens) -> gpui_kit::Div {
    let detail = workspace.details.loads.get(&entry.id);
    // Filled by `sync_details`; formatting here covers a frame that runs before it.
    let formatted;
    let tool = match workspace.details.tools.get(&entry.id).filter(|text| Rc::ptr_eq(&text.item, item)) {
        Some(tool) => tool,
        None => {
            formatted = ToolText::new(item.clone());
            &formatted
        }
    };
    let foreground = gpui_kit::Hsla { a: t.foreground.a * 0.85, ..t.foreground };
    let images = (0..tool_output_image_count(item)).filter_map(|index| {
        let image = workspace.markdown_images.get(&tool_image_destination(item, index))?;
        Some(gpui_kit::img(image).max_w_full().max_h(px(256.)).object_fit(gpui_kit::ObjectFit::Contain))
    });
    let mut body = v_flex().gap(px(6.)).children(images);
    if item.item_type == "command_execution" {
        if let Some(command) = item.str("input").map(str::trim).filter(|c| !c.is_empty()) {
            body = body.child(mono().text_color(foreground).child(selectable("command", control_pictures(command))));
        }
    } else {
        match &tool.input {
            Some(ToolInput::Lines { text, keys }) => {
                let muted = HighlightStyle { color: Some(t.muted_foreground), ..Default::default() };
                let text = selectable("args", text.clone()).highlights(keys.iter().map(|key| (key.clone(), muted)));
                body = body.child(mono().text_color(foreground).child(text));
            }
            Some(ToolInput::Text(text)) => {
                body = body.child(structured(&format!("args:{}", entry.id), text.clone(), t));
            }
            None => {}
        }
    }
    let output_slot: Option<AnyElement> = match detail {
        Some(Detail::Output { output, text, loaded, running, error, scroll, .. }) => {
            if text.is_empty() {
                match (error, loaded, running) {
                    (Some(error), _, _) => {
                        Some(note(format!("Couldn't load output: {error}"), t.error).not_italic().into_any_element())
                    }
                    (None, false, _) => Some(note("Loading output…", t.muted_foreground).into_any_element()),
                    (None, true, true) => None,
                    (None, true, false) => Some(note("No output.", t.muted_foreground).into_any_element()),
                }
            } else {
                Some(
                    v_flex()
                        .gap(px(6.))
                        .children(output.truncated.then(|| note("Earlier output not shown", t.muted_foreground)))
                        .child(
                            mono()
                                .id(ElementId::Name(format!("output:{}", entry.id).into()))
                                .max_h(px(320.))
                                .overflow_y_scroll()
                                .track_scroll(scroll)
                                .text_color(t.muted_foreground)
                                .child(selectable("text", text.clone())),
                        )
                        .into_any_element(),
                )
            }
        }
        Some(Detail::Item { fetched: Fetch::Loading, .. }) => {
            Some(note("Loading output…", t.muted_foreground).into_any_element())
        }
        Some(Detail::Item { fetched: Fetch::Failed(error), .. }) => {
            Some(note(format!("Couldn't load output: {error}"), t.error).not_italic().into_any_element())
        }
        Some(Detail::Item { fetched: Fetch::Loaded(None), .. }) => {
            Some(note("Output is no longer available.", t.muted_foreground).into_any_element())
        }
        _ => match &tool.output {
            Some(text) => Some(structured(&format!("result:{}", entry.id), text.clone(), t).into_any_element()),
            None if matches!(detail, Some(Detail::Item { .. })) && tool_output_image_count(item) == 0 => {
                Some(note("No output.", t.muted_foreground).into_any_element())
            }
            None => None,
        },
    };
    body = body.children(output_slot);
    if let Some(code) = item.raw.get("exitCode").and_then(Value::as_i64).filter(|code| *code != 0) {
        body = body.child(mono().text_color(t.error).child(format!("exit {code}")));
    }
    body
}

/// A file change: its path and line counts, a failed change's diff, and each changed file with
/// its operation, a rename's old path and its type.
fn file_change(
    id: &str,
    item: &TurnItem,
    root: Option<&str>,
    scheme: super::git::DiffColorScheme,
    open_diff: Option<Button>,
    t: &Tokens,
) -> gpui_kit::Div {
    let raw = &item.raw;
    let path = workspace_display_path(item.str("fileName").unwrap_or_default(), root);
    let count = |key: &str| raw.get(key).and_then(Value::as_u64);
    let counts = (count("additions").is_some() || count("deletions").is_some()).then(|| {
        let figure = |key: &str| super::git::group_thousands(count(key).unwrap_or(0));
        h_flex()
            .gap(px(4.))
            .child(div().text_color(super::git::addition_color(t, scheme)).child(format!("+{}", figure("additions"))))
            .child(div().text_color(super::git::deletion_color(t, scheme)).child(format!("-{}", figure("deletions"))))
    });
    let failed_diff = item.str("diffStr").filter(|diff| item.status == "failed" && !diff.trim().is_empty());
    let changes = file_change_lines(item, root);
    v_flex()
        .gap(px(8.))
        .text_xs()
        .child(
            h_flex()
                .flex_wrap()
                .items_center()
                .gap(px(8.))
                .child(div().font_family(FONT_MONO).text_color(t.muted_foreground).child(selectable("path", path)))
                .children(counts)
                .children(open_diff),
        )
        .children(failed_diff.map(|diff| structured(&format!("diff:{id}"), diff.to_owned().into(), t)))
        .when(!changes.is_empty(), |el| {
            el.child(
                v_flex().gap(px(4.)).font_family(FONT_MONO).text_color(t.muted_foreground).children(
                    changes.into_iter().enumerate().map(|(index, change)| {
                        selectable(ElementId::NamedInteger("change".into(), index as u64), change)
                    }),
                ),
            )
        })
}

/// `Open diff` (`Button size="xs" variant="outline"`): the change's recorded diff in the panel.
fn open_diff_button(
    id: &str,
    (thread_id, item_id): (String, String),
    this: &WeakEntity<Workspace>,
    t: &Tokens,
) -> Button {
    let this = this.clone();
    Button::new(ElementId::Name(format!("open-tool-diff:{id}").into()))
        .outline()
        .xsmall()
        .h(px(24.))
        .px(px(8.))
        .rounded(px(6.))
        .border_color(t.input)
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .label("Open diff")
        .on_click(move |_, window, cx| {
            cx.stop_propagation();
            let (thread_id, item_id) = (thread_id.clone(), item_id.clone());
            let _ = this.update(cx, |workspace, cx| workspace.open_tool_call_diff(thread_id, item_id, window, cx));
        })
}

/// Each change of a file change: `operation old → path (type, mime)`.
fn file_change_lines(item: &TurnItem, root: Option<&str>) -> Vec<String> {
    fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
        value.get(key).and_then(Value::as_str).filter(|v| !v.is_empty())
    }
    let changes = item.raw.get("changes").and_then(Value::as_array).into_iter().flatten();
    changes
        .map(|change| {
            let renamed = text(change, "oldPath").map(|old| format!("{old} → ")).unwrap_or_default();
            let kinds: Vec<&str> = [text(change, "fileType"), text(change, "mimeType")].into_iter().flatten().collect();
            let kinds = if kinds.is_empty() { String::new() } else { format!(" ({})", kinds.join(", ")) };
            let operation = text(change, "operation").unwrap_or_default();
            let path = workspace_display_path(text(change, "path").unwrap_or_default(), root);
            format!("{operation} {renamed}{path}{kinds}")
        })
        .collect()
}

/// A file search's pattern and its matches with their previews, or a web search's queries and
/// its results, each linked to its page with its snippet.
fn search(
    entry: &Entry,
    item: &TurnItem,
    root: Option<&str>,
    this: &WeakEntity<Workspace>,
    t: &Tokens,
) -> gpui_kit::Div {
    let raw = &item.raw;
    let text = |value: &Value, key: &str| value.get(key).and_then(Value::as_str).map(str::to_owned);
    let foreground = gpui_kit::Hsla { a: t.foreground.a * 0.85, ..t.foreground };
    let web = item.item_type == "web_search";
    let query = if web {
        let patterns = raw.get("patterns").and_then(Value::as_array).into_iter().flatten();
        let patterns: Vec<&str> = patterns.filter_map(Value::as_str).collect();
        (!patterns.is_empty()).then(|| patterns.join("\n"))
    } else {
        item.str("pattern").filter(|pattern| !pattern.trim().is_empty()).map(str::to_owned)
    };
    let results: Vec<&Value> = raw.get("results").and_then(Value::as_array).into_iter().flatten().collect();
    let body = v_flex()
        .gap(px(8.))
        .text_xs()
        .children(query.map(|query| mono().text_color(foreground).child(selectable("query", query))));
    if results.is_empty() {
        return body;
    }
    let rows = results.into_iter().enumerate().map(|(index, result)| {
        let row = v_flex().id(ElementId::NamedInteger("result".into(), index as u64));
        if !web {
            let location_color = gpui_kit::Hsla { a: t.foreground.a * 0.8, ..t.foreground };
            return row
                .child(
                    div()
                        .font_family(FONT_MONO)
                        .text_color(location_color)
                        .child(selectable("location", location(result, root))),
                )
                .children(text(result, "preview").filter(|p| !p.is_empty()).map(|preview| {
                    div().mt(px(2.)).text_color(t.muted_foreground).child(selectable("preview", preview))
                }));
        }
        let (title, url) = (text(result, "title"), text(result, "url"));
        let heading = match url.as_deref().and_then(web_link) {
            Some(href) => {
                let this = this.clone();
                h_flex()
                    .id("link")
                    .gap(px(4.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(t.foreground)
                    .cursor_pointer()
                    .hover(gpui_kit::Styled::underline)
                    .child(title.or(url).unwrap_or_default())
                    .child(Icon::new(IconName::ExternalLink).size(px(12.)))
                    .on_click(move |event, window, cx| {
                        cx.stop_propagation();
                        let _ = this.update(cx, |workspace, cx| workspace.open_link(&href, event, window, cx));
                    })
                    .into_any_element()
            }
            None => div()
                .font_weight(FontWeight::MEDIUM)
                .text_color(t.foreground)
                .child(selectable("title", title.or(url).unwrap_or_else(|| "Search result".into())))
                .into_any_element(),
        };
        row.child(heading).children(
            text(result, "snippet")
                .filter(|snippet| !snippet.is_empty())
                .map(|snippet| div().text_color(t.muted_foreground).child(selectable("snippet", snippet))),
        )
    });
    let list = v_flex().id(ElementId::Name(format!("results:{}", entry.id).into())).gap(px(if web { 6. } else { 4. }));
    body.child(list.children(rows))
}

/// A file match's `path:line:column`, its path read from the workspace folder.
fn location(result: &Value, root: Option<&str>) -> String {
    let position = |key: &str| result.get(key).and_then(Value::as_u64).map(|n| format!(":{n}")).unwrap_or_default();
    let file = result.get("fileName").and_then(Value::as_str).unwrap_or_default();
    format!("{}{}{}", workspace_display_path(file, root), position("line"), position("column"))
}

/// `resolveExternalWebLinkHref`: an http(s) link, a protocol-relative one read as https.
fn web_link(url: &str) -> Option<String> {
    let url = url.trim();
    let href = url.strip_prefix("//").map_or_else(|| url.to_owned(), |rest| format!("https://{rest}"));
    let lower = href.to_ascii_lowercase();
    (lower.starts_with("http://") || lower.starts_with("https://")).then_some(href)
}

fn selectable(id: impl Into<ElementId>, text: impl Into<SharedString>) -> SelectableText {
    SelectableText::new(id, text)
}

/// `StructuredValue`: a scrolling block of muted, selectable mono text.
fn structured(id: &str, text: SharedString, t: &Tokens) -> gpui_kit::Stateful<gpui_kit::Div> {
    mono()
        .id(ElementId::Name(id.to_owned().into()))
        .max_h(px(320.))
        .overflow_y_scroll()
        .text_color(t.muted_foreground)
        .child(selectable("text", text))
}

/// `commandLabel`: control characters other than tab and newlines show as control pictures.
fn control_pictures(command: &str) -> String {
    command
        .chars()
        .map(|ch| match ch {
            '\t' | '\n' | '\r' => ch,
            '\u{7f}' => '\u{2421}',
            ch if (ch as u32) < 0x20 => char::from_u32(0x2400 + ch as u32).unwrap_or(ch),
            ch => ch,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(value: Value) -> TurnItem {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn frames_replace_or_extend_and_keep_truncation() {
        let (mut output, mut running) = (TerminalText::default(), true);
        apply_frame(
            &mut output,
            &mut running,
            &json!({"kind": "replace", "text": "a\n", "truncated": true, "running": true}),
        );
        apply_frame(
            &mut output,
            &mut running,
            &json!({"kind": "append", "text": "b", "truncated": false, "running": true}),
        );
        assert_eq!((&*output.text_content(), output.truncated, running), ("a\nb", true, true));
        apply_frame(
            &mut output,
            &mut running,
            &json!({"kind": "replace", "text": "done", "truncated": false, "running": false}),
        );
        assert_eq!((&*output.text_content(), output.truncated, running), ("done", true, false));
    }

    #[test]
    fn only_left_out_tool_details_are_fetched() {
        let tool = |extra: Value| {
            let mut value = json!({"id": "i", "type": "dynamic_tool", "status": "completed", "input": {"a": 1}});
            value.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            item(value)
        };
        assert!(!needs_fetch(&tool(json!({})), true));
        assert!(needs_fetch(&tool(json!({"outputOmitted": true})), true));
        assert!(needs_fetch(&tool(json!({"input": {"summary": "x", "truncated": true}})), true));
        let command =
            item(json!({"id": "c", "type": "command_execution", "status": "completed", "outputOmitted": true}));
        assert!(!needs_fetch(&command, true));
        assert!(needs_fetch(&command, false));
    }

    #[test]
    fn tool_values_format_like_the_web() {
        assert_eq!(
            format_value(&json!([{"type": "text", "text": "a"}, {"type": "text", "text": "b"}])).unwrap(),
            "a\n\nb"
        );
        assert_eq!(format_value(&json!({"k": 1})).unwrap(), "{\n  \"k\": 1\n}");
        assert_eq!(format_value(&json!("")), None);
        let lines = arg_lines(&json!({"path": "a.rs", "limit": 5, "empty": "", "none": null})).unwrap();
        assert_eq!(
            lines,
            [("path", "a.rs"), ("limit", "5"), ("empty", "\"\""), ("none", "null")]
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
        );
        assert!(arg_lines(&json!({"summary": "x", "truncated": true})).is_none());
        assert_eq!(control_pictures("a\u{1}b\tc"), "a\u{2401}b\tc");
    }

    #[test]
    fn viewed_images_preview_only_image_files() {
        let viewed = |value: Value| {
            viewed_image_path(&crate::work_log::entry_from_item(&Rc::new(item(value)))).map(str::to_owned)
        };
        let tool = |extra: Value| {
            let mut value = json!({"id": "i", "type": "dynamic_tool", "status": "completed", "toolName": "view_image"});
            value.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            value
        };
        assert_eq!(viewed(tool(json!({"viewedImagePath": " shots/a.PNG "}))).as_deref(), Some("shots/a.PNG"));
        assert_eq!(viewed(tool(json!({"viewedImagePath": "notes.txt"}))), None);
        assert_eq!(viewed(tool(json!({"viewedImagePath": "a\nb.png"}))), None);
        assert_eq!(viewed(tool(json!({}))), None);
    }

    #[test]
    fn task_lists_list_every_step_with_its_status() {
        let tasks = item(json!({"id": "t", "type": "todo_list", "status": "running", "steps": [
            {"text": "Read", "status": "completed"}, {"text": "Write", "status": "running"},
            {"text": "Ship", "status": "pending"},
        ]}));
        assert_eq!(todo_steps(&tasks).as_deref(), Some("✓ Read\n◉ Write\n○ Ship"));
        let empty = item(json!({"id": "t", "type": "todo_list", "status": "running", "steps": []}));
        assert_eq!(todo_steps(&empty), None);
    }

    #[test]
    fn changes_searches_and_questions_open_onto_their_details() {
        let opens = |value: Value| has_inspector(&crate::work_log::entry_from_item(&Rc::new(item(value))));
        let base = |kind: &str, extra: Value| {
            let mut value = json!({"id": "i", "type": kind, "status": "completed"});
            value.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            value
        };
        assert!(opens(base("file_change", json!({"fileName": "a.rs"}))));
        assert!(opens(base("file_search", json!({"pattern": "fn main"}))));
        assert!(!opens(base("file_search", json!({"pattern": " ", "results": []}))));
        assert!(opens(base("web_search", json!({"results": [{"url": "https://a.dev"}]}))));
        assert!(!opens(base("web_search", json!({"patterns": []}))));
        let asked = base(
            "user_input_request",
            json!({"questions": [
                {"question": "Which?", "options": [{"label": "A"}, {"label": "B"}]},
                {"question": "Why?", "options": []},
            ]}),
        );
        assert!(opens(asked.clone()));
        assert_eq!(questions_text(&item(asked)).as_deref(), Some("Which?\n• A\n• B\n\nWhy?"));
        assert!(!opens(base("user_input_request", json!({"questions": []}))));

        let change = item(base(
            "file_change",
            json!({"fileName": "a.rs", "changes": [
                {"operation": "update", "path": "/repo/src/a.rs"},
                {"operation": "rename", "oldPath": "old.rs", "path": "new.rs", "fileType": "text", "mimeType": "text/x-rust"},
            ]}),
        ));
        assert_eq!(
            file_change_lines(&change, Some("/repo")),
            ["update repo/src/a.rs", "rename old.rs → repo/new.rs (text, text/x-rust)"]
        );
        let found = json!({"fileName": "/repo/src/a.rs", "line": 4, "column": 2});
        assert_eq!(location(&found, Some("/repo")), "repo/src/a.rs:4:2");
        assert_eq!(web_link("//a.dev/x").as_deref(), Some("https://a.dev/x"));
        assert_eq!(web_link("javascript:alert(1)"), None);
    }

    #[test]
    fn tool_outputs_count_their_raster_images() {
        let tool =
            |output: Value| item(json!({"id": "i", "type": "dynamic_tool", "status": "completed", "output": output}));
        let png = json!({"type": "image", "mimeType": "IMAGE/PNG", "data": "x"});
        let svg = json!({"type": "image", "mimeType": "image/svg+xml"});
        let sourced = json!({"type": "image", "source": {"type": "base64", "media_type": "image/webp"}});
        let linked = json!({"type": "image", "source": {"type": "url", "media_type": "image/png"}});
        assert_eq!(tool_output_image_count(&tool(json!([png, svg, sourced, linked, {"type": "text"}]))), 2);
        assert_eq!(tool_output_image_count(&tool(json!({"content": [png]}))), 1);
        assert_eq!(tool_output_image_count(&tool(png.clone())), 1);
        assert_eq!(tool_output_image_count(&tool(json!(vec![png.clone(); 10]))), 8);
        let omitted =
            json!({"id": "i", "type": "dynamic_tool", "status": "completed", "outputOmitted": true, "output": [png]});
        assert_eq!(tool_output_image_count(&item(omitted)), 0);
    }

    #[test]
    fn argument_lines_mark_each_key() {
        let tool = item(
            json!({"id": "i", "type": "dynamic_tool", "status": "completed", "input": {"path": "a.rs", "limit": 5}}),
        );
        let Some(ToolInput::Lines { text, keys }) = ToolText::new(Rc::new(tool)).input else { panic!("lines") };
        assert_eq!(&*text, "path a.rs\nlimit 5");
        assert_eq!(keys.iter().map(|key| &text[key.clone()]).collect::<Vec<_>>(), ["path", "limit"]);
    }
}
