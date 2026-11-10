//! The composer's text rules: completion triggers (`detectComposerTrigger` in
//! `apps/web/src/composer-logic.ts`), the file links and skills a pick inserts
//! (`packages/shared/src/composerTrigger.ts`, `composerInlineTokens.ts`) and
//! prompt history (`composerPromptHistory.ts`). Offsets are UTF-8 bytes.

use std::fmt::Write as _;
use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TriggerKind {
    Path,
    Skill,
    SlashCommand,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Trigger {
    pub kind: TriggerKind,
    pub query: String,
    pub range: Range<usize>,
}

fn is_space(c: char) -> bool {
    matches!(c, ' ' | '\n' | '\t' | '\r')
}

/// `\p{Sc}`, for the symbols a keyboard types.
pub fn is_currency(c: char) -> bool {
    matches!(c, '$' | '€' | '£' | '¥' | '₹' | '¢' | '₩' | '₽' | '₺' | '₪' | '₫' | '฿' | '₴' | '₦' | '₱')
}

/// The completion the caret is in, if any.
pub fn detect(text: &str, cursor: usize) -> Option<Trigger> {
    let cursor = text.floor_char_boundary(cursor);
    let start = text[..cursor].rfind(is_space).map_or(0, |i| i + 1);
    let token = &text[start..cursor];
    let trigger = |kind, query: &str| Some(Trigger { kind, query: query.to_owned(), range: start..cursor });
    if let Some(query) = token.strip_prefix('/') {
        // A second slash in the word makes it a path, even after the caret.
        let end = text[cursor..].find(is_space).map_or(text.len(), |i| cursor + i);
        if text[start + 1..end].contains('/') {
            return None;
        }
        return trigger(TriggerKind::SlashCommand, query);
    }
    if let Some(symbol) = token.chars().next().filter(|c| is_currency(*c)) {
        let query = &token[symbol.len_utf8()..];
        return if is_currency_amount(query) { None } else { trigger(TriggerKind::Skill, query) };
    }
    token.strip_prefix('@').and_then(|query| trigger(TriggerKind::Path, query))
}

/// `$5`, `€9.99` and `$10k.` are money, not skills:
/// `^\.?[0-9][0-9,._]*(?:[kKmMbBtT]|[eE][0-9]+)?[.,;:!?)]*$`.
fn is_currency_amount(query: &str) -> bool {
    let rest = query.strip_prefix('.').unwrap_or(query);
    let mut chars = rest.chars().peekable();
    if !chars.next().is_some_and(|c| c.is_ascii_digit()) {
        return false;
    }
    while chars.next_if(|c| c.is_ascii_digit() || matches!(c, ',' | '.' | '_')).is_some() {}
    match chars.peek() {
        Some('k' | 'K' | 'm' | 'M' | 'b' | 'B' | 't' | 'T') => {
            chars.next();
        }
        Some('e' | 'E') => {
            chars.next();
            if chars.next_if(char::is_ascii_digit).is_none() {
                return false;
            }
            while chars.next_if(char::is_ascii_digit).is_some() {}
        }
        _ => {}
    }
    chars.all(|c| matches!(c, '.' | ',' | ';' | ':' | '!' | '?' | ')'))
}

/// A replacement ending in a space swallows the space already after the range.
pub fn extend_for_trailing_space(text: &str, range: Range<usize>, replacement: &str) -> Range<usize> {
    if replacement.ends_with(' ') && text[range.end..].starts_with(' ') { range.start..range.end + 1 } else { range }
}

fn basename(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// `[name](path)`, as `serializeComposerFileLink` writes a picked file.
pub fn file_link(path: &str) -> String {
    let label = basename(path).replace('\\', "\\\\").replace('[', "\\[").replace(']', "\\]");
    format!("[{label}]({})", encode_destination(path))
}

/// `encodeURI`, then `( ) # ? \` escaped too.
fn encode_destination(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for byte in path.bytes() {
        let c = byte as char;
        if byte.is_ascii_alphanumeric() || ";,/:@&=+$-_.!~*'".contains(c) {
            out.push(c);
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

fn decode_destination(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            out.push(u8::from_str_radix(text.get(i + 1..i + 3)?, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// What a chip in the editor stands for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Chip {
    File {
        path: String,
    },
    Skill {
        name: String,
    },
    /// `[label](t3-context://v1/{kind}/{id})`: an attachment or other context record.
    Context {
        kind: String,
        id: String,
        label: String,
    },
    /// `model:<instance>/<model>`: a model named for the agent; `label` is its display name.
    Model {
        instance_id: String,
        model: String,
        label: String,
    },
}

impl Chip {
    pub fn id(&self) -> String {
        match self {
            Chip::File { path } => format!("file:{path}"),
            Chip::Skill { name } => format!("skill:{name}"),
            Chip::Context { kind, id, .. } => format!("context:{kind}/{id}"),
            Chip::Model { instance_id, model, .. } => model_mention(instance_id, model),
        }
    }

    pub fn label(&self) -> &str {
        match self {
            Chip::File { path } => basename(path),
            Chip::Skill { name } => name,
            Chip::Context { label, .. } | Chip::Model { label, .. } => label,
        }
    }

    /// The chip an editor token id names; context and model chips come back without their label.
    pub fn from_id(id: &str) -> Option<Self> {
        if let Some(path) = id.strip_prefix("file:") {
            return Some(Chip::File { path: path.to_owned() });
        }
        if let Some(name) = id.strip_prefix("skill:") {
            return Some(Chip::Skill { name: name.to_owned() });
        }
        if let Some((instance_id, model)) = id.strip_prefix(MODEL_PREFIX).and_then(|rest| rest.split_once('/')) {
            return Some(Chip::Model {
                instance_id: instance_id.to_owned(),
                model: model.to_owned(),
                label: String::new(),
            });
        }
        let (kind, id) = id.strip_prefix("context:")?.split_once('/')?;
        Some(Chip::Context { kind: kind.to_owned(), id: id.to_owned(), label: String::new() })
    }
}

/// `formatComposerContextReference`: `[label](t3-context://v1/{kind}/{id})`, `!` first for images.
pub fn context_link(kind: &str, id: &str, label: &str) -> String {
    let label: String =
        label.chars().map(|c| if matches!(c, '[' | ']' | '\\' | '\n' | '\r') { ' ' } else { c }).collect();
    let label = label.split_whitespace().collect::<Vec<_>>().join(" ");
    let label = if label.is_empty() { kind.to_owned() } else { label.chars().take(200).collect() };
    format!("{}[{label}]({CONTEXT_PREFIX}{kind}/{id})", if kind == "image" { "!" } else { "" })
}

const CONTEXT_PREFIX: &str = "t3-context://v1/";

/// `collectInlineContextIds`: the chip id of each context link anywhere in `text`, once, chip or
/// not (fenced code included).
pub fn context_reference_ids(text: &str) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    if !text.contains(CONTEXT_PREFIX) {
        return ids;
    }
    let mut at = 0;
    while let Some(open) = text[at..].find('[') {
        let start = at + open;
        match parse_context_link(&text[start..]).filter(|(_, chip)| !chip.label().contains('\n')) {
            Some((len, chip)) => {
                let id = chip.id();
                if !ids.contains(&id) {
                    ids.push(id);
                }
                at = start + len;
            }
            None => at = start + 1,
        }
    }
    ids
}

/// `removeInlineContextReference`: the byte ranges that remove every reference to chip `id` from
/// `text`, chip or not, each with one neighbouring space (the one after it, else the one before),
/// last first so they can be applied in order.
pub fn context_reference_removals(text: &str, id: &str) -> Vec<Range<usize>> {
    let mut found: Vec<Range<usize>> = Vec::new();
    if !text.contains(CONTEXT_PREFIX) {
        return found;
    }
    let mut at = 0;
    while let Some(open) = text[at..].find('[') {
        let start = at + open;
        match parse_context_link(&text[start..]).filter(|(_, chip)| !chip.label().contains('\n')) {
            Some((len, chip)) => {
                let end = start + len;
                if chip.id() == id {
                    let range = if text[end..].starts_with(' ') {
                        start..end + 1
                    } else if text[..start].ends_with(' ') {
                        start - 1..end
                    } else {
                        start..end
                    };
                    found.push(range);
                }
                at = end;
            }
            None => at = start + 1,
        }
    }
    found.reverse();
    found
}

/// The file links and known skills in `text` that render as chips: each must
/// start the text or follow whitespace, and be followed by whitespace
/// (`FILE_LINK_TOKEN_REGEX`, `SKILL_TOKEN_REGEX`).
pub fn chips(text: &str, is_skill: impl Fn(&str) -> bool) -> Vec<(Range<usize>, Chip)> {
    let mut found = Vec::new();
    let mut i = 0;
    while i < text.len() {
        let at_word_start = i == 0 || text[..i].ends_with(is_space);
        let rest = &text[i..];
        let next = rest.chars().next().map_or(1, char::len_utf8);
        if !at_word_start {
            i += next;
            continue;
        }
        let parsed = if let Some(found) = parse_context_link(rest) {
            Some(found)
        } else if rest.starts_with('[') {
            parse_file_link(rest).map(|(len, path)| (len, Chip::File { path }))
        } else if rest.starts_with(is_currency) {
            parse_skill(&rest[next..])
                .filter(|name| is_skill(name))
                .map(|name| (next + name.len(), Chip::Skill { name }))
        } else {
            None
        };
        match parsed {
            Some((len, chip))
                if text[i + len..].starts_with(is_space)
                    || (matches!(chip, Chip::Context { .. }) && i + len == text.len()) =>
            {
                found.push((i..i + len, chip));
                i += len;
            }
            _ => i += next,
        }
    }
    found
}

/// [`chips`], plus the `model:` references `model_label` knows, labelled by it. Like a
/// `$skill`, a reference chips only once whitespace follows it, so `gpt-6` cannot chip on the
/// way to `gpt-6.1` (`collectModelTokens`). In `rich` text, fenced code stays as written.
pub fn chips_with_models(
    text: &str,
    rich: bool,
    is_skill: impl Fn(&str) -> bool,
    model_label: impl Fn(&str, &str) -> Option<String>,
) -> ChipSpans {
    let mut found = chips(text, is_skill);
    let taken = found.len();
    for mention in model_mentions(text) {
        let range = mention.range;
        if !text[range.end..].starts_with(char::is_whitespace)
            || found[..taken].iter().any(|(other, _)| other.start < range.end && range.start < other.end)
        {
            continue;
        }
        let Some(label) = model_label(&mention.instance_id, &mention.model) else { continue };
        found.push((range, Chip::Model { instance_id: mention.instance_id, model: mention.model, label }));
    }
    if rich && text.contains(['`', '~']) {
        let code = crate::composer_edit::fenced_ranges(text);
        found.retain(|(range, _)| !code.iter().any(|block| block.start < range.end && range.start < block.end));
    }
    found.sort_by_key(|(range, _)| range.start);
    found
}

/// `skillChipReplacements`, once the known skills or models change: which `tokens` (each its
/// range, token id and label) go back to text, and which runs of `text` become skill or model
/// chips. Other chips stay; a model chip whose name changed comes back relabelled. In `rich`
/// text, one inside fenced code goes back to text.
pub fn reconcile_chips(
    text: &str,
    rich: bool,
    tokens: &[(Range<usize>, &str, &str)],
    is_skill: impl Fn(&str) -> bool,
    model_label: impl Fn(&str, &str) -> Option<String>,
) -> (Vec<Range<usize>>, ChipSpans) {
    let code = if rich && text.contains(['`', '~']) { crate::composer_edit::fenced_ranges(text) } else { Vec::new() };
    let in_code = |range: &Range<usize>| code.iter().any(|block| block.start < range.end && range.start < block.end);
    let stale: Vec<Range<usize>> = tokens
        .iter()
        .filter(|(range, id, label)| match Chip::from_id(id) {
            Some(Chip::Skill { name }) => in_code(range) || !is_skill(&name),
            Some(Chip::Model { instance_id, model, .. }) => {
                in_code(range) || model_label(&instance_id, &model).as_deref() != Some(*label)
            }
            _ => false,
        })
        .map(|(range, ..)| range.clone())
        .collect();
    let kept: Vec<&Range<usize>> = tokens.iter().map(|(range, ..)| range).filter(|r| !stale.contains(r)).collect();
    let fresh = chips_with_models(text, rich, &is_skill, &model_label)
        .into_iter()
        .filter(|(range, chip)| {
            matches!(chip, Chip::Skill { .. } | Chip::Model { .. })
                && !kept.iter().any(|other| other.start < range.end && range.start < other.end)
        })
        .collect();
    (stale, fresh)
}

/// [`reconcile_chips`] after an undo or redo: only chips it brought back for a skill or model
/// that is gone return to text (a renamed model's relabelled); plain text stays as it was.
pub fn reconcile_restored_chips(
    text: &str,
    rich: bool,
    tokens: &[(Range<usize>, &str, &str)],
    is_skill: impl Fn(&str) -> bool,
    model_label: impl Fn(&str, &str) -> Option<String>,
) -> (Vec<Range<usize>>, ChipSpans) {
    let (stale, mut fresh) = reconcile_chips(text, rich, tokens, is_skill, model_label);
    fresh.retain(|(range, _)| stale.contains(range));
    (stale, fresh)
}

/// Rich text turned on or off: the web remounts its editor, rebuilding every chip from the text
/// (`buildDocJson`). Which `tokens` go back to text and which runs become chips, so the editor
/// ends with exactly [`chips_with_models`]'s; a chip already in place stays as it is.
pub fn remount_chips(
    text: &str,
    rich: bool,
    tokens: &[(Range<usize>, &str, &str)],
    is_skill: impl Fn(&str) -> bool,
    model_label: impl Fn(&str, &str) -> Option<String>,
) -> (Vec<Range<usize>>, ChipSpans) {
    let rebuilt = chips_with_models(text, rich, is_skill, model_label);
    let same = |(range, id, label): &(Range<usize>, &str, &str), (other, chip): &(Range<usize>, Chip)| {
        range == other && *id == chip.id() && (!matches!(chip, Chip::Model { .. }) || *label == chip.label())
    };
    let stale = tokens.iter().filter(|token| !rebuilt.iter().any(|chip| same(token, chip)));
    let stale = stale.map(|(range, ..)| range.clone()).collect();
    let fresh = rebuilt.into_iter().filter(|chip| !tokens.iter().any(|token| same(token, chip))).collect();
    (stale, fresh)
}

const MODEL_PREFIX: &str = "model:";

/// `formatComposerModelMention`: the prompt text naming a provider instance's model for the
/// agent. No `@`, which would read as a file mention.
pub fn model_mention(instance_id: &str, model: &str) -> String {
    format!("{MODEL_PREFIX}{instance_id}/{model}")
}

/// A `model:<instance>/<model>` reference and where it sits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelMention {
    pub instance_id: String,
    pub model: String,
    pub range: Range<usize>,
}

/// `collectComposerModelMentions`: the references [`model_mention`] writes, at the start of the
/// text or after whitespace, `,`, `(`, `[`, `"` or `'`. Model slugs may hold `/` and balanced
/// brackets; sentence punctuation and an unbalanced closing bracket after one are prose.
pub fn model_mentions(text: &str) -> Vec<ModelMention> {
    let mut found = Vec::new();
    let mut scanned = 0;
    for (start, _) in text.match_indices(MODEL_PREFIX) {
        let after_boundary =
            text[..start].chars().next_back().is_none_or(|c| c.is_whitespace() || ",([\"'".contains(c));
        if start < scanned || !after_boundary {
            continue;
        }
        let rest = &text[start + MODEL_PREFIX.len()..];
        // `ProviderInstanceId`: a letter, then letters, digits, `_` or `-`.
        let instance_len =
            rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-')).unwrap_or(rest.len());
        let instance_id = &rest[..instance_len];
        let Some(slug_text) = rest[instance_len..].strip_prefix('/') else { continue };
        if !instance_id.starts_with(|c: char| c.is_ascii_alphabetic()) {
            continue;
        }
        let slug_len = slug_text.find(|c: char| c.is_whitespace() || ",\"'".contains(c)).unwrap_or(slug_text.len());
        if slug_len == 0 {
            continue;
        }
        let head = MODEL_PREFIX.len() + instance_len + 1;
        scanned = start + head + slug_len;
        let model = trim_model_slug(&slug_text[..slug_len]);
        if model.is_empty() {
            continue;
        }
        found.push(ModelMention {
            instance_id: instance_id.to_owned(),
            model: model.to_owned(),
            range: start..start + head + model.len(),
        });
    }
    found
}

/// `trimModelSlug`: `foo(bar)` keeps its brackets; `(model:a/b).` drops the `).`.
fn trim_model_slug(slug: &str) -> &str {
    let mut trimmed = slug;
    while let Some(last) = trimmed.chars().next_back() {
        let opening = match last {
            ')' => Some('('),
            ']' => Some('['),
            _ => None,
        };
        let count = |c: char| trimmed.matches(c).count();
        let unbalanced = opening.is_some_and(|open| count(open) < count(last));
        if !".;:!?".contains(last) && !unbalanced {
            break;
        }
        trimmed = &trimmed[..trimmed.len() - last.len_utf8()];
    }
    trimmed
}

/// `threadTitleTextFromPrompt`'s `modelMentionsAsSlugs`: a title names a mentioned model by its
/// slug, without the reference syntax.
pub fn model_mentions_as_slugs(text: &str) -> std::borrow::Cow<'_, str> {
    let mentions = model_mentions(text);
    if mentions.is_empty() {
        return text.into();
    }
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    for mention in mentions {
        out.push_str(&text[cursor..mention.range.start]);
        out.push_str(&mention.model);
        cursor = mention.range.end;
    }
    out.push_str(&text[cursor..]);
    out.into()
}

/// Chips with their byte ranges in the text they came from.
pub type ChipSpans = Vec<(Range<usize>, Chip)>;

/// `handlePaste`: the chips in pasted `text`, with a space after a mention, skill or model that
/// ends it and before one that starts it unless the caret follows whitespace; `None` when it has
/// none.
pub fn pasted_chips(
    text: &str,
    after_space: bool,
    rich: bool,
    is_skill: impl Fn(&str) -> bool,
    model_label: impl Fn(&str, &str) -> Option<String>,
) -> Option<(String, ChipSpans)> {
    // A trailing line break lets a mention that ends the paste parse, as the web's does.
    let mut found: Vec<_> = chips_with_models(
        &format!(
            "{text}
"
        ),
        rich,
        is_skill,
        model_label,
    )
    .into_iter()
    .filter(|(range, _)| range.end <= text.len())
    .collect();
    let spaced = |chip: &Chip| matches!(chip, Chip::File { .. } | Chip::Skill { .. } | Chip::Model { .. });
    let mut padded = text.to_owned();
    if found.last().is_some_and(|(range, chip)| range.end == text.len() && spaced(chip)) {
        padded.push(' ');
    }
    if found.first().is_some_and(|(range, chip)| range.start == 0 && spaced(chip)) && !after_space {
        padded.insert(0, ' ');
        for (range, _) in &mut found {
            *range = range.start + 1..range.end + 1;
        }
    }
    (!found.is_empty()).then_some((padded, found))
}

/// `[label](dest)` whose label is the destination's basename and whose
/// destination has no URI scheme; returns its length and decoded path.
fn parse_file_link(text: &str) -> Option<(usize, String)> {
    let (close, label) = parse_link_label(text)?;
    let dest_text = text[close + 1..].strip_prefix('(')?;
    let end = dest_text.find(|c: char| c == ')' || c.is_whitespace())?;
    if !dest_text[end..].starts_with(')') || end == 0 {
        return None;
    }
    let dest = &dest_text[..end];
    let path = decode_destination(dest)?;
    let scheme = dest.split_once(':').is_some_and(|(scheme, _)| {
        scheme.len() > 1 && scheme.chars().all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c))
    });
    (!scheme && basename(&path) == label).then(|| (close + 2 + end + 1, path))
}

/// `[label]` at the start of `text`, unescaped; returns the `]` offset too.
fn parse_link_label(text: &str) -> Option<(usize, String)> {
    let mut label = String::new();
    let mut chars = text.strip_prefix('[')?.char_indices().map(|(i, c)| (i + 1, c));
    loop {
        let (i, c) = chars.next()?;
        match c {
            '\\' => label.push(chars.next()?.1),
            ']' => return Some((i, label)),
            _ => label.push(c),
        }
    }
}

fn parse_context_link(text: &str) -> Option<(usize, Chip)> {
    let bang = usize::from(text.starts_with('!'));
    let (close, label) = parse_link_label(&text[bang..])?;
    let after = &text[bang + close + 1..];
    let target = after.strip_prefix('(')?.strip_prefix(CONTEXT_PREFIX)?;
    let end = target.find(')')?;
    let (kind, id) = target[..end].split_once('/')?;
    let valid = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if !valid(kind) || !valid(id) {
        return None;
    }
    let len = bang + close + 1 + 1 + CONTEXT_PREFIX.len() + end + 1;
    Some((len, Chip::Context { kind: kind.to_owned(), id: id.to_owned(), label }))
}

/// A skill name after the sigil: alphanumerics, `:`, `_`, `-`, with a letter.
fn parse_skill(text: &str) -> Option<String> {
    let end = text.find(|c: char| !(c.is_ascii_alphanumeric() || ":_-".contains(c))).unwrap_or(text.len());
    let name = &text[..end];
    (name.starts_with(|c: char| c.is_ascii_alphanumeric()) && name.contains(|c: char| c.is_ascii_alphabetic()))
        .then(|| name.to_owned())
}

/// Where Up/Down browsing stands: the entry shown and the text it put there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryPosition {
    pub index: usize,
    pub recalled: String,
}

/// One Up (`back`) or Down step. `None` leaves the prompt alone; otherwise the
/// new prompt and position (`None` once past the newest entry).
pub fn history_step(
    entries: &[String],
    position: Option<&HistoryPosition>,
    prompt: &str,
    back: bool,
) -> Option<(String, Option<HistoryPosition>)> {
    let active = position.filter(|p| p.recalled == prompt && p.index < entries.len()).map(|p| p.index);
    let index = match (active, back) {
        (None, true) if prompt.is_empty() => entries.len().checked_sub(1)?,
        (None, _) => return None,
        (Some(index), true) => index.checked_sub(1)?,
        (Some(index), false) if index + 1 >= entries.len() => return Some((String::new(), None)),
        (Some(index), false) => index + 1,
    };
    let recalled = entries[index].clone();
    Some((recalled.clone(), Some(HistoryPosition { index, recalled })))
}

/// `ATTACHMENT_ONLY_BOOTSTRAP_PROMPT`: the text of a message sent with files alone.
pub const ATTACHMENT_ONLY_PROMPT: &str = "[User attached one or more files without additional text. Respond using the conversation context and the attached files.]";

/// `recallableComposerPrompt`: a sent message as the composer takes it back, trimmed and
/// without context chips, whose records are gone.
pub fn recallable_prompt(text: &str) -> String {
    let text = strip_context_links(text.strip_prefix("Ultrathink:\n").unwrap_or(text));
    let text = text.trim();
    if text == ATTACHMENT_ONLY_PROMPT { String::new() } else { text.to_owned() }
}

/// The user's own messages, oldest first, as history recalls them, consecutive repeats once.
pub fn history_entries<'a>(messages: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut entries: Vec<String> = Vec::new();
    for text in messages.map(recallable_prompt) {
        if !text.is_empty() && entries.last().is_none_or(|last| *last != text) {
            entries.push(text);
        }
    }
    entries
}

