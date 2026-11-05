//! The composer's text rules: completion triggers (`detectComposerTrigger` in
//! `apps/web/src/composer-logic.ts`), the file links and skills a pick inserts
//! (`packages/shared/src/composerTrigger.ts`, `composerInlineTokens.ts`) and
//! prompt history (`composerPromptHistory.ts`). Offsets are UTF-8 bytes.

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
fn is_currency(c: char) -> bool {
    matches!(c, '$' | '€' | '£' | '¥' | '₹' | '¢' | '₩' | '₽' | '₺' | '₪' | '₫' | '฿' | '₴' | '₦' | '₱')
}

/// The completion the caret is in, if any.
pub fn detect(text: &str, cursor: usize) -> Option<Trigger> {
    let cursor = cursor.min(text.len());
    let start = text[..cursor].rfind(is_space).map(|i| i + 1).unwrap_or(0);
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
            out.push_str(&format!("%{byte:02X}"));
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
}

impl Chip {
    pub fn id(&self) -> String {
        match self {
            Chip::File { path } => format!("file:{path}"),
            Chip::Skill { name } => format!("skill:{name}"),
            Chip::Context { kind, id, .. } => format!("context:{kind}/{id}"),
        }
    }

    pub fn label(&self) -> &str {
        match self {
            Chip::File { path } => basename(path),
            Chip::Skill { name } => name,
            Chip::Context { label, .. } => label,
        }
    }

    /// The chip an editor token id names; context chips come back without their label.
    pub fn from_id(id: &str) -> Option<Self> {
        if let Some(path) = id.strip_prefix("file:") {
            return Some(Chip::File { path: path.to_owned() });
        }
        if let Some(name) = id.strip_prefix("skill:") {
            return Some(Chip::Skill { name: name.to_owned() });
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

#[cfg(test)]
mod tests {
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
