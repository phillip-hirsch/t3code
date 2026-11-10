//! A change request body split into prose, media and media tables (`splitPullRequestBody`,
//! `packages/client-runtime/src/pullRequestMarkdown.ts`), and the media a host serves only with a
//! credential (`sourceControlClients.mediaUrl`, GitHub's `gitHubMediaUrl`).

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Media {
    Video,
    Image,
}

/// `PullRequestBodySegment`. Offsets are bytes into the body that was split.
#[derive(Clone, Debug, PartialEq)]
pub enum Segment {
    /// `source_offset` is where `text` starts in the body, so a task ticked in it writes back.
    Markdown {
        text: String,
        source_offset: usize,
    },
    Attachment {
        url: String,
        media: Media,
    },
    Table {
        rows: Vec<Vec<Cell>>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Cell {
    pub header: bool,
    pub segments: Vec<Segment>,
}

/// How far a `<video>` tag may reach for its closing tag. Real embeds are one to three lines,
/// and the bound keeps an unclosed tag from making each one rescan the rest of the body.
const VIDEO_TAG_MAX_LINES: usize = 8;
/// Bounded like [`VIDEO_TAG_MAX_LINES`]; room for a before/after table with several rows.
const TABLE_MAX_LINES: usize = 64;
const IMAGE_EXTENSIONS: [&str; 9] = ["png", "jpg", "jpeg", "gif", "webp", "avif", "heic", "heif", "svg"];
const VIDEO_EXTENSIONS: [&str; 5] = ["mp4", "webm", "mov", "m4v", "ogv"];
const STRUCTURE_TAGS: [&str; 7] = ["table", "thead", "tbody", "tfoot", "tr", "td", "th"];

pub fn split(body: &str) -> Vec<Segment> {
    split_at(body, 0)
}

/// `\w` in a JavaScript pattern.
fn is_word(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// A fence line, up to three spaces and then three or more backticks or tildes: its marker run
/// and its info string, less the `\r` of a CRLF body, which is how GitHub stores one written in
/// its web editor.
fn fence(line: &str) -> Option<(&str, &str)> {
    let rest = line.trim_start();
    if line[..line.len() - rest.len()].chars().count() > 3 {
        return None;
    }
    let marker = rest.chars().next().filter(|c| matches!(c, '`' | '~'))?;
    let run = rest.len() - rest.trim_start_matches(marker).len();
    if run < 3 {
        return None;
    }
    let (fence, info) = rest.split_at(run);
    let info = info.strip_suffix('\r').unwrap_or(info);
    (!info.contains(['\r', '\u{2028}', '\u{2029}'])).then_some((fence, info))
}

/// `^(?: {4}| {0,3}\t)`: a tab after up to three spaces still reaches the fourth column.
fn indented_code(line: &str) -> bool {
    let spaces = line.len() - line.trim_start_matches(' ').len();
    spaces >= 4 || line.as_bytes().get(spaces) == Some(&b'\t')
}

fn is_web_url(url: &str) -> bool {
    url::Url::parse(url).is_ok_and(|url| matches!(url.scheme(), "http" | "https"))
}

fn strip_web_scheme(text: &str) -> Option<&str> {
    text.strip_prefix("https://").or_else(|| text.strip_prefix("http://"))
}

/// `^<?(https?:\/\/\S+?)>?$`.
fn bare_url(line: &str) -> Option<&str> {
    let text = line.strip_prefix('<').unwrap_or(line);
    let after = strip_web_scheme(text)?;
    if after.is_empty() || after.contains(char::is_whitespace) {
        return None;
    }
    let scheme = text.len() - after.len();
    Some(match after.strip_suffix('>') {
        Some(rest) if !rest.is_empty() => &text[..scheme + rest.len()],
        _ => text,
    })
}

/// `^\[[^\]]*\]\((https?:\/\/[^\s)]+)\)$`.
fn markdown_link_url(line: &str) -> Option<&str> {
    let rest = line.strip_prefix('[')?;
    let rest = rest[rest.find(']')? + 1..].strip_prefix('(')?.strip_suffix(')')?;
    let after = strip_web_scheme(rest)?;
    (!after.is_empty() && !after.contains(|c: char| c.is_whitespace() || c == ')')).then_some(rest)
}

/// `\.(?:ext|...)(?:$|[?#])`, case-insensitively, anywhere in `url`.
fn has_extension(url: &str, extensions: &[&str]) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.match_indices('.').any(|(at, _)| {
        let rest = &lower[at + 1..];
        extensions.iter().any(|extension| {
            rest.strip_prefix(extension).is_some_and(|after| after.is_empty() || after.starts_with(['?', '#']))
        })
    })
}

fn attachment_from_line(line: &str) -> Option<(String, Media)> {
    let line = line.trim();
    let url = bare_url(line).or_else(|| markdown_link_url(line))?;
    if !is_web_url(url) {
        return None;
    }
    if has_extension(url, &VIDEO_EXTENSIONS) || host_media(url).is_some_and(|media| media.video) {
        return Some((url.to_owned(), Media::Video));
    }
    has_extension(url, &IMAGE_EXTENSIONS).then(|| (url.to_owned(), Media::Image))
}

/// `^\s*<name\b`, case-insensitively.
fn opens_tag(line: &str, name: &str) -> bool {
    let Some(rest) = line.trim_start().strip_prefix('<') else { return false };
    rest.get(..name.len()).is_some_and(|head| head.eq_ignore_ascii_case(name))
        && !rest.as_bytes().get(name.len()).copied().is_some_and(is_word)
}

/// `<\/name>\s*$`, case-insensitively.
fn closes_tag(line: &str, name: &str) -> bool {
    let line = line.trim_end();
    line.len()
        .checked_sub(name.len() + 3)
        .and_then(|at| line.get(at..))
        .and_then(|tail| tail.strip_prefix("</")?.strip_suffix('>'))
        .is_some_and(|tail| tail.eq_ignore_ascii_case(name))
}

/// How many `<table\b` and `<\/table\s*>` a line holds.
fn table_tag_counts(line: &str) -> (usize, usize) {
    let lower = line.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let opens =
        lower.match_indices("<table").filter(|(at, _)| !bytes.get(at + 6).copied().is_some_and(is_word)).count();
    let closes = lower.match_indices("</table").filter(|(at, _)| lower[at + 7..].trim_start().starts_with('>')).count();
    (opens, closes)
}

/// `<(?:video|source)\b[^>]*\bsrc\s*=\s*["']([^"']+)["']`, case-insensitively: the first tag with
/// one. `[^>]*` is greedy, so the tag's last `src` with a quoted value wins.
fn video_tag_source(html: &str) -> Option<&str> {
    let lower = html.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    lower.match_indices('<').find_map(|(at, _)| {
        let name_end = ["video", "source"]
            .iter()
            .find_map(|name| lower[at + 1..].starts_with(name).then(|| at + 1 + name.len()))?;
        if bytes.get(name_end).copied().is_some_and(is_word) {
            return None;
        }
        let tag_end = lower[name_end..].find('>').map_or(lower.len(), |i| name_end + i);
        lower[name_end..tag_end]
            .rmatch_indices("src")
            .map(|(i, _)| name_end + i)
            .filter(|&i| !is_word(bytes[i - 1]))
            .find_map(|i| quoted_value(&html[i + 3..]))
    })
}

/// `\s*=\s*["']([^"']+)["']` at the start of `text`.
fn quoted_value(text: &str) -> Option<&str> {
    let rest = text.trim_start().strip_prefix('=')?.trim_start().strip_prefix(['"', '\''])?;
    let end = rest.find(['"', '\''])?;
    (end > 0).then(|| &rest[..end])
}

/// One `<(\/?)(table|thead|tbody|tfoot|tr|td|th)\b((?:[^<>"']|"[^"]*"|'[^']*')*)>`.
struct Tag<'a> {
    start: usize,
    end: usize,
    closing: bool,
    name: &'static str,
    attributes: &'a str,
}

/// The structure tag at `at`, a `<`. Quoted attribute values may hold `>`.
fn structure_tag_at(html: &str, at: usize) -> Option<Tag<'_>> {
    let bytes = html.as_bytes();
    let mut i = at + 1;
    let closing = bytes.get(i) == Some(&b'/');
    i += usize::from(closing);
    let name_start = i;
    while bytes.get(i).copied().is_some_and(is_word) {
        i += 1;
    }
    let name = STRUCTURE_TAGS.iter().find(|tag| tag.eq_ignore_ascii_case(&html[name_start..i])).copied()?;
    let attributes_start = i;
    loop {
        match *bytes.get(i)? {
            b'>' => break,
            b'<' => return None,
            quote @ (b'"' | b'\'') => i += html[i + 1..].find(char::from(quote))? + 2,
            _ => i += 1,
        }
    }
    Some(Tag { start: at, end: i + 1, closing, name, attributes: &html[attributes_start..i] })
}

