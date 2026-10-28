#!/usr/bin/env bash
# Opt-in, repeatable vanilla reference run for side-by-side parity work (never
# part of `cargo t`). Starts the vanilla server from the tuff-vanilla-data build
# cache with debug flags: DONT_SAVE_WORLD (so every run starts from the same
# world copy), VERBOSE_SERVER_EVENTS and BLOCK_BREAK logging, and DEV_COMMANDS
# (/raid, /debugpath, /debugmobspawning, /warden_spawn_tracker) as triggers.
# Feeds a console script, then writes the raw log and a normalized event list
# (timestamps and thread names stripped) for diffing between runs.
#
# Usage: TUFF_ACCEPT_MOJANG_EULA=1 scripts/vanilla-parity-run.sh SCRIPT [OUT_DIR]
#   SCRIPT: console commands, one per line; `sleep N` pauses N seconds, `#`
#           starts a comment. `stop` is sent at the end. Player-bound triggers
#           (/raid, /debugpath) are written `execute as @p at @p run ...`: the
#           unattended vanilla console has no player, so `@p` selects nothing
#           and the line is a no-op there, while the tuff replay rewrites `@p`
#           to its attached bot, so those triggers run on the tuff side only
#           unless VANILLA_CLIENT=1 joins a harness client (below).
# Env: WORLD=<dir> world to copy in (default: fresh world from SEED),
#      SEED, VANILLA_FLAGS="NAME ..." extra MC_DEBUG_<NAME> flags, STAGES="surface
#      carvers structures features ore_veins blending fluid_generation" worldgen
#      stages to disable (adds MC_DEBUG_DISABLE_<STAGE> here and writes the
#      matching tuff.worldgen.debug.disable* config keys to
#      OUT_DIR/tuff-stage-config.txt for the tuff side), TIMEOUT (default 300), VANILLA_CACHE_DIR,
#      VANILLA_JAVA, TUFF_SIDE=1 also replays SCRIPT against an in-process tuff
#      (tests/server/parity_run.rs) to write OUT_DIR/tuff-events.txt and the
#      forceloaded chunk set to OUT_DIR/tuff-tickets.txt (diffed as a set
#      against vanilla's forced-ticket log lines), VANILLA_CLIENT=1 joins a real
#      vanilla client (user "Parity", offline mode, Xvfb when DISPLAY is unset)
#      to the vanilla server before the console script runs, so `@p` triggers
#      act on vanilla too (needs TUFF_ACCEPT_MOJANG_EULA, xvfb-run or DISPLAY;
#      uses one client slot).
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
. "$root/scripts/lib/vanilla-server.sh"
[[ "${TUFF_ACCEPT_MOJANG_EULA:-}" == 1 ]] || { echo "set TUFF_ACCEPT_MOJANG_EULA=1 to run the vanilla server" >&2; exit 2; }
script=${1:?usage: $0 SCRIPT [OUT_DIR]}
[[ -f "$script" ]] || { echo "missing $script" >&2; exit 1; }
script=$(cd "$(dirname "$script")" && pwd)/$(basename "$script")
vanilla_locate

out=${2:-$root/target/vanilla-parity/$(date +%Y%m%d-%H%M%S)}
mkdir -p "$out"
out=$(cd "$out" && pwd)
work=$(mktemp -d "$out/work.XXXXXX")
trap 'kill "${client_pid:-0}" 2>/dev/null; rm -rf "$work"' EXIT

vanilla_prepare "$work" "level-seed=${SEED:-}"
client_pid=
if [[ -n "${VANILLA_CLIENT:-}" ]]; then
    if [[ -z "${DISPLAY:-}" ]] && ! command -v xvfb-run >/dev/null; then
        echo "VANILLA_CLIENT=1 needs xvfb-run or DISPLAY" >&2; exit 2
    fi
    client_port=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])')
    printf "%s\n" white-list=false "server-port=$client_port" >> "$work/server.properties"
fi
if [[ -n "${WORLD:-}" ]]; then
    cp -r "$WORLD" "$work/world"
fi

