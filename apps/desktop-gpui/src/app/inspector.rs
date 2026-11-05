//! What an expanded tool entry shows (`V2ItemInspector.tsx`, `CommandOutputPanel.tsx`):
//! a command with its streamed output, or a tool's input and output. Output is
//! loaded only while its entry is open.

use std::collections::HashMap;
use std::rc::Rc;

use gpui_kit::component::v_flex;
use gpui_kit::{
    AnyElement, App, Context, ElementId, InteractiveElement as _, IntoElement, ParentElement as _, ScrollHandle,
    SharedString, StatefulInteractiveElement as _, Styled as _, Task, WeakEntity, div, px, relative,
};
use serde_json::{Value, json};

use super::Workspace;
use crate::model::TurnItem;
use crate::rpc::StreamEvent;
use crate::terminal_text::TerminalText;
use crate::theme::{FONT_MONO, Tokens};
use crate::timeline::{Row, WorkRow, WorkRowKind};
use crate::work_log::Entry;

/// Output of the open entries, by entry id.
#[derive(Default)]
pub(super) struct Details(HashMap<SharedString, Detail>);

enum Detail {
    /// `orchestration.subscribeCommandOutput`, folded as a terminal shows it.
    Output {
        output: TerminalText,
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

/// `"live"` until the item settles, then its `updatedAt`.
fn revision(item: &TurnItem) -> SharedString {
    match item.status.as_str() {
        "idle" | "pending" | "running" | "waiting" => "live".into(),
        _ => item.str("updatedAt").unwrap_or_default().to_owned().into(),
    }
}

/// Whether an entry opens onto the inspector (`turnItemHasDetail`, for the types it covers).
pub(super) fn has_inspector(entry: &Entry) -> bool {
    let item = &entry.item.0;
    match item.item_type.as_str() {
        "command_execution" => true,
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

fn open_entries(rows: &[Row]) -> impl Iterator<Item = &Rc<Entry>> {
    rows.iter().flat_map(|row| -> Vec<&Rc<Entry>> {
        match row {
            Row::Work(WorkRow { kind: WorkRowKind::Single { entry, expanded: true, .. }, .. }) => vec![entry],
            Row::Work(WorkRow { kind: WorkRowKind::Details { entries }, .. }) => {
                entries.iter().filter(|(_, open)| *open).map(|(entry, _)| entry).collect()
            }
            _ => Vec::new(),
        }
    })
}

impl Workspace {
    /// Starts loading the output of entries that just opened and drops it for closed ones.
    pub(super) fn sync_details(&mut self, cx: &mut Context<Self>) {
        let streams_output = self.server_info.capability("commandOutputStreaming");
        let open: Vec<Rc<Entry>> = open_entries(&self.rows).filter(|entry| has_inspector(entry)).cloned().collect();
        self.details.0.retain(|id, _| open.iter().any(|entry| entry.id == *id));
        for entry in open {
            let item = entry.item.0.clone();
            let streams = item.item_type == "command_execution" && streams_output;
            match self.details.0.get(&entry.id) {
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
                            if let Some(Detail::Output { output, loaded, running, error, scroll, .. }) =
                                this.details.0.get_mut(&id)
                            {
                                let following = scroll.offset().y.abs() + px(8.) >= scroll.max_offset().y;
                                for event in events {
                                    match event {
                                        StreamEvent::Item(frame) => apply_frame(output, running, &frame),
                                        StreamEvent::End(Err(failure)) => *error = Some(failure.to_string().into()),
                                        StreamEvent::End(Ok(())) => *running = false,
                                    }
                                }
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
                        if let Some(Detail::Item { fetched: slot, .. }) = this.details.0.get_mut(&id) {
                            *slot = fetched;
                            cx.notify();
                        }
                    });
                });
                Detail::Item { revision: revision(&item), fetched: Fetch::Loading, _task }
            } else {
                continue;
            };
            self.details.0.insert(entry.id.clone(), detail);
        }
    }

    /// The thread and item an item was projected from, for the item RPCs.
    fn source_of(&self, item_id: &str) -> Option<(String, String)> {
        let projection = self.thread.as_ref()?.state.projection.as_ref()?;
        let row = projection.visible_turn_items.iter().find(|row| row.item.id == item_id)?;
        Some((row.source_thread_id.clone(), row.source_item_id.clone()))
    }
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
    let detail = workspace.details.0.get(&entry.id);
    let fetched = match detail {
        Some(Detail::Item { fetched: Fetch::Loaded(Some(item)), .. }) if item.item_type == entry.item.0.item_type => {
            Some(item.clone())
        }
        _ => None,
    };
    let item = fetched.unwrap_or_else(|| entry.item.0.clone());
    let foreground = gpui_kit::Hsla { a: t.foreground.a * 0.85, ..t.foreground };
    let mut body = v_flex().gap(px(6.));
    if item.item_type == "command_execution" {
        if let Some(command) = item.str("input").map(str::trim).filter(|c| !c.is_empty()) {
            body = body.child(mono().text_color(foreground).child(control_pictures(command)));
        }
    } else if let Some(input) = item.raw.get("input") {
        if let Some(lines) = arg_lines(input) {
            body = body.child(mono().text_color(foreground).children(lines.into_iter().map(|(key, value)| {
                div().child(gpui_kit::StyledText::new(format!("{key} {value}")).with_highlights([(
                    0..key.len(),
                    gpui_kit::HighlightStyle { color: Some(t.muted_foreground), ..Default::default() },
                )]))
            })));
        } else if let Some(text) = format_value(input.get("summary").unwrap_or(input)) {
            body = body.child(structured(&format!("args:{}", entry.id), text, t));
        }
    }
    let output_slot: Option<AnyElement> = match detail {
        Some(Detail::Output { output, loaded, running, error, scroll, .. }) => {
            let text = output.text_content();
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
                                .child(text),
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
        _ => match item.raw.get("output").and_then(format_value) {
            Some(text) => Some(structured(&format!("result:{}", entry.id), text, t).into_any_element()),
            None if matches!(detail, Some(Detail::Item { .. })) => {
                Some(note("No output.", t.muted_foreground).into_any_element())
            }
            None => None,
        },
    };
    body = body.children(output_slot);
    if let Some(code) = item.raw.get("exitCode").and_then(Value::as_i64).filter(|code| *code != 0) {
        body = body.child(mono().text_color(t.error).child(format!("exit {code}")));
    }
    Some(body.into_any_element())
}

/// `StructuredValue`: a scrolling block of muted mono text.
fn structured(id: &str, text: String, t: &Tokens) -> gpui_kit::Stateful<gpui_kit::Div> {
    mono()
        .id(ElementId::Name(id.to_owned().into()))
        .max_h(px(320.))
        .overflow_y_scroll()
        .text_color(t.muted_foreground)
        .child(text)
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
        assert_eq!((output.text_content().as_str(), output.truncated, running), ("a\nb", true, true));
        apply_frame(
            &mut output,
            &mut running,
            &json!({"kind": "replace", "text": "done", "truncated": false, "running": false}),
        );
        assert_eq!((output.text_content().as_str(), output.truncated, running), ("done", true, false));
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
}