/// Every structure tag in `html`, in order and without overlap.
fn structure_tags(html: &str) -> Vec<Tag<'_>> {
    let mut tags = Vec::new();
    let mut from = 0;
    while let Some(at) = html[from..].find('<').map(|i| from + i) {
        match structure_tag_at(html, at) {
            Some(tag) => {
                from = tag.end;
                tags.push(tag);
            }
            None => from = at + 1,
        }
    }
    tags
}

/// `\b(?:col|row)span\b`: rows are laid out independently, so a spanning cell would shift the
/// cells after it.
fn spans(attributes: &str) -> bool {
    let lower = attributes.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    ["colspan", "rowspan"].iter().any(|word| {
        lower.match_indices(word).any(|(at, _)| {
            (at == 0 || !is_word(bytes[at - 1])) && !bytes.get(at + word.len()).copied().is_some_and(is_word)
        })
    })
}

/// Cells of a table made only of rows and cells, or `None` when anything else is in it: a
/// caption, text between cells, a nested table. Cell contents are split on their own, so tables
/// never nest.
fn table_rows(html: &str, html_offset: usize) -> Option<Vec<Vec<Cell>>> {
    let mut rows = Vec::new();
    let mut row: Option<Vec<Cell>> = None;
    let mut cell: Option<(&str, usize)> = None;
    let (mut opened, mut closed) = (false, false);
    let mut cursor = 0;
    for tag in structure_tags(html) {
        if let Some((name, content_start)) = cell {
            if !tag.closing || tag.name != name {
                return None;
            }
            let content = &html[content_start..tag.start];
            // Indented HTML would render as a code block once the cell is markdown on its own.
            if content.split('\n').any(|line| indented_code(line) && !line.trim().is_empty()) {
                return None;
            }
            let segments = split_at(content, html_offset + content_start);
            row.as_mut()?.push(Cell { header: name == "th", segments });
            cell = None;
        } else {
            if !html[cursor..tag.start].trim().is_empty() {
                return None;
            }
            let table_open = opened && !closed;
            match tag.name {
                "table" => {
                    let misplaced = if tag.closing { !table_open || row.is_some() } else { opened };
                    if misplaced {
                        return None;
                    }
                    opened = true;
                    closed = tag.closing;
                }
                "tr" => {
                    if !table_open || tag.closing == row.is_none() {
                        return None;
                    }
                    if tag.closing {
                        rows.push(row.take()?);
                    } else {
                        row = Some(Vec::new());
                    }
                }
                "td" | "th" => {
                    if tag.closing || row.is_none() || spans(tag.attributes) {
                        return None;
                    }
                    cell = Some((tag.name, tag.end));
                }
                _ if !table_open || row.is_some() => return None,
                _ => {}
            }
        }
        cursor = tag.end;
    }
    if !closed || !html[cursor..].trim().is_empty() {
        return None;
    }
    rows.retain(|cells| !cells.is_empty());
    Some(rows)
}

