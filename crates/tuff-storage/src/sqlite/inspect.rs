//! Checking a server file that came from outside (an uploaded backup)
//! without trusting it: opened read-only as immutable, with SQLite's
//! defensive settings on, integrity-checked before any row is read.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use rusqlite::config::DbConfig;
use rusqlite::{Connection, OpenFlags, OptionalExtension};

use super::schema::{APPLICATION_ID, FORMAT_VERSION};

const MAGIC: &[u8; 16] = b"SQLite format 3\0";
/// Largest level document read from an untrusted file.
pub const MAX_LEVEL_BYTES: i64 = 32 << 20;

/// Whether `header` starts like an SQLite database.
pub fn is_database(header: &[u8]) -> bool {
    header.starts_with(MAGIC)
}

/// A `file:` URI for `path`, so it can be opened with URI parameters.
pub fn file_uri(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let mut out = String::from(if text.starts_with('/') {
        "file://"
    } else {
        "file:///"
    });
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/:".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Checks `file` is a sound server file of a format this server reads and
/// returns its level document (uncompressed NBT), if it has one. Errors
/// are for people: they say what is wrong with the file.
pub fn inspect(file: &Path) -> Result<Option<Vec<u8>>, String> {
    let mut header = [0u8; 16];
    File::open(file)
        .and_then(|mut f| f.read_exact(&mut header))
        .map_err(|_| "the file is not a server file".to_owned())?;
    if !is_database(&header) {
        return Err("the file is not a server file".to_owned());
    }
    let sql = |err: rusqlite::Error| format!("the server file is unreadable: {err}");
    let conn = Connection::open_with_flags(
        format!("{}?immutable=1", file_uri(file)),
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(sql)?;
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)
        .map_err(sql)?;
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA, false)
        .map_err(sql)?;
    conn.pragma_update(None, "cell_size_check", true)
        .map_err(sql)?;
    let id: i32 = conn
        .query_row("PRAGMA application_id", [], |row| row.get(0))
        .map_err(sql)?;
    if id != APPLICATION_ID {
        return Err(format!(
            "the file is not a server file (application id {id:#010x})"
        ));
    }
    let version: i32 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(sql)?;
    if version != FORMAT_VERSION {
        return Err(format!(
            "the server file uses format {version}; this server reads format {FORMAT_VERSION}"
        ));
    }
    let check: String = conn
        .query_row("PRAGMA quick_check", [], |row| row.get(0))
        .map_err(sql)?;
    if check != "ok" {
        return Err(format!("the server file is damaged: {check}"));
    }
    let len: Option<i64> = conn
        .query_row(
            "SELECT length(data) FROM documents WHERE kind = 'level' AND key = ''",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql)?;
    let Some(len) = len else {
        return Ok(None);
    };
    if len > MAX_LEVEL_BYTES {
        return Err(format!("the level data is too large ({len} bytes)"));
    }
    let data: Vec<u8> = conn
        .query_row(
            "SELECT data FROM documents WHERE kind = 'level' AND key = ''",
            [],
            |row| row.get(0),
        )
        .map_err(sql)?;
    conn.close().map_err(|(_, err)| sql(err))?;
    Ok(Some(data))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn uris_escape_everything_but_plain_path_characters() {
        assert_eq!(
            file_uri(Path::new("/tmp/a b/w?x#1%.mcserver")),
            "file:///tmp/a%20b/w%3Fx%231%25.mcserver"
        );
        assert_eq!(
            file_uri(Path::new("C:\\worlds\\w.mcserver")),
            "file:///C:/worlds/w.mcserver"
        );
    }
}
