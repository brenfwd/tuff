---
title: Performance
description: Side-by-side benchmarks against vanilla and how to reproduce them.
group: Reference
source: README.md
---

Measured side by side on a 16-vCPU Linux VM (Intel i9-13900HK host):
tuff built with the `release-opt` profile, vanilla 26.3 on its bundled
OpenJDK 25 with `-Xms8G -Xmx8G` and Aikar's G1 flags. Both servers used the
same seed, view and simulation distance, players and durations, with worlds
on disk. Each run started on a quiet machine after a warm-up (village: a
discarded 1-day sprint before a measured 2-day sprint; stress: 120 s before
a 60 s window). Five interleaved runs per server; medians shown.

| Benchmark | Sim distance | Metric | tuff | Vanilla |
| --- | --- | --- | --- | --- |
| Village (1 player, 32-chunk view) | 32 | ticks per second | 386 | 91.6 |
| | 32 | mean / p99 tick (ms) | 2.58 / 4.52 | 10.9 / 12.8 |
| | 32 | CPU cores used | 4.8 | 1.0 |
| | 10 | ticks per second | 1019 | 529 |
| | 10 | mean / p99 tick (ms) | 0.97 / 2.23 | 1.88 / 3.17 |
| | 10 | CPU cores used | 2.8 | 1.1 |
| Stress (50 players: 12 flying, 12 mining, 26 walking; 1250 mobs) | 10 | TPS | 20.0 (cap) | 2.1 |
| | 10 | mean / p99 tick (ms) | 48 / 106 | 192 / 254 |
| | 10 | CPU cores used | 15.4 | 4.0 |
| | 32 | TPS | 14.2 | 1.9 |
| | 32 | mean / p99 tick (ms) | 70 / 180 | 196 / 241 |
| | 32 | CPU cores used | 14.8 | 3.5 |

tuff's lead comes from spreading the tick across cores, not from doing less
work per core: on the village at simulation distance 32 it runs 4.2x the
tick rate using about 4.8 cores against vanilla's 1, and per core it is
slightly slower. Memory is not compared: vanilla's RSS is dominated by its
pre-allocated 8 GB heap. The village run also ends with fewer entities on
tuff (about 540 vs 810), so mob behaviour is not yet an exact match.

To reproduce, run `cargo bench --bench village -- --server both` or
`cargo bench --bench stress -- --server both` (`--server` also takes
`tuff` or `vanilla`; see Benchmarks below).


## Benchmarks

Both benchmarks start a real server and are run only through `cargo bench`.

    cargo bench --bench village
    cargo bench --bench village -- --fly_chunks 16 --sprint 1d
    cargo bench --bench stress -- --preset normal --players 16 --duration 2m
    cargo bench --profile dev --bench stress -- --players 2 --duration 5s --warmup 2s

`village` flies one bot through fresh terrain to the nearest village, then
runs `/tick sprint` (flags `--seed`, `--fly_chunks`, `--speed`,
`--sprint`, `--instance_dir`, `--tick_lockstep`, `--out`, `--label`).
`stress` runs a fleet of bots in a child process and prints a JSON report;
`--compare a.json b.json` diffs two reports. `--profile dev` is cargo's
flag for a quick unoptimised build. `benches/stress/README.md` lists every
stress flag.

The stress fleet has four kinds of load: flyers (spectators flying
outwards, forcing chunk generation), miners (creative players digging
pits), walkers (survival players walking, building, fighting and opening
chests) and entities (mobs summoned around spawn). `--players <n>`
(default 8) sizes the load and `--preset` splits it:

| Preset | Flyers | Miners | Walkers | Entities |
|--------|--------|--------|---------|----------|
| `synthetic` (default) | n | n/2 | 0 | 100 n |
| `normal` | 0 | 0 | n | 4 n |
| `mixed` | n/4 | n/4 | the rest | 25 n |

`--flyers`, `--miners`, `--walkers` and `--entities` set one count
directly and leave the others to the preset.

Both benchmarks can also run the vanilla server jar that the build caches.
`--server vanilla` runs it instead of tuff, and `--server both` runs tuff
and then vanilla with the same load and prints the two reports side by
side, with the ratio of each figure and `n/a` where a server cannot
measure it. Running vanilla means accepting the Minecraft EULA
(https://aka.ms/MinecraftEULA), so it needs `--accept-mojang-eula` or
`TUFF_ACCEPT_MOJANG_EULA=1`. Its instance is a throwaway directory under
`target/vanilla-bench/`; `--java`, `--vanilla_jar` and `--java_opts`
(default `-Xmx4G`) choose the JVM. This runs 36 flyers, plus the default
preset's 4 miners and 800 mobs, on each server:

    cargo bench --bench stress -- --server both --accept-mojang-eula --flyers 36

To watch a run with a real client, pass `--ops Alice,Bob` (those players
become level 4 operators; names match ignoring case), plus `--host <ip>`
(default `127.0.0.1`) and `--port <n>` (default `0`, an ephemeral port).
The bench prints the address to join. Bench servers run in offline mode,
so a client joins without authentication.
