#!/usr/bin/env bash
# Regenerates tests/fixtures/worldgen/vanilla/: chunks the vanilla server
# generated for seed 1 in all three dimensions, trimmed to what the world
# generation tests compare (status, blocks, biomes, heightmaps, structures).
# The server is ticked only until it starts; chunks are generated frozen.
#
# The server runs four times. Three runs load a data pack
# (scripts/worldgen/WorldgenDatapack.java) that stops generation after one
# stage and disable structures, so their chunks are per-stage references:
# "noise" (density and aquifers), "surface" (plus material rules),
# "carvers" (plus carvers). The fourth, "full", is unmodified vanilla.
#
# Needs the tuff-vanilla-data build cache (any `cargo build` creates it).
# VANILLA_CACHE_DIR and VANILLA_JAVA override its location and the JVM;
# KEEP_WORK=1 keeps the worlds and vanilla's logs for inspection.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
version=26.3
cache=${VANILLA_CACHE_DIR:-$root/target/vanilla}
jar="$cache/$version/server.jar"
inner="$cache/$version/work/versions/$version/server-$version.jar"
java=${VANILLA_JAVA:-$(find "$cache/java" -path '*/bin/java' -type f 2>/dev/null | head -n 1)}
[[ -f "$jar" && -f "$inner" ]] || { echo "missing $jar; run cargo build first" >&2; exit 1; }
[[ -x "$java" ]] || { echo "no JVM found; set VANILLA_JAVA" >&2; exit 1; }

classpath=$(find "$cache/$version/work/libraries" -name '*.jar' | sort | paste -sd: -)
work=$(mktemp -d)
if [[ -z "${KEEP_WORK:-}" ]]; then
    trap 'rm -rf "$work"' EXIT
else
    echo "keeping $work"
fi

# Keep in sync with CENTERS in src/worldgen/tests/vanilla.rs. Ticking is
# frozen first so the saved chunks are exactly what generation produced:
# no fluid flow, leaf distance fixes, vine growth or freezing afterwards.
commands() {
    echo "tick freeze"
    echo "forceload add 0 0"
    echo "forceload add 1600 -800"
    echo "forceload add -3200 4800"
    echo "forceload add -9008 -2000"
    echo "forceload add 5760 -8832"
    echo "forceload add 384 -1536"
    echo "forceload add -1536 -2944"
    echo "forceload add 3072 -2432"
    echo "forceload add -1536 896"
    echo "forceload add -768 -2816"
    echo "forceload add 256 -1280"
    echo "forceload add -896 1792"
    echo "forceload add 256 8960"
    echo "forceload add -192 -1440"
    echo "forceload add 640 10240"
    echo "forceload add -2496 -2976"
    echo "execute in minecraft:the_nether run forceload add 0 0"
    echo "execute in minecraft:the_nether run forceload add 480 -320"
    echo "execute in minecraft:the_nether run forceload add -96 96"
    echo "execute in minecraft:the_nether run forceload add 192 288"
    echo "execute in minecraft:the_nether run forceload add -288 96"
    echo "execute in minecraft:the_nether run forceload add 0 192"
    echo "execute in minecraft:the_end run forceload add 0 0"
    echo "execute in minecraft:the_end run forceload add 1600 160"
    echo "execute in minecraft:the_end run forceload add -48 -16"
    echo "execute in minecraft:the_end run forceload add -192 -1152"
    echo "execute in minecraft:the_end run forceload add -1056 0"
    echo "execute in minecraft:the_end run forceload add 96 0"
    echo "execute in minecraft:the_end run forceload add 1424 64"
    echo "forceload add 304 288"
    echo "forceload add 16 976"
    echo "forceload add -400 240"
    echo "forceload add -1776 -528"
    echo "forceload add -2352 128"
    echo "forceload add -5520 0"
    echo "forceload add 336 9104"
    echo "execute in minecraft:the_nether run forceload add 240 -640"
    echo "execute in minecraft:the_nether run forceload add 208 -640"
    echo "forceload add 1120 2176"
    echo "forceload add 624 816"
    echo "forceload add -256 -928"
    echo "forceload add -768 -1312"
    echo "forceload add 2000 -3008"
    echo "execute in minecraft:the_nether run forceload add 352 -160"
    echo "execute in minecraft:the_nether run forceload add 208 16"
    echo "execute in minecraft:the_nether run forceload add 496 -160"
    echo "execute in minecraft:the_nether run forceload add -256 -464"
    echo "execute in minecraft:the_nether run forceload add 240 -960"
    echo "forceload add 1744 688"
    echo "forceload add -160 -1808"
    echo "forceload add 272 9552"
    echo "forceload add 352 9280"
    echo "forceload add -2416 288"
    echo "forceload add -4080 -1376"
    echo "forceload add -144 -944"
    echo "forceload add -208 -320"
    echo "forceload add 448 160"
    echo "forceload add 432 -656"
    echo "forceload add -288 768"
    echo "forceload add -208 16"
    echo "execute in minecraft:the_end run forceload add 96 1568"
    echo "forceload add 1888 240"
    echo "forceload add 1216 -1216"
    echo "forceload add 1616 -1408"
}

for variant in noise surface carvers full; do
    dir="$work/$variant"
    mkdir -p "$dir/world/datapacks"
    echo "eula=true" > "$dir/eula.txt"
    structures=false
    packs=vanilla
    if [[ $variant == full ]]; then
        structures=true
    else
        "$java" -cp "$classpath" "$root/scripts/worldgen/WorldgenDatapack.java" "$inner" "$dir/world/datapacks/stage" "$variant"
        packs="vanilla,file/stage"
    fi
    cat > "$dir/server.properties" <<PROPS
level-name=world
level-seed=1
online-mode=false
server-port=0
enable-status=false
management-server-enabled=false
sync-chunk-writes=true
region-file-compression=deflate
generate-structures=$structures
initial-enabled-packs=$packs
PROPS
    (
        cd "$dir"
        {
            sleep 20
            commands
            sleep 180
            echo "save-all flush"
            sleep 10
            echo "stop"
            sleep 20
        } |
            timeout 360 "$java" -jar "$jar" --nogui > vanilla.log 2>&1 || true
    )
    if ! grep -q "Done (" "$dir/vanilla.log"; then
        echo "vanilla did not start ($variant):" >&2
        tail -n 40 "$dir/vanilla.log" >&2
        exit 1
    fi
done

cd "$root"
MC_WORLDGEN_FIXTURE_WORK="$work" \
    cargo test --quiet --lib -- --ignored --exact worldgen::tests::vanilla::trim_worldgen_fixture
echo "wrote tests/fixtures/worldgen/vanilla/*.nbt"