/// The table opening at line `start`, through its matching close. The line returned is the last
/// one looked at even when the table is not usable, so the caller never re-reads an inner table.
fn media_table_at(lines: &[&str], start: usize, start_offset: usize) -> (usize, Option<Vec<Vec<Cell>>>) {
    let last_candidate = (start + TABLE_MAX_LINES).min(lines.len()) - 1;
    let mut depth = 0_isize;
    let mut end = start;
    loop {
        let (opens, closes) = table_tag_counts(lines[end]);
        depth += opens.cast_signed() - closes.cast_signed();
        if depth <= 0 || end == last_candidate {
            break;
        }
        end += 1;
    }
    if depth != 0 || !closes_tag(lines[end], "table") {
        return (end, None);
    }
    // Lifting attachment links out of a table's cells would tear the table apart.
    let rows = table_rows(&lines[start..=end].join("\n"), start_offset);
    let has_media = rows.as_ref().is_some_and(|rows| {
        rows.iter()
            .flatten()
            .any(|cell| cell.segments.iter().any(|segment| matches!(segment, Segment::Attachment { .. })))
    });
    (end, rows.filter(|_| has_media))
}

/// Blank lines around a run are dropped, but never leading spaces: four of them open an indented
/// code block, so trimming a run would silently turn code into prose.
fn flush(segments: &mut Vec<Segment>, markdown: &mut Vec<&str>, start: usize) {
    let joined = markdown.join("\n");
    markdown.clear();
    let mut text = joined.as_str();
    while let Some(rest) = text.strip_prefix('\n').or_else(|| text.strip_prefix("\r\n")) {
        text = rest;
    }
    if !text.trim().is_empty() {
        let source_offset = start + joined.len() - text.len();
        segments.push(Segment::Markdown { text: text.trim_end().to_owned(), source_offset });
    }
}

