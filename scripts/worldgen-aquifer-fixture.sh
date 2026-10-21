#!/usr/bin/env bash
# Regenerates tests/fixtures/worldgen/seed_aquifer.nbt: the noise-stage chunk
# (density and aquifers only) that the vanilla server generates for the seed
# and chunk of tests/vanilla/seed_aquifer.rs. Uses the same one-stage data
# pack as scripts/worldgen-fixture.sh.
#
# Needs the tuff-vanilla-data build cache (any `cargo build` creates it).
# VANILLA_CACHE_DIR and VANILLA_JAVA override its location and the JVM.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
version=26.3
seed=-5713689679960382707
block_x=-683
block_z=2713
cache=${VANILLA_CACHE_DIR:-$root/target/vanilla}
jar="$cache/$version/server.jar"
inner="$cache/$version/work/versions/$version/server-$version.jar"
java=${VANILLA_JAVA:-$(find "$cache/java" -path '*/bin/java' -type f 2>/dev/null | head -n 1)}
[[ -f "$jar" && -f "$inner" ]] || { echo "missing $jar; run cargo build first" >&2; exit 1; }
[[ -x "$java" ]] || { echo "no JVM found; set VANILLA_JAVA" >&2; exit 1; }

classpath=$(find "$cache/$version/work/libraries" -name '*.jar' | sort | paste -sd: -)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

mkdir -p "$work/world/datapacks"
echo "eula=true" > "$work/eula.txt"
"$java" -cp "$classpath" "$root/scripts/worldgen/WorldgenDatapack.java" "$inner" "$work/world/datapacks/stage" noise
cat > "$work/server.properties" <<PROPS
level-name=world
level-seed=$seed
online-mode=false
server-port=0
enable-status=false
management-server-enabled=false
sync-chunk-writes=true
region-file-compression=deflate
generate-structures=false
initial-enabled-packs=vanilla,file/stage
PROPS
(
    cd "$work"
    {
        sleep 20
        echo "tick freeze"
        echo "forceload add $block_x $block_z"
        sleep 90
        echo "save-all flush"
        sleep 10
        echo "stop"
        sleep 15
    } | timeout 240 "$java" -jar "$jar" --nogui > vanilla.log 2>&1 || true
)
grep -q "Done (" "$work/vanilla.log" || { tail -n 40 "$work/vanilla.log" >&2; exit 1; }

cd "$root"
MC_AQUIFER_REGION_DIR="$work/world/dimensions/minecraft/overworld/region" \
    cargo test --quiet -p tuff-feature-builtin-vanilla-worldgen --lib -- --ignored --exact \
    tests::vanilla::seed_aquifer::trim_seed_aquifer_fixture
echo "wrote tests/fixtures/worldgen/seed_aquifer.nbt"
