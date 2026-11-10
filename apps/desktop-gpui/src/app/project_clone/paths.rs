//! Where a clone lands and what the user typed means: `getCloneDirectoryName`,
//! `getCloneDirectoryPath`, `getCloneDestinationPath`, `getAddProjectInitialQuery`,
//! `normalizePastedCloneUrl` and friends (`client-runtime/src/operations/projects.ts`), the
//! browse path rules they lean on (`client-runtime/src/state/projects.ts`), and ghq's
//! `<host>/<owner>/<repo>` layout (`cloneUrlRepositoryPath`, `shared/src/cloneUrl.ts`).

use crate::project_groups::is_drive_path;

/// `getAbsolutePathKind`: a drive or UNC path reads as Windows, a leading `/` as Unix.
fn is_unix_absolute(value: &str) -> bool {
    !is_drive_path(value) && !value.starts_with("\\\\") && value.starts_with('/')
}

fn is_windows_absolute(value: &str) -> bool {
    is_drive_path(value) || value.starts_with("\\\\")
}

/// `preferredPathSeparator`.
fn preferred_separator(value: &str) -> char {
    if is_windows_absolute(value) {
        '\\'
    } else if is_unix_absolute(value) || !value.contains('\\') {
        '/'
    } else {
        '\\'
    }
}

/// `hasTrailingPathSeparator`: a Unix path ends only at `/`.
pub(crate) fn has_trailing_separator(value: &str) -> bool {
    value.ends_with('/') || (!is_unix_absolute(value) && value.ends_with('\\'))
}

/// `getBrowseDirectoryPath`: the path through its last separator.
fn browse_directory_path(value: &str) -> &str {
    if has_trailing_separator(value) {
        return value;
    }
    let last = if is_unix_absolute(value) { value.rfind('/') } else { value.rfind(['/', '\\']) };
    last.map_or(value, |at| &value[..=at])
}

/// `ensureBrowseDirectoryPath`: the trimmed path, ending at a separator.
pub(crate) fn ensure_browse_directory_path(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() || has_trailing_separator(trimmed) {
        return trimmed.to_owned();
    }
    format!("{trimmed}{}", preferred_separator(trimmed))
}

/// `appendBrowsePathSegment`: the folder `segment` inside the path's directory.
fn append_browse_segment(value: &str, segment: &str) -> String {
    format!("{}{segment}{}", browse_directory_path(value), preferred_separator(value))
}

/// `getAddProjectInitialQuery`: the base folder to browse from, else the home folder.
pub(crate) fn add_project_initial_query(base: Option<&str>) -> String {
    match base.map(str::trim).filter(|base| !base.is_empty()) {
        None => "~/".to_owned(),
        Some(base) => ensure_browse_directory_path(base),
    }
}

/// `getCloneDirectoryName`: the repository's own name, without `.git`; empty when a URL names
/// only a host (or a host and port).
pub(crate) fn clone_directory_name(input: &str) -> String {
    let without_query = input.split(['?', '#']).next().unwrap_or_default().trim();
    let scheme = without_query.find("://");
    let has_host = scheme.is_some() || is_scp_user_host(without_query);
    let path = scheme.map_or(without_query, |at| &without_query[at + 3..]);
    let segments: Vec<&str> = path.split(['/', '\\', ':']).filter(|s| !s.trim().is_empty()).collect();
    if has_host && segments.len() < 2 {
        return String::new();
    }
    let last = segments.last().map_or("", |s| s.trim());
    if has_host && segments.len() == 2 && last.bytes().all(|b| b.is_ascii_digit()) {
        return String::new();
    }
    last.strip_suffix(".git").unwrap_or(last).to_owned()
}

/// `/^[^/\\:]+@[^/\\:]+:/`: `user@host:` before any slash.
fn is_scp_user_host(value: &str) -> bool {
    let Some(end) = value.find(['/', '\\', ':']) else { return false };
    let prefix = &value[..end];
    value[end..].starts_with(':') && prefix.char_indices().any(|(at, c)| c == '@' && at > 0 && at + 1 < prefix.len())
}

/// `parseCloneUrl`'s host and path for an `ssh://`, `http(s)://` or scp-like URL.
fn parse_clone_url(url: &str) -> Option<(&str, &str)> {
    let trimmed = url.trim();
    for scheme in ["ssh://", "https://", "http://"] {
        if let Some(rest) = trimmed.strip_prefix(scheme) {
            return parse_authority_url(rest);
        }
    }
    parse_scp_url(trimmed)
}

