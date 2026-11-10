//! The context records behind a prompt's `t3-context://` chips (`composerContextRecords.ts`,
//! `packages/shared/src/composerContextReferences.ts`), and the clipboard fragment that carries
//! them from one draft to another (`composerContextClipboard.ts`, `useComposerContextImport.ts`).

use std::collections::{HashMap, HashSet};

use serde_json::{Value, json};

const HREF_PREFIX: &str = "t3-context://v1/";
/// `COMPOSER_CONTEXT_MAX_RECORDS`.
const MAX_RECORDS: usize = 200;
/// `MAX_FRAGMENT_CHARS`, in UTF-16 units.
const MAX_FRAGMENT_CHARS: usize = 16_000_000;

/// `clampContextText`'s marker.
const TRUNCATION_MARKER: &str = "\n… truncated …";

/// `clampContextText`: `value` cut to `max` UTF-16 units, marker included, at a char boundary.
pub fn clamp_text(value: &str, max: usize) -> String {
    if value.encode_utf16().count() <= max {
        return value.to_owned();
    }
    let budget = max.saturating_sub(TRUNCATION_MARKER.encode_utf16().count());
    let mut units = 0;
    let end = value.char_indices().find(|(_, c)| {
        units += c.len_utf16();
        units > budget
    });
    let mut clamped = value[..end.map_or(value.len(), |(at, _)| at)].to_owned();
    clamped.push_str(TRUNCATION_MARKER);
    clamped
}

/// `sanitizeComposerContextLabel`: one line of at most 200 chars, `kind` when nothing is left.
pub fn sanitize_label(label: &str, kind: &str) -> String {
    let spaced: String =
        label.chars().map(|c| if matches!(c, '[' | ']' | '\\' | '\r' | '\n') { ' ' } else { c }).collect();
    let label: String = spaced.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(200).collect();
    if label.is_empty() { kind.to_owned() } else { label }
}

