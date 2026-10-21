//! SQL functions and views for reading the NBT blobs of a tuff server file
//! (`world.mcserver`). The same registration runs on the server's own
//! connections and in the loadable extension (`crates/tuff-sqlite-ext`).
//!
//! Functions:
//! - `nbt_json(blob)`: the NBT as JSON (see [`decode::to_json`]); errors on
//!   a blob that does not decode. `nbt_try_json(blob)` returns NULL then.
//! - `nbt_snbt(blob)`: the NBT as SNBT text.
//! - `nbt_get(blob, path)`: one value; numbers as INTEGER or REAL, strings
//!   as TEXT, lists, compounds and arrays as JSON text, NULL when absent.
//! - `nbt_error(blob)`: why a blob does not decode, NULL when it does.
//! - `block_at(chunk_blob, x, y, z)`: the block state at a world position.
//! - `nbt_uuid(json)`: hyphenated UUID text from a JSON `[a,b,c,d]` int array.
//! - `sqlar_text(data, sz)`: the contents of an `sqlar` row as text.
//! - `tuff_entity_index_rebuild()`: creates and refills `entity_index`.
//!
//! Temporary views: `entity_list`, `block_entities`, `players`, `poi_records`.

pub mod decode;

use std::io::Read;

use rusqlite::functions::{Context, FunctionFlags};
use rusqlite::types::{Value, ValueRef};
use rusqlite::{Connection, Error, params};
use tuff_nbt::Tag;

use decode::Found;

fn user_error(message: String) -> Error {
    Error::UserFunctionError(message.into())
}

fn blob_arg<'a>(ctx: &'a Context<'_>, i: usize) -> rusqlite::Result<Option<&'a [u8]>> {
    match ctx.get_raw(i) {
        ValueRef::Null => Ok(None),
        ValueRef::Blob(b) | ValueRef::Text(b) => Ok(Some(b)),
        _ => Err(user_error(format!("argument {} must be a blob", i + 1))),
    }
}

fn decoded(ctx: &Context<'_>, i: usize) -> rusqlite::Result<Option<Tag>> {
    blob_arg(ctx, i)?
        .map(|blob| decode::decode(blob).map_err(user_error))
        .transpose()
}

fn tag_value(tag: &Tag) -> rusqlite::Result<Value> {
    Ok(match tag {
        Tag::Byte(v) => Value::Integer((*v).into()),
        Tag::Short(v) => Value::Integer((*v).into()),
        Tag::Int(v) => Value::Integer((*v).into()),
        Tag::Long(v) => Value::Integer(*v),
        Tag::Float(v) if v.is_finite() => Value::Real((*v).into()),
        Tag::Double(v) if v.is_finite() => Value::Real(*v),
        Tag::String(s) => Value::Text(s.clone()),
        other => Value::Text(decode::to_json(other).map_err(user_error)?),
    })
}

/// Largest `sqlar` entry `sqlar_text` inflates.
const MAX_SQLAR_TEXT: usize = 64 << 20;

fn sqlar_text(data: &[u8], size: i64) -> Result<String, String> {
    let size = usize::try_from(size).map_err(|_| "negative size")?;
    if size > MAX_SQLAR_TEXT {
        return Err(format!("entry is larger than {MAX_SQLAR_TEXT} bytes"));
    }
    let bytes = if data.len() == size {
        data.to_vec()
    } else {
        let mut out = Vec::new();
        flate2::read::ZlibDecoder::new(data)
            .take(size as u64 + 1)
            .read_to_end(&mut out)
            .map_err(|err| err.to_string())?;
        if out.len() != size {
            return Err("entry does not inflate to its stored size".into());
        }
        out
    };
    String::from_utf8(bytes).map_err(|_| "entry is not UTF-8 text".into())
}

/// Registers the functions and creates the temporary views on `conn`.
pub fn register(conn: &Connection) -> rusqlite::Result<()> {
    register_functions(conn)?;
    create_views(conn)
}

