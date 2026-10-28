# Introduction

A Minecraft Java Edition server written in Rust, for game version 26.3.
The vanilla game runs as a set of plugins on an inert core, and external
plugins are sandboxed WebAssembly modules. See `CONTRIBUTING.md` for
development guidelines.


## Release notes: v0.1.0-alpha.2 (unreleased)

The next alpha, undated until it is tagged. The support statement below
for alpha.1 still holds: build from source, worlds from 1.13 or newer,
Linux and macOS supported (Windows best effort), anti-cheat logs only by
default, and the admin panel needs a reverse proxy for remote access.

What changed since alpha.1:

- Item and block duplication prevention. Items are now tracked as
  move-only stacks through inventories, menus, hoppers, dispensers,
  projectiles, mobs, plugins and chunk unloads, and falling blocks and
  endermen own the block they carry. Each world save is written whole and
  refused if it would break item conservation, with the save journal
  recovered before offline tools open a world.
- Vanilla duplication glitches are kept on purpose and can be switched off:
  `tuff.blocks.tntDuplication`, `tuff.blocks.carpetRailDuplication`,
  `tuff.blocks.gravityBlockPortalDuplication` and
  `tuff.blocks.permanentBlockBreaking` (default `true`; see
  `docs/settings.md`). Other piston and rail dupers are fixed.
- Menus and trades: trades are paid at the click, menu contents drop on
  death, stale station menus close, and replaced station results no longer
  leak items.
- Mobs: animals, water mobs, horses, donkeys and llamas now despawn the way
  vanilla does.
- Anti-cheat: new `tuff.anticheat.kickOperators` (default `false`) so
  checked operators are never kicked; the baseline movement and reach checks
  always run; floating is not counted while nearby chunks are unknown.
- World and sessions: each player's data is leased to one session at a time,
  a session crashing while leaving no longer loses items in flight, audible
  and global events are scoped by range and dimension like vanilla, and
  block corrections no longer leak the true block.
- Storage and performance: save journal syncs are grouped, chunk writes are
  batched, and ownership accounting stays off the hot path.
- Admin panel: the login command prints a clickable link to the panel.
- macOS: dylib plugin features use the correct library naming.

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
