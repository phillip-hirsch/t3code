//! The web's `RegExp` (flags `g`/`gi`, no `u`) on `fancy_regex`: escapes mean what Annex B says,
//! `\w`, `\d` and word boundaries are ASCII, `\s` is JavaScript's set, and `i` never folds a
//! non-ASCII character onto an ASCII one.

use std::borrow::Cow;

const WORD: &str = "[0-9A-Za-z_]";
const NOT_WORD: &str = "[^0-9A-Za-z_]";
const DIGIT: &str = "[0-9]";
const NOT_DIGIT: &str = "[^0-9]";
/// JavaScript's `WhiteSpace` and `LineTerminator`: Unicode `White_Space` without U+0085, with U+FEFF.
const SPACE: &str = "[\t\n\u{B}\u{C}\r \u{A0}\u{1680}\u{2000}-\u{200A}\u{2028}\u{2029}\u{202F}\u{205F}\u{3000}\u{FEFF}]";
const NOT_SPACE: &str = "[^\t\n\u{B}\u{C}\r \u{A0}\u{1680}\u{2000}-\u{200A}\u{2028}\u{2029}\u{202F}\u{205F}\u{3000}\u{FEFF}]";
const BOUNDARY: &str = "(?:(?<=[0-9A-Za-z_])(?![0-9A-Za-z_])|(?<![0-9A-Za-z_])(?=[0-9A-Za-z_]))";
const NOT_BOUNDARY: &str =
    "(?:(?<=[0-9A-Za-z_])(?=[0-9A-Za-z_])|(?<![0-9A-Za-z_])(?![0-9A-Za-z_]))";

/// Unicode simple case folding pairs `ſ` with `s` and the Kelvin sign with `k`; JavaScript's `i`
/// does not. The pattern and the searched text both trade them for unassigned code points of the
/// same UTF-8 and UTF-16 length, which fold to nothing, so match ranges stay put.
const UNFOLDED: [(char, char); 2] = [('\u{17F}', '\u{378}'), ('\u{212A}', '\u{FDD0}')];

fn unfold_char(c: char) -> char {
    UNFOLDED
        .iter()
        .find(|(from, _)| *from == c)
        .map_or(c, |(_, to)| *to)
}

/// The searched text, with the characters JavaScript's `i` keeps apart traded as [`UNFOLDED`] says.
pub(crate) fn unfold(text: &str) -> Cow<'_, str> {
    if text.contains(|c| UNFOLDED.iter().any(|(from, _)| *from == c)) {
        Cow::Owned(text.chars().map(unfold_char).collect())
    } else {
        Cow::Borrowed(text)
    }
}

/// JavaScript's `[^]` and `[]`: any character, newlines included, and none.
const ANY: &str = r"[\x{0}-\x{10FFFF}]";
const NOTHING: &str = r"[^\x{0}-\x{10FFFF}]";

/// `pattern` (JavaScript syntax) written for `fancy_regex` with JavaScript's escapes and character
/// classes, or `None` where JavaScript rejects it.
pub(crate) fn translate(pattern: &str) -> Option<String> {
    let groups = Groups::of(pattern);
    let mut out = String::with_capacity(pattern.len());
    let mut rest = pattern;
    while let Some(c) = rest.chars().next() {
        rest = &rest[c.len_utf8()..];
        match c {
            '\\' => {
                let (atom, after) = escape(rest, &groups, false)?;
                atom.push(false, &mut out);
                rest = after;
            }
            '[' => rest = translate_class(rest, &groups, &mut out)?,
            c => out.push(unfold_char(c)),
        }
    }
    Some(out)
}

/// The capture groups of a pattern: JavaScript reads `\N` as a backreference only up to their
/// count, and `\k` as one only when a group is named.
struct Groups<'a> {
    count: usize,
    /// Each named group's name and number.
    names: Vec<(&'a str, usize)>,
}

impl<'a> Groups<'a> {
    fn of(pattern: &'a str) -> Self {
        let bytes = pattern.as_bytes();
        let mut groups = Self {
            count: 0,
            names: Vec::new(),
        };
        let (mut ix, mut class) = (0, false);
        while ix < bytes.len() {
            match bytes[ix] {
                b'\\' => ix += 1,
                b'[' => class = true,
                b']' => class = false,
                b'(' if !class => {
                    let rest = &pattern[ix + 1..];
                    if !rest.starts_with('?') {
                        groups.count += 1;
                    } else if let Some(name) = rest.strip_prefix("?<")
                        && !name.starts_with(['=', '!'])
                    {
                        groups.count += 1;
                        let name = name.split_once('>').map_or(name, |(name, _)| name);
                        groups.names.push((name, groups.count));
                    }
                }
                _ => {}
            }
            ix += 1;
        }
        groups
    }
}

