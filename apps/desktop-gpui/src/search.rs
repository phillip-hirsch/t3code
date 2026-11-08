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
    fn tail(s: &[char]) -> &[char] {
        s.get(1..).unwrap_or_default()
    }
    if left.len().abs_diff(right.len()) > usize::from(!swap_only) {
        return false;
    }
    let i = left.iter().zip(right).take_while(|(a, b)| a == b).count();
    let (l, r) = (&left[i..], &right[i..]);
    let swapped = l.len() > 1 && r.len() > 1 && l[0] == r[1] && l[1] == r[0] && l[2..] == r[2..];
    if swap_only {
        return swapped;
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
    } else {
        u8::from(quality >= 0.4)
    }
}

const DAY_MS: f64 = 86_400_000.;
/// `SEARCH_RECENCY_HALF_LIFE_MS`: activity recency and open counts halve every two weeks.
const HALF_LIFE_MS: f64 = 14. * DAY_MS;
/// `PICK_WINDOW_MS`: a pick counts for three weeks from when it was made.
const PICK_WINDOW_MS: f64 = 21. * DAY_MS;

fn decay(age_ms: f64) -> f64 {
    2f64.powf(-age_ms.max(0.) / HALF_LIFE_MS)
}

/// How a thread was used, as `threadSearchSubject` reads it: the user's latest activity and the
/// thread's opens, this client's own latest open merged in.
#[derive(Clone, Debug, Default)]
pub struct ThreadUse {
    /// Epoch ms.
    pub activity_at: Option<i64>,
    pub opens: Option<crate::model::ThreadOpens>,
}

impl ThreadUse {
    /// What the user wrote, not agent wakes; a thread the user never wrote in counts from its
    /// creation, and an older server's missing field falls back to the latest user message.
    pub fn of(thread: &crate::model::ThreadShell, local_opened_at: Option<i64>) -> Self {
        let parse = crate::timestamp::parse_ms;
        let activity_at = match &thread.latest_user_authored_message_at {
            Some(authored) => parse(authored.as_deref().unwrap_or(&thread.created_at)),
            None => thread.latest_user_message_at.as_deref().and_then(parse).or_else(|| parse(&thread.updated_at)),
        };
        #[expect(clippy::cast_precision_loss, reason = "epoch ms are far below 2^52")]
        let local = local_opened_at.map(|at| at as f64).filter(|at| *at > 0.);
        let server = thread.opens.as_ref();
        let opens = match local {
            Some(at) if at > server.map_or(0., |o| o.opened_at) => Some(crate::model::ThreadOpens {
                opened_at: at,
                ..server.cloned().unwrap_or(crate::model::ThreadOpens { score: 1., ..Default::default() })
            }),
            _ => server.cloned(),
        };
        Self { activity_at, opens }
    }

    /// `threadUsageBoost`: 1 for a stale, never-opened thread, about 2 for one active today, and
    /// up to 2.5 for one opened often.
    #[expect(clippy::cast_possible_truncation, clippy::cast_precision_loss, reason = "a boost clamped to 1..=2.5")]
    pub fn boost(&self, now: i64) -> f32 {
        let now = now as f64;
        let recency = self.activity_at.map_or(0., |at| decay(now - at as f64));
        let opens = self.opens.as_ref().map_or(0., |o| o.score * decay(now - o.opened_at));
        (1. + recency + 0.5 * (1. + opens).log2()).min(2.5) as f32
    }

    /// `threadRecentUse`: the later of the user's last message and last open.
    #[expect(clippy::cast_possible_truncation, reason = "epoch ms")]
    pub fn recent(&self) -> i64 {
        self.activity_at.unwrap_or(0).max(self.opens.as_ref().map_or(0, |o| o.opened_at as i64))
    }

