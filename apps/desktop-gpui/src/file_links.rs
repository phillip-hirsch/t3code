//! Which inline code spans and markdown links name files, and how their chips read
//! (`packages/shared/src/markdownLinks.ts` and `fileLinks.ts`).

use std::collections::{HashMap, HashSet};

/// A path with the `:line:column` (or `#L12C3`) it pointed at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilePosition {
    pub path: String,
    pub line: Option<u32>,
    pub column: Option<u32>,
}

const RELATIVE_PREFIXES: [&str; 3] = ["~/", "./", "../"];

const POSIX_ROOTS: [&str; 24] = [
    "/Users/",
    "/home/",
    "/tmp/",
    "/var/",
    "/etc/",
    "/opt/",
    "/mnt/",
    "/Volumes/",
    "/private/",
    "/root/",
    "/usr/",
    "/bin/",
    "/sbin/",
    "/lib/",
    "/lib64/",
    "/srv/",
    "/dev/",
    "/proc/",
    "/sys/",
    "/run/",
    "/boot/",
    "/media/",
    "/workspace/",
    "/workspaces/",
];

const EXTENSIONLESS: [&str; 27] = [
    "Makefile",
    "makefile",
    "GNUmakefile",
    "Dockerfile",
    "Containerfile",
    "Justfile",
    "justfile",
    "Rakefile",
    "Gemfile",
    "Procfile",
    "Brewfile",
    "Caddyfile",
    "Vagrantfile",
    "Jenkinsfile",
    "Podfile",
    "Fastfile",
    "BUILD",
    "WORKSPACE",
    "LICENSE",
    "LICENCE",
    "COPYING",
    "NOTICE",
    "AUTHORS",
    "CONTRIBUTORS",
    "CHANGELOG",
    "README",
    "CODEOWNERS",
];

const GENERIC_TLDS: [&str; 24] = [
    "com", "net", "org", "io", "dev", "app", "ai", "co", "edu", "gov", "mil", "info", "biz", "xyz", "me", "tv", "gg",
    "chat", "cloud", "site", "online", "tech", "store", "link",
];

// C++ sources end in `.cc`, so `allocator.cc:217` names a file, not a host and port.
const COUNTRY_TLDS: [&str; 32] = [
    "uk", "de", "fr", "nl", "se", "no", "fi", "dk", "pl", "ch", "at", "be", "es", "it", "pt", "eu", "us", "ca", "au",
    "nz", "jp", "kr", "cn", "br", "ru", "mx", "ie", "cz", "tr", "sg", "hk", "cc",
];

fn is_digits(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit())
}

/// `isWindowsAbsolutePath`: a drive path (`C:\`, `C:/`) or a UNC share.
fn is_windows_absolute(path: &str) -> bool {
    let b = path.as_bytes();
    path.starts_with("\\\\")
        || (b.len() > 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && matches!(b[2], b'\\' | b'/'))
}

/// `POSITION_SUFFIX_CAPTURE_PATTERN` (`:12` or `:12:3` at the end): the path before it, line, column.
fn split_suffix(path: &str) -> Option<(&str, u32, Option<u32>)> {
    let (head, last) = path.rsplit_once(':')?;
    if !is_digits(last) {
        return None;
    }
    let last_value = last.parse().ok()?;
    match head.rsplit_once(':') {
        Some((path, line)) if is_digits(line) => Some((path, line.parse().ok()?, Some(last_value))),
        _ => Some((head, last_value, None)),
    }
}

fn without_position(path: &str) -> &str {
    split_suffix(path).map_or(path, |(path, ..)| path)
}