pub fn register_functions(conn: &Connection) -> rusqlite::Result<()> {
    let pure = FunctionFlags::SQLITE_UTF8
        | FunctionFlags::SQLITE_DETERMINISTIC
        | FunctionFlags::SQLITE_INNOCUOUS;
    conn.create_scalar_function(c"nbt_json", 1, pure, |ctx| {
        decoded(ctx, 0)?
            .map(|tag| decode::to_json(&tag).map_err(user_error))
            .transpose()
    })?;
    conn.create_scalar_function(c"nbt_try_json", 1, pure, |ctx| {
        Ok(blob_arg(ctx, 0)?
            .and_then(|blob| decode::decode(blob).ok())
            .and_then(|tag| decode::to_json(&tag).ok()))
    })?;
    conn.create_scalar_function(c"nbt_error", 1, pure, |ctx| {
        Ok(blob_arg(ctx, 0)?.and_then(|blob| {
            decode::decode(blob)
                .and_then(|tag| decode::to_json(&tag).map(drop))
                .err()
        }))
    })?;
    conn.create_scalar_function(c"nbt_snbt", 1, pure, |ctx| {
        decoded(ctx, 0)?
            .map(|tag| decode::to_snbt(&tag).map_err(user_error))
            .transpose()
    })?;
    conn.create_scalar_function(c"nbt_get", 2, pure, |ctx| {
        let Some(tag) = decoded(ctx, 0)? else {
            return Ok(Value::Null);
        };
        let path: String = ctx.get(1)?;
        match decode::get_path(&tag, &path).map_err(user_error)? {
            None => Ok(Value::Null),
            Some(Found::Element(v)) => Ok(Value::Integer(v)),
            Some(Found::Tag(found)) => tag_value(found),
        }
    })?;
    conn.create_scalar_function(c"block_at", 4, pure, |ctx| {
        let Some(tag) = decoded(ctx, 0)? else {
            return Ok(None);
        };
        let x: i64 = ctx.get(1)?;
        let y: i64 = ctx.get(2)?;
        let z: i64 = ctx.get(3)?;
        decode::block_at(&tag, x, y, z).map_err(user_error)
    })?;
    conn.create_scalar_function(c"nbt_uuid", 1, pure, |ctx| {
        Ok(match ctx.get_raw(0) {
            ValueRef::Text(t) => std::str::from_utf8(t).ok().and_then(decode::uuid_from_json),
            _ => None,
        })
    })?;
    conn.create_scalar_function(c"sqlar_text", 2, pure, |ctx| {
        let Some(data) = blob_arg(ctx, 0)? else {
            return Ok(None);
        };
        let size: i64 = ctx.get(1)?;
        sqlar_text(data, size).map(Some).map_err(user_error)
    })?;
    conn.create_scalar_function(
        c"tuff_entity_index_rebuild",
        0,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DIRECTONLY,
        |ctx| {
            // SAFETY: the connection is only used for the duration of this
            // call, on the thread SQLite invoked the function from.
            let conn = unsafe { ctx.get_connection()? };
            rebuild_entity_index(&conn)
        },
    )?;
    Ok(())
}

const VIEWS: &str = "
CREATE TEMP VIEW IF NOT EXISTS entity_list AS
SELECT e.dimension AS dimension, e.x AS cx, e.z AS cz,
       nbt_uuid(json_extract(j.value, '$.UUID')) AS uuid,
       json_extract(j.value, '$.id') AS type,
       json_extract(j.value, '$.Pos[0]') AS x,
       json_extract(j.value, '$.Pos[1]') AS y,
       json_extract(j.value, '$.Pos[2]') AS z,
       json_extract(j.value, '$.CustomName') AS custom_name,
       j.value AS nbt
FROM main.entities e, json_each(nbt_try_json(e.data), '$.Entities') j;

