//! Search matching shared by the sidebar search and the command palette: a port
//! of `packages/shared/src/paletteSearch.ts` (word-start matching, field
//! weights, typo fallback, ranking classes). Joined-word matches ("vscode" for
//! "VS Code") and search "picks" are not ported.

use std::ops::Range;

/// A field folded for matching. Folding lowercases one char at a time, so
/// offsets stay char offsets into the source.
pub struct Field {
    text: Vec<char>,
    word_starts: Vec<bool>,
}

fn fold_char(c: char) -> char {
    if c.is_ascii() {
        return c.to_ascii_lowercase();
    }
    let mut lower = c.to_lowercase();
    match (lower.next(), lower.next()) {
        (Some('ς'), None) => 'σ',
        (Some('\u{2018}' | '\u{2019}'), None) => '\'',
        (Some(l), None) => strip_accent(l),
        _ => c,
    }
}

/// Latin letters with a diacritic fold to their base letter (`NFKD` minus marks).
fn strip_accent(c: char) -> char {
    const FROM: &str = "àáâãäåāăąçćĉċčďđèéêëēĕėęěĝğġģĥħìíîïĩīĭįıĵķĺļľŀłñńņňòóôõöøōŏőŕŗřśŝşšţťŧùúûüũūŭůűųŵýÿŷźżž";
    const TO: &str = "aaaaaaaaacccccddeeeeeeeeegggghhiiiiiiiiijklllllnnnnooooooooorrrssssttttuuuuuuuuuuwyyyzzz";
    FROM.chars().zip(TO.chars()).find(|&(from, _)| from == c).map_or(c, |(_, to)| to)
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32, 0x3040..=0x30ff | 0x3400..=0x4dbf | 0x4e00..=0x9fff | 0xf900..=0xfaff)
}

fn is_symbol(c: char) -> bool {
    // `\p{So}` approximated by the emoji and pictograph blocks.
    matches!(c as u32, 0x2190..=0x2bff | 0x1f000..=0x1faff)
}

fn word_start(before_previous: Option<char>, previous: Option<char>, c: char, next: Option<char>) -> bool {
    let after_apostrophe =
        matches!(previous, Some('\'' | '\u{2019}')) && before_previous.is_some_and(char::is_alphanumeric);
    (c.is_alphanumeric() || is_symbol(c))
        && !after_apostrophe
        && match previous {
            None => true,
            Some(p) => {
                !p.is_alphanumeric()
                    || (p.is_lowercase() && c.is_uppercase())
                    || (p.is_uppercase() && c.is_uppercase() && next.is_some_and(char::is_lowercase))
                    || p.is_numeric() != c.is_numeric()
                    || is_cjk(c)
            }
        }
}

impl Field {
    pub fn new(source: &str) -> Self {
        let chars: Vec<char> = source.chars().collect();
        let word_starts = (0..chars.len())
            .map(|i| {
                let at = |j: Option<usize>| j.and_then(|j| chars.get(j)).copied();
                word_start(at(i.checked_sub(2)), at(i.checked_sub(1)), chars[i], at(Some(i + 1)))
            })
            .collect();
        Self { text: chars.into_iter().map(fold_char).collect(), word_starts }
    }

    fn find(&self, token: &[char], from: usize) -> Option<usize> {
        if token.is_empty() || token.len() > self.text.len() {
            return None;
        }
        (from..=self.text.len() - token.len()).find(|&i| self.text[i..i + token.len()] == *token)
    }

    fn word_start_at(&self, index: usize) -> bool {
        self.word_starts.get(index).copied().unwrap_or(false)
    }
}

/// `foldSearchQuery`: folded tokens, wrapping quotes and extra whitespace removed.
pub fn fold_query(query: &str) -> Vec<Vec<char>> {
    let quotes: &[char] = &['"', '\'', '`', '\u{201c}', '\u{201d}', '\u{2018}', '\u{2019}'];
    query
        .trim()
        .trim_matches(quotes)
        .split_whitespace()
        .map(|token| token.chars().map(fold_char).collect::<Vec<_>>())
        .filter(|token| !token.is_empty())
        .collect()
}

