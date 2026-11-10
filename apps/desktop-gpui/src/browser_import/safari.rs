//! `SafariCookies.ts`: Safari's `Cookies.binarycookies` jar, read only with Full Disk Access.

use std::path::Path;

use super::{ImportedCookie, Reason, SameSite};

/// Safari's timestamps count seconds from 2001-01-01.
const APPLE_EPOCH_OFFSET: i64 = 978_307_200;
const PAGE_HEADER: usize = 12;
const RECORD_HEADER: usize = 56;
const FLAG_SECURE: u32 = 0x1;
const FLAG_HTTP_ONLY: u32 = 0x4;
/// macOS privacy protection refuses with `EPERM`; `EACCES` is a plain permission problem that Full
/// Disk Access does not fix.
const EPERM: i32 = 1;

fn denied(error: &std::io::Error) -> bool {
    error.raw_os_error() == Some(EPERM)
}

/// `safariAccessDenied`: listing the jar needs no Full Disk Access, but opening it does.
pub(super) fn access_denied(jar: &Path) -> bool {
    std::fs::File::open(jar).is_err_and(|error| denied(&error))
}

pub(super) fn read(jar: &Path) -> Result<Vec<ImportedCookie>, Reason> {
    let bytes = std::fs::read(jar)
        .map_err(|error| if denied(&error) { Reason::NeedsFullDiskAccess } else { Reason::ReadFailed })?;
    parse(&bytes).ok_or(Reason::ReadFailed)
}

fn u32_be(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(bytes.get(at..at.checked_add(4)?)?.try_into().ok()?))
}

fn u32_le(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at.checked_add(4)?)?.try_into().ok()?))
}

fn offset_le(bytes: &[u8], at: usize) -> Option<usize> {
    usize::try_from(u32_le(bytes, at)?).ok()
}

fn c_string(bytes: &[u8], start: usize) -> String {
    let rest = bytes.get(start..).unwrap_or_default();
    let end = rest.iter().position(|&byte| byte == 0).unwrap_or(rest.len());
    String::from_utf8_lossy(&rest[..end]).into_owned()
}