/// `base_offset` is where `body` starts in the whole body, so a table cell's tasks write back.
fn split_at(body: &str, base_offset: usize) -> Vec<Segment> {
    let mut segments = Vec::new();
    let mut markdown: Vec<&str> = Vec::new();
    let mut markdown_start = base_offset;
    let mut open_fence: Option<&str> = None;
    let lines: Vec<&str> = body.split('\n').collect();
    let mut table_scanned_through: Option<usize> = None;
    let mut line_start = base_offset;
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        if markdown.is_empty() {
            markdown_start = line_start;
        }
        let last = 'line: {
            if let Some((run, info)) = fence(line) {
                // A fence closes only on the same marker, at least as long as the one that opened
                // it, and with nothing after it: `~~~` cannot end a ``` block, and ```` ```ts ````
                // is an info string opening a nested fence, not a close.
                match open_fence {
                    None => open_fence = Some(run),
                    Some(open) if run.as_bytes()[0] == open.as_bytes()[0] && run.len() >= open.len() => {
                        if info.trim().is_empty() {
                            open_fence = None;
                        }
                    }
                    Some(_) => {}
                }
                markdown.push(line);
                break 'line index;
            }
            if open_fence.is_some() || indented_code(line) {
                markdown.push(line);
                break 'line index;
            }
            if table_scanned_through.is_none_or(|through| index > through) && opens_tag(line, "table") {
                let (end, rows) = media_table_at(&lines, index, line_start);
                table_scanned_through = Some(end);
                if let Some(rows) = rows {
                    flush(&mut segments, &mut markdown, markdown_start);
                    segments.push(Segment::Table { rows });
                    break 'line end;
                }
            }
            if let Some((url, media)) = attachment_from_line(line) {
                flush(&mut segments, &mut markdown, markdown_start);
                segments.push(Segment::Attachment { url, media });
                break 'line index;
            }
            if !opens_tag(line, "video") {
                markdown.push(line);
                break 'line index;
            }
            let last_candidate = (index + VIDEO_TAG_MAX_LINES).min(lines.len()) - 1;
            let mut cursor = index;
            while cursor < last_candidate && !closes_tag(lines[cursor], "video") {
                cursor += 1;
            }
            let source = closes_tag(lines[cursor], "video")
                .then(|| lines[index..=cursor].join("\n"))
                .and_then(|html| video_tag_source(&html).filter(|source| is_web_url(source)).map(str::to_owned));
            match source {
                Some(url) => {
                    flush(&mut segments, &mut markdown, markdown_start);
                    segments.push(Segment::Attachment { url, media: Media::Video });
                    cursor
                }
                // Unclosed, or nothing linkable in it: prose. Only this line is consumed, so the
                // lines that were looked at are still parsed on their own terms.
                None => {
                    markdown.push(line);
                    index
                }
            }
        };
        for consumed in &lines[index..=last] {
            line_start += consumed.len() + 1;
        }
        index = last + 1;
    }
    flush(&mut segments, &mut markdown, markdown_start);
    segments
}

