#!/usr/bin/env bash
# Regenerates tests/fixtures/vanilla-26.3/light.r.0.0.mca: a 7x7 chunk area
# (chunks 3..9) generated and lit by the vanilla server (seed 1), trimmed to
# blocks and light. The light engine tests relight it and compare the inner
# chunks with vanilla's light.
#
# Needs the tuff-vanilla-data build cache (any `cargo build` creates it).
# VANILLA_CACHE_DIR and VANILLA_JAVA override its location and the JVM;
# KEEP_WORK=1 keeps the world and vanilla's log for inspection.
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

echo "eula=true" > "$work/eula.txt"
cat > "$work/server.properties" <<PROPS
level-name=world
level-seed=1
online-mode=false
server-port=0
enable-status=false
management-server-enabled=false
sync-chunk-writes=true
region-file-compression=deflate
PROPS
(
    cd "$work"
    {
        sleep 20
        echo "forceload add 48 48 159 159"
        sleep 40
        echo "save-all flush"
        sleep 10
        echo "stop"
        sleep 20
    } |
        run_with_timeout 150 "$java" -jar "$jar" --nogui > vanilla.log 2>&1 || true
)

if ! grep -q "Done (" "$work/vanilla.log"; then
    echo "vanilla did not start:" >&2
    tail -n 40 "$work/vanilla.log" >&2
    exit 1
fi

cd "$root"
MC_LIGHT_FIXTURE_WORLD="$work/world" \
    cargo test --quiet --lib -- --ignored --exact light::tests::vanilla::trim_light_fixture
echo "wrote tests/fixtures/vanilla-$version/light.r.0.0.mca"
