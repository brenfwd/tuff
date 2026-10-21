# Vanilla dev-only and debug features

Catalog of the debugging, developer and diagnostic features built into the
vanilla 26.3 server and client jars, how to switch each on with a stock jar,
and what tuff can do with it. The /chase command is
covered in [chase-side-by-side.md](chase-side-by-side.md) and only listed here.
For a hands-on how-to (launcher setup, what tuff feeds today) see
[debug-overlays.md](debug-overlays.md); where this catalog says tuff has no
sender, that guide is current for pois, village sections, neighbor updates
and game events.

## How the gating works

There are three independent gates. Know which one a feature sits behind.

1. Debug flags. A master switch, the system property `MC_DEBUG_ENABLED`
   (true or empty value), must be set. Each feature then has its own property
   named `MC_DEBUG_<NAME>`. A flag is read once, at class load, with
   `System.getProperty`, so it must be a `-D` JVM argument (`java -DMC_DEBUG_ENABLED=true
   -DMC_DEBUG_PATHFINDING=true -jar ...`), not an environment variable and not
   a program argument. Without the master switch every flag is false, even if
   set. `-DMC_DEBUG_PRINT_PROPERTIES=true` (also needs the master switch) prints
   every available flag name and type at startup; use it to confirm a jar's
   list. The server and client jars share the same constants class and the same
   flag list, so the same property means the same thing in both, where the code
   path exists.
2. "Running in an IDE". A static boolean that is false on a stock jar. Nothing
   in the shipped code sets it from a property or argument; the dev launcher
   sets it. On a stock jar it can only be flipped by a patched jar or a java
   agent. Where a feature has an IDE-only gate, it is marked "needs IDE/patch".
3. Permission. Debug subscriptions and dev commands also need operator rights
   (see below); the debug-subscription path accepts a singleplayer owner
   without op only when the IDE flag is set.

Two helpers for this doc: "server flag" means the property takes effect when
set on the server JVM, "client flag" when set on the client JVM. The client
sets which debug subscriptions to request from its own flags, so for the
server-driven renderers the flag goes on the CLIENT, and the server only needs
to answer.

## Summary table

Tuff column: "yes" implemented, "part" partly, "no" not implemented.
Use column: P parity testing, R render tuff state with vanilla client, D
debugging tuff, - no value.

| Feature | Side | Enable on stock jar | Tuff | Use |
|---|---|---|---|---|
| Debug subscription protocol (values and events) | both | client `MC_DEBUG_ENABLED` plus per-renderer flags; sender must be op | yes (all feeds except raids; wire orientations need the redstone experiments pack) | R |
| Pathfinding renderer + /debugpath | both | client `MC_DEBUG_PATHFINDING` | yes | R, P |
| Structures renderer | both | client `MC_DEBUG_STRUCTURES` | yes | R, P |
| Brain renderer | both | client `MC_DEBUG_BRAIN` | part (villagers only) | R |
| POI renderer | both | client `MC_DEBUG_POI` | yes | R, P |
| Village sections renderer | both | client `MC_DEBUG_VILLAGE_SECTIONS` | yes | R |
| Bee and hive renderer | both | client `MC_DEBUG_BEES` | yes | R |
| Raid renderer + /raid | both | client `MC_DEBUG_RAIDS` (command: dev commands) | no | R, P |
| Goal selector renderer | both | client `MC_DEBUG_GOAL_SELECTOR` | yes | R |
| Breeze renderer | both | client `MC_DEBUG_BREEZE_MOB` | yes | R |
| Game event listener renderer | both | client `MC_DEBUG_GAME_EVENT_LISTENERS` | yes | R |
| Neighbor update renderer | both | client `MC_DEBUG_NEIGHBORSUPDATE` | yes | R, P |
| Redstone wire order renderer | both | client `MC_DEBUG_EXPERIMENTAL_REDSTONEWIRE_UPDATE_ORDER` | yes (with the `redstone_experiments` pack) | R, P |
| Entity-block intersection renderer | both | client `MC_DEBUG_ENTITY_BLOCK_INTERSECTION` | yes | R, P |
| Client-side visualizers (heightmap, light, water, collision, support blocks, solid faces, chunk borders, octree) | client | F3 debug options screen, no flag | n/a | D, P |
| Chunks-on-server visualizer | client | integrated server only | n/a | - |
| F3 overlay, hotkeys, charts | client | default keys | n/a | D |
| Server tick-time chart (F3+2) | both | default keys, op | yes | D |
| /chase | both | `MC_DEBUG_CHASE_COMMAND` | yes (config flag) | see chase doc |
| Dev commands: /raid /debugpath /debugmobspawning /warden_spawn_tracker /spawn_armor_trims /serverpack /debugconfig | server | `MC_DEBUG_DEV_COMMANDS` | no | P, D |
| /test (gametest) | server | always on, op | yes | P |
| /test export, create-time extras | server | needs IDE/patch | no | - |
| /debug start/stop/function | server | always on, op | no | D |
| /jfr | server | always on when JFR available, owner level | no | D |
| `--jfrProfile`, `--tracy` | both | launch argument | n/a | D |
| Gametest server entry point | server | alternate main class | n/a | P |
| Server world and gen flags (DISABLE_*, DONT_SAVE_WORLD, ...) | server | server `MC_DEBUG_*` | n/a | P, D |
| Network fake latency/jitter | server | `MC_DEBUG_FAKE_LATENCY_MS`, `..._JITTER_MS` | n/a | D |
| Behavior flags (IGNORE_LOCAL_MOB_CAP, UNLOCK_ALL_TRADES, DISABLE_LIQUID_SPREADING, ...) | server | server `MC_DEBUG_*` | n/a | P |
| Verbose logging flags (BLOCK_BREAK, VERBOSE_SERVER_EVENTS, ...) | both | `MC_DEBUG_*` | n/a | D |
| Server launch options (--demo, --safeMode, --forceUpgrade, ...) | server | program args | n/a | P |
| Client launch options (--demo, --offlineDeveloperMode, ...) | client | program args | n/a | D |
| Other system properties (`max.bg.threads`, management TLS) | server | -D | n/a | - |

