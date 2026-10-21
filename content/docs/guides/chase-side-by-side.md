---
title: /chase side-by-side compare
description: "## What /chase does"
group: Guides
source: docs/guides/chase-side-by-side.md
---

## What /chase does

/chase is a developer command in the vanilla server. One server "leads":
it listens on a TCP port and streams the position of its FIRST online
player (the first entry of the player list, so use a single player per
server). Another server "follows": it connects to that port and teleports
its own first online player to every position it hears. Walk on the
leader and the follower's player is moved to the same spot, so two
clients (one per server) look at the same place and you can compare
terrain, structures, lighting and entities by eye.

Wire protocol (vanilla, plain TCP, no handshake, no auth, no encryption):

- The leader sends one ASCII line per change: `t <dim> <x> <y> <z> <yaw> <pitch>\n`.
- `<dim>` is `o` (overworld), `n` (nether) or `e` (end). Any other
  dimension is not sent at all.
- The five numbers are written with 2 decimals, `.` as separator.
- The leader checks every 100 ms and only sends when the position,
  rotation or dimension changed since the last send.
- The follower reads lines, ignores unknown message types, and parses the
  coordinates as 32-bit floats. For each `t` line it runs, as its first
  player with owner permission, `execute in <dimension> run tp @s x y z yaw pitch`
  (3 decimals). So dimension changes are followed too.
- The follower reconnects every 5 seconds if the leader is down or the
  connection drops.
- Defaults: leader binds `0.0.0.0`, follower connects to `localhost`, port
  `10000`. Lead port range 1024-65535, follow port 1-65535.

Subcommands:

- `/chase lead [<bind_address> [<port>]]`
- `/chase follow [<host> [<port>]]`
- `/chase stop` (stops whichever of lead/follow is running)

A server can run only one of lead or follow at a time.

## Enabling it on vanilla

Vanilla only registers /chase when two JVM system properties are set. A
stock dedicated server jar can do this with no patching. Both are read
when the server starts (commands are built at startup), so they must be
on the command line before `-jar`:

    java -DMC_DEBUG_ENABLED=true -DMC_DEBUG_CHASE_COMMAND=true -jar server.jar nogui

`MC_DEBUG_ENABLED` is the master switch for all debug flags; without it
`MC_DEBUG_CHASE_COMMAND` is ignored. The "running from an IDE" check is
NOT needed for /chase (it gates other dev commands). Optionally add
`-DMC_DEBUG_PRINT_PROPERTIES=true` to print available debug flags at
startup.

Then in the server console or as an op (level 4 is fine) in game:
`/chase lead` or `/chase follow localhost 10000`.

## Enabling it on tuff

/chase is off by default. Turn it on with the config key
`tuff.features.chase-command.enabled = true` (in `tuff.properties`, with
`--config tuff.features.chase-command.enabled true`, or with `/tuff config`).
While it is off the command is not registered at all. It needs owner
permission (op level 4, or the console). Tuff binds the socket only while a
lead is running, on IPv4 or IPv6.

Wire compatibility: tuff uses vanilla's line format (`t o|n|e x y z yaw pitch`),
the same 100 ms interval, default host, bind address and port, 5 s reconnect,
the same `execute in <dim> run tp @s ...` application, and the same port
ranges and messages, so vanilla and tuff can lead and follow each other in
either direction. This has been checked by reading the code but not yet
against a running vanilla server.

## Side-by-side compare, vanilla vs tuff

1. Pick one seed and use it on both. Set `level-seed=<seed>` in
   `server.properties` of both, with a fresh world directory on each. Keep
   other worldgen settings equal (`level-type`, `generate-structures`,
   `difficulty`, `gamemode`).
2. Run the two servers on different game ports, e.g. vanilla 25565 and
   tuff 25566 (set `server-port` / tuff's port setting). Do not use the
   port tuff tests avoid (25565) if another server already holds it.
3. Offline mode on both: `online-mode=false`. This lets two clients with
   different usernames join from one machine without Mojang accounts
   clashing. Launch two vanilla clients with distinct usernames (the
   client harness, `cargo test -p tuff-client-harness --test client`
   style runs, uses offline mode and limits real clients to 4 machine-wide
   via its slot locks; for manual runs use two launcher profiles or a
   launcher that supports offline names). Each server must have exactly
   ONE player online, because chase uses the first player only.
4. Start both servers, join client A to the leader and client B to the
   follower. Op both players (`op <name>` in each console) or run the
   chase commands from the console where supported.
5. Choose the leader. Prefer vanilla as leader: it is the reference, you
   fly around it freely, and tuff chases it. On vanilla run `/chase lead`;
   on tuff run `/chase follow localhost 10000`. (Swap roles if you want
   the tuff player to drive.)
6. Fly with client A in creative (`/gamemode creative`, double-tap jump).
   Move slowly: the follower teleports into chunks that may not be
   generated yet, and the teleport is applied every 100 ms at most.
   Generation speed matters more than position fidelity.
7. Look at both windows side by side. Tips for spotting worldgen
   differences:
   - Press F3 on both to compare coordinates, biome and chunk coordinates.
   - Compare at spawn first, then along a straight line, then at biome
     borders, rivers, caves, ocean monuments, villages, strongholds.
   - Use `/locate structure <id>` on both, then `/tp` the leader to the
     result; a differing location points at a structure placement bug.
   - Use `/locate biome` and `/fillbiome`-free checks (F3 biome line).
   - Turn off fancy rendering and set the same render distance; compare
     with the F3+G chunk border overlay to spot a missing or partial chunk.
   - Take paired screenshots from identical positions; the follower's yaw
     and pitch follow the leader, so the framing matches.
   - Check the nether (`e`/`n` are followed too) and the end by moving
     the leader through a portal; the follower switches dimension itself
     but does not use the portal, so its portal-linked areas will not
     exist (see limitations).
8. `/chase stop` on both when finished.

## tuff vs tuff (two builds)

Run build A and build B as separate tuff processes with different game
ports and different world directories but the same seed and config. Make
A lead (`/chase lead`, default port 10000) and B follow
(`/chase follow localhost 10000`). If both run on one machine this works
as-is; to chase across machines use `/chase lead 0.0.0.0 <port>` and
`/chase follow <ip> <port>`. Use this to compare a worldgen change before
and after, or a branch against main.

## Known limitations

- Only the first online player on each side is used. Do not join extra
  players.
- Only teleports position, rotation and dimension. No inventory, game mode,
  time, weather, mob or block state is synchronized; world time and mob
  spawns differ between servers.
- The follower does not walk through portals; nether and end positions are
  teleports, so portal-generated structures differ.
- Dimension names other than the three vanilla ones are never sent
  (custom dimensions are invisible to chase).
- Single leader at a time per follower; no authentication, so bind to
  localhost (`/chase lead 127.0.0.1`) on shared networks. Anyone who can
  connect can only read your position, but anyone who can reach a follower
  port you point at a hostile host can teleport your first player.
- The follower parses coordinates as 32-bit floats, so far from the
  origin (beyond roughly one million blocks) positions lose precision.
- Lines are sent only on change, at most every 100 ms, so the follower may
  lag a little on fast movement.
- Vanilla needs the JVM flags at every launch; there is no runtime toggle.