/// `splitFilePathPosition`: a `:line:column` suffix, else a `#L12C3` hash. Zero positions drop.
pub fn split_position(path: &str, hash: &str) -> FilePosition {
    let positive = |n: u32| (n > 0).then_some(n);
    if let Some((head, line, column)) = split_suffix(path) {
        return FilePosition { path: head.to_owned(), line: positive(line), column: column.and_then(positive) };
    }
    let from_hash = hash.strip_prefix('#').and_then(|h| h.strip_prefix(['L', 'l'])).and_then(|rest| {
        let (line, column) = match rest.find(['C', 'c']) {
            Some(at) => (&rest[..at], Some(&rest[at + 1..])),
            None => (rest, None),
        };
        let column = match column {
            Some(column) if is_digits(column) => Some(column.parse().ok()?),
            Some(_) => return None,
            None => None,
        };
        is_digits(line).then(|| line.parse().ok()).flatten().map(|line: u32| (line, column))
    });
    match from_hash {
        Some((line, column)) => {
            FilePosition { path: path.to_owned(), line: positive(line), column: column.and_then(positive) }
        }
        None => FilePosition { path: path.to_owned(), line: None, column: None },
    }
}

/// `formatFilePathPosition`: the path with its `:line:column` back on.
pub fn with_position(file: &FilePosition) -> String {
    match (file.line, file.column) {
        (Some(line), Some(column)) => format!("{}:{line}:{column}", file.path),
        (Some(line), None) => format!("{}:{line}", file.path),
        _ => file.path.clone(),
    }
}

/// `ELIDED_PATH_SEGMENT_PATTERN`: a `...` or `…` segment that shortened the path.
fn is_elided(path: &str) -> bool {
    path.split(['/', '\\']).any(|segment| segment == "…" || (segment.len() >= 3 && segment.bytes().all(|b| b == b'.')))
}

/// `FILE_EXTENSION_PATTERN`.
fn has_extension(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(_, ext)| {
        !ext.is_empty() && ext.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
    })
}

/// `VERSION_SUFFIX_PATTERN` (`\d\.\d[^.]*$`): `v1.2` reads as a version, not a file.
fn has_version_suffix(name: &str) -> bool {
    let b = name.as_bytes();
    name.rfind('.')
        .is_some_and(|at| at > 0 && at + 1 < b.len() && b[at - 1].is_ascii_digit() && b[at + 1].is_ascii_digit())
}

/// `looksLikeHostname`.
fn looks_like_hostname(segment: &str, has_position: bool) -> bool {
    if segment.starts_with('.') {
        return false;
    }
    let lowered = segment.to_lowercase();
    if lowered == "localhost" {
        return true;
    }
    if segment.split('.').count() > 1 && segment.split('.').all(is_digits) {
        return true;
    }
    let labels: Vec<&str> = lowered.split('.').collect();
    let [_, .., last] = labels.as_slice() else { return false };
    GENERIC_TLDS.contains(last) || (!has_position && COUNTRY_TLDS.contains(last))
}

/// `inlineCodeFilePathCandidate`: the file path an inline code span names, if it reads as one.
pub fn inline_code_candidate(code: &str) -> Option<String> {
    let trimmed = code.trim();
    if trimmed.is_empty() || trimmed.contains(|c: char| c.is_whitespace() || c == '`') {
        return None;
    }
    let candidate = if is_windows_absolute(trimmed) { trimmed.to_owned() } else { trimmed.replace('\\', "/") };
    let has_position = split_suffix(&candidate).is_some();
    if (!has_position && !candidate.contains(['/', '\\'])) || is_elided(without_position(&candidate)) {
        return None;
    }
    let explicit = RELATIVE_PREFIXES.iter().any(|p| candidate.starts_with(p))
        || candidate.starts_with('/')
        || is_windows_absolute(&candidate);
    if !explicit {
        let bare = without_position(&candidate);
        if bare.ends_with('/') {
            return None;
        }
        let first = bare.split('/').next().unwrap_or(bare);
        if looks_like_hostname(first, has_position) {
            return None;
        }
        let basename = bare.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next().unwrap_or_default();
        if has_version_suffix(basename) || (!has_position && !has_extension(basename)) {
            return None;
        }
    }
    Some(candidate)
}

/// `safeDecodeURIComponent`: `%XX` escapes as UTF-8, or the text unchanged when they don't decode.
fn decode(text: &str) -> String {
    if !text.contains('%') {
        return text.to_owned();
    }
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = text.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok());
            let Some(byte) = hex else { return text.to_owned() };
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| text.to_owned())
}

/// `stripSlashPrefixedWindowsDrive`: `/C:/x` from a file URL is `C:/x`.
fn strip_slash_drive(path: &str) -> &str {
    match path.strip_prefix('/') {
        Some(rest) if is_windows_absolute(rest) => rest,
        _ => path,
    }
}