## Debug subscription protocol (the key mechanism)

What it is. Since the 26.x protocol, the server-driven renderers use one
generic mechanism instead of custom payloads. There is a registry of debug
subscriptions. The client sends one serverbound packet (Debug Subscription
Request) listing the subscription ids it wants. For operators only, the server
then streams updates through four clientbound packets: a block-position value,
a chunk-position value, an entity value, and a one-shot event; plus a debug
sample packet used for the tick-time chart. Some subscriptions are temporary:
the client drops values after a fixed number of ticks (entity-block
intersections 100, neighbor updates 200, redstone wire orientation 200, game
events 60).

Registered subscriptions (value payload in parentheses):
dedicated_server_tick_time, bees (bee info), bee_hives, brains (brain dump),
breezes, goal_selectors, entity_paths (path plus max node distance),
entity_block_intersections, pois, redstone_wire_orientations, village_sections
(unit marker), raids (list of block positions), structures (bounding box plus
pieces, each piece with start flag), game_event_listeners, neighbor_updates,
game_events.

How the client requests them. On every client tick it builds the set of wanted
subscriptions from its own flags (table above) plus the tick-time sample type
when the F3 FPS charts are showing, and sends the request packet only when the
set changes. That is why the flags go on the client. The server stores the set,
and honours it only if the player is an operator (or the singleplayer owner
under the IDE flag).

Enable on a stock jar: start the vanilla client with
`-DMC_DEBUG_ENABLED=true -DMC_DEBUG_<RENDERER>=true`, join as an op. No server
flag needed against a non-vanilla server because the server side is the part
tuff would implement.

Tuff status. The serverbound request is decoded (`crates/tuff-core/src/protocol/play/serverbound.rs`,
`DebugSubscriptionRequest`, capped at 32 ids). Session handling
(`crates/tuff-core/src/world/session/packets.rs`) acts only on the dedicated server
tick time id and sends the debug sample packet (`crates/tuff-core/src/world/coordinator/tick.rs`).
The `DebugBlockValue`, `DebugChunkValue`, `DebugEntityValue` and `DebugEvent`
packet ids exist in the trace decoder only; there are no encoders. The
registry id list exists in `tuff_vanilla_data::registry::DebugSubscription`. No
parity item tracks this: the gizmos area marks everything not_applicable and
the network areas do not call out the debug packets as missing.