/// `parseBinaryCookies`: a big-endian header of page sizes, then little-endian pages of records.
/// Anything truncated or overlapping fails the whole jar rather than inventing cookies.
pub(super) fn parse(bytes: &[u8]) -> Option<Vec<ImportedCookie>> {
    if bytes.get(..4)? != b"cook" {
        return None;
    }
    let page_count = usize::try_from(u32_be(bytes, 4)?).ok()?;
    let mut page_start = page_count.checked_mul(4)?.checked_add(8)?;
    if page_start > bytes.len() {
        return None;
    }
    let mut cookies = Vec::new();
    for index in 0..page_count {
        let page_size = usize::try_from(u32_be(bytes, 8 + index * 4)?).ok()?;
        let page_end = page_start.checked_add(page_size)?;
        if page_size < PAGE_HEADER || page_end > bytes.len() {
            return None;
        }
        let page = &bytes[page_start..page_end];
        page_start = page_end;

        let cookie_count = offset_le(page, 4)?;
        let table_end = cookie_count.checked_mul(4)?.checked_add(PAGE_HEADER)?;
        if table_end > page.len() {
            return None;
        }
        let mut accepted: Vec<(usize, usize)> = Vec::new();
        for record in 0..cookie_count {
            let start = offset_le(page, 8 + record * 4)?;
            if start < table_end || start.checked_add(RECORD_HEADER)? > page.len() {
                return None;
            }
            let size = offset_le(page, start)?;
            let end = start.checked_add(size)?;
            if size < RECORD_HEADER || end > page.len() || accepted.iter().any(|&(s, e)| start < e && end > s) {
                return None;
            }
            accepted.push((start, end));
            let cookie = &page[start..end];
            let flags = u32_le(cookie, 8)?;
            let offsets = [16, 20, 24, 28].map(|at| offset_le(cookie, at));
            let [Some(domain), Some(name), Some(path), Some(value)] = offsets else { return None };
            if [domain, name, path, value].iter().any(|&offset| offset < RECORD_HEADER || offset >= cookie.len()) {
                return None;
            }
            let expiry = f64::from_le_bytes(cookie.get(40..48)?.try_into().ok()?);
            let (domain, name) = (c_string(cookie, domain), c_string(cookie, name));
            if domain.is_empty() || name.is_empty() {
                continue;
            }
            let path = Some(c_string(cookie, path)).filter(|path| !path.is_empty()).unwrap_or_else(|| "/".into());
            let (host, domain) = ImportedCookie::scoped(&domain);
            #[expect(clippy::cast_possible_truncation, reason = "a finite timestamp; `as` saturates")]
            let expires = (expiry > 0.).then(|| expiry.floor() as i64 + APPLE_EPOCH_OFFSET);
            cookies.push(ImportedCookie {
                host,
                domain,
                name,
                value: c_string(cookie, value),
                path,
                secure: flags & FLAG_SECURE != 0,
                http_only: flags & FLAG_HTTP_ONLY != 0,
                expires,
                // Safari's SameSite encoding is undocumented; guessing None would widen it.
                same_site: SameSite::Lax,
            });
        }
    }
    // Pages end with an 8-byte checksum and an optional length-prefixed plist.
    let trailer = bytes.len() - page_start;
    let valid = trailer == 0
        || trailer == 8
        || (trailer >= 12 && usize::try_from(u32_be(bytes, page_start + 8)?).ok()?.checked_add(12) == Some(trailer));
    valid.then_some(cookies)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(flags: u32, domain: &str, name: &str, path: &str, value: &str, expiry: f64) -> Vec<u8> {
        let strings = [domain, name, path, value].map(|text| [text.as_bytes(), &[0]].concat());
        let mut offsets = Vec::new();
        let mut at = RECORD_HEADER;
        for text in &strings {
            offsets.push(u32::try_from(at).unwrap());
            at += text.len();
        }
        let mut header = vec![0u8; RECORD_HEADER];
        header[..4].copy_from_slice(&u32::try_from(at).unwrap().to_le_bytes());
        header[8..12].copy_from_slice(&flags.to_le_bytes());
        for (index, offset) in offsets.iter().enumerate() {
            header[16 + index * 4..20 + index * 4].copy_from_slice(&offset.to_le_bytes());
        }
        header[40..48].copy_from_slice(&expiry.to_le_bytes());
        [header, strings.concat()].concat()
    }

    fn jar(records: &[Vec<u8>], trailer: &[u8]) -> Vec<u8> {
        let table = PAGE_HEADER + records.len() * 4;
        let mut page = vec![0, 0, 1, 0];
        page.extend(u32::try_from(records.len()).unwrap().to_le_bytes());
        let mut at = table;
        for record in records {
            page.extend(u32::try_from(at).unwrap().to_le_bytes());
            at += record.len();
        }
        page.extend([0; 4]);
        page.extend(records.concat());
        let mut bytes = b"cook".to_vec();
        bytes.extend(1u32.to_be_bytes());
        bytes.extend(u32::try_from(page.len()).unwrap().to_be_bytes());
        bytes.extend(page);
        bytes.extend(trailer);
        bytes
    }

    #[test]
    fn jars_parse_their_records_and_reject_damage() {
        let records = [
            record(FLAG_SECURE | FLAG_HTTP_ONLY, ".example.com", "sid", "/", "abc", 700_000_000.5),
            record(0, "host.example.com", "pref", "", "x", 0.),
            record(0, "", "nameless", "/", "x", 0.),
        ];
        let cookies = parse(&jar(&records, &[0; 8])).unwrap();
        assert_eq!(cookies.len(), 2, "a record without a domain is skipped");
        let sid = &cookies[0];
        assert_eq!(
            (sid.host.as_str(), sid.domain.as_deref(), sid.value.as_str()),
            ("example.com", Some(".example.com"), "abc")
        );
        assert_eq!((sid.secure, sid.http_only, sid.expires), (true, true, Some(700_000_000 + APPLE_EPOCH_OFFSET)));
        assert_eq!((cookies[1].path.as_str(), cookies[1].domain.as_deref(), cookies[1].expires), ("/", None, None));

        let mut plist = vec![0; 8];
        plist.extend(3u32.to_be_bytes());
        plist.extend(b"abc");
        assert_eq!(parse(&jar(&records, &plist)).map(|c| c.len()), Some(2), "a length-prefixed plist");

        let valid = jar(&records, &[0; 8]);
        assert!(parse(&valid[..valid.len() - 20]).is_none(), "truncated");
        assert!(parse(&jar(&records, &[0; 5])).is_none(), "a damaged trailer");
        assert!(parse(b"nope").is_none());
        let overlapping = {
            let mut bytes = jar(&records[..2], &[]);
            // Point the second record at the first.
            let table = 8 + 4 + 8;
            let first = bytes[table..table + 4].to_vec();
            bytes[table + 4..table + 8].copy_from_slice(&first);
            bytes
        };
        assert!(parse(&overlapping).is_none(), "overlapping records");
    }
}