/// `hasExternalScheme`: `https:`, `mailto:`; not a drive or a bare `:12` position.
fn has_external_scheme(path: &str) -> bool {
    if is_windows_absolute(path) {
        return false;
    }
    let Some((scheme, rest)) = path.split_once(':') else { return false };
    let mut chars = scheme.chars();
    let valid = chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'));
    if !valid {
        return false;
    }
    if rest.starts_with("//") {
        return true;
    }
    let position_only = match rest.split_once(':') {
        Some((line, column)) => is_digits(line) && is_digits(column),
        None => is_digits(rest),
    };
    !position_only
}

/// A file or folder name in `RELATIVE_FILE_PATH_PATTERN`: words of name characters, spaces between.
fn is_name(segment: &str, allow_at: bool) -> bool {
    !segment.is_empty()
        && !segment.starts_with(' ')
        && !segment.ends_with(' ')
        && segment.chars().all(|c| {
            c.is_alphanumeric()
                || c == ' '
                || matches!(c, '.' | '_' | '$' | '+' | '(' | ')' | '[' | ']' | '-')
                || (allow_at && c == '@')
        })
}

/// `RELATIVE_FILE_PATH_PATTERN`: `dir/`, `dir/file.ext`, `dir/file:12`.
fn is_relative_file_path(path: &str) -> bool {
    let Some((dirs, last)) = path.rsplit_once('/') else { return false };
    dirs.split('/').all(|segment| is_name(segment, true)) && (last.is_empty() || is_name(without_position(last), true))
}

/// `RELATIVE_FILE_NAME_PATTERN`: `file.ext`, with no `@` so addresses stay text.
fn is_relative_file_name(path: &str) -> bool {
    let bare = without_position(path);
    bare.rsplit_once('.').is_some_and(|(stem, ext)| {
        is_name(stem, false) && !ext.is_empty() && ext.chars().all(|c| c.is_alphanumeric() || matches!(c, '_' | '-'))
    })
}

/// `looksLikeFilePath`.
fn looks_like_file_path(path: &str, authored: &str) -> bool {
    if is_windows_absolute(path) || RELATIVE_PREFIXES.iter().any(|p| path.starts_with(p)) {
        return true;
    }
    if path.starts_with('/') {
        let basename = authored.rsplit('/').next().unwrap_or_default();
        return POSIX_ROOTS.iter().any(|root| authored.starts_with(root))
            || split_suffix(authored).is_some()
            || EXTENSIONLESS.contains(&basename)
            || has_extension(basename);
    }
    EXTENSIONLESS.contains(&path) || is_relative_file_path(authored) || is_relative_file_name(path)
}

/// `parseMarkdownFileLink`: the file a markdown link destination names, if any.
pub fn parse_file_link(href: &str) -> Option<FilePosition> {
    let trimmed = href.trim();
    let normalized = trimmed.strip_prefix('<').and_then(|rest| rest.strip_suffix('>')).unwrap_or(trimmed);
    if normalized.is_empty() || normalized.starts_with('#') || normalized.starts_with("//") {
        return None;
    }
    let (raw_path, raw_hash) = match normalized.get(..5).filter(|s| s.eq_ignore_ascii_case("file:")) {
        Some(_) => file_url(normalized)?,
        None => {
            let (before_hash, hash) = normalized.split_at(normalized.find('#').unwrap_or(normalized.len()));
            (before_hash.split('?').next().unwrap_or_default().to_owned(), hash.to_owned())
        }
    };
    let decoded = decode(raw_path.trim());
    let path = strip_slash_drive(&decoded);
    let hash = decode(raw_hash.trim());
    if path.is_empty() || has_external_scheme(path) {
        return None;
    }
    let position = split_position(path, &hash);
    looks_like_file_path(&position.path, path).then_some(position)
}