flags=(-DMC_DEBUG_ENABLED=true "-Dlog4j2.configurationFile=$root/scripts/vanilla-parity/log4j2-debug.xml")
for name in DONT_SAVE_WORLD VERBOSE_SERVER_EVENTS BLOCK_BREAK DEV_COMMANDS ${VANILLA_FLAGS:-}; do
    flags+=("-DMC_DEBUG_$name=true")
done

tuff_keys=()
for stage in ${STAGES:-}; do
    flags+=("-DMC_DEBUG_DISABLE_$(echo "$stage" | tr a-z A-Z)=true")
    camel=$(echo "$stage" | awk -F_ '{for(i=1;i<=NF;i++) printf toupper(substr($i,1,1)) substr($i,2)}')
    tuff_keys+=("tuff.worldgen.debug.disable$camel=true")
done
if [[ ${#tuff_keys[@]} -gt 0 ]]; then printf '%s\n' "${tuff_keys[@]}" > "$out/tuff-stage-config.txt"; fi

start_client() {
    for _ in $(seq 1 120); do
        grep -q "Done (" "$out/vanilla.log" 2>/dev/null && break
        sleep 1
    done
    runner=(env)
    [[ -z "${DISPLAY:-}" ]] && runner=(xvfb-run -a -s "-screen 0 1920x1080x24" env)
    (cd "$root" && "${runner[@]}" TUFF_CLIENT_TESTS=1 TUFF_ACCEPT_MOJANG_EULA=1 \
        PARITY_SERVER_ADDR="127.0.0.1:$client_port" PARITY_USERNAME=Parity \
        PARITY_SENTINEL="$work/client.stop" PARITY_READY="$work/client.ready" \
        cargo test -q -p tuff-client-harness --test parity vanilla_side -- --ignored --nocapture) \
        > "$out/vanilla-client.log" 2>&1 &
    wait
}

if [[ -n "${VANILLA_CLIENT:-}" ]]; then start_client & client_pid=$!; fi
(
    cd "$work"
    {
        if [[ -n "${VANILLA_CLIENT:-}" ]]; then
            while ! grep -q "Done (" "$out/vanilla.log" 2>/dev/null; do sleep 1; done
            for _ in $(seq 1 600); do [[ -f "$work/client.ready" ]] && break; sleep 1; done
            [[ -f "$work/client.ready" ]] || echo "vanilla client did not join; see vanilla-client.log" >&2
        fi
        sleep 15
        while IFS= read -r line || [[ -n "$line" ]]; do
            line=${line%%#*}
            [[ -z "${line// }" ]] && continue
            if [[ "$line" =~ ^sleep[[:space:]]+([0-9.]+) ]]; then
                sleep "${BASH_REMATCH[1]}"
            else
                echo "$line"
            fi
        done < "$script"
        sleep 2
        touch "$work/client.stop"
        echo "stop"
        sleep 20
    } | run_with_timeout "${TIMEOUT:-300}" "$java" "${flags[@]}" -jar "$jar" --nogui > "$out/vanilla.log" 2>&1 || true
)

grep -q "Done (" "$out/vanilla.log" || { echo "vanilla did not start:" >&2; tail -n 40 "$out/vanilla.log" >&2; exit 1; }
sed -E 's/^\[[0-9:.]+\] \[[^]]*\/([A-Z]+)\]: /\1 /' "$out/vanilla.log" > "$out/events.txt"
echo "log: $out/vanilla.log"
echo "events: $out/events.txt ($(wc -l < "$out/events.txt") lines)"

if [[ -n "${TUFF_SIDE:-}" ]]; then
    TUFF_PARITY_OUT="$out" TUFF_PARITY_SCRIPT="$script" SEED="${SEED:-1}" \
        cargo test -q --test server replay_parity_script_and_write_tuff_events -- --nocapture || true
fi

if [[ -f "$out/tuff-events.txt" ]]; then
    diff_args=("$out/events.txt" "$out/tuff-events.txt")
    if [[ -f "$out/tuff-tickets.txt" ]]; then diff_args+=("$out/tuff-tickets.txt"); fi
    cargo run -q -p tuff-bot --bin parity-diff -- "${diff_args[@]}" || true
fi
