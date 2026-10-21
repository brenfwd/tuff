#!/usr/bin/env bash
# Checks that vanilla accepts a world written by this server: writes a world
# through the real region actors, runs the vanilla server (from the
# tuff-vanilla-data build cache) on it with console commands that freeze the game
# (so primed TNT keeps its fuse), force-load the area and test blocks and
# entities from vanilla's side, then checks
# vanilla's log for chunk errors and that the terrain and edits survived
# vanilla's own save.
#
# Needs the tuff-vanilla-data build cache (any `cargo build` creates it).
# VANILLA_CACHE_DIR and VANILLA_JAVA override its location and the JVM;
# KEEP_WORK=1 keeps the world and vanilla's log for inspection.
# --force-upgrade starts vanilla with --forceUpgrade --eraseCache, so every
# chunk, entity and POI region is run through vanilla's upgrader first and any
# per-chunk upgrade failure fails the check.
set -euo pipefail

vanilla_args=(--nogui)
limit=120
force_upgrade=
case "${1:-}" in
    --force-upgrade) force_upgrade=1; vanilla_args+=(--forceUpgrade --eraseCache); limit=240 ;;
    "") ;;
    *) echo "usage: $0 [--force-upgrade]" >&2; exit 2 ;;
esac

root=$(cd "$(dirname "$0")/.." && pwd)
. "$root/scripts/lib/vanilla-server.sh"
vanilla_locate

work=$(mktemp -d)
if [[ -z "${KEEP_WORK:-}" ]]; then
    trap 'rm -rf "$work"' EXIT
else
    echo "keeping $work"
fi
export MC_VANILLA_COMPAT_WORLD="$work/world"

cd "$root"
cargo test --quiet -p tuff-tests --test world -- --ignored --exact vanilla_compat::export_world_for_vanilla

vanilla_prepare "$work" sync-chunk-writes=true
(
    cd "$work"
    {
        if [[ -n "$force_upgrade" ]]; then
            for _ in $(seq "$limit"); do grep -q "Done (" vanilla.log 2>/dev/null && break; sleep 1; done
        fi
        sleep 15
        head -n 2 vanilla-commands.txt
        sleep 15
        tail -n +3 vanilla-commands.txt
        sleep 5
        echo "save-all flush"
        sleep 10
        echo "stop"
        sleep 20
    } |
        run_with_timeout "$limit" "$java" -jar "$jar" "${vanilla_args[@]}" > vanilla.log 2>&1 || true
)

if ! grep -q "Done (" "$work/vanilla.log"; then
    echo "vanilla did not start:" >&2
    tail -n 40 "$work/vanilla.log" >&2
    exit 1
fi
if [[ -n "$force_upgrade" ]] && ! grep -q "World optimization finished" "$work/vanilla.log"; then
    echo "vanilla did not finish the forced upgrade:" >&2
    grep -E "Upgrading|Forcing" "$work/vanilla.log" >&2 || true
    exit 1
fi
problems=$(grep -E '/ERROR\]|/WARN\].*(upgrad|invalid position|region)|Exception|Recoverable errors|wrong location|Couldn.t load|Failed to (load|read|convert|replace)|Chunk file at|has invalid position' "$work/vanilla.log" | grep -v 'server.properties' || true)
if [[ -n "$problems" ]]; then
    echo "vanilla reported problems loading the world:" >&2
    echo "$problems" >&2
    exit 1
fi

checks=$(($(wc -l < "$work/vanilla-commands.txt") - 2))
passed=$(grep -c "Test passed" "$work/vanilla.log" || true)
if [[ "$passed" -ne "$checks" ]]; then
    echo "vanilla passed $passed of $checks checks:" >&2
    grep -E "Test (passed|failed)|forceload|Chunk" "$work/vanilla.log" >&2 || true
    exit 1
fi

cargo test --quiet -p tuff-tests --test world -- --ignored --exact vanilla_compat::verify_world_after_vanilla
echo "vanilla $version loaded the world cleanly${force_upgrade:+ after --forceUpgrade}"