/// `parseFileUrlHref`: a `file:` URL's path and hash; another host is a UNC share.
fn file_url(href: &str) -> Option<(String, String)> {
    let rest = href[5..].strip_prefix("//")?;
    let (rest, hash) = rest.split_at(rest.find('#').unwrap_or(rest.len()));
    let rest = rest.split('?').next().unwrap_or_default();
    let (host, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    if path.is_empty() {
        return None;
    }
    let path = match host.to_lowercase().as_str() {
        "" | "localhost" => path.to_owned(),
        host => format!("\\\\{host}{}", path.replace('/', "\\")),
    };
    Some((path, hash.to_owned()))
}

fn normalize_slashes(path: &str) -> String {
    path.replace('\\', "/")
}

/// `isMarkdownFileLinkLabel`: the link's text is its own path (or a tail of it), so the chip
/// stands alone instead of following the text.
pub fn is_file_link_label(label: &str, href: &str) -> bool {
    let Some(destination) = parse_file_link(href) else { return false };
    let normalize = |path: &str| {
        let path = normalize_slashes(path);
        path.strip_prefix("./").unwrap_or(&path).trim_end_matches('/').to_owned()
    };
    let label = split_position(label.trim(), "");
    if label.line.is_some_and(|line| Some(line) != destination.line)
        || label.column.is_some_and(|column| Some(column) != destination.column)
    {
        return false;
    }
    let (mut label_path, mut destination_path) = (normalize(&label.path), normalize(&destination.path));
    if label_path.is_empty() {
        return true;
    }
    if is_windows_absolute(&destination.path) {
        label_path = label_path.to_lowercase();
        destination_path = destination_path.to_lowercase();
    }
    destination_path == label_path || destination_path.ends_with(&format!("/{label_path}"))
}

fn basename(path: &str) -> &str {
    let trimmed = path.trim_end_matches(['/', '\\']);
    if trimmed.is_empty() {
        return path;
    }
    trimmed.rsplit(['/', '\\']).next().unwrap_or(trimmed)
}

fn parents(path: &str) -> Vec<&str> {
    let mut segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    segments.pop();
    segments
}

/// `buildFileLinkParentSuffixByPath`: for files that share a name, the shortest parent tail (at
/// least two folders) that tells them apart.
pub fn parent_suffixes<'a>(paths: impl IntoIterator<Item = &'a str>) -> HashMap<String, String> {
    let mut groups: HashMap<String, Vec<String>> = HashMap::new();
    for path in paths {
        let normalized = normalize_slashes(path);
        let Some(name) = normalized.split('/').rfind(|s| !s.is_empty()).map(str::to_owned) else { continue };
        let group = groups.entry(name).or_default();
        if !group.contains(&normalized) {
            group.push(normalized);
        }
    }
    let mut suffixes = HashMap::new();
    for group in groups.values().filter(|group| group.len() > 1) {
        for path in group {
            let segments = parents(path);
            if segments.is_empty() {
                continue;
            }
            let tail = |segments: &[&str], depth: usize| segments[segments.len().saturating_sub(depth)..].join("/");
            let unique = (1..=segments.len())
                .find(|&depth| {
                    let candidate = tail(&segments, depth);
                    !group.iter().any(|other| other != path && tail(&parents(other), depth) == candidate)
                })
                .unwrap_or(segments.len());
            suffixes.insert(path.clone(), tail(&segments, unique.max(2).min(segments.len())));
        }
    }
    suffixes
}

/// `fileLinkLabel`: `name · parent/tail · L12:C3`.
pub fn chip_label(file: &FilePosition, suffixes: &HashMap<String, String>) -> String {
    let mut parts = vec![basename(&file.path).to_owned()];
    parts.extend(suffixes.get(&normalize_slashes(&file.path)).cloned());
    if let Some(line) = file.line {
        parts.push(match file.column {
            Some(column) => format!("L{line}:C{column}"),
            None => format!("L{line}"),
        });
    }
    parts.join(" · ")
}

