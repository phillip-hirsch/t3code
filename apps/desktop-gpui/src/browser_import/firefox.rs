//! `FirefoxCookies.ts`: `cookies.sqlite`, whose values are stored in the clear.

use std::path::Path;

use super::{ImportedCookie, Reason, SameSite};
use crate::sqlite::Snapshot;

/// Schemas 10 to 14 defaulted `sameSite` to Lax and kept the declared value in `rawSameSite`.
const RAW_SAME_SITE: std::ops::RangeInclusive<i64> = 10..=14;
/// Schema 16+ stores `expiry` in milliseconds; older ones in seconds.
const EXPIRY_MILLISECONDS: i64 = 16;

/// `0` none, `1` lax, `2` strict. Schema 9 added the column without a default, so old rows
/// are `NULL`; a Lax default over a declared None stays unspecified, not cross-site.
fn same_site(value: Option<i64>, raw: Option<i64>) -> SameSite {
    match (value, raw) {
        (Some(1), Some(0)) | (None, _) => SameSite::Unspecified,
        (Some(0), _) => SameSite::NoRestriction,
        (Some(1), _) => SameSite::Lax,
        (Some(2), _) => SameSite::Strict,
        _ => SameSite::Unspecified,
    }
}

pub(super) fn read(database: &Path) -> Result<Vec<ImportedCookie>, Reason> {
    let snapshot = Snapshot::open(database).map_err(|_| Reason::ReadFailed)?;
    read_connection(snapshot.connection()).map_err(|_| Reason::ReadFailed)
}

/// Firefox containers and private windows have no WebView equivalent, so only cookies outside
/// them (an empty `originAttributes`) are read; importing the rest would merge separate sessions.
fn read_connection(connection: &rusqlite::Connection) -> rusqlite::Result<Vec<ImportedCookie>> {
    let schema: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let raw = if RAW_SAME_SITE.contains(&schema) { "rawSameSite" } else { "NULL" };
    let sql = format!(
        "SELECT host, name, value, path, expiry, isSecure, isHttpOnly, sameSite, {raw} FROM moz_cookies \
         WHERE originAttributes = ''"
    );
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map([], |row| {
        let secure = row.get::<_, i64>(5)? == 1;
        let expiry: i64 = row.get(4)?;
        let (host, domain) = ImportedCookie::scoped(&row.get::<_, String>(0)?);
        Ok(ImportedCookie {
            host,
            domain,
            name: row.get(1)?,
            value: row.get(2)?,
            path: row.get(3)?,
            secure,
            http_only: row.get::<_, i64>(6)? == 1,
            expires: (expiry > 0).then_some(if schema >= EXPIRY_MILLISECONDS { expiry / 1000 } else { expiry }),
            same_site: same_site(row.get(7)?, row.get(8)?),
        })
    })?;
    rows.collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn database(schema: i64, raw: bool) -> rusqlite::Connection {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        let raw_column = if raw { ", rawSameSite INTEGER" } else { "" };
        connection
            .execute_batch(&format!(
                "PRAGMA user_version = {schema};
                 CREATE TABLE moz_cookies(originAttributes TEXT, host TEXT, name TEXT, value TEXT, path TEXT,
                   expiry INTEGER, isSecure INTEGER, isHttpOnly INTEGER, sameSite INTEGER{raw_column});"
            ))
            .unwrap();
        connection
    }

    #[test]
    fn cookies_outside_containers_read_with_their_expiry_and_same_site() {
        let connection = database(16, false);
        connection
            .execute_batch(
                "INSERT INTO moz_cookies VALUES ('', '.example.com', 'sid', 'abc', '/', 1700000000123, 1, 1, 0);
                 INSERT INTO moz_cookies VALUES ('', 'host.example.com', 'pref', 'x', '/a', 0, 0, 0, NULL);
                 INSERT INTO moz_cookies VALUES ('^userContextId=1', '.work.com', 'c', 'y', '/', 0, 0, 0, 1);",
            )
            .unwrap();
        let cookies = read_connection(&connection).unwrap();
        assert_eq!(cookies.len(), 2, "the container's cookie is left out");
        let sid = &cookies[0];
        assert_eq!(
            (sid.host.as_str(), sid.domain.as_deref(), sid.value.as_str()),
            ("example.com", Some(".example.com"), "abc")
        );
        assert_eq!(
            (sid.expires, sid.same_site, sid.secure, sid.http_only),
            (Some(1_700_000_000), SameSite::NoRestriction, true, true)
        );
        let pref = &cookies[1];
        assert_eq!((pref.domain.as_deref(), pref.expires, pref.same_site), (None, None, SameSite::Unspecified));

        let older = database(12, true);
        older
            .execute_batch(
                "INSERT INTO moz_cookies VALUES ('', 'a.com', 'lax-default', 'v', '/', 1700000000, 0, 0, 1, 0);
                 INSERT INTO moz_cookies VALUES ('', 'a.com', 'strict', 'v', '/', 1700000000, 0, 0, 2, 2);",
            )
            .unwrap();
        let cookies = read_connection(&older).unwrap();
        assert_eq!(cookies[0].same_site, SameSite::Unspecified, "Lax only by default");
        assert_eq!(cookies[0].expires, Some(1_700_000_000), "seconds before schema 16");
        assert_eq!(cookies[1].same_site, SameSite::Strict);
    }
}
