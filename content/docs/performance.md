---
title: Performance
description: Side-by-side benchmarks against vanilla and how to reproduce them.
group: Reference
source: README.md
---

Measured side by side on a 16-vCPU Linux machine: tuff built with the
`release-opt` profile, vanilla 26.3 on its bundled OpenJDK 25 with
`-Xms8G -Xmx8G` and Aikar's G1 flags. Both servers were warmed up first:
the stress run ran 120 s before a 60 s measured window, and the village run
did a discarded 1-day sprint before the measured 10-day sprint. Three
alternating runs per server; medians shown.

| Benchmark | Metric | tuff | Vanilla | Ratio |
| --- | --- | --- | --- | --- |
| Village, 10-day sprint (1 player, 32-chunk view) | ticks per second | 1026 | 89.6 | 11.5x |
| Village | mean tick (ms) | 0.97 | 11.15 | 11.5x lower |
| Village | p99 tick (ms) | 2.13 | 12.9 | 6.1x lower |
| Village | server memory (MiB) | 1379 | 8966 | 6.5x lower |
| Village | CPU cores used | 2.70 | 1.02 | |
| Stress, 50 players mixed (12 flying, 12 mining, 26 walking; ~1250 entities; view 10) | TPS | 20.35 | 2.15 | 9.5x (tuff at the 20 TPS cap) |
| Stress | mean tick (ms) | 48.2 | 190 | 3.9x lower |
| Stress | p99 tick (ms) | 102 | 253 | 2.5x lower |
| Stress | server memory (MiB) | 2514 | 9005 | 3.6x lower |
| Stress | CPU cores used | 15.35 | 3.90 | |

tuff parallelises ticking across cores, so it uses more CPU; in the stress
run it is near saturation on 16 cores, so the mean-tick ratio is a fairer
capacity comparison than TPS. Vanilla's memory is high partly because its
8 GB heap is pre-touched.

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