How tuff could use it: this is the best lever. If tuff implements the
operator gate, tracks per-player subscription sets, and writes the four value
packets, a stock vanilla client with the flags above becomes a free visual
inspector of tuff's internals (paths, structure boxes, POIs, raids, hives,
neighbor update order). Compare side by side with a vanilla server running the
same seed, using the same layout as the chase guide. Cost per renderer is small
(one encoder plus one emit site), and the work is pure additive.

## Server-driven renderers (each needs the subscription protocol)

Common to all: client only flag, op required, tuff has no sender yet. Each
renders overlays in the 3D world.

### Pathfinding (`MC_DEBUG_PATHFINDING`)
Draws each mob's current navigation path as colored nodes and lines, with open
and closed sets and node labels, plus the max node distance. Subscription
`entity_paths`. Parity use: compare node-by-node what tuff's navigation picks
against vanilla for the same mob and target. The companion server command
`/debugpath <to>` (dev commands) makes the executing mob compute a path to a
position and reports "Path not found" or "Target not reached", which gives a
deterministic trigger. Tuff: no mob pathfinding debug output; any navigation
work in the mobs areas would benefit.

### Structures (`MC_DEBUG_STRUCTURES`)
Draws structure start and piece bounding boxes in the world, with starts
highlighted. Subscription `structures`, delivered per chunk. Parity use: very
high for worldgen. Instead of reading a structure placement table, you fly
around and see where tuff thinks each structure and each piece is, next to
vanilla's, for the same seed. Related server flag
`MC_DEBUG_KEEP_JIGSAW_BLOCKS_DURING_STRUCTURE_GEN` leaves jigsaw blocks in
generated structures, which makes piece connection errors visible.

### Brains (`MC_DEBUG_BRAIN`)
Floating text over villagers and other brain mobs: name, profession, xp,
health, current activity, memories, gossip, schedule, inventory, plus the
route to a target. Subscription `brains`. Parity: villager behavior, sleep and
work schedules, gossip, breeding. Needs a brain model in tuff to feed.

### Points of interest (`MC_DEBUG_POI`)
Shows POI blocks (beds, job sites, bells) with ticket counts and the brain
renderer's ownership. Subscription `pois`. Parity: POI registration, release,
and job-site claiming; tuff has POI handling for villagers and bees, so this
verifies it directly.

### Village sections (`MC_DEBUG_VILLAGE_SECTIONS`)
Marks the 16-block sections that count as part of a village (for example
sections near a POI). Subscription `village_sections`. Parity: raid and golem
spawn conditions.

### Bees and hives (`MC_DEBUG_BEES`)
Labels bees (state, goal, flower, hive, ticks) and hives (honey level, bee
count, sedated). Subscriptions `bees`, `bee_hives`, and it also enables goal
selectors. Parity: a good fit because tuff already has bee behavior work.

### Raids (`MC_DEBUG_RAIDS`)
Draws raid center markers. Subscription `raids`. The raid logic also checks
this flag on the server for extra logging. Pair with `/raid`.

### Goal selectors (`MC_DEBUG_GOAL_SELECTOR`)
Lists each mob's active and queued goals with priority. Subscription
`goal_selectors`. Parity: compare the goal sets mob by mob.

### Breeze (`MC_DEBUG_BREEZE_MOB`)
Draws the breeze's target, jump and wind charge geometry. Subscription
`breezes`. Narrow.

### Game event listeners (`MC_DEBUG_GAME_EVENT_LISTENERS`)
Draws each game event listener's radius around sculk sensors, wardens, and
allays, and flashes game events as they are dispatched. Subscriptions
`game_event_listeners`, `game_events` (60 tick expiry). Parity: vibration
propagation, the hardest part of sculk behavior to verify blind.

### Neighbor updates (`MC_DEBUG_NEIGHBORSUPDATE`)
Flashes a box at every position that received a neighbor update, with a count.
Subscription `neighbor_updates` (200 tick expiry). Parity: very useful for
redstone and block-update-order tests, which are otherwise silent.

### Redstone wire orientation (`MC_DEBUG_EXPERIMENTAL_REDSTONEWIRE_UPDATE_ORDER`)
Shows the update direction and order used by the experimental wire evaluator.
Subscription `redstone_wire_orientations`. tuff runs the experimental
evaluator (and carries orientations on neighbour updates) when the world has
the `redstone_experiments` feature pack, and then sends each changed wire's
orientation; without the pack the feed stays empty.