/// Media a host serves only to a request carrying a credential (`SourceControlMediaUrl`).
#[derive(Debug, PartialEq, Eq)]
pub struct HostMedia {
    /// The canonical URL to fetch, the same for every spelling of one file.
    pub fetch_url: String,
    /// The URL alone says it is a video, as GitHub's extensionless upload links do.
    pub video: bool,
}

const GITHUB_RAW_HOST: &str = "raw.githubusercontent.com";
const GITHUB_LFS_HOST: &str = "media.githubusercontent.com";

/// `sourceControlClients.mediaUrl`; GitHub is the only host that has media of this kind. Hosts
/// that sign their own links (`objects.githubusercontent.com`) load on their own and are not here.
pub fn host_media(url: &str) -> Option<HostMedia> {
    let parsed = url::Url::parse(url).ok()?;
    if parsed.scheme() != "https" {
        return None;
    }
    let host = parsed.host_str()?.to_ascii_lowercase();
    let path = parsed.path();
    let search = parsed.query().filter(|query| !query.is_empty()).map(|query| format!("?{query}")).unwrap_or_default();
    let media = |fetch_url: String, video: bool| Some(HostMedia { fetch_url, video });
    match host.as_str() {
        GITHUB_RAW_HOST | GITHUB_LFS_HOST => media(format!("https://{host}{path}{search}"), false),
        "github.com" | "www.github.com" => {
            let parts: Vec<&str> = path.strip_prefix('/')?.split('/').collect();
            let id = |part: &str| !part.is_empty() && part.bytes().all(|b| is_word(b) || b == b'-');
            match parts.as_slice() {
                // GitHub links an uploaded video bare, with no extension, and an image as markdown.
                ["user-attachments", "assets", asset] if id(asset) => media(format!("https://github.com{path}"), true),
                [owner, repository, "assets", number, asset]
                    if !owner.is_empty()
                        && !repository.is_empty()
                        && !number.is_empty()
                        && number.bytes().all(|b| b.is_ascii_digit())
                        && id(asset) =>
                {
                    media(format!("https://github.com{path}"), false)
                }
                // `blob` and `raw` both address file bytes; `raw` is the one the token is honoured on.
                [owner, repository, "raw" | "blob", file @ ..]
                    if !owner.is_empty() && !repository.is_empty() && !file.is_empty() && !path.ends_with('/') =>
                {
                    media(format!("https://{GITHUB_RAW_HOST}/{owner}/{repository}/{}", file.join("/")), false)
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// `markdownImageSourceFragment`: the `#...` a media link carries, such as a start time.
pub fn source_fragment(url: &str) -> &str {
    url.find('#').map_or("", |at| &url[at..])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn markdown(text: &str, source_offset: usize) -> Segment {
        Segment::Markdown { text: text.to_owned(), source_offset }
    }

    fn video(url: &str) -> Segment {
        Segment::Attachment { url: url.to_owned(), media: Media::Video }
    }

    /// The body as one prose run: nothing in it lifts out.
    fn stays_prose(body: &str) {
        assert_eq!(split(body), vec![markdown(body, 0)], "{body:?}");
    }

    // GitHub writes a dropped image into the body as an `<img>` tag, so a bare attachment link on
    // its own line is the shape it uses for a video.
    #[test]
    fn a_dropped_video_lifts_out_and_keeps_the_prose_around_it() {
        let asset = "https://github.com/user-attachments/assets/2f8c1a90-1b2c-4d5e-8f90-abcdef123456";
        assert_eq!(
            split(&format!("Before\n\n{asset}\n\nAfter")),
            vec![markdown("Before", 0), video(asset), markdown("After", 89)]
        );
    }

    #[test]
    fn each_prose_run_sits_at_its_offset_in_the_body_including_inside_a_media_table() {
        fn prose(segments: &[Segment], out: &mut Vec<(String, usize)>) {
            for segment in segments {
                match segment {
                    Segment::Markdown { text, source_offset } => out.push((text.clone(), *source_offset)),
                    Segment::Table { rows } => rows.iter().flatten().for_each(|cell| prose(&cell.segments, out)),
                    Segment::Attachment { .. } => {}
                }
            }
        }
        let body = "\n\n- [ ] Before\r\n\nhttps://example.com/shot.png\n\n\n<video controls>\n  <source src=\"https://example.com/a.mp4\">\n</video>\n    - [ ] code\n- [x] After\n\r\n<table>\r\n<tr><td>\r\n\r\n- [ ] In a cell\r\n\r\n</td><td>\r\n\r\nhttps://example.com/b.mp4\r\n\r\n</td></tr>\r\n</table>\r\n- [ ] Last\n";
        let mut runs = Vec::new();
        prose(&split(body), &mut runs);
        let texts: Vec<&str> = runs.iter().map(|(text, _)| text.as_str()).collect();
        assert_eq!(texts, ["- [ ] Before", "    - [ ] code\n- [x] After", "- [ ] In a cell", "- [ ] Last"]);
        for (text, offset) in &runs {
            assert_eq!(&body[*offset..offset + text.len()], text);
        }
    }

    #[test]
    fn a_video_tag_gives_its_source_even_across_lines() {
        assert_eq!(
            split("<video controls>\n  <source src=\"https://example.com/a.webm\">\n</video>"),
            vec![video("https://example.com/a.webm")]
        );
        assert_eq!(
            split("<VIDEO data-src=\"x\" SRC='https://example.com/b.mp4'></video>"),
            vec![video("https://example.com/b.mp4")]
        );
    }

    #[test]
    fn what_is_not_a_standalone_attachment_stays_prose() {
        for body in [
            // The `<img>` GitHub writes for a dropped image.
            "<img width=\"1414\" alt=\"image\" src=\"https://github.com/user-attachments/assets/7195f963-51a9-4331-be74-3e06be760422\" />",
            "https://example.com/page\n\n[the docs](https://example.com/docs)",
            "```\nhttps://example.com/demo.mp4\n```",
            "```\r\nhttps://example.com/demo.mp4\r\n```",
            "```\n~~~\nhttps://example.com/demo.mp4\n```",
            "Before <video src=\"https://example.com/a.mp4\"></video> after",
            "```\n```javascript\nhttps://example.com/demo.mp4\n```",
            "    https://example.com/demo.mp4",
            "<video src=\"https://example.com/a.mp4\">\nstill prose\nmore prose",
            "<video src=\"javascript:alert(1)\"></video>",
            "See [recording](https://drop.example.com/a.mp4) for context.\n\n```md\nhttps://drop.example.com/a.png\n```",
        ] {
            stays_prose(body);
        }
    }

    #[test]
    fn a_closed_crlf_fence_lets_a_video_after_it_lift_out() {
        let code = "```html\r\n<video src=\"https://example.com/example.mp4\"></video>\r\n```";
        assert_eq!(
            split(&format!("{code}\r\n\r\nhttps://example.com/demo.mp4\r\n")),
            vec![markdown(code, 0), video("https://example.com/demo.mp4")]
        );
    }

    #[test]
    fn media_on_custom_hosts_is_known_by_its_extension() {
        for (body, media) in [
            ("https://drop.example.com/capture.PNG?token=abc#preview", Media::Image),
            ("[Recording](https://drop.example.com/recording.mov)", Media::Video),
            ("<https://drop.example.com/shot.webp>", Media::Image),
        ] {
            assert!(matches!(split(body).as_slice(), [Segment::Attachment { media: m, .. }] if *m == media), "{body}");
        }
        assert_eq!(
            split("<https://drop.example.com/shot.webp>"),
            vec![Segment::Attachment { url: "https://drop.example.com/shot.webp".into(), media: Media::Image }]
        );
    }

    const BEFORE: &str = "https://github.com/user-attachments/assets/c69c7c1c-ef2f-47dd-97e8-fe42e40712eb";
    const AFTER: &str = "https://github.com/user-attachments/assets/1867b6ec-4f88-4248-b2df-6b0f9808105b";
    const VIDEO: &str = "https://example.com/a.mp4";

    fn cell(header: bool, segments: Vec<Segment>) -> Cell {
        Cell { header, segments }
    }

    #[test]
    fn a_before_after_video_table_stays_together_with_each_cell_split_on_its_own() {
        let body = format!(
            "Intro\n\n<table>\n<tr><th>Before</th><th>After</th></tr>\n<tr>\n<td>\n\n{BEFORE}\n\n</td>\n<td>\n\n{AFTER}\n\n</td>\n</tr>\n</table>\n\nOutro"
        );
        let segments = split(&body);
        assert_eq!(segments.len(), 3);
        assert_eq!(segments[0], markdown("Intro", 0));
        assert!(matches!(&segments[2], Segment::Markdown { text, .. } if text == "Outro"));
        assert_eq!(
            segments[1],
            Segment::Table {
                rows: vec![
                    vec![cell(true, vec![markdown("Before", 23)]), cell(true, vec![markdown("After", 38)])],
                    vec![cell(false, vec![video(BEFORE)]), cell(false, vec![video(AFTER)])],
                ]
            }
        );
    }

    #[test]
    fn a_quoted_angle_bracket_in_a_cells_attributes_stays_out_of_its_text() {
        let body =
            format!("<table>\n<tr><td title=\"Before > after\">Caption</td><td>\n\n{VIDEO}\n\n</td></tr>\n</table>");
        let segments = split(&body);
        let [Segment::Table { rows }] = segments.as_slice() else { panic!("expected a table") };
        assert!(matches!(rows[0][0].segments.as_slice(), [Segment::Markdown { text, .. }] if text == "Caption"));
    }

    #[test]
    fn a_table_without_media_stays_markdown() {
        stays_prose("<table>\n<tr><td>a</td><td>b</td></tr>\n</table>");
    }

    #[test]
    fn a_media_table_the_rows_cannot_hold_splits_line_by_line() {
        for (name, open, close) in [
            ("a caption", "<table>\n<caption>Before upgrading</caption>\n<tr><td>", "</td></tr>\n</table>"),
            ("text outside cells", "<table>\nBefore upgrading\n<tr><td>", "</td></tr>\n</table>"),
            ("text in a row outside cells", "<table>\n<tr>Before upgrading<td>", "</td></tr>\n</table>"),
            ("a nested table", "<table>\n<tr><td>\n<table>\n<tr><td>", "</td></tr>\n</table>\n</td></tr>\n</table>"),
            ("an unclosed table", "<table>\n<tr><td>", ""),
            ("a spanning cell", "<table>\n<tr><td colspan=\"2\">", "</td></tr>\n</table>"),
            (
                "an indented cell",
                "<table>\n  <tr>\n    <th>\n      Before\n    </th>\n  </tr>\n  <tr><td>",
                "</td></tr>\n</table>",
            ),
            ("a cell indented by a space and a tab", "<table>\n<tr><td>\n \tBefore\n</td><td>", "</td></tr>\n</table>"),
        ] {
            let body = format!("{open}\n\n{VIDEO}\n\n{close}");
            let mut expected = vec![markdown(open, 0), video(VIDEO)];
            if !close.is_empty() {
                expected.push(markdown(close, body.len() - close.len()));
            }
            assert_eq!(split(&body), expected, "{name}");
        }
    }

    #[test]
    fn host_media_is_read_like_github_reads_its_links() {
        let read = |url: &str| host_media(url).map(|media| (media.fetch_url, media.video));
        assert_eq!(
            read("https://github.com/user-attachments/assets/2f8c1a90-1b2c"),
            Some(("https://github.com/user-attachments/assets/2f8c1a90-1b2c".into(), true))
        );
        assert_eq!(
            read("https://www.github.com/team/repo/assets/123/abc-def"),
            Some(("https://github.com/team/repo/assets/123/abc-def".into(), false))
        );
        assert_eq!(
            read("https://github.com/team/repo/blob/main/docs/a.png?raw=1"),
            Some(("https://raw.githubusercontent.com/team/repo/main/docs/a.png".into(), false))
        );
        assert_eq!(
            read("https://raw.githubusercontent.com/team/repo/main/a.png?token=x"),
            Some(("https://raw.githubusercontent.com/team/repo/main/a.png?token=x".into(), false))
        );
        assert_eq!(read("http://github.com/user-attachments/assets/abc"), None, "only https carries the credential");
        assert_eq!(read("https://objects.githubusercontent.com/a.png"), None, "signed links load on their own");
        assert_eq!(read("https://github.com/team/repo/pull/3"), None);
        assert_eq!(read("https://github.com/team/repo/blob/main/"), None);
        assert_eq!(source_fragment("https://x.dev/a.mp4#t=5"), "#t=5");
        assert_eq!(source_fragment("https://x.dev/a.mp4"), "");
    }
}
