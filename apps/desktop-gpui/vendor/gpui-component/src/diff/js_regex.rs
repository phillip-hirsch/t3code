//! The web's `RegExp` (flags `g`/`gi`, no `u`) on `fancy_regex`: `\w`, `\d` and word boundaries
//! are ASCII, `\s` is JavaScript's set, and `i` never folds a non-ASCII character onto an ASCII one.

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

/// `pattern` (JavaScript syntax) written for `fancy_regex` with JavaScript's `\w \W \d \D \s \S \b
/// \B` and character classes.
pub(crate) fn translate(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len());
    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                let Some(escaped) = chars.next() else {
                    out.push('\\');
                    break;
                };
                match escaped {
                    'b' => out.push_str(BOUNDARY),
                    'B' => out.push_str(NOT_BOUNDARY),
                    escaped => out.push_str(ClassAtom::escape(escaped).pattern()),
                }
            }
            '[' => translate_class(&mut chars, &mut out),
            c => out.push(unfold_char(c)),
        }
    }
    out
}

/// One item of a JavaScript class, written for `fancy_regex`.
enum ClassAtom {
    /// A character, or a piece of an escape such as `\x41` that `fancy_regex` reads as JavaScript does.
    Char(String),
    /// `\w`, `\d`, `\s` and their negations.
    Set(&'static str),
    /// An unescaped `-`: a range's, or a literal one.
    Dash,
}

impl ClassAtom {
    fn escape(escaped: char) -> Self {
        match escaped {
            'w' => Self::Set(WORD),
            'W' => Self::Set(NOT_WORD),
            'd' => Self::Set(DIGIT),
            'D' => Self::Set(NOT_DIGIT),
            's' => Self::Set(SPACE),
            'S' => Self::Set(NOT_SPACE),
            escaped => Self::Char(format!("\\{}", unfold_char(escaped))),
        }
    }

    fn pattern(&self) -> &str {
        match self {
            Self::Char(text) => text,
            Self::Set(set) => set,
            Self::Dash => r"\-",
        }
    }
}

/// A class after its `[`. JavaScript's classes neither nest nor have set operations, so `[`, `&`,
/// `~` and a `-` outside a range, which `fancy_regex` reads as syntax there, are escaped.
fn translate_class(chars: &mut std::iter::Peekable<std::str::Chars<'_>>, out: &mut String) {
    let negated = chars.next_if_eq(&'^').is_some();
    if chars.next_if_eq(&']').is_some() {
        out.push_str(if negated { ANY } else { NOTHING });
        return;
    }
    let mut atoms = Vec::new();
    let mut closed = false;
    while let Some(c) = chars.next() {
        atoms.push(match c {
            ']' => {
                closed = true;
                break;
            }
            '\\' => chars.next().map_or_else(|| ClassAtom::Char("\\".to_owned()), ClassAtom::escape),
            '-' => ClassAtom::Dash,
            '[' | '&' | '~' => ClassAtom::Char(format!("\\{c}")),
            c => ClassAtom::Char(unfold_char(c).to_string()),
        });
    }
    out.push('[');
    if negated {
        out.push('^');
    }
    let mut ix = 0;
    while ix < atoms.len() {
        out.push_str(atoms[ix].pattern());
        if let (Some(ClassAtom::Dash), Some(high)) = (atoms.get(ix + 1), atoms.get(ix + 2)) {
            // A `\w`-like escape on either side makes the `-` a literal (Annex B).
            let range = !matches!(atoms[ix], ClassAtom::Set(_)) && !matches!(high, ClassAtom::Set(_));
            out.push_str(if range { "-" } else { r"\-" });
            out.push_str(high.pattern());
            ix += 3;
        } else {
            ix += 1;
        }
    }
    // An unterminated class stays so, and `fancy_regex` rejects it as JavaScript does.
    if closed {
        out.push(']');
    }
}
