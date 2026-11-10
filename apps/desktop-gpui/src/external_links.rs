//! Web links in chat Markdown: the host mark drawn before a link's text
//! (`MarkdownExternalLinkContent`) and the hover preview a pull request link opens
//! (`PullRequestLinkPreview`, `resolvePullRequestPreviewTarget`).

use std::net::{Ipv4Addr, Ipv6Addr};

use serde::Deserialize;

use crate::pull_request_detail::{Actor, Reference};
use crate::pull_requests::State;

/// `resolveExternalWebLinkHost`: the host of an `http(s)` link (`//host` reads as https).
pub fn web_link_host(href: &str) -> Option<String> {
    let href = if href.starts_with("//") { format!("https:{href}") } else { href.to_owned() };
    let url = url::Url::parse(&href).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    url.host_str().filter(|host| !host.is_empty()).map(str::to_owned)
}

/// What draws before a web link's text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkMark {
    /// A source control host's mark: its asset path, and whether it is one color that follows the text.
    Host(&'static str, bool),
    /// The host's favicon, loaded from this URL.
    Favicon(String),
    /// Lucide's globe, for a host whose favicon must not be asked of a third party.
    Globe,
}

/// `MarkdownLinkFavicon`: the host's source control mark (`findByPublicDomain`), else its favicon
/// for a public host, else the globe.
pub fn link_mark(host: &str) -> LinkMark {
    let host = host.to_ascii_lowercase();
    let on = |domain: &str| host == domain || host.strip_suffix(domain).is_some_and(|rest| rest.ends_with('.'));
    let mark = [
        ("github.com", "icons/app/source-control/github.svg", true),
        ("gitlab.com", "icons/app/source-control/gitlab.svg", false),
        ("bitbucket.org", "icons/app/source-control/bitbucket.svg", false),
        ("codeberg.org", "icons/app/source-control/forgejo.svg", false),
        ("git.cafe", "icons/app/source-control/gitcafe.svg", false),
        ("origin.cursor.com", "icons/app/cursor.svg", true),
    ]
    .into_iter()
    .find(|(domain, ..)| on(domain));
    match mark {
        Some((_, path, monochrome)) => LinkMark::Host(path, monochrome),
        None => favicon_url(&host).map_or(LinkMark::Globe, LinkMark::Favicon),
    }
}

/// `faviconUrlForOrigin`: Google's favicon service, only for public hosts.
fn favicon_url(host: &str) -> Option<String> {
    is_public_favicon_host(host).then(|| {
        let domain: String = url::form_urlencoded::byte_serialize(host.as_bytes()).collect();
        format!("https://www.google.com/s2/favicons?domain={domain}&sz=32")
    })
}

/// RFC 6762 Appendix G's private TLDs, and a few more (`PRIVATE_FAVICON_TLDS`).
const PRIVATE_TLDS: [&str; 11] =
    ["alt", "corp", "example", "home", "internal", "intranet", "invalid", "lan", "onion", "private", "test"];
const INTERNAL_LABELS: [&str; 3] = ["corp", "internal", "intranet"];

/// `isSpecialPurposeIpv4Address`.
fn special_ipv4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    crate::preview::is_private_network_host(&ip.to_string())
        || a >= 224
        || (a, b, c) == (192, 0, 0)
        || (a, b, c) == (192, 0, 2)
        || (a, b, c) == (192, 88, 99)
        || (a, b, c) == (198, 51, 100)
        || (a, b, c) == (203, 0, 113)
}

fn ipv6_in(ip: Ipv6Addr, prefix: [u16; 8], length: u32) -> bool {
    let (ip, prefix) = (u128::from(ip), u128::from(Ipv6Addr::from(prefix)));
    length == 0 || (ip ^ prefix) >> (128 - length) == 0
}