    /// `activePick`: when the thread was last picked for this query (exactly, else as a prefix
    /// of three letters or more), while that pick lasts.
    #[expect(clippy::cast_precision_loss, reason = "epoch ms")]
    pub fn picked_at(&self, tokens: &[Vec<char>], now: i64) -> Option<f64> {
        let query = picked_query(tokens);
        let prefix = query.chars().count() >= 3;
        let live = self.opens.iter().flat_map(|o| &o.picks).filter(|pick| now as f64 - pick.at <= PICK_WINDOW_MS);
        let latest = |picks: &mut dyn Iterator<Item = &crate::model::ThreadPick>| {
            picks.map(|pick| pick.at).max_by(f64::total_cmp)
        };
        latest(&mut live.clone().filter(|pick| pick.query == query))
            .or_else(|| latest(&mut live.filter(|pick| prefix && pick.query.starts_with(&query))))
    }
}

/// `pickedQuery`: the folded query cut to the 32 chars the server stores.
fn picked_query(tokens: &[Vec<char>]) -> String {
    let folded: String = tokens.iter().map(|t| t.iter().collect::<String>()).collect::<Vec<_>>().join(" ");
    folded.chars().take(32).collect::<String>().trim().to_owned()
}

/// `messageSearchTokens(..).long` is non-empty: the server message search runs.
pub fn has_long_token(query: &str) -> bool {
    query.split_whitespace().any(|token| {
        let len = token.chars().count();
        len >= 3 || (len == 2 && token.chars().any(|c| is_cjk(c) || ('\u{ac00}'..='\u{d7af}').contains(&c)))
    })
}

/// `searchableBranch`: the app's worktree prefix ("t3/", "t3code/" before it) matches nearly
/// every thread, so it is dropped, but only before more text.
pub fn searchable_branch(branch: &str) -> &str {
    ["t3/", "t3code/"]
        .iter()
        .find_map(|prefix| branch.strip_prefix(prefix).filter(|rest| rest.starts_with(|c: char| !c.is_whitespace())))
        .unwrap_or(branch)
}

/// `threadFieldQuery`: pull request prefixes ("pr 12", "pull request #12", "#12") and the
/// worktree prefix come off, so a query matches the stored fields.
fn thread_field_query(query: &str) -> &str {
    let mut query = ["pr", "pull request", "pull"]
        .iter()
        .find_map(|prefix| {
            let rest = query.strip_prefix(prefix)?.trim_start();
            rest.strip_prefix('#').unwrap_or(rest).starts_with(|c: char| c.is_ascii_digit()).then_some(rest)
        })
        .unwrap_or(query);
    if let Some(rest) = query.strip_prefix('#') {
        query = rest.trim_start();
    }
    searchable_branch(query)
}

/// `REFERENCE_PREFIX_MIN`: IDs and links have many word starts, so a short query would hit them all.
const REFERENCE_PREFIX_MIN: usize = 8;

/// `matchReference`: a long prefix of the thread ID or a pull request reference, or any long
/// part of one that holds a digit. A number is never cut short (".../pull/2288" is not 22889).
fn matches_reference<'a>(query: &str, mut references: impl Iterator<Item = &'a str>) -> bool {
    if query.chars().count() < REFERENCE_PREFIX_MIN {
        return false;
    }
    let not_cut = |text: &str, end: usize| !text[end..].starts_with(char::is_numeric);
    let anywhere = query.chars().any(char::is_numeric);
    references.any(|reference| {
        let folded = reference.to_lowercase();
        if !anywhere {
            return folded.starts_with(query);
        }
        folded.find(query).is_some_and(|at| not_cut(&folded, at + query.len()))
            || (query.starts_with(folded.as_str()) && not_cut(query, folded.len()))
    })
}

/// What thread search matches a thread by (`ThreadSearchSubject`).
pub struct ThreadSubject<'a> {
    pub id: &'a str,
    pub title: &'a str,
    pub branch: Option<&'a str>,
    pub project: Option<&'a str>,
    pub pull_requests: &'a [crate::thread_pull_requests::SearchPullRequest],
}