/// `(?:[^/\s]*@)?(\[…\]|[^@:/\s]+)(:\d+)?/(.+)` after the scheme.
fn parse_authority_url(rest: &str) -> Option<(&str, &str)> {
    let slash = rest.find('/')?;
    let (authority, path) = (&rest[..slash], &rest[slash + 1..]);
    if path.is_empty() || authority.contains(char::is_whitespace) {
        return None;
    }
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
    let (host, port) = if host_port.starts_with('[') {
        let close = host_port.find(']')?;
        (&host_port[..=close], &host_port[close + 1..])
    } else {
        host_port.split_at(host_port.find(':').unwrap_or(host_port.len()))
    };
    let host_ok = if let Some(inner) = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        !inner.is_empty() && !inner.contains(['@', '/', ']'])
    } else {
        !host.is_empty() && !host.contains(['@', ':', '/'])
    };
    let port_ok = port.is_empty()
        || port.strip_prefix(':').is_some_and(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
    (host_ok && port_ok).then_some((host, path))
}

/// `^(?:[^@/\s]+@)?(\[…\]|[^:/\s[\]]+):(?!/)(.+)$`.
fn parse_scp_url(value: &str) -> Option<(&str, &str)> {
    let host_path = |rest: &str| -> Option<(usize, usize)> {
        let host_end = if rest.starts_with('[') {
            let close = rest.find(']')?;
            let inner = &rest[1..close];
            if inner.is_empty() || inner.contains(['@', '/']) || inner.contains(char::is_whitespace) {
                return None;
            }
            close + 1
        } else {
            let end = rest.find(':')?;
            let host = &rest[..end];
            if host.is_empty() || host.contains(['/', '[', ']']) || host.contains(char::is_whitespace) {
                return None;
            }
            end
        };
        let path = rest[host_end..].strip_prefix(':')?;
        (!path.is_empty() && !path.starts_with('/')).then_some((host_end, host_end + 1))
    };
    let user = value.find('@').filter(|&at| {
        let user = &value[..at];
        !user.is_empty() && !user.contains('/') && !user.contains(char::is_whitespace)
    });
    if let Some(at) = user {
        let rest = &value[at + 1..];
        if let Some((host_end, path_start)) = host_path(rest) {
            return Some((&rest[..host_end], &rest[path_start..]));
        }
    }
    let (host_end, path_start) = host_path(value)?;
    Some((&value[..host_end], &value[path_start..]))
}