/// One escape or class item, written for `fancy_regex`.
enum Atom {
    Char(char),
    /// `\w`, `\d`, `\s` and their negations.
    Set(&'static str),
    /// A lone UTF-16 surrogate, which UTF-8 text never holds.
    Surrogate(u32),
    /// A word boundary or a backreference, outside a class.
    Raw(String),
    /// An unescaped `-` in a class: a range's, or a literal one.
    Dash,
}

impl Atom {
    fn push(&self, class: bool, out: &mut String) {
        match self {
            Self::Char(c) => push_literal(*c, out),
            Self::Dash => push_literal('-', out),
            Self::Set(set) => out.push_str(set),
            Self::Raw(raw) => out.push_str(raw),
            Self::Surrogate(_) if class => {}
            Self::Surrogate(_) => out.push_str(NOTHING),
        }
    }

    /// The code point this item stands for as a range end; a `\w`-like set has none.
    fn range_end(&self) -> Option<u32> {
        match self {
            Self::Char(c) => Some(u32::from(*c)),
            Self::Dash => Some(u32::from('-')),
            Self::Surrogate(unit) => Some(*unit),
            Self::Set(_) | Self::Raw(_) => None,
        }
    }
}

/// `c` as a literal, traded as [`UNFOLDED`] says.
fn push_literal(c: char, out: &mut String) {
    let c = unfold_char(c);
    if c.is_ascii_alphanumeric() || !c.is_ascii() {
        out.push(c);
    } else {
        out.push_str(&format!(r"\x{{{:X}}}", u32::from(c)));
    }
}

fn hex(text: &str, digits: usize) -> Option<u32> {
    text.get(..digits)
        .filter(|hex| hex.bytes().all(|b| b.is_ascii_hexdigit()))
        .and_then(|hex| u32::from_str_radix(hex, 16).ok())
}

/// A legacy octal escape (Annex B): up to three digits, at most `\377`.
fn octal(after: &str) -> Option<(Atom, &str)> {
    let longest = if after.starts_with(['0', '1', '2', '3']) {
        3
    } else {
        2
    };
    let len = after
        .bytes()
        .take(longest)
        .take_while(|b| (b'0'..=b'7').contains(b))
        .count();
    let value = u8::from_str_radix(&after[..len], 8).ok()?;
    Some((Atom::Char(char::from(value)), &after[len..]))
}

/// What the text after a `\` stands for in JavaScript without the `u` flag (Annex B), and the
/// text after the escape. A backslash before a character with no escape meaning is that character.
fn escape<'a>(after: &'a str, groups: &Groups<'_>, class: bool) -> Option<(Atom, &'a str)> {
    let c = after.chars().next()?;
    let rest = &after[c.len_utf8()..];
    let atom = match c {
        'w' => Atom::Set(WORD),
        'W' => Atom::Set(NOT_WORD),
        'd' => Atom::Set(DIGIT),
        'D' => Atom::Set(NOT_DIGIT),
        's' => Atom::Set(SPACE),
        'S' => Atom::Set(NOT_SPACE),
        'b' if class => Atom::Char('\u{8}'),
        'b' => Atom::Raw(BOUNDARY.into()),
        'B' if !class => Atom::Raw(NOT_BOUNDARY.into()),
        'f' => Atom::Char('\u{C}'),
        'n' => Atom::Char('\n'),
        'r' => Atom::Char('\r'),
        't' => Atom::Char('\t'),
        'v' => Atom::Char('\u{B}'),
        'c' => {
            return match rest.bytes().next() {
                Some(l)
                    if l.is_ascii_alphabetic() || class && (l.is_ascii_digit() || l == b'_') =>
                {
                    Some((Atom::Char(char::from(l % 32)), &rest[1..]))
                }
                // `\c` without a control letter is a backslash, and the `c` follows it.
                _ => Some((Atom::Char('\\'), after)),
            };
        }
        'x' => match hex(rest, 2) {
            Some(unit) => return Some((Atom::Char(char::from_u32(unit)?), &rest[2..])),
            None => Atom::Char('x'),
        },
        'u' => match hex(rest, 4) {
            Some(unit) => return Some(code_unit(unit, &rest[4..])),
            None => Atom::Char('u'),
        },
        'k' if !groups.names.is_empty() => {
            let (name, rest) = rest.strip_prefix('<')?.split_once('>')?;
            let (_, n) = groups
                .names
                .iter()
                .find(|(known, _)| *known == name)
                .filter(|_| !class)?;
            return Some((backreference(*n), rest));
        }
        '1'..='9' if !class => {
            let digits = after.bytes().take_while(u8::is_ascii_digit).count();
            match after[..digits].parse::<usize>() {
                Ok(n) if n <= groups.count => return Some((backreference(n), &after[digits..])),
                _ if c >= '8' => Atom::Char(c),
                _ => return octal(after),
            }
        }
        '0'..='7' => return octal(after),
        c => Atom::Char(c),
    };
    Some((atom, rest))
}

