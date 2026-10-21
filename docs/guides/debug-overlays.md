# Vanilla debug overlays against tuff

How to switch on the vanilla client's debug overlays and use them to look at
a tuff server. Background and the full flag catalog:
[vanilla-dev-features.md](vanilla-dev-features.md).

## How an overlay turns on

Server-driven renderers need all of these:

1. `-DMC_DEBUG_ENABLED=true` on the client JVM. Without it every other
   `MC_DEBUG_*` property is ignored.
2. The per-renderer property, also a client JVM `-D` argument (read once at
   startup; environment variables and game arguments do nothing).
3. Operator rights on the server. The client only asks for data; the server
   sends it to ops only.

No F3 key or keybind toggles these renderers: with the flags set they draw
whenever the server sends data. `-DMC_DEBUG_PRINT_PROPERTIES=true` (with the
master flag) prints every flag the jar knows.

| Overlay | Per-renderer flag | Server feed asked for |
|---|---|---|
| Points of interest | `MC_DEBUG_POI` | `pois` |
| Village sections | `MC_DEBUG_VILLAGE_SECTIONS` | `village_sections` |
| Neighbor updates | `MC_DEBUG_NEIGHBORSUPDATE` | `neighbor_updates` |
| Game event listeners and events | `MC_DEBUG_GAME_EVENT_LISTENERS` | `game_event_listeners`, `game_events` |
| Pathfinding | `MC_DEBUG_PATHFINDING` | `entity_paths` |
| Structures | `MC_DEBUG_STRUCTURES` | `structures` |
| Brains | `MC_DEBUG_BRAIN` | `brains` |
| Bees and hives | `MC_DEBUG_BEES` (also asks `goal_selectors`) | `bees`, `bee_hives` |
| Raids | `MC_DEBUG_RAIDS` | `raids` |
| Goal selectors | `MC_DEBUG_GOAL_SELECTOR` | `goal_selectors` |
| Breeze | `MC_DEBUG_BREEZE_MOB` | `breezes` |
| Entity-block intersections | `MC_DEBUG_ENTITY_BLOCK_INTERSECTION` | `entity_block_intersections` |
| Redstone wire order | `MC_DEBUG_EXPERIMENTAL_REDSTONEWIRE_UPDATE_ORDER` | `redstone_wire_orientations` |

The server tick-time chart is different: no -D flag. Press F3+2 (FPS charts);
the client then asks for the tick-time sample feed. It still needs op.

A handy set for tuff today:

    -DMC_DEBUG_ENABLED=true -DMC_DEBUG_STRUCTURES=true -DMC_DEBUG_PATHFINDING=true -DMC_DEBUG_GOAL_SELECTOR=true -DMC_DEBUG_POI=true -DMC_DEBUG_NEIGHBORSUPDATE=true

## Passing the flags

### Official launcher

1. Open the Minecraft Launcher, choose Installations.
2. Hover the installation for 26.3 (or create one), click the three dots,
   Edit.
3. Click More options.
4. In JVM arguments, append the flags separated by spaces after the existing
   text (keep the default `-Xmx` and friends).
5. Save, Play.

### Prism Launcher / MultiMC

1. Right-click the instance, Edit (or select it and choose Edit Instance).
2. Open Settings, then Java.
3. Tick "Java arguments" (to override the global value) and put the flags in
   the box, space separated.
4. Close, launch the instance.

### The repo client harness

`crates/tuff-client-harness` builds the JVM command line itself (`launch.rs`):
fixed memory and GC flags, the version manifest's JVM arguments, the library
and natives paths, and its own control-hook properties. There is no
environment variable, option or test parameter for extra `-D` arguments
(the env vars it reads are only for the assets, cache, java, lock and test
opt-in). So the overlay flags cannot be passed through the harness today; use
a launcher against a hand-run tuff for overlay work, or add an extra-args
hook to `build_command` first.

## What tuff feeds today

Region code answers every subscription (`crates/tuff-core/src/world/debug_feeds.rs`,
`crates/tuff-core/src/world/region/debug.rs`) except `raids` (raid state is
region-local, nothing world-wide to read); enabling that flag is harmless but
draws nothing. `redstone_wire_orientations` only has data in a world with the
`redstone_experiments` feature pack, where each wire the experimental
evaluator changes reports the orientation it used.

Known differences from vanilla: goal names are tuff's goal type names; brains
are sent for villagers only, with a few memories and no behaviors; bees report
no travel ticks or blacklisted hives; entity-block intersections use the
current box, not the swept movement.

These draw only from data the client already has, so they work against tuff
with no server support and without any -D flag: chunk borders (F3+G),
heightmap, block and sky light levels, light sections, water levels, collision
boxes, entity supporting blocks, solid faces, entity hitboxes (F3+B), and the
octree. Toggle them on the F3+Q debug options screen. They are the quickest
check of the chunk, light and heightmap data tuff sends.

## Tuff side

- The player must be op (or hold the nodes below). Defaults grant both nodes
  to operators at the moderator level and above, so a plain `op` works.
- `tuff.debug.subscriptions` gates the world feeds (everything except the
  tick-time chart).
- `tuff.debug.tick_time` gates the tick-time chart.
- Either node can be granted to a non-op, or denied to an op, with the
  permission system.

## Quick start

1. Run tuff in offline mode: `cargo run --release -- --offline`. On a new
   server also set `white-list=false` in `server.properties` or whitelist
   yourself.
2. On the server console: `op <yourname>`.
3. Launch the vanilla 26.3 client with a username to match, using the flags
   above, and join `localhost`.
4. Press F3+2: the tick-time chart appears. Walk near a village or bed/job
   block for POI and village-section markers; place and break redstone
   components for neighbor update boxes; make a vibration (walk by a sculk
   sensor) for game events. F3+G shows chunk borders with no flags.
5. If nothing draws, check in order: master flag set, per-renderer flag
   spelled exactly, you are op, and the overlay is one of the four fed ones.