/// The paths every chip in `markdown` names (`extractInlineCodeSpans` and
/// `extractMarkdownLinkHrefs`), for [`parent_suffixes`].
pub fn document_paths(markdown: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut paths = Vec::new();
    let mut push = |path: String| {
        if seen.insert(path.clone()) {
            paths.push(path);
        }
    };
    for (index, segment) in markdown.split("```").enumerate() {
        if index % 2 == 1 {
            continue;
        }
        for line in segment.lines() {
            for (part_index, part) in line.split('`').enumerate() {
                if part_index % 2 == 1 && line.split('`').count() > part_index + 1 {
                    if let Some(candidate) = inline_code_candidate(part) {
                        push(normalize_slashes(&split_position(&candidate, "").path));
                    }
                } else {
                    for href in link_destinations(part) {
                        if let Some(file) = parse_file_link(href) {
                            push(normalize_slashes(&file.path));
                        }
                    }
                }
            }
        }
    }
    paths
}

/// `[label](destination)` destinations in `text`, without their titles.
fn link_destinations(text: &str) -> impl Iterator<Item = &str> {
    text.match_indices("](").filter_map(move |(at, _)| {
        let rest = text[at + 2..].trim_start();
        if let Some(inner) = rest.strip_prefix('<') {
            return inner.find('>').map(|end| &inner[..end]);
        }
        let mut depth = 0usize;
        let end = rest.find(|c: char| match c {
            '(' => {
                depth += 1;
                false
            }
            ')' if depth == 0 => true,
            ')' => {
                depth -= 1;
                false
            }
            c => c.is_whitespace(),
        })?;
        (end > 0).then(|| &rest[..end])
    })
}

/// `resolveMarkdownFileLinkTarget` for a chip's path: absolute paths stay; relative ones join
/// the workspace, and `~/` the home its path implies. A network share outside it is no link.
pub fn resolve_target(path: &str, cwd: Option<&str>) -> Option<String> {
    let relative =
        RELATIVE_PREFIXES.iter().any(|p| path.starts_with(p)) || (!path.starts_with('/') && !is_windows_absolute(path));
    if !relative {
        let network = path.starts_with("\\\\") || path.starts_with("//");
        let inside = cwd.is_some_and(|cwd| {
            let (cwd, path) = (normalize_slashes(cwd), normalize_slashes(path));
            path.to_lowercase().starts_with(&format!("{}/", cwd.trim_end_matches('/').to_lowercase()))
        });
        return (!network || inside).then(|| path.to_owned());
    }
    let cwd = cwd?;
    let windows = |p: &str| is_windows_absolute(p) || p.contains(":\\");
    let join = |base: &str, next: &str| {
        let base = base.trim_end_matches(['/', '\\']);
        if windows(base) {
            format!("{base}\\{}", next.replace('/', "\\"))
        } else {
            format!("{base}/{}", next.trim_start_matches('/'))
        }
    };
    if let Some(rest) = path.strip_prefix("~/") {
        return Some(match home_of(cwd) {
            Some(home) => join(&home, rest),
            None => path.to_owned(),
        });
    }
    Some(join(cwd, path))
}

