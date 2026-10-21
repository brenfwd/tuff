use rusqlite::Connection;
use tuff_nbt::{Compound, Compression, NamedTag, Tag, lz4};

use super::decode::{self, Found, MAX_DEPTH, MAX_NBT_BYTES};
use super::*;

fn raw(root: Compound) -> Vec<u8> {
    let mut out = Vec::new();
    NamedTag::new("", root).write(&mut out).unwrap();
    out
}

fn sector(id: u8, root: Compound) -> Vec<u8> {
    let raw = raw(root);
    let payload = match id {
        1 => Compression::Gzip.compress(raw).unwrap(),
        2 => Compression::Zlib.compress(raw).unwrap(),
        4 => lz4::compress_stream(&raw),
        _ => raw,
    };
    let mut blob = vec![id];
    blob.extend(payload);
    blob
}

fn sample() -> Compound {
    Compound::new()
        .with("b", Tag::Byte(-1))
        .with("l", Tag::Long(i64::MAX))
        .with("f", Tag::Float(0.5))
        .with("nan", Tag::Double(f64::NAN))
        .with("s", Tag::String("a\"b\n".into()))
        .with("la", Tag::LongArray(vec![1, -2]))
        .with(
            "list",
            Tag::List(vec![Tag::Compound(Compound::new().with("id", "x"))]),
        )
}

const SAMPLE_JSON: &str = r#"{"b":-1,"l":9223372036854775807,"f":0.5,"nan":"NaN","s":"a\"b\n","la":[1,-2],"list":[{"id":"x"}]}"#;

fn uuid_ints() -> Vec<i32> {
    vec![0x0123_4567, -0x7654_3211, 0x1122_3344, 0x5566_7788]
}

#[tuff_core::watchdog]
#[test]
fn json_is_the_same_for_every_storage_form() {
    let mut forms = vec![
        raw(sample()),
        Compression::Gzip.compress(raw(sample())).unwrap(),
        Compression::Zlib.compress(raw(sample())).unwrap(),
    ];
    forms.extend([1, 2, 3, 4].map(|id| sector(id, sample())));
    for blob in forms {
        let tag = decode::decode(&blob).unwrap();
        assert_eq!(decode::to_json(&tag).unwrap(), SAMPLE_JSON);
    }
}

#[tuff_core::watchdog]
#[test]
fn malformed_blobs_are_errors() {
    let good = sector(2, sample());
    let cases: Vec<Vec<u8>> = vec![
        Vec::new(),
        vec![2, 0x78, 0x9C, 1, 2, 3],
        good[..good.len() / 2].to_vec(),
        vec![3, 10, 0, 0, 1],
        vec![4, 1, 2, 3],
        vec![130, 0],
        b"not nbt at all".to_vec(),
    ];
    for blob in cases {
        assert!(decode::decode(&blob).is_err(), "{blob:?} decoded");
    }
}

#[tuff_core::watchdog]
#[test]
fn decompression_bombs_fail_instead_of_allocating() {
    let mut huge = raw(Compound::new().with("a", Tag::ByteArray(vec![0; MAX_NBT_BYTES])));
    huge.truncate(MAX_NBT_BYTES + 64);
    let bomb = Compression::Zlib.compress(huge.clone()).unwrap();
    assert!(bomb.len() < MAX_NBT_BYTES / 100);
    let mut blob = vec![2];
    blob.extend(&bomb);
    let err = decode::decode(&blob).unwrap_err();
    assert!(err.contains("size") || err.contains("large"), "{err}");
    assert!(decode::decode(&bomb).is_err());
    let mut plain = vec![3];
    plain.extend(&huge);
    assert!(decode::decode(&plain).is_err());
}

#[tuff_core::watchdog]
#[test]
fn deep_nesting_is_refused() {
    let mut tag = Tag::Compound(Compound::new());
    for _ in 0..MAX_DEPTH + 8 {
        tag = Tag::List(vec![tag]);
    }
    let mut bytes = Vec::new();
    NamedTag::new("", tag).write(&mut bytes).unwrap();
    assert!(decode::decode(&bytes).is_err());
}

#[tuff_core::watchdog]
#[test]
fn paths_reach_values_and_array_elements() {
    let tag = Tag::Compound(sample().with("UUID", Tag::IntArray(uuid_ints())));
    let get = |p| decode::get_path(&tag, p).unwrap();
    assert_eq!(get("b"), Some(Found::Tag(&Tag::Byte(-1))));
    assert_eq!(
        get("$.list[0].id"),
        Some(Found::Tag(&Tag::String("x".into())))
    );
    assert_eq!(get("list[-1].id"), get("list[0].id"));
    assert_eq!(get("la[1]"), Some(Found::Element(-2)));
    assert_eq!(get("la[2]"), None);
    assert_eq!(get("missing.x"), None);
    assert_eq!(get("b.x"), None);
    assert!(decode::get_path(&tag, "list[").is_err());
    assert_eq!(
        decode::uuid_text([0x0123_4567, -0x7654_3211, 0x1122_3344, 0x5566_7788]),
        "01234567-89ab-cdef-1122-334455667788"
    );
}

