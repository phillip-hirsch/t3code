//! URLs and file paths in terminal output (`apps/web/src/terminal-links.ts`), and where a
//! path link points (`resolvePathLinkTarget` in `packages/shared/src/fileLinks.ts`).
//! Positions are char indexes into the line.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LinkKind {
    Url,
    Path,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct LinkMatch {
    pub kind: LinkKind,
    pub text: String,
    pub start: usize,
    pub end: usize,
}

type Range = (usize, usize);

fn overlaps(a: Range, b: Range) -> bool {
    a.0 < b.1 && b.0 < a.1
}

/// ``[^\s"'`<>]``.
fn is_link_char(c: char) -> bool {
    !c.is_whitespace() && !matches!(c, '"' | '\'' | '`' | '<' | '>')
}

/// `[\p{L}\p{M}\p{N}._-]`.
fn is_word(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '.' | '_' | '-') || is_mark(c)
}

fn is_mark(c: char) -> bool {
    matches!(c as u32, 0x0300..=0x036f | 0x1ab0..=0x1aff | 0x1dc0..=0x1dff | 0x20d0..=0x20ff | 0xfe20..=0xfe2f)
}

fn is_sep(c: char) -> bool {
    c == '/' || c == '\\'
}

pub(super) fn is_terminal_url(text: &str) -> bool {
    let lower = text.get(..8).unwrap_or(text).to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

fn starts_with_ci(chars: &[char], at: usize, prefix: &str) -> bool {
    let mut i = at;
    for p in prefix.chars() {
        match chars.get(i) {
            Some(c) if c.eq_ignore_ascii_case(&p) => i += 1,
            _ => return false,
        }
    }
    true
}

/// How many chars the path prefix `~/ ./ ../ / X:\ \\` takes at `at`.
fn path_prefix(chars: &[char], at: usize) -> Option<usize> {
    let c = |i: usize| chars.get(at + i).copied();
    match c(0)? {
        '~' if c(1).is_some_and(is_sep) => Some(2),
        '.' if c(1).is_some_and(is_sep) => Some(2),
        '.' if c(1) == Some('.') && c(2).is_some_and(is_sep) => Some(3),
        '\\' if c(1) == Some('\\') => Some(2),
        '/' => Some(1),
        l if l.is_ascii_alphabetic() && c(1) == Some(':') && c(2).is_some_and(is_sep) => Some(3),
        _ => None,
    }
}

/// `(?::\d+){0,2}` at `at`: its length.
fn position_suffix(chars: &[char], at: usize) -> usize {
    let mut len = 0;
    for _ in 0..2 {
        let digits = chars[(at + len + 1).min(chars.len())..].iter().take_while(|c| c.is_ascii_digit()).count();
        if chars.get(at + len) != Some(&':') || digits == 0 {
            break;
        }
        len += 1 + digits;
    }
    len
}

fn take_while(chars: &[char], at: usize, f: impl Fn(char) -> bool) -> usize {
    chars[at.min(chars.len())..].iter().take_while(|c| f(**c)).count()
}

/// `URL_PATTERN`: the raw match length at `at`.
fn match_url(chars: &[char], at: usize) -> Option<usize> {
    let scheme = if starts_with_ci(chars, at, "https://") {
        8
    } else if starts_with_ci(chars, at, "http://") {
        7
    } else {
        return None;
    };
    let rest = take_while(chars, at + scheme, is_link_char);
    (rest > 0).then_some(scheme + rest)
}

/// `FILE_PATH_PATTERN`.
fn match_path(chars: &[char], at: usize) -> Option<usize> {
    if let Some(prefix) = path_prefix(chars, at) {
        let rest = take_while(chars, at + prefix, is_link_char);
        return (rest > 0).then_some(prefix + rest);
    }
    if at > 0 && is_word(chars[at - 1]) {
        return None;
    }
    let mut len = take_while(chars, at, is_word);
    if len == 0 {
        return None;
    }
    let mut segments = 0;
    while chars.get(at + len).copied().is_some_and(is_sep) {
        let next = path_segment(chars, at + len + 1);
        if next == 0 {
            break;
        }
        len += 1 + next;
        segments += 1;
    }
    (segments > 0).then(|| len + position_suffix(chars, at + len))
}

/// One segment after a separator in `FILE_PATH_PATTERN`: word chars and `$+@` (`posts.$id`,
/// `.pnpm/react@18`), `[id]` or `[[...]]` groups, and `(group)` only before another separator,
/// so `src/foo.ts(3,10)` stays a compiler location. A `]` needs its `[`.
fn path_segment(chars: &[char], at: usize) -> usize {
    let mut i = at;
    loop {
        let group = match chars.get(i) {
            Some(&c) if is_word(c) || matches!(c, '$' | '+' | '@') => Some(1),
            Some('[') => {
                let open = take_while(chars, i, |c| c == '[').min(2);
                let word = take_while(chars, i + open, is_word);
                let close = take_while(chars, i + open + word, |c| c == ']').min(2);
                (word > 0 && close > 0).then_some(open + word + close)
            }
            Some('(') => {
                let word = take_while(chars, i + 1, is_word);
                let closes = word > 0 && chars.get(i + 1 + word) == Some(&')');
                (closes && chars.get(i + 2 + word).copied().is_some_and(is_sep)).then_some(word + 2)
            }
            _ => None,
        };
        match group {
            Some(len) => i += len,
            None => return i - at,
        }
    }
}

/// `QUOTED_FILE_PATH_PATTERN` at an opening quote: (path, position, whole match) lengths.
fn match_quoted(chars: &[char], at: usize) -> Option<(usize, usize, usize)> {
    let quote = *chars.get(at)?;
    if !matches!(quote, '"' | '\'' | '`') {
        return None;
    }
    let start = at + 1;
    let quoted_word = |c: char| is_word(c) || c == ' ';
    let words = take_while(chars, start, quoted_word);
    // A path prefix, or words then a separator, then anything up to the quote.
    let leads =
        path_prefix(chars, start).is_some() || (words > 0 && chars.get(start + words).copied().is_some_and(is_sep));
    let close = take_while(chars, start, |c| c != quote && c != '\r' && c != '\n');
    let path = if leads && chars.get(start + close) == Some(&quote) {
        close
    } else {
        // Or `name.ext` with an optional position: the last dot, then an extension without spaces.
        let dot = (start + 1..start + words).rev().find(|i| chars[*i] == '.')?;
        if dot + 1 == start + words || chars[dot + 1..start + words].contains(&' ') {
            return None;
        }
        let len = words + position_suffix(chars, start + words);
        if chars.get(start + len) != Some(&quote) {
            return None;
        }
        len
    };
    let position = position_suffix(chars, start + path + 1);
    Some((path, position, path + 2 + position))
}

fn trim_closing(text: &str, kind: LinkKind) -> &str {
    let punctuation: &[char] = match kind {
        LinkKind::Path => &['.', ',', ';', ':', '!', '?'],
        LinkKind::Url => &['.', ',', ';', '!', '?'],
    };
    let mut out = text.trim_end_matches(punctuation);
    for (open, close) in [('(', ')'), ('[', ']'), ('{', '}')] {
        while out.ends_with(close) && out.matches(open).count() < out.matches(close).count() {
            out = &out[..out.len() - 1];
        }
    }
    out
}

/// `REQUEST_ROUTE_PATTERN` and `SCHEME_URL_PATTERN`: spans that are never paths.
fn non_path_ranges(chars: &[char]) -> Vec<Range> {
    const METHODS: [&str; 10] =
        ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS", "CONNECT", "TRACE", "proxying"];
    let mut ranges = Vec::new();
    for at in 0..chars.len() {
        let boundary = at == 0 || chars[at - 1].is_whitespace() || matches!(chars[at - 1], '"' | '\'' | '`');
        if boundary {
            for method in METHODS {
                let mut i = at + method.chars().count();
                if !chars[at..].iter().zip(method.chars()).all(|(a, b)| *a == b) || i > chars.len() {
                    continue;
                }
                let spaces = take_while(chars, i, char::is_whitespace);
                if spaces == 0 {
                    continue;
                }
                i += spaces;
                if matches!(chars.get(i), Some('"' | '\'' | '`')) {
                    i += 1;
                }
                if chars.get(i) == Some(&'/') {
                    i += 1;
                } else {
                    let name = take_while(chars, i, |c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
                    if name == 0 || chars.get(i + name) != Some(&'/') {
                        continue;
                    }
                    i += name + 1;
                }
                i += take_while(chars, i, |c| !c.is_whitespace() && c != '?');
                if chars.get(i) != Some(&'?') {
                    continue;
                }
                i += 1 + take_while(chars, i + 1, |c| !c.is_whitespace());
                ranges.push((at.saturating_sub(usize::from(at > 0)), i));
            }
        }
        let scheme_char = |c: char| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-');
        if at > 0 && scheme_char(chars[at - 1]) {
            continue;
        }
        let sign = usize::from(matches!(chars[at], '+' | '-'));
        let first = at + sign;
        if !chars.get(first).is_some_and(char::is_ascii_alphabetic) {
            continue;
        }
        let scheme = 1 + take_while(chars, first + 1, scheme_char);
        let colon = first + scheme;
        if scheme >= 2
            && chars.get(colon) == Some(&':')
            && chars.get(colon + 1) == Some(&'/')
            && chars.get(colon + 2) == Some(&'/')
        {
            let rest = take_while(chars, colon + 3, is_link_char);
            if rest > 0 {
                ranges.push((at, colon + 3 + rest));
            }
        }
    }
    ranges
}

/// `extractTerminalLinks`.
pub(super) fn extract(line: &str) -> Vec<LinkMatch> {
    let chars: Vec<char> = line.chars().collect();
    let text = |start: usize, len: usize| chars[start..start + len].iter().collect::<String>();
    let mut found: Vec<LinkMatch> = Vec::new();
    // `POWERSHELL_PROMPT_PATH_PATTERN`.
    if line.starts_with("PS ")
        && path_prefix(&chars, 3).is_some()
        && let Some(close) =
            chars[3..].iter().position(|c| matches!(c, '>' | '\r' | '\n')).filter(|i| chars[3 + i] == '>')
    {
        found.push(LinkMatch { kind: LinkKind::Path, text: text(3, close), start: 3, end: 3 + close });
    }
    let collides = |found: &[LinkMatch], range: Range| found.iter().any(|m| overlaps(range, (m.start, m.end)));
    let scan = |found: &mut Vec<LinkMatch>, kind: LinkKind, quoted: bool| {
        let mut at = 0;
        while at < chars.len() {
            let (start, raw_len, raw, position) = if quoted {
                match match_quoted(&chars, at) {
                    Some((path, position, total)) => {
                        (at + 1, total, format!("{}{}", text(at + 1, path), text(at + path + 2, position)), position)
                    }
                    None => {
                        at += 1;
                        continue;
                    }
                }
            } else {
                let len = if kind == LinkKind::Url { match_url(&chars, at) } else { match_path(&chars, at) };
                match len {
                    Some(len) => (at, len, text(at, len), 0),
                    None => {
                        at += 1;
                        continue;
                    }
                }
            };
            at += raw_len;
            let trimmed = if quoted { raw.as_str() } else { trim_closing(&raw, kind) };
            if trimmed.is_empty() || (kind == LinkKind::Path && is_terminal_url(trimmed)) {
                continue;
            }
            let end = start + trimmed.chars().count() + usize::from(position > 0);
            if !collides(found, (start, end)) {
                found.push(LinkMatch { kind, text: trimmed.to_owned(), start, end });
            }
        }
    };
    scan(&mut found, LinkKind::Url, false);
    scan(&mut found, LinkKind::Path, true);
    scan(&mut found, LinkKind::Path, false);
    let excluded = non_path_ranges(&chars);
    found.retain(|m| m.kind == LinkKind::Url || !excluded.iter().any(|r| overlaps((m.start, m.end), *r)));
    found.sort_by_key(|m| m.start);
    found
}

fn is_windows_style(path: &str) -> bool {
    let b = path.as_bytes();
    path.starts_with("\\\\") || (b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':')
}

fn is_absolute(path: &str) -> bool {
    path.starts_with('/') || is_windows_style(path)
}

/// `splitFilePathPosition`: the path and its `:line[:column]` suffix.
pub(super) fn split_position(text: &str) -> (&str, &str) {
    let chars: Vec<char> = text.chars().collect();
    for start in 0..chars.len() {
        if chars[start] == ':' && position_suffix(&chars, start) == chars.len() - start {
            let byte = text.char_indices().nth(start).map_or(text.len(), |(i, _)| i);
            if byte > 0 {
                return text.split_at(byte);
            }
        }
    }
    (text, "")
}

/// `inferHomeFromCwd`: `/Users/x`, `/home/x` or `X:\Users\x`.
fn home_from_cwd(cwd: &str) -> Option<String> {
    let parts: Vec<&str> = cwd.split(['/', '\\']).collect();
    let (sep, users) = if is_windows_style(cwd) { ('\\', 1) } else { ('/', 1) };
    let named = parts.get(users).is_some_and(|p| matches!(*p, "Users" | "home"));
    let name = parts.get(users + 1).filter(|p| !p.is_empty())?;
    named.then(|| parts[..users + 2].join(&sep.to_string())).filter(|_| !name.is_empty())
}

fn join(base: &str, path: &str, sep: char) -> String {
    let base = base.trim_end_matches(['/', '\\']);
    let path = path.trim_start_matches(['/', '\\']);
    let path = if sep == '\\' { path.replace('/', "\\") } else { path.to_owned() };
    format!("{base}{sep}{path}")
}

/// `resolvePathLinkTarget`: a path link anchored at the terminal's directory, its position kept.
pub(super) fn resolve_path(text: &str, cwd: &str) -> String {
    let (path, position) = split_position(text);
    let sep = if is_windows_style(cwd) { '\\' } else { '/' };
    let resolved = if path.starts_with("~/") || (sep == '\\' && path.starts_with("~\\")) {
        match home_from_cwd(cwd) {
            Some(home) => join(&home, &path[2..], if is_windows_style(&home) { '\\' } else { '/' }),
            None => path.to_owned(),
        }
    } else if !is_absolute(path) {
        join(cwd, path.strip_prefix(".\\").or_else(|| path.strip_prefix("./")).unwrap_or(path), sep)
    } else {
        path.to_owned()
    };
    format!("{resolved}{position}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn links(line: &str) -> Vec<(LinkKind, String, usize, usize)> {
        extract(line).into_iter().map(|m| (m.kind, m.text, m.start, m.end)).collect()
    }

    fn path(text: &str, start: usize) -> (LinkKind, String, usize, usize) {
        (LinkKind::Path, text.to_owned(), start, start + text.chars().count())
    }

    fn url(text: &str, start: usize) -> (LinkKind, String, usize, usize) {
        (LinkKind::Url, text.to_owned(), start, start + text.chars().count())
    }

    #[test]
    fn powershell_prompts_keep_the_whole_path() {
        for p in [
            r"C:\dev\ai-test\dir 1",
            "C:\\",
            r"\\server\my share\dir 1",
            "/home/Jane Doe/project",
            r"C:\dev\folder (draft)\name!",
        ] {
            assert_eq!(links(&format!("PS {p}> ")), [path(p, 3)], "{p}");
        }
        let prompt = r"PS C:\dev\dir 1\test> ";
        assert_eq!(
            links(&format!("{prompt}echo https://example.com")),
            [path(r"C:\dev\dir 1\test", 3), url("https://example.com", prompt.len() + 5)]
        );
        assert_eq!(links(r"see C:\dev\project for details"), [path(r"C:\dev\project", 4)]);
    }

    #[test]
    fn windows_unicode_and_diagnostic_paths() {
        for p in [r"src\index.ts:12", r".\src\index.ts:12:7", r"..\src\index.ts:12", r"\\server\share\index.ts:12"] {
            assert_eq!(links(&format!("error at {p}: failed")), [path(p, 9)], "{p}");
        }
        for p in ["src/żółć.ts:12:7", "żółć/main.ts:12", "src/cafe\u{301}.ts:12", "src/𐐀.ts:12"] {
            assert_eq!(links(&format!("see {p} for details")), [path(p, 4)], "{p}");
        }
        for p in [
            "src/app/[id]/page.tsx:3:10",
            "src/routes/(app)/+page.svelte:12:4",
            "node_modules/.pnpm/react@18.3.1/node_modules/react/index.js:12:5",
            "app/routes/posts.$id.tsx:5",
        ] {
            assert_eq!(links(&format!("see {p} for details")), [path(p, 4)], "{p}");
        }
        assert_eq!(links("see src/foo.ts(3,10) here"), [path("src/foo.ts", 4)]);
        for (line, p) in [
            ("./main.go:10:5: undefined: x", "./main.go:10:5"),
            ("C:\\dev\\app\\src\\main.c:10:5: error: expected ';'", "C:\\dev\\app\\src\\main.c:10:5"),
            ("wrote ./out/report.txt:", "./out/report.txt"),
        ] {
            let start = line[..line.find(p).unwrap()].chars().count();
            assert_eq!(links(line), [path(p, start)], "{line}");
        }
        assert_eq!(links("wrote /tmp/report?draft"), [path("/tmp/report?draft", 6)]);
    }

    #[test]
    fn quoted_paths_and_their_positions() {
        let text = r"C:\Users\Jane Doe\project\index.ts:12:7";
        for q in ['"', '\'', '`'] {
            assert_eq!(links(&format!("see {q}{text}{q}.")), [path(text, 5)]);
        }
        for p in [r"src\my folder\index.ts", "./my folder/index.ts", "/home/Jane Doe/file)"] {
            let len = p.chars().count();
            assert_eq!(links(&format!("\"{p}\":12:7: error")), [(LinkKind::Path, format!("{p}:12:7"), 1, len + 7)]);
        }
        assert_eq!(links("error: \"something went wrong\""), []);
        let line = "\"C:\\Jane Doe\\index.ts:12\" https://example.com/docs src/index.ts";
        assert_eq!(
            links(line),
            [
                path("C:\\Jane Doe\\index.ts:12", 1),
                url("https://example.com/docs", 26),
                path("src/index.ts", line.len() - 12)
            ]
        );
    }

    #[test]
    fn urls_trim_punctuation_and_routes_are_not_paths() {
        assert_eq!(
            links("(https://example.com/docs), ./src/main.ts:12."),
            [url("https://example.com/docs", 1), path("./src/main.ts:12", 28)]
        );
        assert_eq!(links("GET https://example.test/items/foo:"), [url("https://example.test/items/foo:", 4)]);
        for line in [
            " GET /api/trpc/post.list,user.me?batch=1 200 in 35ms",
            "GET /api?+debug 200",
            "Started GET \"/posts?page=2\" for ::1",
            "proxying api/trpc/post.list?batch=1 to the backend",
            "connected to redis://127.0.0.1:6379/0",
            "    at main (file:///Users/jane/app/src/index.mjs:3:7)",
            "-redis://localhost:6379/0",
        ] {
            assert_eq!(links(line), [], "{line}");
        }
        assert_eq!(
            links("src/db.ts:4 redis://localhost:6379/0 C:/app/src/db.ts:9"),
            [path("src/db.ts:4", 0), path("C:/app/src/db.ts:9", 37)]
        );
        assert!(is_terminal_url("HTTPS://example.com/docs"));
        assert!(!is_terminal_url("httpsdocs/readme.md"));
    }

    #[test]
    fn paths_resolve_against_the_terminal_directory() {
        assert_eq!(
            resolve_path(r"src\my folder\index.ts:12:7", r"C:\Users\Jane Doe\project"),
            r"C:\Users\Jane Doe\project\src\my folder\index.ts:12:7"
        );
        assert_eq!(resolve_path("C:/Users/j/p/src/main.ts:12", r"C:\Users\j\p"), "C:/Users/j/p/src/main.ts:12");
        assert_eq!(resolve_path("main.c:10:5", "/Users/j/project"), "/Users/j/project/main.c:10:5");
        assert_eq!(resolve_path("~/notes.md", "/home/jane/app"), "/home/jane/notes.md");
        assert_eq!(split_position("a/b.ts:3:4"), ("a/b.ts", ":3:4"));
    }
}