CREATE TEMP VIEW IF NOT EXISTS block_entities AS
SELECT c.dimension AS dimension, c.x AS cx, c.z AS cz,
       json_extract(j.value, '$.id') AS type,
       json_extract(j.value, '$.x') AS x,
       json_extract(j.value, '$.y') AS y,
       json_extract(j.value, '$.z') AS z,
       json_extract(j.value, '$.CustomName') AS custom_name,
       j.value AS nbt
FROM main.chunks c, json_each(nbt_try_json(c.data), '$.block_entities') j;

CREATE TEMP VIEW IF NOT EXISTS players AS
SELECT p.uuid AS uuid,
       (SELECT json_extract(u.value, '$.name')
          FROM main.sqlar s, json_each(sqlar_text(s.data, s.sz)) u
         WHERE s.name = 'usercache.json'
           AND json_extract(u.value, '$.uuid') = p.uuid
         LIMIT 1) AS name,
       json_extract(p.j, '$.Dimension') AS dimension,
       json_extract(p.j, '$.Pos[0]') AS x,
       json_extract(p.j, '$.Pos[1]') AS y,
       json_extract(p.j, '$.Pos[2]') AS z,
       json_extract(p.j, '$.Health') AS health,
       json_extract(p.j, '$.XpLevel') AS xp_level,
       json_extract(p.j, '$.XpTotal') AS xp_total,
       p.j AS nbt
FROM (SELECT key AS uuid, nbt_try_json(data) AS j
        FROM main.documents WHERE kind = 'player') p;

CREATE TEMP VIEW IF NOT EXISTS poi_records AS
SELECT p.dimension AS dimension, p.x AS cx, p.z AS cz,
       CAST(s.key AS INTEGER) AS section_y,
       json_extract(r.value, '$.type') AS type,
       json_extract(r.value, '$.pos[0]') AS x,
       json_extract(r.value, '$.pos[1]') AS y,
       json_extract(r.value, '$.pos[2]') AS z,
       json_extract(r.value, '$.free_tickets') AS free_tickets,
       r.value AS nbt
FROM main.pois p, json_each(nbt_try_json(p.data), '$.Sections') s,
     json_each(s.value, '$.Records') r;
";

/// Creates the temporary views. They read the `main` schema's tables, so a
/// file without them still gets the views; querying one then fails.
pub fn create_views(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(VIEWS)
}

const ENTITY_INDEX_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS entity_index(
    dimension TEXT NOT NULL,
    cx INTEGER NOT NULL,
    cz INTEGER NOT NULL,
    uuid TEXT,
    type TEXT,
    x REAL,
    y REAL,
    z REAL,
    custom_name TEXT
);
CREATE INDEX IF NOT EXISTS entity_index_chunk ON entity_index(dimension, cx, cz);
CREATE INDEX IF NOT EXISTS entity_index_uuid ON entity_index(uuid);
CREATE INDEX IF NOT EXISTS entity_index_type ON entity_index(type);
";

/// One entity as the derived index holds it.
#[derive(Clone, Debug, PartialEq)]
pub struct IndexedEntity {
    pub uuid: Option<String>,
    pub kind: Option<String>,
    pub pos: Option<[f64; 3]>,
    pub custom_name: Option<String>,
}

/// The entities of one entity chunk blob, for the derived index.
pub fn indexed_entities(blob: &[u8]) -> Result<Vec<IndexedEntity>, String> {
    let Tag::Compound(root) = decode::decode(blob)? else {
        return Err("entity chunk root is not a compound".into());
    };
    let Some(Tag::List(entities)) = root.get("Entities") else {
        return Ok(Vec::new());
    };
    Ok(entities
        .iter()
        .filter_map(|e| match e {
            Tag::Compound(e) => Some(e),
            _ => None,
        })
        .map(|e| IndexedEntity {
            uuid: match e.get("UUID") {
                Some(Tag::IntArray(v)) if v.len() == 4 => {
                    Some(decode::uuid_text([v[0], v[1], v[2], v[3]]))
                }
                _ => None,
            },
            kind: match e.get("id") {
                Some(Tag::String(s)) => Some(s.clone()),
                _ => None,
            },
            pos: match e.get("Pos") {
                Some(Tag::List(p)) if p.len() == 3 => {
                    let mut out = [0.0; 3];
                    p.iter()
                        .zip(&mut out)
                        .all(|(v, slot)| match v {
                            Tag::Double(d) => {
                                *slot = *d;
                                true
                            }
                            _ => false,
                        })
                        .then_some(out)
                }
                _ => None,
            },
            custom_name: e.get("CustomName").map(|name| match name {
                Tag::String(s) => s.clone(),
                other => decode::to_json(other).unwrap_or_default(),
            }),
        })
        .collect())
}