fn chunk(cx: i32, cz: i32) -> Compound {
    let stone = Compound::new().with("Name", "minecraft:stone");
    let chest = Compound::new().with("Name", "minecraft:chest").with(
        "Properties",
        Compound::new()
            .with("facing", "north")
            .with("type", "single"),
    );
    // 4 bits per entry, 16 per long; index of (1, 2, 3) local is
    // (2 << 8) | (3 << 4) | 1 = 561 -> long 35, slot 1.
    let mut data = vec![0i64; 256];
    data[35] = 1 << 4;
    let section = Compound::new().with("Y", Tag::Byte(-1)).with(
        "block_states",
        Compound::new()
            .with("palette", Tag::List(vec![stone.into(), chest.into()]))
            .with("data", Tag::LongArray(data)),
    );
    let air = Compound::new().with("Y", Tag::Byte(0)).with(
        "block_states",
        Compound::new().with(
            "palette",
            Tag::List(vec![Compound::new().with("Name", "minecraft:air").into()]),
        ),
    );
    let chest_entity = Compound::new()
        .with("id", "minecraft:chest")
        .with("x", cx * 16 + 1)
        .with("y", -14)
        .with("z", cz * 16 + 3)
        .with(
            "Items",
            Tag::List(vec![
                Compound::new()
                    .with("id", "minecraft:diamond")
                    .with("count", 3)
                    .with("Slot", Tag::Byte(0))
                    .into(),
            ]),
        );
    Compound::new()
        .with("xPos", cx)
        .with("zPos", cz)
        .with("sections", Tag::List(vec![section.into(), air.into()]))
        .with("block_entities", Tag::List(vec![chest_entity.into()]))
}

#[tuff_core::watchdog]
#[test]
fn block_at_reads_packed_palette_indexes() {
    let root = Tag::Compound(chunk(-1, 2));
    let at = |x, y, z| decode::block_at(&root, x, y, z).unwrap();
    assert_eq!(
        at(-15, -14, 35).as_deref(),
        Some("minecraft:chest[facing=north,type=single]")
    );
    assert_eq!(at(-16, -14, 35).as_deref(), Some("minecraft:stone"));
    assert_eq!(at(-16, 5, 35).as_deref(), Some("minecraft:air"));
    assert_eq!(at(-16, 40, 35), None);
    assert_eq!(at(0, -14, 35), None);
}

fn entity(id: &str, pos: [f64; 3]) -> Tag {
    Compound::new()
        .with("id", id)
        .with("UUID", Tag::IntArray(uuid_ints()))
        .with("Pos", Tag::List(pos.map(Tag::Double).to_vec()))
        .into()
}

fn world() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE documents(kind TEXT, key TEXT, data BLOB, PRIMARY KEY(kind, key));
         CREATE TABLE chunks(dimension TEXT, x INTEGER, z INTEGER, data BLOB);
         CREATE TABLE entities(dimension TEXT, x INTEGER, z INTEGER, data BLOB);
         CREATE TABLE pois(dimension TEXT, x INTEGER, z INTEGER, data BLOB);
         CREATE TABLE sqlar(name TEXT PRIMARY KEY, mode INT, mtime INT, sz INT, data BLOB);",
    )
    .unwrap();
    register(&conn).unwrap();
    let entities = Compound::new().with(
        "Entities",
        Tag::List(vec![
            entity("minecraft:villager", [1.5, 64.0, -3.5]),
            entity("minecraft:cow", [2.0, 65.0, -1.0]),
        ]),
    );
    conn.execute(
        "INSERT INTO entities VALUES ('minecraft:overworld', 0, -1, ?1)",
        [sector(2, entities)],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO entities VALUES ('minecraft:overworld', 5, 5, x'02deadbeef')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO chunks VALUES ('minecraft:overworld', -1, 2, ?1)",
        [sector(2, chunk(-1, 2))],
    )
    .unwrap();
    let uuid = "01234567-89ab-cdef-1122-334455667788";
    let player = Compound::new()
        .with("Dimension", "minecraft:the_nether")
        .with("Pos", Tag::List(vec![Tag::Double(1.0); 3]))
        .with("Health", Tag::Float(20.0))
        .with("XpLevel", 7);
    conn.execute(
        "INSERT INTO documents VALUES ('player', ?1, ?2)",
        rusqlite::params![uuid, raw(player)],
    )
    .unwrap();
    let cache = format!(r#"[{{"name":"Alex","uuid":"{uuid}","expiresOn":"x"}}]"#);
    let packed = Compression::Zlib
        .compress(cache.clone().into_bytes())
        .unwrap();
    conn.execute(
        "INSERT INTO sqlar VALUES ('usercache.json', 420, 0, ?1, ?2)",
        rusqlite::params![cache.len() as i64, packed],
    )
    .unwrap();
    let poi = Compound::new().with(
        "Sections",
        Compound::new().with(
            "4",
            Compound::new().with(
                "Records",
                Tag::List(vec![
                    Compound::new()
                        .with("type", "minecraft:home")
                        .with("pos", Tag::IntArray(vec![1, 70, 2]))
                        .with("free_tickets", 1)
                        .into(),
                ]),
            ),
        ),
    );
    conn.execute(
        "INSERT INTO pois VALUES ('minecraft:overworld', 0, 0, ?1)",
        [sector(2, poi)],
    )
    .unwrap();
    conn
}