/// `scoreThreadSearch` without picks: titles and pull request numbers count fully, pull request
/// titles and branches less, the project least. `ranges` are the title's.
pub fn match_thread(tokens: &[Vec<char>], thread: &ThreadSubject, typos: bool) -> Option<Match> {
    let query: String = tokens.iter().map(|t| t.iter().collect::<String>()).collect::<Vec<_>>().join(" ");
    let field_tokens = fold_query(thread_field_query(&query));
    let title = Field::new(thread.title);
    let numbers: Vec<Field> = thread.pull_requests.iter().map(|pr| Field::new(&format!("#{}", pr.number))).collect();
    let titles: Vec<Option<Field>> =
        thread.pull_requests.iter().map(|pr| pr.title.as_deref().map(Field::new)).collect();
    let branch = thread.branch.map(|b| Field::new(searchable_branch(b)));
    let project = thread.project.map(Field::new);
    let mut fields = vec![Weighted { mid_word: true, ..Weighted::new(&title, 1.) }];
    for (number, title) in numbers.iter().zip(&titles) {
        fields.push(Weighted::new(number, 1.));
        fields.extend(title.as_ref().map(|title| Weighted::new(title, 0.5)));
    }
    fields.extend(branch.as_ref().map(|branch| Weighted::new(branch, 0.6)));
    fields.extend(project.as_ref().map(|project| Weighted { context: true, ..Weighted::new(project, 0.25) }));
    if let Some(found) = match_fields(&field_tokens, &fields, typos) {
        // A multi-word match the title shows none of must sit in one field to explain it.
        let explained = !found.ranges.is_empty() || field_tokens.len() < 2 || has_hint(&field_tokens, thread, typos);
        return explained.then_some(found);
    }
    let references = thread.pull_requests.iter().flat_map(|pr| pr.references.iter().map(String::as_str));
    matches_reference(&query, std::iter::once(thread.id).chain(references)).then(|| Match {
        quality: 0.6,
        strong: true,
        ranges: Vec::new(),
    })
}