const WHOLE_WORD: f32 = 1.0;
const WHOLE_NAME: f32 = 1.5;
const MID_WORD: f32 = 0.3;
const TYPO: f32 = 0.45;
const FILLER: &[&str] = &["a", "an", "and", "for", "in", "of", "on", "the", "to", "with"];

#[derive(Clone, Copy)]
struct TokenMatch {
    score: f32,
    start: usize,
    end: usize,
}

fn within_one_edit(left: &[char], right: &[char], swap_only: bool) -> bool {
    if left.len().abs_diff(right.len()) > if swap_only { 0 } else { 1 } {
        return false;
    }
    let i = left.iter().zip(right).take_while(|(a, b)| a == b).count();
    let (l, r) = (&left[i..], &right[i..]);
    let swapped = l.len() > 1 && r.len() > 1 && l[0] == r[1] && l[1] == r[0] && l[2..] == r[2..];
    if swap_only {
        return swapped;
    }
    fn tail(s: &[char]) -> &[char] {
        s.get(1..).unwrap_or_default()
    }
    swapped || l == r || tail(l) == tail(r) || tail(l) == r || l == tail(r)
}

/// `matchToken`, without joined words.
fn match_token(field: &Field, token: &[char], typos: bool, mid_word: bool) -> Option<TokenMatch> {
    let alnum = |c: char| c.is_alphanumeric();
    let own_boundary = !alnum(token[0]) && !is_symbol(token[0]);
    let mut best: Option<TokenMatch> = None;
    let mut from = 0;
    while let Some(index) = field.find(token, from) {
        from = index + 1;
        if field.word_start_at(index) || own_boundary {
            let end = index + token.len();
            let whole = end >= field.text.len() || field.word_start_at(end) || !alnum(field.text[end]);
            let mut run_end = end;
            while run_end < field.text.len() && alnum(field.text[run_end]) {
                run_end += 1;
            }
            let score = if whole { WHOLE_WORD } else { 0.7 + 0.2 * token.len() as f32 / (run_end - index) as f32 };
            if best.is_none_or(|b| score > b.score) {
                best = Some(TokenMatch { score, start: index, end });
            }
            if score == WHOLE_WORD {
                return best;
            }
        } else if mid_word && token.len() >= 4 && best.is_none() {
            best = Some(TokenMatch { score: MID_WORD, start: index, end: index + token.len() });
        }
    }
    if best.is_some_and(|b| b.score > MID_WORD) || !typos || token.len() < 3 || !token.iter().any(|c| c.is_alphabetic())
    {
        return best;
    }
    for index in 0..field.text.len() {
        if !field.word_start_at(index) {
            continue;
        }
        let first = field.text[index];
        let kept =
            first == token[0] || (token.len() > 3 && first == token[1] && field.text.get(index + 1) == Some(&token[0]));
        if !kept {
            continue;
        }
        let mut run_end = index + 1;
        while run_end < field.text.len() && alnum(field.text[run_end]) {
            run_end += 1;
        }
        let word = &field.text[index..run_end];
        let short = token.len() <= 4;
        let lengths: &[usize] =
            if short { &[token.len(), token.len() + 1] } else { &[token.len(), token.len() - 1, token.len() + 1] };
        for &length in lengths {
            if length <= word.len() && within_one_edit(token, &word[..length], short && length == token.len()) {
                return Some(TokenMatch { score: TYPO, start: index, end: index + length });
            }
        }
    }
    best
}

/// One field to match a query against.
pub struct Weighted<'a> {
    pub field: &'a Field,
    pub weight: f32,
    /// Only a match at a word start counts, without typos.
    pub keyword: bool,
    /// Where the thing lives (project, environment): a hit here is not strong.
    pub context: bool,
    /// A token of 4+ chars may match inside a word (thread titles only).
    pub mid_word: bool,
}

