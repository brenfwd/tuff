#!/usr/bin/env bash
# Builds the loadable SQLite extension (crates/tuff-sqlite-ext) and loads it into
# Python's sqlite3, a SQLite library other than the server's bundled one, to
# check the entry point, a function and the views.
set -euo pipefail
cd "$(dirname "$0")/.."
target_dir=${CARGO_TARGET_DIR:-target}/sqlite-ext
CARGO_TARGET_DIR=$target_dir cargo build --quiet --manifest-path crates/tuff-sqlite-ext/Cargo.toml
python3 -I - "$target_dir/debug/libtuff_sqlite" <<'PY'
import sqlite3, sys, zlib
c = sqlite3.connect(":memory:")
c.enable_load_extension(True)
c.load_extension(sys.argv[1])
nbt = bytes([10, 0, 0, 8, 0, 2]) + b"id" + bytes([0, 3]) + b"cow" + bytes([0])
row = c.execute("SELECT nbt_json(?), nbt_get(?, 'id')", (b"\x02" + zlib.compress(nbt), nbt)).fetchone()
assert row == ('{"id":"cow"}', "cow"), row
views = {r[0] for r in c.execute("SELECT name FROM sqlite_temp_master WHERE type = 'view'")}
assert views == {"entity_list", "block_entities", "players", "poi_records"}, views
print("libtuff_sqlite loads and answers")
PY
