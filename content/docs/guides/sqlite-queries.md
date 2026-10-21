---
title: Querying a server file with SQL
description: "A tuff server file (world.mcserver) is an SQLite database. Chunks, entity chunks and points of interest are stored as (dimension, x, z, data) rows in..."
group: Guides
source: docs/guides/sqlite-queries.md
---

A tuff server file (`world.mcserver`) is an SQLite database. Chunks, entity
chunks and points of interest are stored as `(dimension, x, z, data)` rows in
`chunks`, `entities` and `pois`, where `data` is a vanilla region sector: one
compression id byte followed by zlib NBT. `documents(kind, key, data)` holds
uncompressed NBT for the level (`level`), saved data, players (`player`,
keyed by hyphenated UUID), statistics and advancements. The blobs are the
canonical data; everything below only reads them.

A running server may hold the file locked. Query a copy, a
backup, or the file of a stopped server.

## Loading the functions

The server registers the functions and views on its own connections. For the
`sqlite3` shell, download `tuff-sqlite-ext-<platform>.tar.gz` from a release
(or build it with `cargo build --release --manifest-path
crates/tuff-sqlite-ext/Cargo.toml`, which writes
`crates/tuff-sqlite-ext/target/release/libtuff_sqlite.so`, `.dylib` or
`tuff_sqlite.dll`; `scripts/check-sqlite-ext.sh` builds it and checks it loads) and load it:

```
$ sqlite3 world.mcserver
sqlite> .load ./libtuff_sqlite
sqlite> SELECT count(*) FROM entity_list;
```

On Windows: `.load ./tuff_sqlite`. The `sqlite3` binary must allow
extensions (the stock shell does; Python's `sqlite3` needs
`conn.enable_load_extension(True)` first). Loading creates the views as
temporary views of that connection; nothing is written to the file.

## Functions

| Function | Result |
| --- | --- |
| `nbt_json(blob)` | The NBT as JSON. Compounds are objects; lists, byte, int and long arrays are arrays; every integer (longs too, exact to 64 bits) is a plain number; NaN and infinities are the strings `"NaN"`, `"Infinity"`, `"-Infinity"`. Errors when the blob does not decode. |
| `nbt_try_json(blob)` | Like `nbt_json`, but NULL when the blob does not decode. The views use it. |
| `nbt_error(blob)` | Why a blob does not decode, or NULL. |
| `nbt_snbt(blob)` | SNBT text, as `/data get` prints it. |
| `nbt_get(blob, path)` | One value: numbers as INTEGER or REAL, strings as TEXT, compounds, lists and arrays as JSON text, NULL when absent. Paths look like `Pos[1]`, `Inventory[0].id`, `"key with spaces".x`; negative indexes count from the end. |
| `block_at(chunk_blob, x, y, z)` | The block state at a world position, `minecraft:chest[facing=north,type=single]`, NULL outside the chunk or its stored sections. |
| `nbt_uuid(json)` | Hyphenated UUID from a JSON `[a,b,c,d]` int array. |
| `sqlar_text(data, sz)` | An `sqlar` row's contents as text (inflating it when stored compressed). |
| `tuff_entity_index_rebuild()` | Creates and refills `entity_index`; returns the number of entities. |

Every function accepts any of the stored forms: a region sector (gzip, zlib,
uncompressed or LZ4), or a whole NBT file that is gzip, zlib or
uncompressed. Decoding refuses more than 32 MiB of NBT, nesting deeper than
512, and output text larger than 128 MiB.

## Views

| View | Columns |
| --- | --- |
| `entity_list` | `dimension, cx, cz, uuid, type, x, y, z, custom_name, nbt` |
| `block_entities` | `dimension, cx, cz, type, x, y, z, custom_name, nbt` |
| `players` | `uuid, name, dimension, x, y, z, health, xp_level, xp_total, nbt` |
| `poi_records` | `dimension, cx, cz, section_y, type, x, y, z, free_tickets, nbt` |

`nbt` is the JSON of one entity, block entity, player or record, for
`json_extract`. `players.name` comes from `usercache.json` when the file
holds one. The views decode every blob they scan, so filter on the chunk
columns where you can.

## Examples

All villagers:

```sql
SELECT dimension, uuid, x, y, z, json_extract(nbt, '$.VillagerData.profession')
FROM entity_list WHERE type = 'minecraft:villager';
```

Where entity UUID X is:

```sql
SELECT dimension, x, y, z FROM entity_list
WHERE uuid = '01234567-89ab-cdef-1122-334455667788';
```

Players by last position:

```sql
SELECT name, uuid, dimension, round(x), round(y), round(z)
FROM players ORDER BY dimension, x, z;
```

Chests holding diamonds:

```sql
SELECT b.dimension, b.x, b.y, b.z, json_extract(i.value, '$.count') AS diamonds
FROM block_entities b, json_each(b.nbt, '$.Items') i
WHERE b.type = 'minecraft:chest'
  AND json_extract(i.value, '$.id') = 'minecraft:diamond';
```

The block at a position (the chunk holding it is `x >> 4, z >> 4`):

```sql
SELECT block_at(data, 100, 64, -200) FROM chunks
WHERE dimension = 'minecraft:overworld' AND x = 100 >> 4 AND z = -200 >> 4;
```

Villager homes:

```sql
SELECT dimension, x, y, z FROM poi_records WHERE type = 'minecraft:home';
```

Chunks whose entity data does not decode:

```sql
SELECT dimension, x, z, nbt_error(data) FROM entities
WHERE nbt_error(data) IS NOT NULL;
```

## The entity index

`entity_index(dimension, cx, cz, uuid, type, x, y, z, custom_name)` is an
optional table that makes entity lookups by UUID or type instant instead of
a scan of every entity chunk. It is derived data and never authoritative.
It is off by default. To turn it on, create the table:

```sql
SELECT tuff_entity_index_rebuild();
```

and set `tuff.storage.sqlite.entityIndex` to `true` (read at startup). The
server then keeps the table current on every entity chunk write, at about
0.1 ms per write. With the setting off the table goes stale, so rebuild it
before querying it. Run the same function to rebuild it at any time, or
`DROP TABLE entity_index;` to remove it.

## The panel's SQL console

The web panel has an **SQL** page for worlds on the SQLite backend (a
region-file world shows "SQLite backend only"). It needs the permission
`tuff.web.sql`, which owner-level operators hold by default. It has the
functions and views above, autocomplete for tables, columns and functions,
`Ctrl+Enter` (`Cmd+Enter`) to run, paging, a JSON viewer for `nbt_json`
output, query plans, CSV and JSON export, history and per-user saved
queries.

Queries run on their own read-only connection, never on the server's
database thread, so a slow query does not stall the world. With the
default normal locking that connection reads the live file (WAL gives it
a consistent view); with `--sqlite_locking exclusive` nothing else can
open the file, so the console queries a snapshot copy of it that is at
most a few seconds old. One statement per user runs at a time, and a
statement's pages stream from one open cursor, so paging sees one
consistent view. The file must be on a local disk (WAL needs shared
memory), and a long read keeps the write-ahead log from being
checkpointed, so the log grows until the read ends or the cursor idles
out after two minutes.

- Read-only: `query_only` is on, and only reads, reporting pragmas and the
  functions above (except `tuff_entity_index_rebuild()`) are allowed. No
  temporary tables, triggers, `ATTACH` or extensions.
- A statement stops after `tuff.web.sql.timeoutSeconds` (default 60), or
  when you press Cancel; a page holds at most 1000 rows or
  about 4 MiB. Exports hold at most 100000 rows.
- Every statement, with its full SQL and outcome, goes to the panel audit
  log.

### Writes

Writes are off unless all of these hold, and the server checks each one
on every request:

- the user has `tuff.web.sql.write` (owners only by default);
- the setting `tuff.web.sql.allowWrite` is `true` (default `false`);
- read-write mode is on in that browser tab. It is off on every page load,
  needs a confirmation to turn on, and shows a red banner while it is on.

A write first runs as a preview in a transaction that is rolled back,
showing how many rows it would change. **Commit** runs it again and
commits only if the same number of rows change; **Rollback** discards it.
A write that runs past the time limit is rolled back.

The rows of `chunks`, `entities` and `pois` can be edited while the
server runs. The preview reports which chunks the statement touches and
refuses if the server has any of them loaded, since a later save would
overwrite the edit; unload them first (move players away, remove forced
chunks). A commit pauses saving, checks again, commits, and resumes, and
warns if a touched chunk loaded in between. Their structure (`DROP`,
`ALTER`, indexes) stays off limits.

The server keeps players, settings and lists in memory and writes them
back later, so the console still refuses writes to `documents`, `meta`,
`properties`, `tuff_settings`, `ops`, `whitelist`, `banned_players`,
`banned_ips`, `user_cache`, `plugin_grants` and `sqlar`, and refuses
triggers. It can change `entity_index` and tables you create. To edit
those, stop the server and use the `sqlite3` shell.
