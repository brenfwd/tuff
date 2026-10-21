#!/usr/bin/env bash
# Regenerates tests/fixtures/worldgen/locate.txt: what the vanilla server's
# /locate answers for seed 1 from fixed positions, for the locate tests.
# Each output line is the query (dimension, position, kind, target) followed
# by vanilla's feedback line.
#
# Needs the tuff-vanilla-data build cache (any `cargo build` creates it).
# VANILLA_CACHE_DIR and VANILLA_JAVA override its location and the JVM;
# KEEP_WORK=1 keeps the world and vanilla's log.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
. "$root/scripts/lib/timeout.sh"
version=26.3
cache=${VANILLA_CACHE_DIR:-$root/target/vanilla}
jar="$cache/$version/server.jar"
java=${VANILLA_JAVA:-$(find "$cache/java" -path '*/bin/java' -type f 2>/dev/null | head -n 1)}
[[ -f "$jar" ]] || { echo "missing $jar; run cargo build first" >&2; exit 1; }
[[ -x "$java" ]] || { echo "no JVM found; set VANILLA_JAVA" >&2; exit 1; }

work=$(mktemp -d)
if [[ -z "${KEEP_WORK:-}" ]]; then
    trap 'rm -rf "$work"' EXIT
else
    echo "keeping $work"
fi

# dimension x y z kind target
queries() {
    cat <<'Q'
overworld 0 64 0 structure minecraft:monument
overworld 0 64 0 structure minecraft:village_plains
overworld 0 64 0 structure #minecraft:village
overworld 0 64 0 structure minecraft:stronghold
overworld 2000 64 -3000 structure minecraft:stronghold
overworld 0 64 0 structure minecraft:mansion
overworld 0 64 0 structure minecraft:desert_pyramid
overworld 0 64 0 structure #minecraft:shipwreck
overworld 500 64 500 structure minecraft:swamp_hut
overworld 0 64 0 structure minecraft:pillager_outpost
overworld 0 64 0 biome minecraft:jungle
overworld 0 64 0 biome #minecraft:is_forest
overworld 0 -40 0 biome minecraft:deep_dark
overworld 300 64 -200 biome minecraft:mushroom_fields
the_nether 0 64 0 structure minecraft:fortress
the_nether 0 64 0 structure minecraft:bastion_remnant
the_nether 0 64 0 biome minecraft:warped_forest
Q
}

dir="$work/server"
mkdir -p "$dir"
echo "eula=true" > "$dir/eula.txt"
cat > "$dir/server.properties" <<PROPS
level-name=world
level-seed=1
online-mode=false
server-port=0
enable-status=false
management-server-enabled=false
generate-structures=true
PROPS
(
    cd "$dir"
    {
        sleep 20
        echo "tick freeze"
        queries | while read -r dimension x y z kind target; do
            echo "execute in minecraft:$dimension positioned $x $y $z run locate $kind $target"
        done
        sleep 120
        echo "stop"
        sleep 20
    } | run_with_timeout 300 "$java" -jar "$jar" --nogui > vanilla.log 2>&1 || true
)
grep -q "Done (" "$dir/vanilla.log" || { tail -n 40 "$dir/vanilla.log" >&2; exit 1; }

out="$root/tests/fixtures/worldgen/locate.txt"
answers=$(grep -E "\]: (System chat: )?(The nearest|Could not find|There is no)" "$dir/vanilla.log" |
    sed -E 's/^.*\]: (System chat: )?//')
if grep -qi "unexpected error" "$dir/vanilla.log"; then
    echo "vanilla answered with an error:" >&2
    grep -i -A3 "unexpected error" "$dir/vanilla.log" | head -n 20 >&2
    exit 1
fi
[[ $(echo "$answers" | wc -l) -eq $(queries | wc -l) ]] || {
    echo "expected one answer per query:" >&2
    echo "$answers" >&2
    exit 1
}
paste -d '|' <(queries) <(echo "$answers") > "$out"
echo "wrote $out"
