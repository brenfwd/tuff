//! SQLite's archive format: files in a `sqlar` table, as the `sqlite3`
//! shell's `-A` mode reads and writes them (`sqlite3 <file> -A -x`
//! extracts every entry). `data` holds the zlib (RFC 1950) stream of the
//! content when that is smaller than the content, else the content itself;
//! `sz` is always the original size, so `length(data) = sz` means stored
//! as is. Directories have NULL data and symlinks `sz = -1`; neither is a
//! file here.
//!
//! Entries are untrusted: a read is capped at a caller-given size, checked
//! against the stored blob length before it is fetched, and decompression
//! stops one byte past the claimed size rather than trusting it.

use std::io::{self, Read, Write};
use std::time::{SystemTime, UNIX_EPOCH};

use flate2::Compression;
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use rusqlite::{Connection, OptionalExtension, params};

/// The table exactly as the `sqlite3` shell creates it.
pub const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS sqlar(
  name TEXT PRIMARY KEY,  -- name of the file
  mode INT,               -- access permissions
  mtime INT,              -- last modification time
  sz INT,                 -- original file size
  data BLOB               -- compressed content
)";
/// A regular file readable by everyone, writable by its owner.
pub const FILE_MODE: i64 = 0o100_644;

fn invalid(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

/// The bytes stored for `contents`: its zlib stream when that is smaller.
pub fn encode(contents: &[u8]) -> Vec<u8> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    let compressed = encoder
        .write_all(contents)
        .and_then(|()| encoder.finish())
        .ok();
    match compressed {
        Some(compressed) if compressed.len() < contents.len() => compressed,
        _ => contents.to_vec(),
    }
}