### Entity-block intersection (`MC_DEBUG_ENTITY_BLOCK_INTERSECTION`)
Highlights blocks an entity overlaps for inside-block effects (cobwebs, honey,
berry bushes, powder snow) with a short expiry. Subscription
`entity_block_intersections`. Parity: collision and inside-block handling.

## Client-side visualizers (no server support)

These need no flag: they are entries in the F3 debug options screen and draw
from data the client already has, so they work against tuff today.

- Chunk borders (F3+G): section and chunk boundaries.
- Heightmap visualizer: draws the client's copy of the heightmaps from chunk
  data. Parity: shows what tuff sent in each chunk packet's heightmap
  array; any off-by-one or wrong type shows as a floating plane.
- Light visualizers (block, sky, sky light sections): draws light values per
  block and light section. Parity: compares lighting tuff sent against the
  client's own view; a good check for the light data in chunk packets.
- Water level, collision boxes, supporting blocks, solid faces: show the
  client's collision world. Useful to detect shape-sync problems and to verify
  the client's idea of the supporting block for movement checks.
- Octree and section visibility: renderer internals, low value for tuff.
- Entity hitboxes (F3+B): the client's hitboxes plus eye line and look vector.
  The flag `MC_DEBUG_SHOW_LOCAL_SERVER_ENTITY_HIT_BOXES` additionally draws the
  integrated server's hitbox, so it is only for singleplayer.
- Chunks on server visualizer: uses the integrated server, so no value against
  a dedicated server.
- `MC_DEBUG_SHAPES` draws block shapes in the level renderer for a client
  debugging pass; low value.

Tuff status: nothing to implement. Use them now to eyeball tuff's chunk, light
and heightmap data.

## F3 overlay, hotkeys and charts (client)

Default behavior, no flag. The debug modifier is F3; combinations (defaults):
B hitboxes, G chunk borders, H advanced tooltips, C copy location / hold to
crash, D clear chat, A reload chunks, T reload resources, Q help/options screen,
N spectate toggle, F4 game mode switcher, P pause on focus loss, S dump
dynamic textures, L start profiling (10 second sample), V dump version, I copy
target data, plus F3+1 profiler chart, F3+2 FPS charts (which also request the
server tick-time subscription), F3+3 network charts, F3+4 lightmap texture.
The overlay is a list of entries (position, looking-at block and fluid state
and tags, entity data, biome, light, heightmap, local difficulty, day count,
spawn counts, tps, memory, chunk stats, sound mood, and so on) selectable on
the debug options screen. Extra keybinds appear when `MC_DEBUG_HOTKEYS` is set
(chunk section path, octree and visibility toggles) and `MC_DEBUG_FEATURE_COUNT`
(feature statistics dump key). `MC_DEBUG_SHOW_SERVER_DEBUG_VALUES` makes the
light, biome and looking-at entries also show the integrated server's view,
which only works in singleplayer.

Tuff status: F3 works with any server; the tick-time chart is supported by
tuff (tuff sends the debug sample packet when the tick-time subscription is
requested). Several F3 entries (tps, entity counts) rely on server data tuff
may not send.

## Server tick-time chart

F3+2 enables a subscription that asks the dedicated server for per-tick
samples broken into dimensions (tick, scheduled tasks, idle, and more). Server
side it needs op. Tuff: implemented (see above), used by `crates/tuff-bot`; the
dimension breakdown could be checked against vanilla's for parity of the data
shape.

## Commands

### /chase (`MC_DEBUG_CHASE_COMMAND`)
Records and replays player movement between two server processes. See
[chase-side-by-side.md](chase-side-by-side.md). Tuff: no.

### Dev commands (`MC_DEBUG_DEV_COMMANDS` or the IDE flag)
All registered together when either gate is on. The first six need the
gamemaster level; /raid and /debugconfig need admin. /debugconfig registers
only on a dedicated server.

- `/raid`: start (with omen level), stop, check, sound, spawnleader, setomen,
  glow. Builds and manipulates raids in the local village; reports the raid
  state. Parity value: high, gives a deterministic way to start a raid and
  advance waves. Tuff: no raid command.
- `/debugpath <to>`: makes the executing mob compute a path and reports
  success, partial or failure. Parity: high with the pathfinding renderer.