impl<'a> Weighted<'a> {
    pub fn new(field: &'a Field, weight: f32) -> Self {
        Self { field, weight, keyword: false, context: false, mid_word: false }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Match {
    /// About 0.3 (inside a word) to 1.5 (exact name).
    pub quality: f32,
    /// Every token matched at a word start without a typo.
    pub strong: bool,
    /// Char ranges matched in the first field.
    pub ranges: Vec<Range<usize>>,
}

/// `matchSearchFields`: every token must match some field; quality is the mean
/// of each token's best weighted score, plus a bonus for the whole name or phrase.
pub fn match_fields(tokens: &[Vec<char>], fields: &[Weighted], typos: bool) -> Option<Match> {
    if tokens.is_empty() {
        return None;
    }
    let filler_optional = tokens.iter().any(|t| !FILLER.contains(&t.iter().collect::<String>().as_str()));
    let mut ranges = Vec::new();
    let (mut total, mut counted, mut strong) = (0., 0, true);
    for token in tokens {
        let mut best = 0f32;
        let mut primary_hit = None;
        let mut at_word_start = false;
        for (i, weighted) in fields.iter().enumerate() {
            if weighted.field.text.is_empty() {
                continue;
            }
            if best >= weighted.weight && i > 0 && at_word_start {
                continue;
            }
            let Some(hit) = match_token(weighted.field, token, typos && !weighted.keyword, weighted.mid_word) else {
                continue;
            };
            if hit.score < if weighted.keyword { 0.7 } else { 0. } {
                continue;
            }
            if i == 0 {
                primary_hit = Some(hit);
            }
            if hit.score >= 0.7 && !weighted.context {
                at_word_start = true;
            }
            best = best.max(hit.score * weighted.weight);
        }
        if best == 0. {
            if filler_optional && FILLER.contains(&token.iter().collect::<String>().as_str()) {
                continue;
            }
            return None;
        }
        strong &= at_word_start;
        ranges.extend(primary_hit.map(|h| h.start..h.end));
        total += best;
        counted += 1;
    }
    let mut quality = total / counted.max(1) as f32;
    if let Some(primary) = fields.first() {
        let joined: Vec<char> = tokens.join(&' ');
        if primary.field.text == joined {
            quality += WHOLE_NAME - WHOLE_WORD;
        } else if tokens.len() > 1 && primary.field.find(&joined, 0).is_some() {
            quality += 0.25;
        }
    }
    Some(Match { quality, strong, ranges: merge(ranges) })
}

fn merge(mut ranges: Vec<Range<usize>>) -> Vec<Range<usize>> {
    ranges.sort_by_key(|r| r.start);
    let mut merged: Vec<Range<usize>> = Vec::new();
    for range in ranges {
        match merged.last_mut() {
            Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
            _ => merged.push(range),
        }
    }
    merged
}

/// `matchClass`: whole words and prefixes share a class.
pub fn class(quality: f32) -> u8 {
    if quality >= WHOLE_NAME {
        3
    } else if quality >= 0.7 {
        2
    } else if quality >= 0.4 {
        1
    } else {
        0
    }
}

/// `threadUsageBoost` without open history: recency of the last user message.
pub fn usage_boost(age_ms: i64) -> f32 {
    const HALF_LIFE: f64 = 14. * 86_400_000.;
    let recency = 2f64.powf(-(age_ms.max(0) as f64) / HALF_LIFE);
    (1. + recency).min(2.5) as f32
}

/// `messageSearchTokens(..).long` is non-empty: the server message search runs.
pub fn has_long_token(query: &str) -> bool {
    query.split_whitespace().any(|token| {
        let len = token.chars().count();
        len >= 3 || (len == 2 && token.chars().any(|c| is_cjk(c) || ('\u{ac00}'..='\u{d7af}').contains(&c)))
    })
}

/// `findSearchTokenRanges`: char ranges of `text` holding a long query token;
/// a token under four chars must start a word.
pub fn token_ranges(text: &str, query: &str) -> Vec<Range<usize>> {
    let field = Field::new(text);
    let mut ranges = Vec::new();
    for token in fold_query(query) {
        let len = token.len();
        if !(len >= 3 || (len == 2 && token.iter().copied().any(is_cjk))) {
            continue;
        }
        let needs_start = len < 4 && token[0].is_alphanumeric();
        let mut from = 0;
        while let Some(index) = field.find(&token, from) {
            if !needs_start || field.word_start_at(index) {
                ranges.push(index..index + len);
                from = index + len;
            } else {
                from = index + 1;
            }
        }
    }
    merge(ranges)
}

/// Splits `text` at char `ranges` into (part, highlighted) runs.
pub fn split_ranges(text: &str, ranges: &[Range<usize>]) -> Vec<(String, bool)> {
    let chars: Vec<char> = text.chars().collect();
    let mut parts = Vec::new();
    let mut at = 0;
    for range in ranges {
        let (start, end) = (range.start.min(chars.len()), range.end.min(chars.len()));
        if start > at {
            parts.push((chars[at..start].iter().collect(), false));
        }
        if end > start {
            parts.push((chars[start..end].iter().collect(), true));
        }
        at = at.max(end);
    }
    if at < chars.len() {
        parts.push((chars[at..].iter().collect(), false));
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quality(query: &str, title: &str) -> Option<f32> {
        let field = Field::new(title);
        let tokens = fold_query(query);
        let mut weighted = Weighted::new(&field, 1.);
        weighted.mid_word = true;
        let found = match_fields(&tokens, &[weighted], false).or_else(|| {
            let mut weighted = Weighted::new(&field, 1.);
            weighted.mid_word = true;
            match_fields(&tokens, &[weighted], true)
        });
        found.map(|m| m.quality)
    }

    #[test]
    fn whole_words_beat_prefixes_beat_mid_word() {
        let whole = quality("auth", "Share auth tokens").unwrap();
        let prefix = quality("auth", "Authentication flow").unwrap();
        let mid = quality("auth", "Fix oauth login").unwrap();
        assert!(whole > prefix && prefix > mid, "{whole} {prefix} {mid}");
        assert_eq!(quality("del", "Pick a model"), None, "short tokens never match mid-word");
        assert_eq!(quality("sidebar", "Sidebar"), Some(1.5), "an exact name gets the whole-name bonus");
    }

    #[test]
    fn word_starts_follow_case_digits_and_acronyms() {
        assert!(quality("schema", "JSONSchema").is_some_and(|q| q >= 1.));
        assert!(quality("code", "t3code").is_some_and(|q| q >= 1.), "a letter after a digit starts a word");
        assert_eq!(quality("ode", "t3code"), None, "no boundary inside a word");
        assert!(quality("3", "t3code").is_some_and(|q| q >= 1.), "digits start a word");
        assert!(quality("s", "Julius's notes").is_none(), "a possessive does not start a word");
    }

    #[test]
    fn typos_are_a_fallback_that_keeps_the_first_letter() {
        assert!(quality("railwya", "Railway deploy").is_some());
        assert!(quality("githbu", "GitHub actions").is_some());
        assert_eq!(quality("cron", "Config"), None);
        assert_eq!(quality("ing", "Nightly"), None);
    }

    #[test]
    fn every_token_must_match_except_filler() {
        assert!(quality("fix the sidebar", "Fix sidebar").is_some());
        assert_eq!(quality("fix composer", "Fix sidebar"), None);
    }

    #[test]
    fn snippet_ranges_skip_short_and_mid_word_tokens() {
        assert_eq!(token_ranges("Rename sidebar row", "the side row"), vec![7..11, 15..18]);
        assert_eq!(token_ranges("another", "the"), Vec::<Range<usize>>::new(), "short tokens need a word start");
        assert_eq!(split_ranges("ab cd", &[3..5]), vec![("ab ".to_owned(), false), ("cd".to_owned(), true)]);
        assert!(has_long_token("fix it") && !has_long_token("a b"));
    }
}