/// Recovers a file's contents from its `sz` and `data`, refusing anything
/// over `max` bytes.
pub fn decode(name: &str, sz: i64, data: &[u8], max: usize) -> io::Result<Vec<u8>> {
    let size = usize::try_from(sz)
        .map_err(|_| invalid(format!("archive entry {name:?} is not a regular file")))?;
    if size > max {
        return Err(invalid(format!(
            "archive entry {name:?} claims {size} bytes, over the {max} byte limit"
        )));
    }
    if data.len() == size {
        return Ok(data.to_vec());
    }
    let mut out = Vec::new();
    ZlibDecoder::new(data)
        .take(size as u64 + 1)
        .read_to_end(&mut out)
        .map_err(|err| invalid(format!("archive entry {name:?} is corrupt: {err}")))?;
    if out.len() != size {
        return Err(invalid(format!(
            "archive entry {name:?} holds {}{} bytes, not the {size} it claims",
            out.len().min(size + 1),
            if out.len() > size { "+" } else { "" },
        )));
    }
    Ok(out)
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

pub fn write(conn: &Connection, name: &str, contents: &[u8]) -> rusqlite::Result<()> {
    write_encoded(conn, name, contents.len(), &encode(contents))
}

/// Stores `data`, already [`encode`]d from `size` bytes of content, so the
/// compression can run before the database thread is involved.
pub fn write_encoded(
    conn: &Connection,
    name: &str,
    size: usize,
    data: &[u8],
) -> rusqlite::Result<()> {
    conn.prepare_cached(
        "INSERT OR REPLACE INTO sqlar(name, mode, mtime, sz, data) VALUES (?1, ?2, ?3, ?4, ?5)",
    )?
    .execute(params![name, FILE_MODE, unix_now(), size as i64, data])
    .map(drop)
}

pub fn remove(conn: &Connection, name: &str) -> rusqlite::Result<()> {
    conn.prepare_cached("DELETE FROM sqlar WHERE name = ?1")?
        .execute(params![name])
        .map(drop)
}

/// A file's contents, or `None` if the archive has no such entry.
pub fn read(conn: &Connection, name: &str, max: usize) -> io::Result<Option<Vec<u8>>> {
    let header = conn
        .prepare_cached("SELECT sz, length(data), data IS NULL FROM sqlar WHERE name = ?1")
        .and_then(|mut statement| {
            statement
                .query_row(params![name], |row| {
                    Ok((
                        row.get::<_, Option<i64>>(0)?,
                        row.get::<_, Option<i64>>(1)?,
                        row.get::<_, bool>(2)?,
                    ))
                })
                .optional()
        })
        .map_err(io::Error::other)?;
    let Some((sz, stored, is_null)) = header else {
        return Ok(None);
    };
    let (Some(sz), Some(stored), false) = (sz, stored, is_null) else {
        return Err(invalid(format!(
            "archive entry {name:?} is not a regular file"
        )));
    };
    if usize::try_from(stored).map_or(true, |stored| stored > max) {
        return Err(invalid(format!(
            "archive entry {name:?} stores {stored} bytes, over the {max} byte limit"
        )));
    }
    let data: Vec<u8> = conn
        .prepare_cached("SELECT data FROM sqlar WHERE name = ?1")
        .and_then(|mut statement| statement.query_row(params![name], |row| row.get(0)))
        .map_err(io::Error::other)?;
    decode(name, sz, &data, max).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMIT: usize = 1 << 16;

    fn archive() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        conn
    }

    fn zlib(contents: &[u8]) -> Vec<u8> {
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::best());
        encoder.write_all(contents).unwrap();
        encoder.finish().unwrap()
    }

    /// What `sqlite3 -A -x` does with a row: `sqlar_uncompress(data, sz)`
    /// returns `data` when `sz = length(data)`, else inflates it to `sz`.
    fn shell_extract(conn: &Connection, name: &str) -> (i64, i64, Vec<u8>) {
        let (mode, mtime, sz, data): (i64, i64, i64, Vec<u8>) = conn
            .query_row(
                "SELECT mode, mtime, sz, data FROM sqlar WHERE name = ?1",
                params![name],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        let contents = if sz as usize == data.len() {
            data
        } else {
            let mut out = Vec::new();
            ZlibDecoder::new(&data[..]).read_to_end(&mut out).unwrap();
            assert_eq!(out.len() as i64, sz);
            out
        };
        (mode, mtime, contents)
    }

    #[tuff_core::watchdog]
    #[test]
    fn table_matches_the_shells_archive_layout() {
        let conn = archive();
        let columns: Vec<(String, String, bool)> = conn
            .prepare("SELECT name, type, pk FROM pragma_table_info('sqlar') ORDER BY cid")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        let expected = [
            ("name", "TEXT", true),
            ("mode", "INT", false),
            ("mtime", "INT", false),
            ("sz", "INT", false),
            ("data", "BLOB", false),
        ];
        assert_eq!(
            columns,
            expected.map(|(name, kind, pk)| (name.to_owned(), kind.to_owned(), pk))
        );
        conn.execute_batch(SCHEMA).unwrap();
    }

    #[tuff_core::watchdog]
    #[test]
    fn entries_extract_like_the_shell_does() {
        let conn = archive();
        let text = b"hello hello hello hello hello hello hello hello".repeat(20);
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let random: Vec<u8> = (0..300)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state >> 24) as u8
            })
            .collect();
        write(&conn, "text.txt", &text).unwrap();
        write(&conn, "random.bin", &random).unwrap();
        write(&conn, "empty", b"").unwrap();

        let stored: Vec<u8> = conn
            .query_row(
                "SELECT data FROM sqlar WHERE name = 'text.txt'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(stored.len() < text.len(), "compressible content is zlib");
        assert_eq!(stored[0] & 0x0f, 8, "a zlib header, not raw deflate");
        let stored: Vec<u8> = conn
            .query_row(
                "SELECT data FROM sqlar WHERE name = 'random.bin'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, random, "incompressible content is stored as is");

        for (name, contents) in [
            ("text.txt", &text[..]),
            ("random.bin", &random[..]),
            ("empty", &[][..]),
        ] {
            let (mode, mtime, extracted) = shell_extract(&conn, name);
            assert_eq!(mode, FILE_MODE);
            assert!(mtime > 1_600_000_000);
            assert_eq!(extracted, contents);
            assert_eq!(read(&conn, name, LIMIT).unwrap().unwrap(), contents);
        }
        write(&conn, "text.txt", b"replaced").unwrap();
        assert_eq!(
            read(&conn, "text.txt", LIMIT).unwrap().unwrap(),
            b"replaced"
        );
        remove(&conn, "text.txt").unwrap();
        assert!(read(&conn, "text.txt", LIMIT).unwrap().is_none());
    }

    #[tuff_core::watchdog]
    #[test]
    fn entries_written_by_the_shell_are_read() {
        let conn = archive();
        let contents = b"abcabcabcabcabcabcabcabcabcabcabcabc".to_vec();
        conn.execute(
            "INSERT INTO sqlar VALUES ('dir', 16877, 0, 0, NULL), ('link', 41471, 0, -1, 'target'), \
             ('file', 33188, 0, ?1, ?2)",
            params![contents.len() as i64, zlib(&contents)],
        )
        .unwrap();
        assert_eq!(read(&conn, "file", LIMIT).unwrap().unwrap(), contents);
        assert!(read(&conn, "dir", LIMIT).is_err());
        assert!(read(&conn, "link", LIMIT).is_err());
    }

    #[tuff_core::watchdog]
    #[test]
    fn oversized_claims_are_refused_before_allocating() {
        let bomb = zlib(&vec![0u8; 1 << 20]);
        assert!(bomb.len() < 2048);
        let err = decode("bomb", 1 << 20, &bomb, LIMIT).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("limit"), "{err}");
        assert!(decode("huge", i64::MAX, &bomb, LIMIT).is_err());
        assert!(decode("negative", -1, b"x", LIMIT).is_err());

        let conn = archive();
        conn.execute(
            "INSERT INTO sqlar VALUES ('big', 33188, 0, ?1, zeroblob(?1))",
            params![LIMIT as i64 + 1],
        )
        .unwrap();
        let err = read(&conn, "big", LIMIT).unwrap_err();
        assert!(err.to_string().contains("stores"), "{err}");
    }

    #[tuff_core::watchdog]
    #[test]
    fn understated_sizes_do_not_inflate_past_the_claim() {
        let bomb = zlib(&vec![7u8; 1 << 20]);
        let err = decode("bomb", 1000, &bomb, LIMIT).unwrap_err();
        assert!(err.to_string().contains("1001+ bytes"), "{err}");
        let conn = archive();
        conn.execute(
            "INSERT INTO sqlar VALUES ('bomb', 33188, 0, 1000, ?1)",
            params![bomb],
        )
        .unwrap();
        assert!(read(&conn, "bomb", LIMIT).is_err());
    }

    #[tuff_core::watchdog]
    #[test]
    fn truncated_and_malformed_streams_fail() {
        let contents = b"some text some text some text some text some text".repeat(8);
        let stream = zlib(&contents);
        let size = contents.len() as i64;
        assert_eq!(decode("ok", size, &stream, LIMIT).unwrap(), contents);
        for cut in [1, stream.len() / 2, stream.len() - 1] {
            assert!(decode("cut", size, &stream[..cut], LIMIT).is_err(), "{cut}");
        }
        let mut corrupt = stream.clone();
        corrupt[0] ^= 0xff;
        assert!(decode("corrupt", size, &corrupt, LIMIT).is_err());
        assert!(decode("garbage", 10, b"not zlib at all", LIMIT).is_err());
        assert!(decode("short", size + 5, &stream, LIMIT).is_err());
    }
}