/// Whether the file has opted in to the derived entity index (by having
/// the `entity_index` table).
pub fn has_entity_index(conn: &Connection) -> rusqlite::Result<bool> {
    conn.prepare_cached(
        "SELECT 1 FROM main.sqlite_schema WHERE type = 'table' AND name = 'entity_index'",
    )?
    .exists([])
}

fn insert_entities(
    conn: &Connection,
    dimension: &str,
    cx: i32,
    cz: i32,
    entities: &[IndexedEntity],
) -> rusqlite::Result<()> {
    let mut insert = conn.prepare_cached(
        "INSERT INTO main.entity_index(dimension, cx, cz, uuid, type, x, y, z, custom_name) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
    )?;
    for e in entities {
        let [x, y, z] = e.pos.map_or([None; 3], |p| p.map(Some));
        insert.execute(params![
            dimension,
            cx,
            cz,
            e.uuid,
            e.kind,
            x,
            y,
            z,
            e.custom_name
        ])?;
    }
    Ok(())
}

/// Brings the derived index up to date after the entity chunk at
/// `(cx, cz)` was written (`Some`) or removed (`None`). Does nothing in a
/// file without the index. A blob that does not decode leaves the chunk
/// unindexed: the index is never authoritative.
pub fn update_entity_index(
    conn: &Connection,
    dimension: &str,
    cx: i32,
    cz: i32,
    blob: Option<&[u8]>,
) -> rusqlite::Result<()> {
    if !has_entity_index(conn)? {
        return Ok(());
    }
    conn.prepare_cached(
        "DELETE FROM main.entity_index WHERE dimension = ?1 AND cx = ?2 AND cz = ?3",
    )?
    .execute(params![dimension, cx, cz])?;
    if let Some(entities) = blob.and_then(|b| indexed_entities(b).ok()) {
        insert_entities(conn, dimension, cx, cz, &entities)?;
    }
    Ok(())
}

/// Creates `entity_index` if needed and refills it from every entity
/// chunk. Returns the number of entities indexed.
pub fn rebuild_entity_index(conn: &Connection) -> rusqlite::Result<i64> {
    conn.execute_batch("SAVEPOINT tuff_entity_index_rebuild")?;
    let result = (|| {
        conn.execute_batch(ENTITY_INDEX_SCHEMA)?;
        conn.execute("DELETE FROM main.entity_index", [])?;
        let mut count = 0i64;
        let mut rows = conn.prepare("SELECT dimension, x, z, data FROM main.entities")?;
        let mut rows = rows.query([])?;
        while let Some(row) = rows.next()? {
            let dimension: String = row.get(0)?;
            let blob = row.get_ref(3)?.as_blob()?;
            if let Ok(entities) = indexed_entities(blob) {
                insert_entities(conn, &dimension, row.get(1)?, row.get(2)?, &entities)?;
                count += entities.len() as i64;
            }
        }
        Ok(count)
    })();
    match result {
        Ok(count) => {
            conn.execute_batch("RELEASE tuff_entity_index_rebuild")?;
            Ok(count)
        }
        Err(err) => {
            let _ = conn.execute_batch(
                "ROLLBACK TO tuff_entity_index_rebuild; RELEASE tuff_entity_index_rebuild",
            );
            Err(err)
        }
    }
}

#[cfg(test)]
mod tests;
