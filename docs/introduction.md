# Introduction

A Minecraft Java Edition server written in Rust, for game version 26.3.
The vanilla game runs as a set of plugins on an inert core, and external
plugins are sandboxed WebAssembly modules. See `CONTRIBUTING.md` for
development guidelines.


## Release notes: v0.1.0-alpha.2

Released on 2026-10-28. The support statement below for alpha.1 still
holds: build from source, worlds from 1.13 or newer, Linux and macOS
supported (Windows best effort), anti-cheat logs only by default, and the
admin panel needs a reverse proxy for remote access. Full key tables are in
`docs/settings.md`.

### Behaviour changes to know before upgrading

- Packet flood limits are on by default: `rate-limit` is now `400` packets
  per second (it was off). Excess recipe placement, command suggestion, book
  edit and sign update packets are dropped without a kick. Set `rate-limit`
  to `0` to turn all of it off.
- Each player now has a chunk budget: `tuff.world.chunkBudgetPerPlayer`
  defaults to `64` undelivered chunk requests (`0` is unlimited). Fast
  travel no longer queues the whole view at every stop.
- Item data shown to other players is stripped: equipment and item metadata
  sent to viewers drop container, bundle, charged projectile, book and
  template components and any component over 4 KiB, and oversized block
  entity data in chunk packets is replaced by an empty compound. A holder's
  own slots are unchanged.
- Entities of one type are capped per chunk on save and load:
  `tuff.world.entityCaps.item` (1024), `.experience_orb`, `.arrow` (256)
  and `.snowball` (128); `0` is unlimited.
- Saves are refused if they would break item conservation, and the save
  journal is recovered before offline tools open a world.
- Animals, water mobs, horses, donkeys and llamas despawn like vanilla.

### Security

- Packet flood limits on by default (above), and block corrections no
  longer leak the true block.
- Item data stripped for other players (above).
- Item and block duplication prevention: items are move-only stacks through
  inventories, menus, hoppers, dispensers, projectiles, mobs, plugins and
  chunk unloads; falling blocks and endermen own the block they carry;
  entity UUIDs are checked on arrival; trades are paid at the click and menu
  contents drop on death. Each player's data is leased to one session at a
  time, and a session crashing while leaving no longer loses items in
  flight.
- Anti-cheat: the baseline movement and reach checks always run, floating is
  not counted while nearby chunks are unknown, and
  `tuff.anticheat.kickOperators` (default `false`) keeps checked operators
  from being kicked. Anti-xray edge ores stay hidden until the cross-region
  halo arrives.

### Settings and defaults

New opt-in keys (all default to vanilla behaviour):

- `tuff.hidePlayerDetails.health` and `tuff.hidePlayerDetails.items` hide
  other players' health and item details.
- `tuff.mobs.spawnCap.perPlayer` caps natural spawns per player;
  `tuff.mobs.collisionCap` limits collisions per mob.
- Despawn, merge radius and lifetime settings (for example
  `tuff.world.item.mergeRadius` and `tuff.world.experience.mergeRadius`),
  an optional void damage height per dimension, configurable scheduled tick
  caps and fire delay, and the item frame map cursor interval.
- The duplication glitches vanilla has are kept and can be switched off:
  `tuff.blocks.tntDuplication`, `tuff.blocks.carpetRailDuplication`,
  `tuff.blocks.gravityBlockPortalDuplication` and
  `tuff.blocks.permanentBlockBreaking` (default `true`).
- The settings table now documents the anti-xray, login, backup and
  Velocity keys.

### Fixes

- Piston and rail dupers fixed; rails check support on neighbour updates;
  candles keep their drop when support is removed.
- Menus: stale station menus close, replaced station results no longer leak
  items, and unloaded containers refuse closed windows.
- `/tp` of a mob into an unloaded region lands correctly; audible and
  global events are scoped by range and dimension like vanilla; a player's
  state is reported before joining the roster.
- Storage: save journal syncs are grouped, chunk writes are batched and
  block entity items are released when a chunk unloads.
- Admin panel: the login command prints a clickable link; a live panel
  removal returns once the view has it.
- macOS: dylib plugin features use the correct library naming.
- Stalled region actors and the coordinator are now reported.

### Plugins and bots

- Plugin authors (WIT, not stable): new `spawn-custom-entity` call and a
  `custom` field on entity interaction and damage events, so interaction
  parts route to their custom entity; a `stash` handle type with
  `stash-new`, `stash-mint`, `stash-contents`, `stash-split`,
  `stash-merge`, `stash-burn`, `give-stash` and `take-into-stash` for holding
  items (stacks can be made, moved and used up but never copied; a stash
  left over at unload is used up); `item-stack` is a new type; entity
  lookups fail with `unavailable` when a region could not be asked.
  Regenerate bindings and update guests. The custom entity example moved to
  `rust-sentry`.
- Bots: no more hopping on farmland, parkour take-offs capped at one sprint
  tick, elytra flights land before `goto`, and bots keep fighting when a
  target lookup is unknown.

### Tooling

- `scripts/install.sh` honours `CARGO_TARGET_DIR`.
- Benches run tuff and vanilla with identical settings.
- Test harness additions: latency proxy and generic corpus replay, real
  client test for custom entity stand-ins, expanded vanilla comparison
  layouts for duplication and portals.

## Release notes: v0.1.0-alpha.1

The first public alpha is a drop-in survival server for 26.3 with
sandboxed WASM plugins, released on 2026-10-21. Its support statement:

- tuff publishes no prebuilt binaries. You build the server on your own
  machine (see "Installing"), because the build downloads Mojang's server
  jar and embeds data generated from it.
- Worlds whose data is from Minecraft 1.13 (data version 1519) or newer
  load and are upgraded. Older data is refused as too old to upgrade.
- Linux and macOS are the supported platforms. Windows is best effort:
  build and run under WSL or Docker.
- Anti-cheat logs only by default; it does not kick or set players back
  until you change its settings.
- The admin panel has no TLS or remote hardening of its own. Keep it on
  localhost and put a reverse proxy (with TLS and authentication) in front
  for remote access.
- Report bugs with the server log and, if possible, the steps to repeat
  them.