/// A backreference to group `n`, which matches empty while the group has not matched, as
/// JavaScript's does.
fn backreference(n: usize) -> Atom {
    Atom::Raw(format!(r"(?({n})\{n})"))
}

/// A `\uXXXX` escape; a surrogate pair written as two of them is its character.
fn code_unit(unit: u32, rest: &str) -> (Atom, &str) {
    if let Some(c) = char::from_u32(unit) {
        return (Atom::Char(c), rest);
    }
    let pair = rest
        .strip_prefix(r"\u")
        .and_then(|low| hex(low, 4))
        .filter(|low| unit < 0xDC00 && (0xDC00..0xE000).contains(low))
        .and_then(|low| char::from_u32(0x10000 + ((unit - 0xD800) << 10) + (low - 0xDC00)));
    match pair {
        Some(c) => (Atom::Char(c), &rest[6..]),
        None => (Atom::Surrogate(unit), rest),
    }
}

/// A class after its `[`, and the text after its `]`. JavaScript's classes neither nest nor
/// have set operations, so every character but a range's `-` is written as a literal.
fn translate_class<'a>(
    mut rest: &'a str,
    groups: &Groups<'_>,
    out: &mut String,
) -> Option<&'a str> {
    let negated = rest.starts_with('^');
    if negated {
        rest = &rest[1..];
    }
    let mut atoms = Vec::new();
    loop {
        let c = rest.chars().next()?;
        rest = &rest[c.len_utf8()..];
        atoms.push(match c {
            ']' => break,
            '\\' => {
                let (atom, after) = escape(rest, groups, true)?;
                rest = after;
                atom
            }
            '-' => Atom::Dash,
            c => Atom::Char(c),
        });
    }
    let mut body = String::new();
    let mut ix = 0;
    while ix < atoms.len() {
        let (Some(Atom::Dash), Some(high)) = (atoms.get(ix + 1), atoms.get(ix + 2)) else {
            atoms[ix].push(true, &mut body);
            ix += 1;
            continue;
        };
        match (atoms[ix].range_end(), high.range_end()) {
            (Some(low), Some(high)) => push_range(low, high, &mut body)?,
            // A `\w`-like escape on either side makes the `-` a literal (Annex B).
            _ => {
                atoms[ix].push(true, &mut body);
                push_literal('-', &mut body);
                high.push(true, &mut body);
            }
        }
        ix += 3;
    }
    if body.is_empty() {
        out.push_str(if negated { ANY } else { NOTHING });
    } else {
        out.push('[');
        if negated {
            out.push('^');
        }
        out.push_str(&body);
        out.push(']');
    }
    Some(rest)
}

/// The code points `low` to `high`, `None` when the range runs backwards, as JavaScript rejects.
/// [`UNFOLDED`] characters in it match as what [`unfold`] trades them for.
fn push_range(low: u32, high: u32, out: &mut String) -> Option<()> {
    if low > high {
        return None;
    }
    let mut start = low;
    for (from, to) in UNFOLDED {
        let from = u32::from(from);
        if (start..=high).contains(&from) {
            push_span(start, from - 1, out);
            out.push_str(&format!(r"\x{{{:X}}}", u32::from(to)));
            start = from + 1;
        }
    }
    push_span(start, high, out);
    Some(())
}

/// `low-high` without the surrogates, which UTF-8 text never holds.
fn push_span(low: u32, high: u32, out: &mut String) {
    let surrogate = |unit: u32| (0xD800..0xE000).contains(&unit);
    let low = if surrogate(low) { 0xE000 } else { low };
    let high = if surrogate(high) { 0xD7FF } else { high };
    if low <= high {
        out.push_str(&format!(r"\x{{{low:X}}}-\x{{{high:X}}}"));
    }
}
