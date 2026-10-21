# Stress benchmark

A repeatable load benchmark. The server runs in the bench process,
bound to `127.0.0.1:0` by default (never 25565), with a fresh temporary
instance directory; a fleet of `tuff-bot` clients runs in a child process so the
reported CPU and memory belong to the server alone.

It is a `harness = false` bench, never a test. Run it only through
`cargo bench --bench stress`; never via `cargo test --all-targets` or
`--benches`.

## Load

- Flyers: spectators flying radially outwards at `--speed` blocks per
  tick at y 160, forcing chunk generation, loading and sending.
- Miners: creative players digging square pits around spawn at
  `--mine_rate` blocks per tick.
- Entities: mobs summoned (persistent) on flat ground around spawn.
- Walkers: survival players doing ordinary play around spawn: walking
  with step-ups and gravity, building and breaking blocks, attacking
  mobs in reach, placing and opening chests.

Presets pick the mix for `--players <n>`:

| preset      | flyers | miners | walkers | entities |
|-------------|--------|--------|---------|----------|
| `synthetic` | n      | n/2    | 0       | 100 n    |
| `normal`    | 0      | 0      | n       | 4 n      |
| `mixed`     | n/4    | n/4    | rest    | 25 n     |

`--flyers`, `--miners`, `--walkers` and `--entities` override the preset.
`--idlers <n>` (0) adds survival players who stand still at spawn.
`--sprinters <n>` (0) adds survival players sprint-jumping around spawn
with the client's physics and collision (anticheat workload).

## Running

```text
cargo bench --bench stress
cargo bench --bench stress -- --preset normal --players 16 --duration 2m
cargo bench --bench stress -- --players 2 --duration 5s --warmup 2s --view_distance 4
```

`--profile dev` skips the long optimised build for a quick smoke run;
compare only numbers measured with the same profile.

Other flags: `--entity_types <a,b,..>`, `--duration <d>` (60s),
`--warmup <d>` (10s), `--seed <n>` (1), `--view_distance <n>` (10),
`--home_radius <blocks>` (40), `--tick_lockstep <always|sprint>`
(always), `--config <key>=<value>` (repeatable, a server config
override), `--label <text>`, `--out <file>`, `--instance_dir <dir>`,
`--ops <name,..>`, `--host <ip>` (127.0.0.1), `--port <n>` (0),
`--linger <d>` (0, off; see `memory.after_leave` below).
Durations take `500ms`, `90s`, `2m` or `1h`.

## Watching live

```text
cargo bench --bench stress -- --ops Alice,Bob --port 25570
cargo bench --bench village -- --ops Alice --host 0.0.0.0 --port 25570
```

`--ops <name,..>` makes the named players operators (level 4) so a human
can join with a real client and watch, e.g. `/gamemode spectator` and
`/tp Fly0`. The bench prints the address to join on start-up (stderr
for stress, stdout for village). `--host <ip>` (127.0.0.1; use 0.0.0.0
or `::` to accept other machines) and `--port <n>` (0, an ephemeral
port) choose the address; the default is never 25565. The server runs
in offline mode, so ops are keyed by offline UUIDs and a client joins
without authentication. A name that clashes with a bot is rejected.
Watchers are not bots: the bot counters come only from the fleet, and
the report's `watchers` lists the ops and which of them were online at
the start and end of the window, since a watcher loading chunks adds
server load.

## Report

One JSON object on stdout (and in `--out`), measured over the window
after the warm-up only:

- `ticks`: count, TPS, mean MSPT, p50/p90/p95/p99/p99.9/max in ms,
  ticks over the 50 ms budget, samples lost by the collector.
- `chunks`: chunks received, per second, per player per second, and
  decompressed bytes per chunk.
- `network`: wire bytes (compressed, as read off the socket) in total,
  per second and per player, payload bytes, compression ratio, packets.
- `packets`: clientbound play packets the bots received, in total, per
  player per second, and by kind (`kinds.<name>`: count, `wire_bytes`
  as framed on the wire, `bytes` decompressed); `player_seconds`
  turns any kind into a rate. `--server both` and `--compare` across
  servers print the top kinds by count, wire bytes and decompressed
  bytes with a tuff/vanilla ratio instead of a row per kind.
- `cpu`: server and fleet CPU seconds and average cores busy.
- `memory`: server RSS at start, end, mean and peak; fleet peak RSS.
  `at_end` adds the live heap (tuff only: the bench's allocator counts
  it, since tuff runs in the bench process) and, on macOS, a
  `vmmap --summary` digest (physical footprint, malloc zone resident,
  allocated and fragmentation). With `--linger <d>`, `after_leave`
  samples RSS every second for that long after every bot has left and
  then records the live heap, loaded chunks and regions, and vmmap.
- `bots`: summed bot counters (digs, block uses, attacks, chests
  opened, deaths, teleports).
- `watchers`: `--ops` names and which were online at the start and end
  of the window.

## Comparing a feature on and off

```text
cargo bench --bench stress -- --label off --out off.json
cargo bench --bench stress -- --config tuff.antiXray.enabled=true --label on --out on.json
cargo bench --bench stress -- --compare off.json on.json
```

`--compare` prints every numeric field shared by both reports with the
relative change. Run each variant at least twice; numbers vary between
runs by several percent.

## Against the vanilla server

```text
cargo bench --bench stress -- --server both --accept-mojang-eula --preset mixed --players 8
```

`--server vanilla|both` runs the vanilla jar the tuff-vanilla-data build
caches as a child process (offline, ephemeral loopback ports, RCON) in
a throwaway instance under `target/vanilla-bench/`. It requires
accepting the Minecraft EULA (https://aka.ms/MinecraftEULA) with
`--accept-mojang-eula` or `TUFF_ACCEPT_MOJANG_EULA=1`. `both` runs each
server in turn and prints the reports side by side, `n/a` where a
server cannot measure a field. A `workload` line marks the comparison
INVALID when one server's bots made under 0.9x the other's moves, and
`per_move.*` rows divide server CPU, wire bytes and packets by moves.
Each report also records `shutdown_s`. `--java`, `--vanilla_jar` and
`--java_opts` (-Xmx4G) tune the JVM.