/// `inferHomeFromCwd`.
fn home_of(cwd: &str) -> Option<String> {
    let user = |prefix: &str| {
        let rest = cwd.strip_prefix(prefix)?;
        let name = rest.split('/').next().filter(|n| !n.is_empty())?;
        Some(format!("{prefix}{name}"))
    };
    if let Some(home) = user("/Users/").or_else(|| user("/home/")) {
        return Some(home);
    }
    let b = cwd.as_bytes();
    let drive = b.len() > 9 && b[0].is_ascii_alphabetic() && b[1] == b':' && matches!(b[2], b'\\' | b'/');
    let users =
        cwd.get(3..8).is_some_and(|s| s.eq_ignore_ascii_case("users")) && matches!(b.get(8), Some(b'\\' | b'/'));
    if drive && users {
        let name = cwd[9..].split(['\\', '/']).next().filter(|n| !n.is_empty())?;
        return Some(cwd[..9 + name.len()].to_owned());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_code_reads_as_a_file_only_when_it_looks_like_one() {
        let yes = [
            "/Users/me/.t3/bin/t3",
            "src/app/mod.rs",
            "src/app/mod.rs:12",
            "./run.sh",
            "~/notes.md",
            "/Users/me/a.txt",
            "C:\\repo\\a.rs",
            "main.rs:4:2",
            "allocator.cc:217",
        ];
        for code in yes {
            assert!(inline_code_candidate(code).is_some(), "{code} is a file");
        }
        let no = [
            "a.rs",
            "npm run build",
            "github.com/org/repo",
            "src/",
            "packages/.../index.ts",
            "deps/foo-1.2",
            "localhost:3000",
            "",
            "src/app",
            ".t3/bin/t3",
        ];
        for code in no {
            assert_eq!(inline_code_candidate(code), None, "{code} is not a file");
        }
        assert_eq!(inline_code_candidate("src\\a.rs").as_deref(), Some("src/a.rs"));
    }

    #[test]
    fn markdown_destinations_name_files_but_not_web_links() {
        let file = |href: &str| parse_file_link(href).map(|p| (p.path, p.line, p.column));
        assert_eq!(file("src/a.rs:12:3"), Some(("src/a.rs".into(), Some(12), Some(3))));
        assert_eq!(file("src/a.rs#L7"), Some(("src/a.rs".into(), Some(7), None)));
        assert_eq!(file("file:///C:/repo/My%20File.md"), Some(("C:/repo/My File.md".into(), None, None)));
        assert_eq!(file("<docs/My Folder/>"), Some(("docs/My Folder/".into(), None, None)));
        assert_eq!(file("README.md"), Some(("README.md".into(), None, None)));
        assert_eq!(file("Makefile"), Some(("Makefile".into(), None, None)));
        for href in ["https://example.com/a.rs", "mailto:a@b.co", "#anchor", "me@example.com", "notes"] {
            assert_eq!(parse_file_link(href), None, "{href} is not a file");
        }
    }

    #[test]
    fn link_labels_that_repeat_the_path_drop_the_text() {
        assert!(is_file_link_label("a.rs", "src/a.rs"));
        assert!(is_file_link_label("src/a.rs:4", "./src/a.rs:4"));
        assert!(!is_file_link_label("a.rs:5", "src/a.rs:4"));
        assert!(!is_file_link_label("the entry point", "src/a.rs"));
        assert!(is_file_link_label("A.RS", "C:\\repo\\a.rs"));
    }

    #[test]
    fn shared_names_get_the_parent_tail_that_tells_them_apart() {
        let suffixes = parent_suffixes([".t3/bin/t3", "packages/t3", "README.md"]);
        assert_eq!(suffixes.get(".t3/bin/t3").map(String::as_str), Some(".t3/bin"));
        assert_eq!(suffixes.get("packages/t3").map(String::as_str), Some("packages"));
        assert!(!suffixes.contains_key("README.md"), "a unique name needs no suffix");
        let file = split_position(".t3/bin/t3:12:4", "");
        assert_eq!(chip_label(&file, &suffixes), "t3 · .t3/bin · L12:C4");
        let deep = parent_suffixes(["a/x/b/c/f.rs", "a/y/b/c/f.rs"]);
        assert_eq!(deep.get("a/x/b/c/f.rs").map(String::as_str), Some("x/b/c"));
    }

    #[test]
    fn documents_list_their_file_paths_outside_code_blocks() {
        let text = "See `/r/.t3/bin/t3` and [t3](packages/t3), not `ls -la`.\n```\n`src/a.rs`\n```\n";
        assert_eq!(document_paths(text), ["/r/.t3/bin/t3", "packages/t3"]);
    }

    #[test]
    fn targets_resolve_against_the_workspace() {
        assert_eq!(resolve_target("src/a.rs:3", Some("/repo")).as_deref(), Some("/repo/src/a.rs:3"));
        assert_eq!(resolve_target("src/a.rs", Some("C:\\repo\\")).as_deref(), Some("C:\\repo\\src\\a.rs"));
        assert_eq!(resolve_target("~/n.md", Some("/Users/me/repo")).as_deref(), Some("/Users/me/n.md"));
        assert_eq!(resolve_target("~/n.md", Some("C:\\Users\\me\\repo")).as_deref(), Some("C:\\Users\\me\\n.md"));
        assert_eq!(resolve_target("/etc/hosts", None).as_deref(), Some("/etc/hosts"));
        assert_eq!(resolve_target("src/a.rs", None), None);
        assert_eq!(resolve_target("\\\\host\\share\\a.rs", Some("C:\\repo")), None);
    }
}