/// Drops `[label](t3-context://…)` chips and one space next to each.
fn strip_context_links(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        let link = rest[open..].find("](t3-context://").and_then(|close| {
            let end = rest[open + close..].find(')')?;
            Some(open + close + end + 1)
        });
        match link {
            Some(end) => {
                // An image chip is `![label](…)`.
                out.push_str(rest[..open].strip_suffix('!').unwrap_or(&rest[..open]));
                rest = &rest[end..];
                if rest.starts_with(' ') {
                    rest = &rest[1..];
                } else if out.ends_with(' ') {
                    out.pop();
                }
            }
            None => {
                out.push_str(&rest[..=open]);
                rest = &rest[open + 1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// `PROVIDER_SEND_TURN_MAX_INPUT_CHARS`, in UTF-16 units like a JS string's length.
pub const MAX_INPUT_CHARS: usize = 120_000;
/// `PASTED_TEXT_ATTACHMENT_THRESHOLD_BYTES`.
const PASTED_TEXT_THRESHOLD_BYTES: usize = 32 * 1024;

/// A JS string's `length`.
pub fn js_length(text: &str) -> usize {
    text.encode_utf16().count()
}

fn group_thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// `getComposerPromptLengthValidationMessage`.
pub fn prompt_length_error(prompt: &str) -> Option<String> {
    let excess = js_length(prompt.trim()).checked_sub(MAX_INPUT_CHARS).filter(|e| *e > 0)?;
    let label = if excess == 1 { "character" } else { "characters" };
    Some(format!(
        "Prompt is {} {label} over the {}-character limit. Shorten or split it before sending.",
        group_thousands(excess),
        group_thousands(MAX_INPUT_CHARS)
    ))
}

/// `pastedTextDisposition`: large pastes, and pastes past the input limit, become a file.
pub fn paste_folds(text: &str, would_exceed_limit: bool) -> bool {
    !text.is_empty() && (would_exceed_limit || text.len() >= PASTED_TEXT_THRESHOLD_BYTES)
}

/// `nextPastedTextFileName`.
pub fn next_pasted_text_name<'a>(existing: impl Iterator<Item = &'a str>) -> String {
    let taken: Vec<String> = existing.map(str::to_lowercase).collect();
    if !taken.iter().any(|n| n == "pasted-text.txt") {
        return "pasted-text.txt".into();
    }
    (2..).map(|n| format!("pasted-text-{n}.txt")).find(|name| !taken.contains(name)).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    #[test]
    fn long_prompts_and_pastes_follow_the_web_limits() {
        assert_eq!(super::prompt_length_error(&"a".repeat(super::MAX_INPUT_CHARS)), None);
        assert_eq!(
            super::prompt_length_error(&format!(" {} ", "a".repeat(super::MAX_INPUT_CHARS + 1))).as_deref(),
            Some("Prompt is 1 character over the 120,000-character limit. Shorten or split it before sending.")
        );
        assert!(super::prompt_length_error(&"😀".repeat(60_001)).unwrap().starts_with("Prompt is 2 characters"));
        assert!(!super::paste_folds("short", false) && super::paste_folds("short", true));
        assert!(super::paste_folds(&"é".repeat(16 * 1024), false), "the threshold counts bytes");
        assert!(!super::paste_folds("", true));
        assert_eq!(super::next_pasted_text_name(["a.txt"].into_iter()), "pasted-text.txt");
        assert_eq!(
            super::next_pasted_text_name(["Pasted-Text.txt", "pasted-text-2.txt"].into_iter()),
            "pasted-text-3.txt"
        );
    }

    use super::*;

    fn kind(text: &str) -> Option<(TriggerKind, String)> {
        detect(text, text.len()).map(|t| (t.kind, t.query))
    }

    #[test]
    fn triggers_start_after_whitespace_and_end_at_it() {
        use TriggerKind::*;
        assert_eq!(kind("@src/ma"), Some((Path, "src/ma".into())));
        assert_eq!(kind("fix @"), Some((Path, String::new())));
        assert_eq!(kind("fix @a b"), None);
        assert_eq!(kind("email@x"), None);
        assert_eq!(kind("/mod"), Some((SlashCommand, "mod".into())));
        assert_eq!(kind("see /usr/bin"), None, "a second slash makes a path");
        assert_eq!(detect("/us/x", 2), None, "even after the caret");
        assert_eq!(kind("$rev"), Some((Skill, "rev".into())));
        assert_eq!(kind("€"), Some((Skill, String::new())));
        assert_eq!(kind("costs $5"), None);
        assert_eq!(kind("$10k."), None);
        assert_eq!(kind("$1e9"), None);
        let t = detect("a\n@x y", 4).unwrap();
        assert_eq!((t.range, t.query), (2..4, "x".into()));
    }

    #[test]
    fn pasted_mentions_and_skills_become_chips_kept_apart_from_their_neighbours() {
        let skill = |name: &str| name == "review";
        let none = |_: &str, _: &str| None;
        assert_eq!(pasted_chips("plain words", false, true, skill, none), None);
        let (text, found) = pasted_chips("[a.rs](src/a.rs)", false, true, skill, none).unwrap();
        assert_eq!(text, " [a.rs](src/a.rs) ");
        assert_eq!(found, [(1..17, Chip::File { path: "src/a.rs".into() })]);
        let (text, found) = pasted_chips("see $review", true, true, skill, none).unwrap();
        assert_eq!((text.as_str(), found), ("see $review ", vec![(4..11, Chip::Skill { name: "review".into() })]));
        assert_eq!(pasted_chips("$unknown", true, true, skill, none), None, "only a skill the provider offers");
        let (text, found) = pasted_chips("model:codex/gpt-6.1", false, true, skill, codex_model).unwrap();
        assert_eq!(text, " model:codex/gpt-6.1 ", "a model chip is spaced like a mention");
        assert_eq!(found, [(1..20, gpt_chip())]);
        let fenced = "```\nmodel:codex/gpt-6.1 $review\n```";
        assert_eq!(pasted_chips(fenced, false, true, skill, codex_model), None, "code stays as written");
        let (_, found) = pasted_chips(fenced, false, false, skill, codex_model).unwrap();
        assert_eq!(found.len(), 2, "plain text has no code blocks");
    }

    fn codex_model(instance_id: &str, model: &str) -> Option<String> {
        (instance_id == "codex" && model == "gpt-6.1").then(|| "GPT-6.1".to_owned())
    }

    fn gpt_chip() -> Chip {
        Chip::Model { instance_id: "codex".into(), model: "gpt-6.1".into(), label: "GPT-6.1".into() }
    }

    fn mentions(text: &str) -> Vec<String> {
        model_mentions(text).into_iter().map(|m| format!("{}/{}", m.instance_id, m.model)).collect()
    }

    #[test]
    fn model_references_read_back_what_a_pick_writes_inside_prose() {
        let text = format!("use {}, then model:codex/gpt-5.4.", model_mention("opencode", "anthropic/opus"));
        let found = model_mentions(&text);
        assert_eq!(
            found.iter().map(|m| (m.instance_id.as_str(), m.model.as_str(), m.range.clone())).collect::<Vec<_>>(),
            [("opencode", "anthropic/opus", 4..33), ("codex", "gpt-5.4", 40..59)]
        );
        assert_eq!(&text[found[0].range.clone()], "model:opencode/anthropic/opus");
        for (text, expected) in [
            ("(model:codex/gpt-6.1).", vec!["codex/gpt-6.1"]),
            ("\"model:codex/gpt-6.1\"", vec!["codex/gpt-6.1"]),
            ("model:codex/gpt-6.1,model:claudeAgent/sonnet", vec!["codex/gpt-6.1", "claudeAgent/sonnet"]),
            ("model:codex/gpt-6.1's output", vec!["codex/gpt-6.1"]),
            ("model:opencode/foo(bar)", vec!["opencode/foo(bar)"]),
            ("model:codex/GPT-6.1-Sol[1m]", vec!["codex/GPT-6.1-Sol[1m]"]),
        ] {
            assert_eq!(mentions(text), expected, "{text}");
        }
        for text in ["remodel:codex/gpt", "model:codex", "model:/gpt", "model:codex/.", "model:1x/gpt", "é model:a"] {
            assert_eq!(mentions(text), Vec::<String>::new(), "{text}");
        }
    }

    #[test]
    fn a_known_model_reference_chips_once_whitespace_follows_it() {
        let found = |text: &str| chips_with_models(text, true, |_| false, codex_model);
        assert_eq!(found("Ask model:codex/gpt-6.1 now"), [(4..23, gpt_chip())]);
        assert_eq!(found("Ask model:openai/gpt-4o now"), [], "only a model the environment offers");
        assert_eq!(found("Ask model:codex/gpt-6.1"), [], "still being typed");
        assert_eq!(found("```\nAsk model:codex/gpt-6.1 now\n```\n"), [], "inside a fenced block");
        assert_eq!(found("~~~sh\nAsk model:codex/gpt-6.1 now"), [], "inside an unclosed block");
        assert_eq!(found("```\nx\n```\nAsk model:codex/gpt-6.1 now"), [(14..33, gpt_chip())], "after one");
        let both = chips_with_models("$review model:codex/gpt-6.1 x", true, |name| name == "review", codex_model);
        assert_eq!(both, [(0..7, Chip::Skill { name: "review".into() }), (8..27, gpt_chip())]);
        assert_eq!(gpt_chip().id(), "model:codex/gpt-6.1");
        assert_eq!(
            Chip::from_id(&gpt_chip().id()),
            Some(Chip::Model { instance_id: "codex".into(), model: "gpt-6.1".into(), label: String::new() })
        );
    }

    #[test]
    fn chips_follow_the_skills_and_models_that_load_or_go() {
        let text = "$review model:codex/gpt-6.1 x";
        let skill = |name: &str| name == "review";
        let none = |_: &str, _: &str| None;
        let review = (0..7, "skill:review", "review");
        let gpt = (8..27, "model:codex/gpt-6.1", "GPT-6.1");
        assert_eq!(
            reconcile_chips(text, true, &[], skill, codex_model),
            (vec![], vec![(0..7, Chip::Skill { name: "review".into() }), (8..27, gpt_chip())]),
            "text restored before the lists loaded"
        );
        assert_eq!(reconcile_chips(text, true, &[review.clone(), gpt.clone()], skill, codex_model), (vec![], vec![]));
        assert_eq!(reconcile_chips(text, true, &[review.clone(), gpt], |_| false, none), (vec![0..7, 8..27], vec![]));
        let renamed = (8..27, "model:codex/gpt-6.1", "GPT-6");
        let (stale, fresh) = reconcile_chips(text, true, &[review, renamed.clone()], skill, codex_model);
        assert_eq!((stale, fresh), (vec![renamed.0.clone()], vec![(renamed.0, gpt_chip())]));
        let pasted = (0..27, "paste:1", "Pasted text");
        assert_eq!(reconcile_chips(text, true, &[pasted], skill, codex_model), (vec![], vec![]), "inside another chip");
        let fenced = format!("```\n{text}\n```");
        assert_eq!(reconcile_chips(&fenced, true, &[], skill, codex_model), (vec![], vec![]), "code stays as written");
        let in_code = [(4..11, "skill:review", "review"), (12..31, "model:codex/gpt-6.1", "GPT-6.1")];
        assert_eq!(
            reconcile_chips(&fenced, true, &in_code, skill, codex_model),
            (vec![4..11, 12..31], vec![]),
            "chips made with rich text off go back to text in code once it is on"
        );
        assert_eq!(reconcile_chips(&fenced, false, &in_code, skill, codex_model), (vec![], vec![]));

        // Turning rich text on or off rebuilds file and context chips from the text too.
        let refs = "```\n[a.ts](src/a.ts) ![shot.png](t3-context://v1/image/img_1) \n```";
        let file = Chip::File { path: "src/a.ts".into() };
        let image = Chip::Context { kind: "image".into(), id: "img_1".into(), label: "shot.png".into() };
        let chipped = [(4..20, "file:src/a.ts", "a.ts"), (21..61, "context:image/img_1", "shot.png")];
        assert_eq!(remount_chips(refs, true, &chipped, skill, none), (vec![4..20, 21..61], vec![]), "on");
        assert_eq!(remount_chips(refs, false, &[], skill, none), (vec![], vec![(4..20, file), (21..61, image)]), "off");
        assert_eq!(remount_chips(refs, false, &chipped, skill, none), (vec![], vec![]));
        // An attachment stays while any reference to it does, chip or not (`collectInlineContextIds`).
        assert_eq!(context_reference_ids(refs), ["context:image/img_1"]);
        let glued = "x[T](t3-context://v1/thread/a)[T](t3-context://v1/thread/a) [a\nb](t3-context://v1/file/f)";
        assert_eq!(context_reference_ids(glued), ["context:thread/a"], "once each; a label holds no newline");
        // Removing an attachment takes every reference with it, with one neighbouring space.
        let both = "a [I](t3-context://v1/image/img_1) b\n```\n[I](t3-context://v1/image/img_1)\n```";
        let removals = context_reference_removals(both, "context:image/img_1");
        assert_eq!(removals, [41..73, 2..35], "fenced too, last first");
        let mut left = both.to_owned();
        for range in removals {
            left.replace_range(range, "");
        }
        assert_eq!(left, "a b\n```\n\n```");
        let at_end = context_reference_removals("x [I](t3-context://v1/image/img_1)", "context:image/img_1");
        assert_eq!(at_end, vec![1..34], "the space before it when none follows");
        assert!(context_reference_removals(both, "context:image/other").is_empty());
        let (stale, fresh) = remount_chips(&fenced, false, &[], skill, codex_model);
        assert_eq!((stale, fresh.len()), (vec![], 2), "skills and models too");

        let gpt = (8..27, "model:codex/gpt-6.1", "GPT-6.1");
        let restored = |model_label: fn(&str, &str) -> Option<String>| {
            reconcile_restored_chips(text, true, std::slice::from_ref(&gpt), skill, model_label)
        };
        assert_eq!(
            restored(|_, _| None),
            (vec![gpt.0.clone()], vec![]),
            "an undo brings back a chip for a model that went"
        );
        assert_eq!(restored(codex_model), (vec![], vec![]), "plain `$review` the user left as text stays text");
        let renamed = |_: &str, _: &str| Some("GPT-6".to_owned());
        let relabelled = Chip::Model { instance_id: "codex".into(), model: "gpt-6.1".into(), label: "GPT-6".into() };
        assert_eq!(restored(renamed), (vec![gpt.0.clone()], vec![(8..27, relabelled)]));
    }

    #[test]
    fn titles_name_a_mentioned_model_by_its_slug() {
        assert_eq!(
            model_mentions_as_slugs("Delegate to model:codex/gpt-6.1-sol and report back"),
            "Delegate to gpt-6.1-sol and report back"
        );
        assert_eq!(
            model_mentions_as_slugs("model:opencode/anthropic/opus, then model:codex/gpt-5.4."),
            "anthropic/opus, then gpt-5.4."
        );
    }

    #[test]
    fn picked_files_become_links_that_parse_back_as_chips() {
        let link = file_link("src/my file (1).ts");
        assert_eq!(link, "[my file (1).ts](src/my%20file%20%281%29.ts)");
        let text = format!("see {link} and $review now");
        let chips = chips(&text, |name| name == "review");
        assert_eq!(chips.len(), 2);
        assert_eq!(&text[chips[0].0.clone()], link);
        assert_eq!(chips[0].1, Chip::File { path: "src/my file (1).ts".into() });
        assert_eq!(chips[1].1, Chip::Skill { name: "review".into() });
        assert!(super::chips("[a](https://x/a) [b](c/b)", |_| true).is_empty(), "urls and trailing links stay text");
        assert!(super::chips("$unknown ", |_| false).is_empty());
        assert_eq!(file_link("C:\\x\\[a].md"), "[\\[a\\].md](C:%5Cx%5C%5Ba%5D.md)");
    }

    #[test]
    fn trailing_space_is_swallowed_once() {
        assert_eq!(extend_for_trailing_space("@a b", 0..2, "x "), 0..3);
        assert_eq!(extend_for_trailing_space("@a", 0..2, "x "), 0..2);
        assert_eq!(extend_for_trailing_space("@a b", 0..2, ""), 0..2);
    }

    #[test]
    fn history_starts_from_an_empty_prompt_and_ends_empty() {
        let entries =
            history_entries(["one", "Ultrathink:\ntwo", "two", "  ", "x [T](t3-context://v1/thread/a) y"].into_iter());
        assert_eq!(entries, ["one", "two", "x y"]);
        assert_eq!(recallable_prompt("look ![a.png](t3-context://v1/image/image_1) here"), "look here");
        assert_eq!(recallable_prompt(ATTACHMENT_ONLY_PROMPT), "");
        assert_eq!(history_step(&entries, None, "draft", true), None);
        let (text, pos) = history_step(&entries, None, "", true).unwrap();
        assert_eq!(text, "x y");
        let (text, pos) = history_step(&entries, pos.as_ref(), &text, true).unwrap();
        assert_eq!(text, "two");
        let (_, oldest) = history_step(&entries, pos.as_ref(), "two", true).unwrap();
        assert_eq!(history_step(&entries, oldest.as_ref(), "one", true), None, "no wrap");
        let (text, pos) = history_step(&entries, oldest.as_ref(), "one", false).unwrap();
        assert_eq!(text, "two");
        assert_eq!(history_step(&entries, pos.as_ref(), "edited", false), None, "an edit ends browsing");
        let newest = HistoryPosition { index: 2, recalled: "x y".into() };
        assert_eq!(history_step(&entries, Some(&newest), "x y", false), Some((String::new(), None)));
    }
}
