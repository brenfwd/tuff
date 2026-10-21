#!/usr/bin/env bash
# Regenerates tests/fixtures/worldgen/reference.txt: random numbers, noise
# and density function values computed by the game's own world generation
# code (scripts/worldgen/WorldgenReference.java) for the worldgen tests.
#
# Needs the tuff-vanilla-data build cache (any `cargo build` creates it).
# VANILLA_CACHE_DIR and VANILLA_JAVA override its location and the JVM.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
version=26.3
cache=${VANILLA_CACHE_DIR:-$root/target/vanilla}
work="$cache/$version/work"
java=${VANILLA_JAVA:-$(find "$cache/java" -path '*/bin/java' -type f 2>/dev/null | head -n 1)}
[[ -d "$work/versions" ]] || { echo "missing $work; run cargo build first" >&2; exit 1; }
[[ -x "$java" ]] || { echo "no JVM found; set VANILLA_JAVA" >&2; exit 1; }

classpath=$(find "$work/versions" "$work/libraries" -name '*.jar' | sort | paste -sd: -)
out="$root/tests/fixtures/worldgen/reference.txt"
mkdir -p "$(dirname "$out")"
(cd "$work" && "$java" -cp "$classpath" "$root/scripts/worldgen/WorldgenReference.java" "$out.part")
mv "$out.part" "$out"
echo "wrote $out"
