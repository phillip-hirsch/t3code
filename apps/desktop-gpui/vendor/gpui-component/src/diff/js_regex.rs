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

/// `pattern` (JavaScript syntax) written for `fancy_regex` with JavaScript's `\w \W \d \D \s \S \b
/// \B`. Classes nest as `fancy_regex` reads them, so the escapes are found where it finds them.
pub(crate) fn translate(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len());
    let mut depth = 0_usize;
    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                let Some(escaped) = chars.next() else {
                    out.push('\\');
                    break;
                };
                match (escaped, depth) {
                    ('w', _) => out.push_str(WORD),
                    ('W', _) => out.push_str(NOT_WORD),
                    ('d', _) => out.push_str(DIGIT),
                    ('D', _) => out.push_str(NOT_DIGIT),
                    ('s', _) => out.push_str(SPACE),
                    ('S', _) => out.push_str(NOT_SPACE),
                    ('b', 0) => out.push_str(BOUNDARY),
                    ('B', 0) => out.push_str(NOT_BOUNDARY),
                    (escaped, _) => {
                        out.push('\\');
                        out.push(unfold_char(escaped));
                    }
                }
            }
            '[' => {
                depth += 1;
                out.push('[');
                // `[^` and a `]` right after the opening are part of the class.
                if let Some(negated) = chars.next_if_eq(&'^') {
                    out.push(negated);
                }
                if let Some(bracket) = chars.next_if_eq(&']') {
                    out.push(bracket);
                }
            }
            ']' if depth > 0 => {
                depth -= 1;
                out.push(']');
            }
            c => out.push(unfold_char(c)),
        }
    }
    out
}