fn query(conn: &Connection, sql: &str) -> Vec<String> {
    let mut stmt = conn.prepare(sql).unwrap();
    let n = stmt.column_count();
    stmt.query_map([], |row| {
        Ok((0..n)
            .map(|i| match row.get_ref(i).unwrap() {
                ValueRef::Null => "NULL".to_owned(),
                ValueRef::Integer(v) => v.to_string(),
                ValueRef::Real(v) => v.to_string(),
                ValueRef::Text(t) => String::from_utf8_lossy(t).into_owned(),
                ValueRef::Blob(_) => "<blob>".to_owned(),
            })
            .collect::<Vec<_>>()
            .join("|"))
    })
    .unwrap()
    .map(Result::unwrap)
    .collect()
}

#[tuff_core::watchdog]
#[test]
fn sql_functions_read_blobs() {
    let conn = world();
    assert_eq!(
        query(
            &conn,
            "SELECT nbt_get(data, 'Entities[0].id'), nbt_get(data, 'Entities[1].Pos[1]'),
                    nbt_get(data, 'Entities[0].UUID[3]'), nbt_get(data, 'nope')
             FROM entities WHERE x = 0"
        ),
        ["minecraft:villager|65|1432778632|NULL"]
    );
    assert_eq!(
        query(&conn, "SELECT block_at(data, -15, -14, 35) FROM chunks"),
        ["minecraft:chest[facing=north,type=single]"]
    );
    assert_eq!(
        query(&conn, "SELECT nbt_snbt(data) FROM documents")[0],
        r#"{Dimension:"minecraft:the_nether",Health:20.0f,Pos:[1.0d,1.0d,1.0d],XpLevel:7}"#
    );
    assert_eq!(
        query(
            &conn,
            "SELECT nbt_error(data) IS NOT NULL, nbt_try_json(data) FROM entities ORDER BY x"
        ),
        [
            "0|".to_owned() + &query(&conn, "SELECT nbt_json(data) FROM entities WHERE x = 0")[0],
            "1|NULL".to_owned()
        ]
    );
    assert!(
        conn.query_row("SELECT nbt_json(data) FROM entities WHERE x = 5", [], |r| {
            r.get::<_, String>(0)
        })
        .is_err()
    );
    assert_eq!(query(&conn, "SELECT nbt_json(NULL)"), ["NULL"]);
}

#[tuff_core::watchdog]
#[test]
fn views_answer_the_guide_queries() {
    let conn = world();
    assert_eq!(
        query(
            &conn,
            "SELECT dimension, cx, cz, uuid, x, y, z FROM entity_list WHERE type = 'minecraft:villager'"
        ),
        ["minecraft:overworld|0|-1|01234567-89ab-cdef-1122-334455667788|1.5|64|-3.5"]
    );
    assert_eq!(
        query(
            &conn,
            "SELECT b.x, b.y, b.z FROM block_entities b, json_each(b.nbt, '$.Items') i
             WHERE b.type = 'minecraft:chest'
               AND json_extract(i.value, '$.id') = 'minecraft:diamond'"
        ),
        ["-15|-14|35"]
    );
    assert_eq!(
        query(
            &conn,
            "SELECT name, dimension, x, health, xp_level FROM players"
        ),
        ["Alex|minecraft:the_nether|1|20|7"]
    );
    assert_eq!(
        query(
            &conn,
            "SELECT section_y, type, x, y, z, free_tickets FROM poi_records"
        ),
        ["4|minecraft:home|1|70|2|1"]
    );
}

#[tuff_core::watchdog]
#[test]
fn entity_index_is_opt_in_and_follows_writes() {
    let conn = world();
    update_entity_index(&conn, "minecraft:overworld", 0, -1, None).unwrap();
    assert!(!has_entity_index(&conn).unwrap());
    assert_eq!(query(&conn, "SELECT tuff_entity_index_rebuild()"), ["2"]);
    assert_eq!(
        query(&conn, "SELECT type, x FROM entity_index ORDER BY type"),
        ["minecraft:cow|2", "minecraft:villager|1.5"]
    );
    let blob = sector(
        2,
        Compound::new().with(
            "Entities",
            Tag::List(vec![entity("minecraft:pig", [9.0, 1.0, 9.0])]),
        ),
    );
    update_entity_index(&conn, "minecraft:overworld", 0, -1, Some(&blob)).unwrap();
    assert_eq!(
        query(&conn, "SELECT type FROM entity_index"),
        ["minecraft:pig"]
    );
    update_entity_index(&conn, "minecraft:overworld", 0, -1, None).unwrap();
    assert_eq!(query(&conn, "SELECT count(*) FROM entity_index"), ["0"]);
}
