//! Thread links in chat Markdown (`packages/shared/src/threadLinks.ts`): `t3-thread://v1/{thread}`,
//! `t3-thread://v2/{environment}/{thread}`, and the hosted `https://app.t3.codes/threads/{env}/{thread}`.

const V1_PREFIX: &str = "t3-thread://v1/";
const V2_PREFIX: &str = "t3-thread://v2/";
/// `DEFAULT_HOSTED_APP_URL` + `/threads/`.
const HOSTED_PREFIX: &str = "https://app.t3.codes/threads/";

/// A link's ids as written; a v1 link names no environment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreadLink {
    pub environment_id: Option<String>,
    pub thread_id: String,
}

/// `TrimmedNonEmptyString`, which thread and environment ids decode through.
fn id(value: &str) -> Option<String> {
    (!value.is_empty() && value.trim() == value).then(|| value.to_owned())
}

/// `decodeURIComponent`: `None` for a stray `%` or bytes that are not UTF-8.
pub fn decode_uri_component(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = value.get(index + 1..index + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn hosted_segment(segment: &str) -> bool {
    !segment.is_empty() && segment.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'%' | b'-'))
}

/// `parseThreadLinkHref`. Thread ids can hold percent escapes of their own, so only a hosted
/// link, which escapes them itself, is decoded.
pub fn parse(href: &str) -> Option<ThreadLink> {
    if let Some(path) = href.strip_prefix(HOSTED_PREFIX) {
        let (environment, thread) = path.split_once('/')?;
        if !hosted_segment(environment) || !hosted_segment(thread) {
            return None;
        }
        return Some(ThreadLink {
            environment_id: Some(id(&decode_uri_component(environment)?)?),
            thread_id: id(&decode_uri_component(thread)?)?,
        });
    }
    if let Some(thread) = href.strip_prefix(V1_PREFIX) {
        return Some(ThreadLink { environment_id: None, thread_id: id(thread)? });
    }
    let (environment, thread) = href.strip_prefix(V2_PREFIX)?.split_once('/')?;
    Some(ThreadLink { environment_id: Some(id(environment)?), thread_id: id(thread)? })
}

/// `percentDecodedThreadLinkId`: the id decoded, when that changes it.
pub fn percent_decoded(thread_id: &str) -> Option<String> {
    decode_uri_component(thread_id).filter(|decoded| decoded != thread_id).and_then(|decoded| id(&decoded))
}

/// `unknownThreadLinkLabel`: what a link names when no thread here matches it.
pub fn unknown_label(link: &ThreadLink, message_environment: Option<&str>) -> &'static str {
    match link.environment_id.as_deref() {
        None => "Thread no longer available",
        Some(environment) if Some(environment) == message_environment => "Thread no longer available",
        Some(_) => "Thread on another machine",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(environment: Option<&str>, thread: &str) -> ThreadLink {
        ThreadLink { environment_id: environment.map(str::to_owned), thread_id: thread.to_owned() }
    }

    #[test]
    fn links_parse_like_the_shared_helper() {
        assert_eq!(parse("t3-thread://v1/abc"), Some(link(None, "abc")));
        assert_eq!(parse("t3-thread://v2/env/mcp:launch:x"), Some(link(Some("env"), "mcp:launch:x")));
        // v2 ids keep their escapes; hosted ones are decoded.
        assert_eq!(parse("t3-thread://v2/env/a%2Fb"), Some(link(Some("env"), "a%2Fb")));
        assert_eq!(parse("https://app.t3.codes/threads/env/a%2Eb"), Some(link(Some("env"), "a.b")));
        assert_eq!(parse("t3-thread://v2/env"), None);
        assert_eq!(parse("t3-thread://v3/x"), None);
        assert_eq!(parse("t3-thread://v1/"), None);
        assert_eq!(parse("https://app.t3.codes/threads/env/a.b"), None);
        assert_eq!(parse("https://example.com/threads/env/abc"), None);
    }

    #[test]
    fn unknown_links_name_where_the_thread_was() {
        let remote = parse("t3-thread://v2/mac/abc").unwrap();
        assert_eq!(unknown_label(&remote, Some("pc")), "Thread on another machine");
        assert_eq!(unknown_label(&remote, Some("mac")), "Thread no longer available");
        assert_eq!(unknown_label(&parse("t3-thread://v1/abc").unwrap(), Some("pc")), "Thread no longer available");
    }

    #[test]
    fn written_ids_decode_only_when_that_changes_them() {
        assert_eq!(percent_decoded("a%20b").as_deref(), Some("a b"));
        assert_eq!(percent_decoded("plain"), None);
        assert_eq!(percent_decoded("bad%zz"), None);
    }
}