- `/debugmobspawning <category> <pos>`: forces a spawn attempt for a mob
  category at a position. Parity: medium, shows spawner rules for one cell.
  Tuff: `tuff.features.debugmobspawning-command.enabled`.
- `/warden_spawn_tracker clear | set <0-4>`: reads or sets the player's
  warden warning level and cooldown. Parity: medium, deterministic warden
  tests. Tuff: `tuff.features.warden-spawn-tracker-command.enabled`.
- `/spawn_armor_trims`: spawns a grid of armor stands wearing every trim and
  material combination (a `*_lag_my_game` variant spawns all). Parity: visual
  only; no value for tuff beyond equipment sync, so tuff does not add it.
- `/serverpack push|pop <uuid>`: pushes or pops server resource packs for
  testing pack handling. Parity: tuff has its own pack flow; medium-low.
- `/debugconfig`: sends a player back to the configuration phase (vault
  config reset and dialog variants) to test the config-phase re-entry. Parity:
  high for the configuration state machine: re-entering config from play is a
  hard client-visible transition tuff must handle.

Tuff status: none of these exist in the command registry
(`crates/tuff-feature-builtin-core-commands`); the registered vanilla commands are op, deop,
whitelist, ban, banlist, pardon, stop and perf. The vanilla-parity commands
area covers the standard commands. To use them for parity, run the dev
commands on the vanilla side only; tuff only needs them if you want to drive
the same scenario on tuff.

### /test (gametest framework)
Always registered, gamemaster level. Runs, locates, resets and clears
structure-based tests in the world (run, runmultiple, runthese, runclosest,
runthat, runfailed, locate, reset*, clear*, stop, pos, create). `export*`
subcommands (writing a test structure back out) appear only with the IDE flag.
Tuff: no. The parity area lists the framework; the value is the tests
themselves.

### Gametest main
A separate entry point, not the normal server main. It takes `--universe`,
`--report <xml path>` (JUnit style), `--tests`, `--verify`, `--repeatCount` and
`--packs`, runs the built-in test instances, then exits. Parity value: high.
The vanilla test instances are a ready-made behavioral spec (redstone, mob
interactions, block behaviors) whose pass criteria could be reused as
conformance scenarios against tuff, either by reading the test definitions or
by replaying their setups. Launch with the server jar on the classpath and
the gametest main class instead of the normal one.

### /debug start | stop | function
Starts and stops the server tick profiler and writes a profile file; the
`function` form traces a datapack function's executed commands to a file.
Always available at admin level. Tuff: no; the function trace is useful for
datapack parity but low priority.

### /jfr start | stop
Starts a Java Flight Recorder capture and reports the file. Registered only if
the JVM's recorder is available, and needs the owner level. Not useful for
tuff directly (it profiles the vanilla JVM); useful when profiling vanilla for
baseline numbers.

## Launch arguments and hidden options

Server (program arguments): `nogui`, `--initSettings` (write properties and
EULA files and exit), `--demo`, `--bonusChest`, `--forceUpgrade` (rewrite all
chunks to the current data version), `--eraseCache`, `--recreateRegionFiles`,
`--safeMode` (vanilla datapack only), `--universe`, `--world`, `--port`,
`--serverId`, `--jfrProfile`, `--pidFile`, `--help`. Of these:

- `--demo` makes the server run in demo mode: fixed short play time with a
  demo gamemode. Parity: only for the demo behavior itself; no value unless
  the project wants it.
- `--forceUpgrade` and `--recreateRegionFiles` use the datafixer to rewrite a
  whole world. Parity value: high for chunk and region IO: run vanilla over a
  tuff-written world to see whether vanilla accepts and preserves it.
- `--safeMode`, `--bonusChest`: low.
- `--jfrProfile`: starts a JFR recording at launch. Pair with the world-load
  flag below.

Client (program arguments): `--demo`, `--disableMultiplayer`, `--disableChat`,
`--fullscreen`, `--checkGlErrors`, `--renderDebugLabels`, `--vulkanValidation`,
`--jfrProfile`, `--tracy` / `--tracyNoImages` (Tracy profiler zones),
`--quickPlay*` (join a server or world straight from launch),
`--offlineDeveloperMode`, `--username`, `--uuid`, `--accessToken`, `--gameDir`,
and others. The ones that matter to tuff:

- `--quickPlayMultiplayer <host>` joins a server at launch: already the basis
  of the client harness; check `crates/tuff-client-harness` for its usage.
- `--offlineDeveloperMode` and `--username`: let a test client connect in
  offline mode with a chosen name. Already used by the harness style.
- `--demo`: client side of demo mode; ignore.

`server.properties` has no debug keys of note beyond `enable-jmx-monitoring`
(registers JMX tick statistics beans), `sync-chunk-writes`, `max-tick-time`
(the watchdog), `region-file-compression`, and the management server keys.
System properties other than the debug flags: `max.bg.threads` caps the
background worker pool; the management server TLS keystore password can be
given as `management.tls.keystore.password` or an environment variable.
Tuff: the worker-pool cap is not needed.

## Server and worldgen debug flags

All need the master switch. These change vanilla behavior to isolate stages,
so they are parity tools: generate the same seed with a stage turned off on
both servers and diff only that stage.

- Generation stage toggles: `MC_DEBUG_DISABLE_SURFACE`, `..._CARVERS`,
  `..._STRUCTURES`, `..._FEATURES`, `..._ORE_VEINS`, `..._BLENDING`,
  `..._FLUID_GENERATION`, `..._AQUIFERS` (declared but unused in the server
  code path searched), `..._BELOW_ZERO_RETROGENERATION` (datafixer).
  Parity: high for worldgen. Each isolates one stage so a diff against tuff's
  output pins the broken stage. tuff has the matching switches as config keys
  (all off by default, applied on restart): `tuff.worldgen.debug.disableSurface`,
  `...disableCarvers`, `...disableStructures` (no structure starts, and
  locate finds nothing), `...disableFeatures` (no decoration, and locate
  finds nothing), `...disableOreVeins`, `...disableBlending` (accepted but a
  no-op: tuff never blends with old chunks) and `...disableFluidGeneration`
  (aquifers and the sea fill become air).

  Running a matched pair: create two fresh worlds with the same seed and
  `level-type`. Start vanilla with the JVM flags before `-jar`, for example
  `java -DMC_DEBUG_ENABLED=true -DMC_DEBUG_DISABLE_SURFACE=true -jar
  server.jar nogui`. Start tuff with the same stage off, in
  `tuff.properties` (`tuff.worldgen.debug.disableSurface = true`) or with
  `--config tuff.worldgen.debug.disableSurface true`. Pre-generate the same
  area on both (fly over it, or `/chase` side by side, see
  chase-side-by-side.md) and diff the region files. Switch one stage off at a
  time; with a stage off on only one side every chunk differs.
- `MC_DEBUG_ONLY_GENERATE_HALF_THE_WORLD` (with `MC_DEBUG_WORLD_RECREATE`):
  restricts generation to one half and moves the initial spawn; for speeding
  world recreation tests. Low.
- `MC_DEBUG_AQUIFERS`, `MC_DEBUG_ORE_VEINS`, `MC_DEBUG_LARGE_DRIPSTONE`,
  `MC_DEBUG_SCULK_CATALYST`: extra output or visualization hooks inside those
  features. Medium for worldgen parity if you need the intermediate values.
- `MC_DEBUG_ENABLE_FARLANDS`: restores the old noise precision edge so the
  Far Lands generate. Low.
- `MC_DEBUG_DONT_SAVE_WORLD`: server never writes chunks or level data.
  Medium: lets a vanilla server be restarted many times on one world copy
  without mutation, ideal for repeatable parity runs.
- `MC_DEBUG_FAKE_LATENCY_MS`, `MC_DEBUG_FAKE_JITTER_MS`: delay packets on the
  integrated server's in-memory connection only (not TCP). Singleplayer only;
  no value for a dedicated tuff comparison.
- `MC_DEBUG_IGNORE_LOCAL_MOB_CAP`: removes the per-player mob cap in natural
  spawning. Medium: stress or isolate spawn rules.
- `MC_DEBUG_DISABLE_LIQUID_SPREADING`: fluids do not spread. Low-medium,
  isolates fluid behavior.
- `MC_DEBUG_UNLOCK_ALL_TRADES`: villagers show every trade. Medium for trade
  table parity.
- `MC_DEBUG_TRIAL_SPAWNER_DETECTS_SHEEP_AS_PLAYERS`,
  `MC_DEBUG_VAULT_DETECTS_SHEEP_AS_PLAYERS`: treat sheep as players so these
  blocks can be tested without a player. Low.