/// `isPublicFaviconHost`: a host whose name may go to a third-party favicon service.
pub fn is_public_favicon_host(host: &str) -> bool {
    // One trailing DNS dot is valid; repeated dots can conceal legacy numeric addresses.
    if host.ends_with("..") {
        return false;
    }
    let host = host.to_ascii_lowercase();
    let host = host.trim_start_matches('[').trim_end_matches(']').trim_end_matches('.');
    if crate::preview::is_private_network_host(host)
        || PRIVATE_TLDS.iter().any(|tld| host == *tld || host.strip_suffix(tld).is_some_and(|rest| rest.ends_with('.')))
        || host.split('.').any(|label| INTERNAL_LABELS.contains(&label))
    {
        return false;
    }
    if let Ok(ip) = host.parse::<Ipv4Addr>() {
        return !special_ipv4(ip);
    }
    if !host.contains(':') {
        return true;
    }
    let Ok(ip) = host.parse::<Ipv6Addr>() else { return false };
    if let Some(mapped) = ip.to_ipv4_mapped() {
        return !special_ipv4(mapped);
    }
    let segments = ip.segments();
    if ipv6_in(ip, [0x64, 0xff9b, 0, 0, 0, 0, 0, 0], 96) {
        let [.., a, b, c, d] = ip.octets();
        let embedded = Ipv4Addr::new(a, b, c, d);
        return !special_ipv4(embedded);
    }
    if segments[0] & 0xe000 != 0x2000 {
        return false;
    }
    if ipv6_in(ip, [0x2001, 0, 0, 0, 0, 0, 0, 0], 23) {
        return (segments[1] == 1 && segments[2..7].iter().all(|s| *s == 0) && (1..=3).contains(&segments[7]))
            || ipv6_in(ip, [0x2001, 3, 0, 0, 0, 0, 0, 0], 32)
            || ipv6_in(ip, [0x2001, 4, 0x0112, 0, 0, 0, 0, 0], 48)
            || ipv6_in(ip, [0x2001, 0x20, 0, 0, 0, 0, 0, 0], 28)
            || ipv6_in(ip, [0x2001, 0x30, 0, 0, 0, 0, 0, 0], 28);
    }
    !(ipv6_in(ip, [0x2001, 0x0db8, 0, 0, 0, 0, 0, 0], 32)
        || ipv6_in(ip, [0x2002, 0, 0, 0, 0, 0, 0, 0], 16)
        || (segments[0] == 0x3fff && segments[1] & 0xf000 == 0))
}

/// `resolvePullRequestPreviewTarget`: the pull request a link names, read through the project of
/// the message's `environment` checked out from its repository. `projects` are that environment's
/// (id, repository key) pairs; `None` while its config has not loaded (`pullRequestsEnabled`).
pub fn preview_target<'a>(
    environment: &str,
    url: &str,
    config_loaded: bool,
    projects: impl IntoIterator<Item = (&'a str, Option<&'a str>)>,
) -> Option<Reference> {
    if !config_loaded {
        return None;
    }
    let (host, repository, number) = crate::pull_request_detail::parse_change_request_url(url)?;
    let project_id = crate::pull_request_detail::project_for_link(projects, &host, &repository)?.to_owned();
    Some(Reference { environment_id: environment.to_owned(), project_id, host: Some(host), repository, number })
}

/// `PullRequestPreview`, what `pullRequests.preview` answers.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Preview {
    pub repository: String,
    pub number: u64,
    pub title: String,
    pub author: Option<Actor>,
    pub state: State,
    #[serde(default)]
    pub is_draft: bool,
    pub created_at: String,
}

/// What the pointer did to the links of every text view in one event: a link wrapped over lines
/// reports from each line, so moving between them leaves one piece and enters another.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HoverBatch<B> {
    pub entered: Option<(String, B)>,
    pub left: bool,
}

/// What one event's batch does to the link the pointer was on.
#[derive(Clone, Debug, PartialEq)]
pub enum HoverChange<B> {
    /// Still on the same link, here now.
    Stay(B),
    /// On another link.
    Enter(String, B),
    /// Off the link.
    Leave,
    Nothing,
}