/// `matchHint` found a field: the first that holds the whole query, else, past the words the
/// project holds, the first that holds the rest.
fn has_hint(tokens: &[Vec<char>], thread: &ThreadSubject, typos: bool) -> bool {
    let holds = |text: &str, tokens: &[Vec<char>]| {
        match_fields(tokens, &[Weighted::new(&Field::new(text), 1.)], typos).is_some()
    };
    let candidates: Vec<String> = thread
        .pull_requests
        .iter()
        .map(|pr| match &pr.title {
            Some(title) => format!("#{} {title}", pr.number),
            None => format!("#{}", pr.number),
        })
        .chain(thread.branch.map(|b| searchable_branch(b).to_owned()))
        .chain(thread.project.map(str::to_owned))
        .collect();
    let hint_for = |tokens: &[Vec<char>]| candidates.iter().any(|text| !text.is_empty() && holds(text, tokens));
    if hint_for(tokens) {
        return true;
    }
    let rest: Vec<Vec<char>> = tokens
        .iter()
        .filter(|token| !thread.project.is_some_and(|project| holds(project, std::slice::from_ref(token))))
        .cloned()
        .collect();
    !rest.is_empty() && rest.len() != tokens.len() && hint_for(&rest)
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

    const DAY: i64 = 86_400_000;

    fn shell(fields: serde_json::Value) -> crate::model::ThreadShell {
        let mut value = serde_json::json!({"id": "t", "projectId": "p", "title": "T", "status": "idle",
            "createdAt": "1970-01-01T00:00:00Z", "updatedAt": "1970-01-02T00:00:00Z"});
        if let (Some(base), serde_json::Value::Object(fields)) = (value.as_object_mut(), fields) {
            base.extend(fields);
        }
        serde_json::from_value(value).expect("a thread shell")
    }

    #[test]
    #[expect(clippy::cast_precision_loss, reason = "test epoch ms")]
    fn a_thread_is_used_when_written_in_or_opened_here_or_anywhere() {
        let written = shell(serde_json::json!({"latestUserAuthoredMessageAt": "1970-01-03T00:00:00Z"}));
        assert_eq!(ThreadUse::of(&written, None).recent(), 2 * DAY);
        assert_eq!(ThreadUse::of(&shell(serde_json::json!({"latestUserAuthoredMessageAt": null})), None).recent(), 0);
        let opened = serde_json::json!({"latestUserAuthoredMessageAt": "1970-01-03T00:00:00Z",
            "opens": {"openedAt": (5 * DAY) as f64, "score": 3, "picks": [{"query": "sidebar", "at": (4 * DAY) as f64}]}});
        let opened = shell(opened);
        assert_eq!(ThreadUse::of(&opened, None).recent(), 5 * DAY, "an open later than the last message");
        let here = ThreadUse::of(&opened, Some(9 * DAY));
        assert_eq!(
            (here.recent(), here.opens.as_ref().map(|o| o.score)),
            (9 * DAY, Some(3.)),
            "a local open keeps the stats"
        );
        assert_eq!(ThreadUse::of(&written, Some(9 * DAY)).opens.map(|o| o.score), Some(1.));
        assert!(ThreadUse::of(&opened, None).boost(5 * DAY) > ThreadUse::of(&written, None).boost(5 * DAY));
        let unreadable = shell(serde_json::json!({"opens": {"openedAt": "soon"}}));
        assert!(unreadable.opens.is_none(), "an unreadable value is no opens, not a lost thread");
        let used = ThreadUse::of(&opened, None);
        assert_eq!(used.picked_at(&fold_query("Sidebar"), 5 * DAY), Some((4 * DAY) as f64));
        assert_eq!(used.picked_at(&fold_query("sid"), 5 * DAY), Some((4 * DAY) as f64), "a prefix of three letters");
        assert_eq!(used.picked_at(&fold_query("si"), 5 * DAY), None);
        assert_eq!(used.picked_at(&fold_query("sidebar"), 30 * DAY), None, "picks last three weeks");
    }

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
    fn threads_match_by_pull_request_number_title_and_reference() {
        let pull_requests = [crate::thread_pull_requests::SearchPullRequest {
            number: 22889,
            title: Some("Speed up the diff worker".into()),
            references: ["pingdotgg/t3code#22889".into(), "https://github.com/pingdotgg/t3code/pull/22889".into()],
        }];
        let thread = ThreadSubject {
            id: "c0ffee12-3456",
            title: "Sidebar polish",
            branch: Some("t3/fix-flaky-ci"),
            project: Some("t3code"),
            pull_requests: &pull_requests,
        };
        let finds = |query: &str| match_thread(&fold_query(query), &thread, false).is_some();
        for query in ["22889", "#22889", "pr 22889", "PR#22889", "pull request #22889", "diff worker", "t3/fix-flaky"] {
            assert!(finds(query), "{query}");
        }
        assert!(finds("https://github.com/pingdotgg/t3code/pull/22889/files"), "a link that goes on");
        assert!(finds("pingdotgg/t3code#22889") && finds("c0ffee12"), "a reference or the thread ID");
        assert!(!finds("github.com/pingdotgg/t3code/pull/2288"), "a number is never cut short");
        assert!(!finds("c0ffee"), "a short ID prefix matches nothing");
        assert!(finds("t3code diff worker"), "the project's words, then a field holding the rest");
        assert!(!finds("speed flaky"), "words spread over unrelated fields are a coincidence");
        assert_eq!(searchable_branch("t3/"), "t3/", "the prefix stays until more text follows");
    }

    #[test]
    #[expect(clippy::single_range_in_vec_init, reason = "a list of one range is the expected value")]
    fn snippet_ranges_skip_short_and_mid_word_tokens() {
        assert_eq!(token_ranges("Rename sidebar row", "the side row"), vec![7..11, 15..18]);
        assert_eq!(token_ranges("another", "the"), Vec::<Range<usize>>::new(), "short tokens need a word start");
        assert_eq!(split_ranges("ab cd", &[3..5]), vec![("ab ".to_owned(), false), ("cd".to_owned(), true)]);
        assert!(has_long_token("fix it") && !has_long_token("a b"));
    }
}