- `MC_DEBUG_MONITOR_TICK_TIMES`: logs per-tick profiles for slow ticks.
  Low-medium for baseline numbers.
- `MC_DEBUG_JFR_PROFILING_ENABLE_LEVEL_LOADING`: records JFR during level load.
  Low.
- `MC_DEBUG_VERBOSE_SERVER_EVENTS`: logs chunk tickets, chunk sender batches,
  light engine and task dispatcher events. High for chunk-sending parity: log
  vanilla's chunk ticket and send ordering and compare with tuff's. Pair with
  `MC_DEBUG_BLOCK_BREAK` (logs server ack of block breaking actions).
- `MC_DEBUG_NAMED_RUNNABLES`, `MC_DEBUG_VERBOSE_COMMAND_ERRORS`,
  `MC_DEBUG_SAVE_STRUCTURES_AS_SNBT`, `MC_DEBUG_STRUCTURE_EDIT_MODE`,
  `MC_DEBUG_VALIDATE_RESOURCE_PATH_CASE`, `MC_DEBUG_OPEN_INCOMPATIBLE_WORLDS`
  (open worlds from newer versions), `MC_DEBUG_ALLOW_LOW_SIM_DISTANCE`
  (allow simulation distance below the normal minimum): developer
  conveniences, low value except SNBT structure saving, which gives readable
  templates for comparing tuff's structure placement inputs.
- `MC_DEBUG_CALCULATE_SOLID` (client): recomputes face-solid data, a
  consistency check for shape code.

Client-only presentation flags (UI narration, shuffle UI/model order, UI
layering rectangles, cursor position, active text areas, subtitles debug,
default skin override, panorama screenshot, social interaction and chat
restrictions, telemetry, realms version check, GL log sync, onboarding screen,
texture atlas dump, library load failure simulation): no value for tuff.
`MC_DEBUG_DUMP_TEXTURE_ATLAS` and the F3+S dynamic texture dump are only for
rendering work.

## Recommended for tuff (ranked)

1. Implement the debug subscription value and event packets (operator gate,
   per-player subscription sets, encoders for block, chunk, entity and event
   packets) and feed them first from structures, then neighbor updates, then
   pathfinding and POIs. Highest leverage: a stock vanilla client with
   `-DMC_DEBUG_ENABLED=true -DMC_DEBUG_STRUCTURES=true` (and the others) shows
   tuff's internal state with no new client code. Start with `structures` and
   `neighbor_updates` since tuff already computes both.
2. Use the existing client-side visualizers now (heightmap, light, collision,
   chunk borders, supporting blocks, hitboxes) when diffing chunk and light
   data against vanilla; free, needs no tuff change.
3. Run the vanilla server with the worldgen stage flags
   (`DISABLE_SURFACE`, `DISABLE_FEATURES`, `DISABLE_CARVERS`,
   `DISABLE_STRUCTURES`, `DISABLE_ORE_VEINS`) and `MC_DEBUG_DONT_SAVE_WORLD`
   to produce stage-isolated, repeatable reference output for worldgen
   parity; mirror the stage switches in tuff behind a config for direct diffs.
4. Use `MC_DEBUG_VERBOSE_SERVER_EVENTS` and `MC_DEBUG_BLOCK_BREAK` on vanilla
   to log chunk ticket, chunk send and block-break ack ordering, and compare
   with tuff's trace.
5. Dev commands on the vanilla side with the dev command flag: `/raid`,
   `/debugpath`, `/warden_spawn_tracker`, `/debugmobspawning` as deterministic
   triggers when comparing behaviors against tuff.
6. Gametest main: mine the built-in test instances as conformance scenarios
   and, if worthwhile, port the pass conditions to tuff tests.
7. `/debugconfig` on vanilla to watch the exact packet sequence for re-entering
   configuration; implement the same transition in tuff (play to config) and
   cover it with a protocol test.
8. `--forceUpgrade` on tuff-written worlds as a vanilla acceptance check of
   region and chunk output.
9. Chase side-by-side as already documented.

Not worth tuff time: demo mode, fake latency (integrated server only), the
presentation flags, `/spawn_armor_trims`, `/jfr`, Tracy and JFR launch args
(these profile vanilla, not tuff; profile tuff with samply).