/// `CONTEXT_ID_PATTERN`: `^[a-z0-9_-]{1,128}$`, any case.
fn valid_id(id: &str) -> bool {
    (1..=128).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// `CONTEXT_KIND_PATTERN`: `^[a-z][a-z0-9-]{0,39}$`.
fn valid_kind(kind: &str) -> bool {
    kind.len() <= 40
        && kind.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
        && kind.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Two FNV-1a passes over the UTF-16 units, forwards and backwards, as the web hashes.
fn fnv1a64(value: &str) -> String {
    let units: Vec<u16> = value.encode_utf16().collect();
    let (mut forward, mut reverse) = (0x811c_9dc5_u32, 0x9dc5_811c_u32);
    for (index, unit) in units.iter().enumerate() {
        forward = (forward ^ u32::from(*unit)).wrapping_mul(0x0100_0193);
        reverse = (reverse ^ u32::from(units[units.len() - 1 - index])).wrapping_mul(0x0100_0193);
    }
    format!("{forward:08x}{reverse:08x}")
}

/// `toComposerContextId`: a producer's id as is when it is a valid context id, else a slug and digest.
pub fn context_id(producer: &str) -> String {
    if valid_id(producer) {
        return producer.to_owned();
    }
    let (mut slug, mut in_run) = (String::new(), false);
    for c in producer.chars() {
        let kept = c.is_ascii_alphanumeric() || c == '_' || c == '-';
        if kept || !in_run {
            slug.push(if kept { c } else { '-' });
        }
        in_run = !kept;
    }
    let slug: String = slug.trim_matches('-').chars().take(48).collect();
    format!("{}-{}", if slug.is_empty() { "ctx" } else { &slug }, fnv1a64(producer))
}

/// `toKindScopedComposerContextId`.
pub fn kind_scoped_id(kind: &str, producer: &str) -> String {
    context_id(&format!("{kind}_{producer}"))
}

/// Each context link in `text` as `collectComposerContextReferences` finds it
/// ([`crate::composer_text::context_references`]): the whole link's byte range, and its kind and
/// id as slices of `text`, which end the link as `kind/id)`.
fn references(text: &str) -> impl Iterator<Item = (std::ops::Range<usize>, &str, &str)> {
    crate::composer_text::context_references(text).map(move |(range, kind, id)| {
        let id_end = range.end - 1;
        let id_start = id_end - id.len();
        let kind_start = id_start - 1 - kind.len();
        (range, &text[kind_start..id_start - 1], &text[id_start..id_end])
    })
}

/// The context ids `text` references, once each, in order (`collectComposerContextReferences`).
pub fn referenced_ids(text: &str) -> Vec<&str> {
    let mut ids: Vec<&str> = Vec::new();
    if !text.contains(HREF_PREFIX) {
        return ids;
    }
    for (_, _, id) in references(text) {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids
}

/// `stripInlineContextReferences`: `text` without its context links.
pub fn strip_references(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    for (range, _, _) in references(text) {
        out.push_str(&text[copied..range.start]);
        copied = range.end;
    }
    out.push_str(&text[copied..]);
    out
}

/// `ensureInlineContextReferences`: `prompt` with a chip after it for each record it doesn't
/// reference yet, a space apart.
pub fn ensure_references(prompt: &str, records: &[Value]) -> String {
    let mut referenced: HashSet<String> = referenced_ids(prompt).into_iter().map(str::to_owned).collect();
    let mut out = prompt.to_owned();
    for record in records {
        let (Some(kind), Some(id)) = (record["kind"].as_str(), id_of(record)) else { continue };
        if !referenced.insert(id.to_owned()) {
            continue;
        }
        if !out.is_empty() && !out.ends_with([' ', '\n', '\t', '\r']) {
            out.push(' ');
        }
        out.push_str(&crate::composer_text::context_link(kind, id, record["label"].as_str().unwrap_or_default()));
        out.push(' ');
    }
    out
}

/// `text` with each reference to an id in `ids` pointing at its new id.
pub fn rewrite_ids(text: &str, ids: &HashMap<String, String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    for (range, _, id) in references(text) {
        if let Some(new) = ids.get(id).filter(|new| *new != id) {
            let id_start = range.end - 1 - id.len();
            out.push_str(&text[copied..id_start]);
            out.push_str(new);
            copied = range.end - 1;
        }
    }
    out.push_str(&text[copied..]);
    out
}

fn id_of(record: &Value) -> Option<&str> {
    record["contextId"].as_str()
}

/// One draft's context records, by context id; a record goes with a message while its chip is
/// in the text.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ContextRecords(Vec<Value>);

impl ContextRecords {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn get(&self, id: &str) -> Option<&Value> {
        self.0.iter().find(|record| id_of(record) == Some(id))
    }

    pub fn iter(&self) -> impl Iterator<Item = &Value> {
        self.0.iter()
    }

    /// Adds `record`, replacing one with its context id. Image and file records stay with
    /// their attachments, which build them.
    pub fn insert(&mut self, record: Value) {
        if matches!(record["kind"].as_str(), Some("image" | "file")) {
            return;
        }
        let Some(id) = id_of(&record) else { return };
        match self.0.iter().position(|held| id_of(held) == Some(id)) {
            Some(at) => self.0[at] = record,
            None => self.0.push(record),
        }
    }

    pub fn remove(&mut self, id: &str) -> Option<Value> {
        let at = self.0.iter().position(|record| id_of(record) == Some(id))?;
        Some(self.0.remove(at))
    }

    /// Replaces the record with `record`'s context id; `false` when none is held.
    pub fn replace(&mut self, record: Value) -> bool {
        let held = id_of(&record).and_then(|id| self.0.iter().position(|held| id_of(held) == Some(id)));
        held.map(|at| self.0[at] = record).is_some()
    }

    pub fn extend(&mut self, records: impl IntoIterator<Item = Value>) {
        for record in records {
            self.insert(record);
        }
    }

    /// The records `text` references, in the order it first does.
    pub fn referenced(&self, text: &str) -> Vec<Value> {
        referenced_ids(text).into_iter().filter_map(|id| self.get(id).cloned()).collect()
    }

    /// Drops the records `text` no longer references.
    pub fn retain_referenced(&mut self, text: &str) {
        if !self.0.is_empty() {
            let live = referenced_ids(text);
            self.0.retain(|record| id_of(record).is_some_and(|id| live.contains(&id)));
        }
    }

    /// Sets aside the records whose chips left `text`, and brings back set-aside ones whose chips
    /// an undo restored (`useRetainedContextPayloads`); `true` when any moved.
    pub fn reconcile(&mut self, retained: &mut ContextRecords, text: &str) -> bool {
        if self.0.is_empty() && retained.0.is_empty() {
            return false;
        }
        let live = referenced_ids(text);
        let referenced = |record: &Value| id_of(record).is_some_and(|id| live.contains(&id));
        let (back, still): (Vec<_>, Vec<_>) = std::mem::take(&mut retained.0).into_iter().partition(referenced);
        let (kept, gone): (Vec<_>, Vec<_>) = std::mem::take(&mut self.0).into_iter().partition(referenced);
        let moved = !back.is_empty() || !gone.is_empty();
        self.0 = kept;
        retained.0 = still;
        retained.extend(gone);
        self.extend(back);
        moved
    }
}

/// `ComposerContextClipboardFragment`: copied records and the machine and thread they came from.
#[derive(Clone, Debug, PartialEq)]
pub struct Fragment {
    pub environment_id: String,
    pub thread_id: Option<String>,
    pub records: Vec<Value>,
}

/// `encodeComposerContextFragment`; `None` past the size limit.
pub fn encode_fragment(fragment: &Fragment) -> Option<String> {
    let mut source = json!({"environmentId": fragment.environment_id});
    if let Some(thread_id) = &fragment.thread_id {
        source["threadId"] = thread_id.as_str().into();
    }
    let encoded = json!({"version": 1, "source": source, "records": fragment.records}).to_string();
    (encoded.encode_utf16().count() <= MAX_FRAGMENT_CHARS).then_some(encoded)
}

/// Whether `record` has the shape every record shares, plus the fields this app reads of
/// attachments; records that don't are dropped, as the web's forward-compatible decode does.
fn well_formed(record: &Value) -> bool {
    let kind = record["kind"].as_str().unwrap_or_default();
    let base = record["version"] == 1
        && valid_kind(kind)
        && id_of(record).is_some_and(valid_id)
        && record["label"].as_str().is_some_and(|label| label.chars().count() <= 200);
    let attachment = || {
        ["attachmentId", "name", "mimeType"]
            .iter()
            .all(|key| record[key].as_str().is_some_and(|s| !s.trim().is_empty()))
            && record["sizeBytes"].is_u64()
    };
    base && (!matches!(kind, "image" | "file") || attachment())
}

/// `decodeComposerContextFragment`; `None` for anything that is not a fragment.
pub fn decode_fragment(raw: &str) -> Option<Fragment> {
    if raw.is_empty() || raw.encode_utf16().count() > MAX_FRAGMENT_CHARS {
        return None;
    }
    let value: Value = serde_json::from_str(raw).ok()?;
    let environment_id = value["source"]["environmentId"].as_str().filter(|id| !id.trim().is_empty())?;
    let records = value["records"].as_array().filter(|records| records.len() <= MAX_RECORDS)?;
    (value["version"] == 1).then(|| Fragment {
        environment_id: environment_id.to_owned(),
        thread_id: value["source"]["threadId"].as_str().map(str::to_owned),
        records: records.iter().filter(|record| well_formed(record)).cloned().collect(),
    })
}

/// `isSameComposerContextPayload`: equal but for the label and context id.
pub fn same_payload(left: &Value, right: &Value) -> bool {
    let strip = |record: &Value| {
        let mut record = record.clone();
        if let Some(fields) = record.as_object_mut() {
            fields.remove("label");
            fields.remove("contextId");
        }
        record
    };
    left["kind"] == right["kind"] && strip(left) == strip(right)
}

/// A pasted record and what it becomes in the draft.
#[derive(Clone, Debug, PartialEq)]
pub enum Imported {
    /// A record the draft keeps, under the context id its chip now names.
    Record(Value),
    /// An attachment whose bytes still have to arrive, under a new local id.
    Attachment { record: Value, local_id: String },
}

/// `importContextRecords`: the pasted records the draft takes, and each pasted context id's
/// new id (a reference the draft refused keeps pointing where it did).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Import {
    pub ids: HashMap<String, String>,
    pub items: Vec<Imported>,
}

/// Plans the import of `records` pasted with `text` into a draft whose record for a context id
/// is `existing`'s. A record the draft already holds is reused; a different one under a taken
/// id gets a fresh id; attachments always arrive as new ones. `accepts` refuses kinds the
/// draft can't carry (a machine in an answer, a thread on a machine its agent can't read).
pub fn plan_import(
    text: &str,
    records: &[Value],
    existing: impl Fn(&str) -> Option<Value>,
    accepts: impl Fn(&Value) -> bool,
    mut fresh: impl FnMut() -> String,
) -> Import {
    let mut wanted: HashSet<&str> = referenced_ids(text).into_iter().collect();
    // An annotation brings its screenshot along (`readPastedComposerContext`).
    for record in records {
        if record["kind"] == "preview-annotation" && id_of(record).is_some_and(|id| wanted.contains(id)) {
            wanted.extend(record["screenshotContextId"].as_str());
        }
    }
    let mut pasted: Vec<&Value> = Vec::new();
    for record in records.iter().filter(|record| id_of(record).is_some_and(|id| wanted.contains(id))) {
        match pasted.iter().position(|held| id_of(held) == id_of(record)) {
            Some(at) => pasted[at] = record,
            None => pasted.push(record),
        }
    }
    let mut import = Import::default();
    for record in pasted {
        let (Some(kind), Some(id)) = (record["kind"].as_str(), id_of(record)) else { continue };
        let known = matches!(
            kind,
            "image"
                | "file"
                | "terminal"
                | "element"
                | "preview-annotation"
                | "review-comment"
                | "thread"
                | "environment"
        );
        if !known || !accepts(record) {
            continue;
        }
        let held = existing(id);
        if held.as_ref().is_some_and(|held| same_payload(held, record)) {
            import.ids.insert(id.to_owned(), id.to_owned());
            continue;
        }
        let new_id = match kind {
            "image" | "file" => {
                let local_id = fresh();
                let new_id = format!("{kind}_{local_id}");
                let record = with_id(record, &new_id);
                import.items.push(Imported::Attachment { record, local_id });
                new_id
            }
            // A machine's record follows from its chip.
            "environment" => id.to_owned(),
            _ => {
                let new_id = if held.is_some() { kind_scoped_id(kind, &fresh()) } else { id.to_owned() };
                import.items.push(Imported::Record(with_id(record, &new_id)));
                new_id
            }
        };
        import.ids.insert(id.to_owned(), new_id);
    }
    for item in &mut import.items {
        if let Imported::Record(record) = item
            && let Some(screenshot) = record["screenshotContextId"].as_str().and_then(|id| import.ids.get(id))
        {
            record["screenshotContextId"] = screenshot.as_str().into();
        }
    }
    import
}

fn with_id(record: &Value, id: &str) -> Value {
    let mut record = record.clone();
    record["contextId"] = id.into();
    record
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(kind: &str, id: &str) -> Value {
        json!({"version": 1, "kind": kind, "contextId": id, "label": id})
    }

    fn link(kind: &str, id: &str) -> String {
        crate::composer_text::context_link(kind, id, id)
    }

    #[test]
    fn producer_ids_fold_into_context_ids_like_the_web() {
        assert_eq!(kind_scoped_id("terminal", "t1"), "terminal_t1");
        // `toKindScopedComposerContextId("review-comment", "pr-reference:https://x/pull/7")`.
        assert_eq!(
            kind_scoped_id("review-comment", "pr-reference:https://x/pull/7"),
            "review-comment_pr-reference-https-x-pull-7-f457e4f5b7c61e88"
        );
        assert_eq!(context_id("::"), "ctx-980630f591da3738");
        assert_eq!(context_id("é-ü 🎉"), "ctx-fa845d6092a03653");
        assert_eq!(context_id("a-éb"), format!("a--b-{}", fnv1a64("a-éb")));
    }

    #[test]
    fn a_record_leaves_with_its_chip_and_comes_back_with_an_undo() {
        let mut records = ContextRecords::default();
        records.insert(record("review-comment", "rc1"));
        records.insert(record("terminal", "terminal_t1"));
        let mut retained = ContextRecords::default();
        let both = format!("a {} b {}", link("terminal", "terminal_t1"), link("review-comment", "rc1"));
        assert!(!records.reconcile(&mut retained, &both));
        let ids = |r: &[Value]| r.iter().map(|r| r["contextId"].as_str().unwrap().to_owned()).collect::<Vec<_>>();
        assert_eq!(ids(&records.referenced(&both)), ["terminal_t1", "rc1"], "in the order the text names them");

        let one = format!("a {}", link("terminal", "terminal_t1"));
        assert!(records.reconcile(&mut retained, &one));
        assert_eq!(ids(&records.referenced(&both)), ["terminal_t1"], "a deleted chip's record is not sent");
        assert!(retained.get("rc1").is_some());
        assert!(records.reconcile(&mut retained, &both), "undo brings the chip and its record back");
        assert!(records.get("rc1").is_some() && retained.is_empty());

        records.retain_referenced("no chips");
        assert!(records.is_empty());
    }

    #[test]
    fn references_need_a_whole_link() {
        let text = format!(
            "{} t3-context://v1/terminal/x) [a](t3-context://v1/Bad/x) [b](t3-context://v1/file/a b)",
            link("file", "file_1")
        );
        assert_eq!(referenced_ids(&text), ["file_1"]);
        let image = crate::composer_text::context_link("image", "image_1", "a.png");
        assert_eq!(strip_references(&format!("a {} b {image}!", link("file", "file_1"))), "a  b !");
        assert_eq!(strip_references("[x] (t3-context://v1/file/f)"), "[x] (t3-context://v1/file/f)");
    }

    #[test]
    fn missing_chips_go_after_the_prompt_once() {
        let held = link("review-comment", "rc1");
        let records = [record("review-comment", "rc1"), record("review-comment", "rc2"), record("terminal", "t")];
        assert_eq!(
            ensure_references(&format!("ask {held}"), &records[..2]),
            format!("ask {held} {} ", link("review-comment", "rc2"))
        );
        assert_eq!(
            ensure_references("line\n", &records[2..]),
            format!("line\n{} ", link("terminal", "t")),
            "a newline is space enough"
        );
        assert_eq!(ensure_references("", &[]), "");
    }

    #[test]
    fn a_fragment_round_trips_and_drops_malformed_records() {
        let attachment = json!({"version": 1, "kind": "file", "contextId": "file_a", "label": "a.txt",
            "attachmentId": "up1", "name": "a.txt", "mimeType": "text/plain", "sizeBytes": 3});
        let fragment = Fragment {
            environment_id: "env".into(),
            thread_id: Some("t1".into()),
            records: vec![record("review-comment", "rc1"), attachment],
        };
        let encoded = encode_fragment(&fragment).unwrap();
        assert_eq!(decode_fragment(&encoded), Some(fragment.clone()));

        let mut broken = serde_json::from_str::<Value>(&encoded).unwrap();
        broken["records"].as_array_mut().unwrap().extend([
            json!({"version": 2, "kind": "terminal", "contextId": "x", "label": ""}),
            json!({"version": 1, "kind": "file", "contextId": "file_b", "label": "b"}),
        ]);
        assert_eq!(decode_fragment(&broken.to_string()).unwrap().records, fragment.records);
        assert_eq!(decode_fragment("plain text"), None);
        assert_eq!(decode_fragment(r#"{"version":1,"source":{},"records":[]}"#), None);
    }

    #[test]
    fn a_paste_reuses_held_records_and_remints_colliding_ones() {
        let held = record("review-comment", "rc1");
        let mut changed = record("review-comment", "rc1");
        changed["text"] = "other".into();
        let image = json!({"version": 1, "kind": "image", "contextId": "image_l1", "label": "a.png",
            "attachmentId": "up1", "name": "a.png", "mimeType": "image/png", "sizeBytes": 3});
        let mut annotation = record("preview-annotation", "pa1");
        annotation["screenshotContextId"] = "image_l1".into();
        let text = format!(
            "{} {} {} {}",
            link("review-comment", "rc1"),
            link("terminal", "terminal_t1"),
            link("preview-annotation", "pa1"),
            link("environment", "environment_e1")
        );
        let pasted = [
            changed.clone(),
            record("terminal", "terminal_t1"),
            annotation,
            image,
            record("thread", "th1"),
            record("environment", "environment_e1"),
        ];
        let mut minted = 0;
        let import = plan_import(
            &text,
            &pasted,
            |id| (id == "rc1").then(|| held.clone()),
            |record| record["kind"] != "environment",
            || {
                minted += 1;
                format!("n{minted}")
            },
        );
        assert_eq!(import.ids["rc1"], "review-comment_n1", "a different record under a taken id is reminted");
        assert_eq!(import.ids["terminal_t1"], "terminal_t1");
        assert_eq!(import.ids["image_l1"], "image_n2", "the annotation's screenshot comes along as a new attachment");
        assert!(!import.ids.contains_key("th1"), "a record the text doesn't name stays behind");
        assert!(!import.ids.contains_key("environment_e1"), "a refused kind keeps its reference");
        let Imported::Record(annotation) = &import.items[2] else { panic!() };
        assert_eq!(annotation["screenshotContextId"], "image_n2");
        assert!(
            matches!(&import.items[3], Imported::Attachment { local_id, record } if local_id == "n2" && record["contextId"] == "image_n2")
        );

        let same = plan_import(
            &link("review-comment", "rc1"),
            std::slice::from_ref(&held),
            |_| Some(held.clone()),
            |_| true,
            || unreachable!(),
        );
        assert_eq!((same.ids["rc1"].as_str(), same.items.len()), ("rc1", 0), "the draft's own record is reused");

        let rewritten = rewrite_ids(&text, &import.ids);
        assert!(rewritten.contains("(t3-context://v1/review-comment/review-comment_n1)"));
        assert!(rewritten.contains("(t3-context://v1/terminal/terminal_t1)"));
    }
}
