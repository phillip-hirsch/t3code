//! Read-only SQLite databases (`binaryPreview.worker.ts`'s sql.js queries): a database's own tables
//! with their row counts, and a table's rows a page at a time as text. A database opens from its
//! bytes into memory, so nothing writes the file and a locked or remote file reads the same way.
//! Queries block; callers run them off the UI thread.
//!
//! [`Snapshot`] reads another app's database file, such as a browser's cookie store.

use std::io::Read as _;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use rusqlite::types::ValueRef;

/// `SQLITE_PAGE_SIZE`: the rows one page shows.
pub(crate) const PAGE_SIZE: usize = 50;
const MAX_TABLES: usize = 100;
const MAX_COLUMNS: usize = 50;

/// A table and its row count.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Table {
    pub(crate) name: String,
    pub(crate) count: u64,
}

/// One page of a table: its first 50 visible columns, and each cell as text, `NULL`, or a blob's size.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Rows {
    pub(crate) columns: Vec<String>,
    pub(crate) rows: Vec<Vec<String>>,
}

/// An open database and the tables a preview may read.
pub(crate) struct Database {
    connection: Connection,
    tables: Vec<Table>,
}

fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn message(error: rusqlite::Error) -> String {
    error.to_string()
}

impl Database {
    /// The database in `bytes`, read only, with its schema untrusted. Empty bytes are an empty
    /// database, as sql.js opens them.
    pub(crate) fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
        let mut connection = Connection::open_in_memory().map_err(message)?;
        if !bytes.is_empty() {
            // A WAL database's header (bytes 18 and 19 set to 2) asks for a `-wal` file that memory
            // cannot open; only the main file is here, so it reads as a rollback-journal database.
            let opened = if bytes.get(18..20) == Some(&[2, 2][..]) {
                let reader = (&bytes[..18]).chain(&[1, 1][..]).chain(&bytes[20..]);
                connection.deserialize_read_exact("main", reader, bytes.len(), true)
            } else {
                connection.deserialize_read_exact("main", bytes, bytes.len(), true)
            };
            opened.map_err(message)?;
        }
        connection.execute_batch("PRAGMA query_only=ON; PRAGMA trusted_schema=OFF;").map_err(message)?;
        let names = {
            let mut statement = connection
                .prepare(
                    "SELECT name FROM sqlite_schema WHERE type='table' AND substr(name, 1, 7) != 'sqlite_' \
                     AND sql NOT LIKE 'CREATE VIRTUAL TABLE%' ORDER BY name LIMIT 101",
                )
                .map_err(message)?;
            statement
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(message)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(message)?
        };
        if names.len() > MAX_TABLES {
            return Err("Database previews are limited to 100 tables.".into());
        }
        let tables = names
            .into_iter()
            .map(|name| {
                let count: i64 = connection
                    .query_row(&format!("SELECT count(*) FROM {}", quote(&name)), [], |row| row.get(0))
                    .map_err(message)?;
                Ok(Table { name, count: u64::try_from(count).unwrap_or_default() })
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(Self { connection, tables })
    }

    /// The tables, by name, without SQLite's own and virtual ones.
    pub(crate) fn tables(&self) -> &[Table] {
        &self.tables
    }

    /// Page `page` of `table`: up to 50 columns that are not hidden, cells cut to 1,024 characters.
    pub(crate) fn rows(&self, table: &str, page: usize) -> Result<Rows, String> {
        let unavailable = || "This table is unavailable.".to_owned();
        if !self.tables.iter().any(|item| item.name == table) {
            return Err(unavailable());
        }
        let offset = page.checked_mul(PAGE_SIZE).ok_or_else(unavailable)?;
        let columns = {
            let mut statement =
                self.connection.prepare(&format!("PRAGMA table_xinfo({})", quote(table))).map_err(message)?;
            let columns = statement
                .query_map([], |row| Ok((row.get::<_, String>(1)?, row.get::<_, i64>(6)?)))
                .map_err(message)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(message)?;
            // `hidden` is 1 for a virtual table's hidden columns; generated columns show.
            columns
                .into_iter()
                .filter(|(_, hidden)| *hidden != 1)
                .map(|(name, _)| name)
                .take(MAX_COLUMNS)
                .collect::<Vec<_>>()
        };
        let fields = columns
            .iter()
            .map(|name| {
                let field = quote(name);
                format!(
                    "CASE WHEN typeof({field}) = 'blob' THEN '[blob: ' || length({field}) || ' bytes]' \
                     ELSE substr(CAST({field} AS TEXT), 1, 1024) END"
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!("SELECT {fields} FROM {} LIMIT {PAGE_SIZE} OFFSET {offset}", quote(table));
        let mut statement = self.connection.prepare(&sql).map_err(message)?;
        let width = columns.len();
        let rows = statement
            .query_map([], |row| {
                (0..width)
                    .map(|index| {
                        Ok(match row.get_ref(index)? {
                            ValueRef::Null => "NULL".to_owned(),
                            ValueRef::Text(text) | ValueRef::Blob(text) => String::from_utf8_lossy(text).into_owned(),
                            ValueRef::Integer(value) => value.to_string(),
                            ValueRef::Real(value) => value.to_string(),
                        })
                    })
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
            .map_err(message)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(message)?;
        Ok(Rows { columns, rows })
    }
}

/// A private copy of the database at a path, with its write-ahead log, so another app's live
/// database is never opened or locked and its uncheckpointed writes still read. The copy is
/// opened query-only with an untrusted schema, and deleted on drop.
pub(crate) struct Snapshot {
    // Declared first so it closes before its folder is deleted.
    connection: Connection,
    _copy: CopyDir,
}

impl Snapshot {
    pub(crate) fn open(path: &Path) -> Result<Self, String> {
        let name = path.file_name().ok_or_else(|| format!("{} is not a file.", path.display()))?;
        let copy = CopyDir::new()?;
        let target = copy.0.join(name);
        std::fs::copy(path, &target).map_err(|error| error.to_string())?;
        let wal = with_suffix(path, "-wal");
        if wal.is_file() {
            std::fs::copy(&wal, with_suffix(&target, "-wal")).map_err(|error| error.to_string())?;
        }
        let connection = Connection::open(&target).map_err(message)?;
        connection.execute_batch("PRAGMA query_only=ON; PRAGMA trusted_schema=OFF;").map_err(message)?;
        Ok(Self { connection, _copy: copy })
    }

    pub(crate) fn connection(&self) -> &Connection {
        &self.connection
    }
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// A new folder only this user can read, removed with what SQLite left in it.
struct CopyDir(PathBuf);

impl CopyDir {
    fn new() -> Result<Self, String> {
        let dir = std::env::temp_dir().join(format!("t3code-sqlite-{}", uuid::Uuid::new_v4()));
        #[cfg_attr(not(unix), expect(unused_mut, reason = "only Unix sets a mode"))]
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder.create(&dir).map_err(|error| error.to_string())?;
        Ok(Self(dir))
    }
}

impl Drop for CopyDir {
    fn drop(&mut self) {
        if let Ok(entries) = std::fs::read_dir(&self.0) {
            for entry in entries.flatten() {
                let _ = std::fs::remove_file(entry.path());
            }
        }
        let _ = std::fs::remove_dir(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshots_read_a_copy_with_its_wal_and_leave_no_copy_behind() {
        let dir = std::env::temp_dir().join(format!("t3-gpui-snapshot-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Cookies");
        let live = Connection::open(&path).unwrap();
        live.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;
             CREATE TABLE cookies(name TEXT); INSERT INTO cookies VALUES ('a'), ('b');",
        )
        .unwrap();
        assert!(with_suffix(&path, "-wal").is_file(), "the rows are still in the log");

        let snapshot = Snapshot::open(&path).unwrap();
        let count: i64 = snapshot.connection().query_row("SELECT count(*) FROM cookies", [], |row| row.get(0)).unwrap();
        assert_eq!(count, 2);
        assert!(snapshot.connection().execute("DELETE FROM cookies", []).is_err(), "query only");
        let copy = snapshot._copy.0.clone();
        drop(snapshot);
        assert!(!copy.exists(), "the copy is deleted");
        drop(live);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A database built by `sql`, as the bytes of its file.
    fn database(sql: &str) -> Vec<u8> {
        let mut connection = Connection::open_in_memory().expect("open");
        connection.execute_batch(sql).expect("schema");
        connection.serialize("main").expect("serialize").to_vec()
    }

    fn names(database: &Database) -> Vec<&str> {
        database.tables().iter().map(|table| table.name.as_str()).collect()
    }

    #[test]
    fn tables_leave_out_sqlite_and_virtual_tables_and_count_their_rows() {
        let bytes = database(
            "CREATE TABLE notes(id INTEGER PRIMARY KEY AUTOINCREMENT, body TEXT);
             INSERT INTO notes(body) VALUES ('a'), ('b');
             CREATE TABLE \"Big \"\"one\"\"\"(x); INSERT INTO \"Big \"\"one\"\"\" VALUES (1);
             CREATE VIEW recent AS SELECT * FROM notes;
             CREATE VIRTUAL TABLE search USING fts5(body);",
        );
        let opened = Database::from_bytes(&bytes).expect("database");
        let tables = names(&opened);
        assert!(!tables.contains(&"sqlite_sequence"), "SQLite's own table: {tables:?}");
        assert!(!tables.contains(&"search"), "the virtual table: {tables:?}");
        assert!(!tables.contains(&"recent"), "a view: {tables:?}");
        assert_eq!(&tables[..2], ["Big \"one\"", "notes"], "by name");
        assert_eq!(opened.tables()[1], Table { name: "notes".into(), count: 2 });
        assert_eq!(opened.tables()[0].count, 1);

        let many = (0..101).map(|index| format!("CREATE TABLE t{index}(x);")).collect::<Vec<_>>().concat();
        assert_eq!(
            Database::from_bytes(&database(&many)).err().as_deref(),
            Some("Database previews are limited to 100 tables.")
        );
        let hundred = (0..100).map(|index| format!("CREATE TABLE t{index}(x);")).collect::<Vec<_>>().concat();
        assert_eq!(Database::from_bytes(&database(&hundred)).expect("100 tables").tables().len(), 100);
    }

    #[test]
    fn rows_come_a_page_at_a_time_as_text() {
        let wide: Vec<String> = (0..60).map(|index| format!("c{index}")).collect();
        let bytes = database(&format!(
            "CREATE TABLE items(id INTEGER, label TEXT, data BLOB, score REAL);
             WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 120)
             INSERT INTO items SELECT i, 'item ' || i, NULL, i / 2.0 FROM n;
             UPDATE items SET label = NULL, data = x'010203' WHERE id = 1;
             UPDATE items SET label = replace(hex(zeroblob(2000)), '00', 'x') WHERE id = 2;
             CREATE TABLE wide({}); INSERT INTO wide(c0) VALUES ('first');",
            wide.join(", ")
        ));
        let opened = Database::from_bytes(&bytes).expect("database");
        let first = opened.rows("items", 0).expect("page 0");
        assert_eq!(first.columns, ["id", "label", "data", "score"]);
        assert_eq!(first.rows.len(), PAGE_SIZE);
        assert_eq!(first.rows[0], ["1", "NULL", "[blob: 3 bytes]", "0.5"]);
        assert_eq!(first.rows[1][1].chars().count(), 1024, "cells are cut to 1,024 characters");
        assert_eq!(first.rows[2], ["3", "item 3", "NULL", "1.5"]);
        assert_eq!(opened.rows("items", 1).expect("page 1").rows[0][0], "51");
        let last = opened.rows("items", 2).expect("page 2");
        assert_eq!((last.rows.len(), last.rows[19][0].as_str()), (20, "120"));
        assert!(opened.rows("items", 3).expect("past the end").rows.is_empty());

        let wide = opened.rows("wide", 0).expect("wide");
        assert_eq!((wide.columns.len(), wide.columns[49].as_str()), (50, "c49"), "up to 50 columns");
        assert_eq!(wide.rows[0][0], "first");

        for unknown in ["missing", "sqlite_schema", "items\"; DROP TABLE items; --"] {
            assert_eq!(opened.rows(unknown, 0).err().as_deref(), Some("This table is unavailable."), "{unknown}");
        }
        assert_eq!(opened.rows("items", usize::MAX).err().as_deref(), Some("This table is unavailable."));
    }

    #[test]
    fn damaged_empty_and_wal_files() {
        assert_eq!(
            Database::from_bytes(b"not a database at all, just text").err().as_deref(),
            Some("file is not a database")
        );
        assert!(Database::from_bytes(&[]).expect("empty").tables().is_empty());
        let mut wal = database("CREATE TABLE kept(x); INSERT INTO kept VALUES ('row');");
        wal[18] = 2;
        wal[19] = 2;
        let opened = Database::from_bytes(&wal).expect("a WAL database's main file");
        assert_eq!(opened.rows("kept", 0).expect("rows").rows, [["row"]]);
    }

    #[test]
    fn the_database_cannot_be_written() {
        let opened = Database::from_bytes(&database("CREATE TABLE t(x);")).expect("database");
        assert!(opened.connection.execute("INSERT INTO t VALUES (1)", []).is_err());
    }
}