/// `UNSAFE_SEGMENT` and `WINDOWS_DEVICE_NAME`: a folder name that could leave the clone root or
/// that Windows reserves.
fn unsafe_segment(segment: &str) -> bool {
    if matches!(segment, "." | "..")
        || segment.chars().any(|c| matches!(c, '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control())
        || segment.ends_with([' ', '.'])
    {
        return true;
    }
    let lower = segment.to_lowercase();
    let device_rest = |name: &str| lower.strip_prefix(name).map(|rest| rest.trim_start_matches(' '));
    let reserved = |rest: Option<&str>| rest.is_some_and(|rest| rest.is_empty() || rest.starts_with('.'));
    let numbered = ["com", "lpt"].iter().any(|name| {
        lower
            .strip_prefix(name)
            .and_then(|rest| {
                let mut chars = rest.chars();
                let digit = chars.next()?;
                (digit.is_ascii_digit() || matches!(digit, '¹' | '²' | '³'))
                    .then(|| chars.as_str().trim_start_matches(' '))
            })
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
    });
    numbered || ["con", "prn", "aux", "nul", "conin$", "conout$"].iter().any(|name| reserved(device_rest(name)))
}

/// `cloneUrlRepositoryPath`: the `<host>/<owner>/<repo>` folders ghq clones a URL into, or
/// `None` for local paths and anything with a folder that could leave the root.
pub(crate) fn clone_url_repository_path(url: &str) -> Option<String> {
    if is_drive_path(url.trim()) {
        return None;
    }
    let (host, path) = parse_clone_url(url)?;
    let path = path.split(['?', '#']).next().unwrap_or_default();
    let mut segments: Vec<&str> = std::iter::once(host).chain(path.split('/').filter(|s| !s.is_empty())).collect();
    if let Some(last) = segments.last_mut() {
        *last = last.strip_suffix(".git").unwrap_or(last);
    }
    if segments.len() < 2 || segments.iter().any(|s| s.is_empty() || unsafe_segment(s)) {
        return None;
    }
    Some(segments.join("/"))
}

/// `getCloneDirectoryPath`: the folder a clone gets below its base, `<repo>` or, with the ghq
/// layout, `<host>/<owner>/<repo>`.
pub(crate) fn clone_directory_path(remote_url: &str, ghq: bool, repository: &str) -> String {
    ghq.then(|| clone_url_repository_path(remote_url)).flatten().unwrap_or_else(|| clone_directory_name(repository))
}

/// `getCloneDirectoryLeafName`: the clone's own folder, which browsing keeps at the end.
pub(crate) fn clone_directory_leaf_name(remote_url: &str, ghq: bool, repository: &str) -> String {
    clone_directory_path(remote_url, ghq, repository).rsplit('/').next().unwrap_or_default().to_owned()
}

/// `getCloneDestinationPath`: `name` inside `directory`, in the directory's separators.
pub(crate) fn clone_destination_path(directory: &str, name: &str) -> String {
    let name = name.trim();
    if name.is_empty() {
        return directory.to_owned();
    }
    let base = ensure_browse_directory_path(directory);
    if base.ends_with('\\') { format!("{base}{}", name.replace('/', "\\")) } else { format!("{base}{name}") }
}

/// The letter an accented Latin letter decomposes to (NFKD minus its combining marks).
fn base_letter(c: char) -> Option<char> {
    const FOLDS: &[(&str, char)] = &[
        ("ÀÁÂÃÄÅàáâãäåĀāĂăĄą", 'a'),
        ("ÇçĆćĈĉĊċČč", 'c'),
        ("Ďď", 'd'),
        ("ÈÉÊËèéêëĒēĔĕĖėĘęĚě", 'e'),
        ("ĜĝĞğĠġĢģ", 'g'),
        ("Ĥĥ", 'h'),
        ("ÌÍÎÏìíîïĨĩĪīĬĭĮįİ", 'i'),
        ("Ĵĵ", 'j'),
        ("Ķķ", 'k'),
        ("ĹĺĻļĽľ", 'l'),
        ("ÑñŃńŅņŇň", 'n'),
        ("ÒÓÔÕÖòóôõöŌōŎŏŐő", 'o'),
        ("ŔŕŖŗŘř", 'r'),
        ("ŚśŜŝŞşŠš", 's'),
        ("ŢţŤť", 't'),
        ("ÙÚÛÜùúûüŨũŪūŬŭŮůŰűŲų", 'u'),
        ("Ŵŵ", 'w'),
        ("ÝýÿŶŷŸ", 'y'),
        ("ŹźŻżŽž", 'z'),
    ];
    FOLDS.iter().find(|(letters, _)| letters.contains(c)).map(|(_, base)| *base)
}

/// `newProjectFolderName` (`shared/src/path.ts`): the name as a lowercase slug of at most 64
/// characters, "project" when nothing is left, and never a name Windows reserves.
pub(crate) fn new_project_folder_name(name: &str) -> String {
    let mut slug = String::new();
    for c in name.chars().map(|c| base_letter(c).unwrap_or(c)).flat_map(char::to_lowercase) {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            slug.push(c);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    // Every character is ASCII, so any length is a char boundary.
    slug.truncate(64);
    let slug = slug.trim_end_matches('-');
    if slug.is_empty() {
        return "project".to_owned();
    }
    let numbered = |prefix: &str| slug.strip_prefix(prefix).is_some_and(|n| matches!(n.as_bytes(), [b'1'..=b'9']));
    let reserved = matches!(slug, "con" | "prn" | "aux" | "nul") || numbered("com") || numbered("lpt");
    if reserved { format!("{slug}-project") } else { slug.to_owned() }
}

/// `getNewProjectPathPreview`: where a new project named `name` goes below its root.
pub(crate) fn new_project_path_preview(root: &str, name: &str) -> String {
    clone_destination_path(root, &new_project_folder_name(name))
}

/// `getCloneDestinationBrowsePath`: entering a folder keeps the clone's own folder at the end,
/// unless the folder entered is it.
pub(crate) fn clone_destination_browse_path(
    browse_directory: &str,
    selected: &str,
    clone_name: &str,
    case_sensitive: bool,
) -> String {
    let selected_path = append_browse_segment(browse_directory, selected);
    if names_match(selected, clone_name, case_sensitive) {
        selected_path
    } else {
        clone_destination_path(&selected_path, clone_name)
    }
}

pub(crate) fn names_match(left: &str, right: &str, case_sensitive: bool) -> bool {
    if case_sensitive { left == right } else { left.to_lowercase() == right.to_lowercase() }
}

/// `filterPinnedBrowseEntries`: while the leaf is the clone's own folder, every folder shows.
pub(crate) fn pinned_filter<'a>(leaf: &'a str, pinned: &str, case_sensitive: bool) -> &'a str {
    if names_match(leaf, pinned, case_sensitive) { "" } else { leaf }
}

/// `REPOSITORY_SHORTHAND`: `owner/name` the way GitHub spells them.
fn is_repository_shorthand(value: &str) -> bool {
    let Some((owner, name)) = value.split_once('/') else { return false };
    let owner_ok = owner.len() <= 39
        && owner.bytes().next().is_some_and(|b| b.is_ascii_alphanumeric())
        && owner.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
    owner_ok && !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// `normalizePastedCloneUrl`: `owner/name` becomes the default host's clone URL (GitHub, over
/// HTTPS); anything else is kept as typed.
pub(crate) fn normalize_pasted_clone_url(input: &str) -> String {
    let trimmed = input.trim();
    if !is_repository_shorthand(trimmed) {
        return trimmed.to_owned();
    }
    let path = if trimmed.ends_with(".git") { trimmed.to_owned() } else { format!("{trimmed}.git") };
    format!("https://github.com/{path}")
}

/// `githubIncompleteOwnerRepositoryMessage`: a GitHub field holding only `owner/`.
pub(crate) fn github_incomplete_owner_message(kind: Option<&str>, input: &str) -> Option<&'static str> {
    if kind != Some("github") {
        return None;
    }
    let trimmed = input.trim();
    let owner = trimmed.trim_end_matches('/');
    let incomplete = owner.len() < trimmed.len()
        && owner.len() <= 39
        && owner.bytes().next().is_some_and(|b| b.is_ascii_alphanumeric())
        && owner.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'));
    incomplete.then_some("Enter the full owner/repository name.")
}

/// `appSignInTargetForRemote` for GitHub, the one host with an app sign-in: the remote's
/// `owner/name` on github.com, in the remote's own casing.
pub(crate) fn github_app_repository(remote_url: &str) -> Option<String> {
    let normalized = remote_url.trim().trim_end_matches('/');
    let normalized = normalized.strip_suffix(".git").or_else(|| normalized.strip_suffix(".GIT")).unwrap_or(normalized);
    let (host, path) = if let Some(at) = normalized.find("://") {
        let rest = &normalized[at + 3..];
        let (authority, path) = rest.split_once('/')?;
        let host = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
        (host.split(':').next().unwrap_or(host), path)
    } else {
        let (user_host, path) = normalized.split_once(':')?;
        (user_host.rsplit_once('@').map_or(user_host, |(_, host)| host), path.trim_start_matches('/'))
    };
    if !host.eq_ignore_ascii_case("github.com") {
        return None;
    }
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    (segments.len() >= 2).then(|| segments.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_names_come_from_the_repository_or_url() {
        let cases = [
            ("owner/repo", "repo"),
            ("https://github.com/owner/repo.git", "repo"),
            ("git@github.com:owner/repo.git", "repo"),
            ("ssh://git@host:22/owner/repo", "repo"),
            ("https://github.com/owner/repo?tab=readme#top", "repo"),
            ("https://github.com", ""),
            ("https://host:8080", ""),
            ("git@github.com:", ""),
            ("repo.git", "repo"),
            ("", ""),
        ];
        for (input, name) in cases {
            assert_eq!(clone_directory_name(input), name, "{input}");
        }
    }

    #[test]
    fn ghq_paths_name_the_host_owner_and_repository() {
        let cases = [
            ("https://github.com/Owner/Repo.git", Some("github.com/Owner/Repo")),
            ("git@gitlab.com:group/sub/project.git", Some("gitlab.com/group/sub/project")),
            ("ssh://git@host.example:2222/owner/repo", Some("host.example/owner/repo")),
            ("https://user@name@host.example/owner/repo?x=1", Some("host.example/owner/repo")),
            ("C:\\code\\repo", None),
            ("/srv/repo.git", None),
            ("https://github.com/owner/..", None),
            ("https://github.com/owner/con.txt", None),
            ("https://github.com/owner/COM1", None),
            ("https://github.com/owner/repo.", None),
            ("https://[::1]/owner/repo", None),
            ("git@host:/abs/path", None),
        ];
        for (url, path) in cases {
            assert_eq!(clone_url_repository_path(url).as_deref(), path, "{url}");
        }
        assert_eq!(clone_directory_path("https://github.com/o/r.git", true, "o/r"), "github.com/o/r");
        assert_eq!(clone_directory_path("https://github.com/o/r.git", false, "o/r"), "r");
        assert_eq!(clone_directory_path("/srv/r.git", true, "/srv/r.git"), "r", "a local path falls back to the name");
        assert_eq!(clone_directory_leaf_name("https://github.com/o/r.git", true, "o/r"), "r");
    }

    #[test]
    fn destinations_follow_the_base_folders_separators() {
        assert_eq!(clone_destination_path("~/code", "repo"), "~/code/repo");
        assert_eq!(clone_destination_path("/srv/code/", "github.com/o/r"), "/srv/code/github.com/o/r");
        assert_eq!(clone_destination_path("C:\\code", "github.com/o/r"), "C:\\code\\github.com\\o\\r");
        assert_eq!(clone_destination_path("~/code", "  "), "~/code");
        assert_eq!(add_project_initial_query(None), "~/");
        assert_eq!(add_project_initial_query(Some("  ")), "~/");
        assert_eq!(add_project_initial_query(Some("D:\\work")), "D:\\work\\");
        assert_eq!(add_project_initial_query(Some("/home/me/src/")), "/home/me/src/");
    }

    #[test]
    fn entering_a_folder_keeps_the_clone_folder_last() {
        assert_eq!(clone_destination_browse_path("/home/me/repo", "src", "repo", true), "/home/me/src/repo");
        assert_eq!(clone_destination_browse_path("/home/me/repo", "repo", "repo", true), "/home/me/repo/");
        assert_eq!(clone_destination_browse_path("C:\\x\\repo", "REPO", "repo", false), "C:\\x\\REPO\\");
        assert_eq!(clone_destination_browse_path("C:\\x\\repo", "src", "repo", false), "C:\\x\\src\\repo");
        assert_eq!(pinned_filter("Repo", "repo", false), "");
        assert_eq!(pinned_filter("Repo", "repo", true), "Repo");
    }

    #[test]
    fn new_projects_get_a_safe_slug_below_their_root() {
        assert_eq!(new_project_folder_name("  My App!! "), "my-app");
        assert_eq!(new_project_folder_name("Café Déjà Vu"), "cafe-deja-vu");
        assert_eq!(new_project_folder_name("--x__y--"), "x-y");
        assert_eq!(new_project_folder_name("日本"), "project");
        assert_eq!(new_project_folder_name("CON"), "con-project");
        assert_eq!(new_project_folder_name("lpt3"), "lpt3-project");
        assert_eq!(new_project_folder_name("com10"), "com10");
        assert_eq!(
            new_project_folder_name(&format!("{}-b", "a".repeat(63))),
            "a".repeat(63),
            "cut at 64, then trimmed"
        );
        assert_eq!(new_project_path_preview("/home/me/T3 Projects", "My App"), "/home/me/T3 Projects/my-app");
        assert_eq!(new_project_path_preview("C:\\Users\\me\\T3 Projects\\", ""), "C:\\Users\\me\\T3 Projects\\project");
    }

    #[test]
    fn pasted_shorthand_becomes_a_github_url() {
        assert_eq!(normalize_pasted_clone_url(" owner/repo "), "https://github.com/owner/repo.git");
        assert_eq!(normalize_pasted_clone_url("owner/repo.git"), "https://github.com/owner/repo.git");
        assert_eq!(normalize_pasted_clone_url("git@host:o/r.git"), "git@host:o/r.git");
        assert_eq!(normalize_pasted_clone_url("-bad/repo"), "-bad/repo");
        assert_eq!(normalize_pasted_clone_url("a/b/c"), "a/b/c");
    }

    #[test]
    fn a_github_owner_alone_asks_for_the_repository() {
        let message = Some("Enter the full owner/repository name.");
        assert_eq!(github_incomplete_owner_message(Some("github"), "octo/"), message);
        assert_eq!(github_incomplete_owner_message(Some("github"), "octo//"), message);
        assert_eq!(github_incomplete_owner_message(Some("github"), "octo/repo"), None);
        assert_eq!(github_incomplete_owner_message(Some("github"), "octo"), None);
        assert_eq!(github_incomplete_owner_message(Some("gitlab"), "octo/"), None);
        assert_eq!(github_incomplete_owner_message(None, "octo/"), None);
    }

    #[test]
    fn app_access_reads_github_remotes_only() {
        assert_eq!(github_app_repository("https://github.com/Octo/Repo.git").as_deref(), Some("Octo/Repo"));
        assert_eq!(github_app_repository("git@github.com:octo/repo.git").as_deref(), Some("octo/repo"));
        assert_eq!(github_app_repository("ssh://git@GitHub.com:22/octo/repo/").as_deref(), Some("octo/repo"));
        assert_eq!(github_app_repository("https://gitlab.com/octo/repo.git"), None);
        assert_eq!(github_app_repository("https://github.com/octo"), None);
    }
}