/// An entered link wins over a leave in the same event, so a move between two pieces of one
/// link stays on it. A link is its URL in its place (`P`), so the same URL elsewhere is another.
pub fn hover_change<B, P: PartialEq>(current: Option<(&str, &P)>, batch: HoverBatch<(B, P)>) -> HoverChange<(B, P)> {
    match batch.entered {
        Some((url, (bounds, place))) if current == Some((url.as_str(), &place)) => HoverChange::Stay((bounds, place)),
        Some((url, entered)) => HoverChange::Enter(url, entered),
        None if batch.left && current.is_some() => HoverChange::Leave,
        None => HoverChange::Nothing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_links_name_their_host() {
        assert_eq!(web_link_host("https://GitHub.com/a/b").as_deref(), Some("github.com"));
        assert_eq!(web_link_host("//docs.rs/x").as_deref(), Some("docs.rs"));
        assert_eq!(web_link_host("http://[::1]:3000/").as_deref(), Some("[::1]"));
        assert_eq!(web_link_host("mailto:a@b.c"), None);
        assert_eq!(web_link_host("src/a.rs"), None);
        assert_eq!(web_link_host("t3-thread://v2/e/t"), None);
    }

    #[test]
    fn hosts_draw_their_mark_favicon_or_globe() {
        assert_eq!(link_mark("github.com"), LinkMark::Host("icons/app/source-control/github.svg", true));
        assert_eq!(link_mark("gist.github.com"), LinkMark::Host("icons/app/source-control/github.svg", true));
        assert_eq!(link_mark("notgithub.com"), LinkMark::Favicon(favicon_url("notgithub.com").unwrap()));
        assert_eq!(link_mark("Codeberg.org"), LinkMark::Host("icons/app/source-control/forgejo.svg", false));
        assert_eq!(
            link_mark("docs.rs"),
            LinkMark::Favicon("https://www.google.com/s2/favicons?domain=docs.rs&sz=32".into())
        );
        assert_eq!(link_mark("localhost"), LinkMark::Globe);
        assert_eq!(link_mark("wiki.corp.example.com"), LinkMark::Globe);
    }

    #[test]
    fn only_public_hosts_go_to_the_favicon_service() {
        for host in ["example.com", "example.com.", "8.8.8.8", "[2606:4700::1111]", "2001:3::1", "2001:4860::8888"] {
            assert!(is_public_favicon_host(host), "{host}");
        }
        for host in [
            "nas",
            "printer.local",
            "box.ts.net",
            "site.test",
            "intranet",
            "app.internal.example.com",
            "example.com..",
            "10.0.0.1",
            "100.64.0.1",
            "224.0.0.1",
            "192.0.2.4",
            "::1",
            "fe80::1",
            "fd7a:115c:a1e0::1",
            "::ffff:10.0.0.1",
            "64:ff9b::a00:1",
            "2001:db8::1",
            "2002::1",
            "2001:2::1",
            "2001::1",
            "3fff::1",
            "not:an:ip",
        ] {
            assert!(!is_public_favicon_host(host), "{host}");
        }
    }

    #[test]
    fn a_pull_request_link_previews_through_its_repositorys_project() {
        let projects = [("p1", Some("github.com/acme/app")), ("p2", Some("github.com/acme/other")), ("p3", None)];
        let target =
            preview_target("e1", "https://github.com/Acme/App/pull/7/files", true, projects).expect("a target");
        assert_eq!(
            (
                target.environment_id.as_str(),
                target.project_id.as_str(),
                target.host.as_deref(),
                target.repository.as_str(),
                target.number
            ),
            ("e1", "p1", Some("github.com"), "acme/app", 7)
        );
        let none = |url| preview_target("e1", url, true, projects);
        assert_eq!(
            preview_target("e1", "https://github.com/acme/app/pull/7", false, projects),
            None,
            "config not loaded"
        );
        assert_eq!(none("https://github.com/acme/elsewhere/pull/7"), None);
        assert_eq!(none("https://github.com/acme/app/issues/7"), None);
    }

    #[test]
    fn an_entered_link_wins_over_a_leave_in_the_same_event() {
        let batch =
            |entered: Option<&str>, left| HoverBatch { entered: entered.map(|url| (url.to_owned(), (1, 'm'))), left };
        let main = |url| Some((url, &'m'));
        assert_eq!(
            hover_change(main("a"), batch(Some("a"), true)),
            HoverChange::Stay((1, 'm')),
            "a wrapped link's next line"
        );
        assert_eq!(hover_change(main("a"), batch(Some("b"), true)), HoverChange::Enter("b".into(), (1, 'm')));
        assert_eq!(hover_change(None, batch(Some("a"), false)), HoverChange::Enter("a".into(), (1, 'm')));
        assert_eq!(hover_change(main("a"), batch(None, true)), HoverChange::Leave);
        assert_eq!(hover_change::<i32, char>(None, batch(None, true)), HoverChange::Nothing);
        // The same URL in another pane is another link: its card belongs to that pane.
        assert_eq!(hover_change(Some(("a", &'s')), batch(Some("a"), true)), HoverChange::Enter("a".into(), (1, 'm')));
    }

    #[test]
    fn previews_read_the_wire_shape() {
        let preview: Preview = serde_json::from_value(serde_json::json!({
            "projectId": "p1", "repository": "acme/app", "number": 7, "title": "Add", "url": "https://github.com/acme/app/pull/7",
            "author": null, "state": "merged", "isDraft": false, "createdAt": "2026-10-01T00:00:00.000Z",
        }))
        .unwrap();
        assert!(preview.author.is_none());
        assert_eq!((preview.state, preview.number), (State::Merged, 7));
    }
}
