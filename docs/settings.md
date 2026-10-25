# Settings

Server settings outside `server.properties` are `tuff.*` keys, including
those that plugins declare. `/tuff config` lists the `tuff.*` keys,
`/tuff config <key>` shows a key's value, kind, default and description,
and `/tuff config <key> <value>` sets it (`""` sets an empty value). A set
is saved to `tuff.properties` in the instance directory, or to the server
file with the sqlite backend. Only values that differ from the default are
saved; comments and blank lines in `tuff.properties` are kept, and lines
for keys this version no longer defines are dropped on the next write.

Each `/tuff` subcommand has its own permission node (`tuff.command.config`,
`tuff.command.plugin`, `tuff.command.restart`, `tuff.command.chunk` and so
on). A node is held by operators at level 4 by default and can be granted
to other players; the console holds every node.

Roles (`/tuff perm role create|delete|grant|deny|revoke|add|remove|priority|list`)
share node settings among their members; every player is in the `default`
role when it exists. A role can also decorate its members' names in chat
and the player list: `/tuff perm role set <role> prefix|suffix|name_color|display_name <value>`
(`clear` removes one). Text values use
[MiniMessage](https://docs.advntr.dev/minimessage/format.html) markup;
the panel also previews `&` and section-sign colour codes. The panel's
Permissions page edits roles, their attributes and node settings.

`server.properties` keys use the `minecraft.` prefix with the same command:
`/tuff config minecraft.hardcore` shows the value, default and effect,
`/tuff config minecraft.hardcore true` sets it and saves
`server.properties`, and `/tuff config minecraft.hardcore reset` restores
the default (`reset` exists only for `minecraft.*` keys). A key this
version does not define is refused unless it is already in the file, where
it is kept and shown as such. Secret values such as `rcon.password` are
shown as `<set>` or `<unset>`. `minecraft.*` changes are recorded in the
web panel's audit log, with secrets redacted; `/tuff config` changes to
`tuff.*` keys are not. The web panel edits the same values.

`--config` overrides a key for one run. It takes a `tuff.*` key, or a
`server.properties` key as `minecraft.<key>` or the bare vanilla spelling
(`--config minecraft.pvp false` or `--config pvp false`). The value is
checked against the same key list and types, and a bad one stops start-up.
`-Dkey=value` is the same thing (`./tuff -Dtuff.web.enabled=true
-Dmax-players=50`). An override is never written back, to
`tuff.properties`, `server.properties` or the server file: saves keep what
the file held. The exception is the world-creation keys (`level-seed`,
`level-type`, `generate-structures`, `generator-settings`): they apply when
a new world is created and are stored with that world; for an existing
world they are ignored and start-up logs a note saying so. Setting an
overridden key at runtime (`/tuff config set`, `/properties`) replaces the
override and saves the new value as usual. Error messages never print the
value.

Every `server.properties` key has one of five effects, shown by the
command, its tab completion and the panel:

- live: applies at once, without a restart.
- restart: saved, and read only when the server starts, for the reason
  given.
- creation: saved, but read only when a world is created. Setting one on
  an existing world, with the command, the panel or by editing the file
  before a start, says so, for example "level-seed only applies when a new
  world is created; the current world keeps seed N".
- permanent: kept in the file and never acted on; not planned.
- unsupported: kept in the file but not acted on yet. No key has this
  effect today.

| Key | Effect | When or why |
| --- | --- | --- |
| `accepts-transfers` | live | next connection |
| `allow-flight` | live | checked on every move |
| `broadcast-console-to-ops` | live | |
| `broadcast-rcon-to-ops` | permanent | remote console is not planned |
| `bug-report-link` | live | players who join from now on |
| `chat-spam-threshold-seconds` | live | online players and later joins |
| `command-spam-threshold-seconds` | live | online players and later joins |
| `difficulty` | live | world difficulty, unless locked (hardcore) |
| `enable-code-of-conduct` | live | players who join from now on; refused without a `codeofconduct` folder |
| `enable-jmx-monitoring` | permanent | there is no JVM to monitor |
| `enable-query` | restart | the query socket is bound when the server starts |
| `enable-rcon` | permanent | remote console is not planned |
| `enable-status` | live | next server list ping |
| `enforce-secure-profile` | restart | players negotiate signed chat when they log in |
| `enforce-whitelist` | live | kicks online players the whitelist no longer admits |
| `entity-broadcast-range-percentage` | live | online players see entities by the new range |
| `feature-packs` | restart | feature flags shape the registries every client receives at login |
| `force-gamemode` | live | online players are put in the default game mode |
| `function-permission-level` | live | functions are recompiled with the new level |
| `gamemode` | live | new players start in it; with `force-gamemode` online players too |
| `generate-structures` | creation | read only when a new world is created |
| `generator-settings` | creation | read only when a new world is created |
| `hardcore` | live | online players get the hardcore flag and a locked hard difficulty |
| `hide-online-players` | live | next server list ping |
| `initial-disabled-packs` | creation | read only when a new world is created |
| `initial-enabled-packs` | creation | read only when a new world is created |
| `level-name` | creation | picks the world when the server starts |
| `level-seed` | creation | read only when a new world is created |
| `level-type` | creation | read only when a new world is created |
| `log-ips` | live | next log line |
| `management-server-allowed-origins` | permanent | the management server is not planned; the admin panel covers its uses |
| `management-server-enabled` | permanent | the management server is not planned; the admin panel covers its uses |
| `management-server-host` | permanent | the management server is not planned; the admin panel covers its uses |
| `management-server-port` | permanent | the management server is not planned; the admin panel covers its uses |
| `management-server-secret` | permanent | the management server is not planned; the admin panel covers its uses |
| `management-server-tls-enabled` | permanent | the management server is not planned; the admin panel covers its uses |
| `management-server-tls-keystore` | permanent | the management server is not planned; the admin panel covers its uses |
| `management-server-tls-keystore-password` | permanent | the management server is not planned; the admin panel covers its uses |
| `max-chained-neighbor-updates` | live | |
| `max-players` | live | next login and server list ping |
| `max-tick-time` | permanent | no tick watchdog stops the server |
| `max-world-size` | live | world border limit, resent to online players |
| `motd` | live | next server list ping |
| `network-compression-threshold` | live | next connection |
| `online-mode` | restart | it changes the login handshake and how player ids are made |
| `op-permission-level` | live | operators added from now on |
| `pause-when-empty-seconds` | live | counted from the next empty tick |
| `player-idle-timeout` | live | online players and later joins |
| `pre-login-max-connections` | restart | admission limits are read when the server starts |
| `pre-login-max-connections-per-ip` | restart | admission limits are read when the server starts |
| `pre-login-timeout-seconds` | restart | admission limits are read when the server starts |
| `prevent-proxy-connections` | live | next online-mode login |
| `pvp` | live | sets the pvp game rule; players are spared player damage when off |
| `query.port` | restart | the query socket is bound when the server starts |
| `rate-limit` | live | online players and later joins |
| `rcon.password` | permanent | remote console is not planned |
| `rcon.port` | permanent | remote console is not planned |
| `region-file-compression` | permanent | region files are always written with deflate |
| `require-resource-pack` | live | players who join from now on |
| `resource-pack` | live | players who join from now on |
| `resource-pack-id` | live | players who join from now on |
| `resource-pack-prompt` | live | players who join from now on |
| `resource-pack-sha1` | live | players who join from now on |
| `server-ip` | restart | the game listener is bound when the server starts |
| `server-port` | restart | the game listener is bound when the server starts |
| `simulation-distance` | live | resent to online players; regions tick by it |
| `spawn-protection` | live | next block change |
| `status-heartbeat-interval` | permanent | the management server is not planned; the admin panel covers its uses |
| `sync-chunk-writes` | permanent | the storage backends choose their own durability |
| `text-filtering-config` | restart | the text filter is created when the server starts; only `text-filtering-version` 0 is supported, and a failing service hides the text |
| `text-filtering-version` | restart | the text filter is created when the server starts; version 1 needs a Microsoft sign-in and is not supported |
| `use-native-transport` | permanent | tokio always uses the native transport |
| `view-distance` | live | resent to online players, who load or drop chunks |
| `white-list` | live | next login; with `enforce-whitelist` kicks online players |

The HTTP server that serves resource packs and the web panel binds
`tuff.http.host` (default `0.0.0.0`) on `tuff.http.port` (default `0`, a
free port chosen at start-up). The panel has its own, stricter access
rules; see Web admin panel.

## Settings and vanilla entries

A `tuff.*` setting exists only where vanilla has no equivalent; anything a
`server.properties` entry or a game rule controls uses that entry. These
tuff keys sit next to a vanilla entry and cover something different:

| Key | Vanilla neighbour | Why it stays |
| --- | --- | --- |
| `tuff.http.host`, `tuff.http.port` | `server-ip`, `server-port` | A separate HTTP listener, not the game port |
| `tuff.net.maxOutboundBytes` | `rate-limit` | A cap on queued outbound bytes, not packets per second |
| `tuff.web.minOpLevel` | `op-permission-level` | The lowest level that may log in to the panel |
| `tuff.resourcePacks.enabled`, `tuff.customContent.prompt` | `resource-pack*` | Plugin-registered and generated packs, not the single vanilla pack |
| `tuff.anticheat.*` | `player_movement_check`, `elytra_movement_check` | The rules switch vanilla's checks; these tune tuff's own |

Fill and clone limits come from the `max_block_modifications` game rule.

## Dimensions

`tuff.world.dimensions.overworld.enabled`, `.nether.enabled` and
`.end.enabled` (default `true`) switch a vanilla dimension off from the
next start: it is not loaded, ticked or sent to clients, portals into it
do nothing, and its saved data stays for when it is switched back on.

`tuff.world.spawnDimension` names the dimension new players spawn in and
respawns fall back to (plugin dimensions allowed). When it is empty, the
first plugin that claims one (`spawn_dimension` in its manifest) picks it,
else the overworld. An unknown key is logged as an error and ignored. At
start-up the world spawn moves to the chosen dimension's spawn point when
it differs from the saved spawn's dimension, or when the saved spawn's
dimension no longer exists. A world left with no dimension at all, vanilla
or plugin, refuses to start; the error names the grant that adds a plugin
dimension (`tuff plugin grant <id> dimension <key>`).

## Mob spawn caps

`tuff.mobs.spawnCap.<category>` sets how many mobs of a category spawn
naturally per 17x17 chunks around players, scaled like vanilla's cap by the
chunks players have loaded. The categories and their vanilla defaults are
`monster` (70), `creature` (10), `ambient` (15), `axolotls` (5),
`undergroundWaterCreature` (5), `waterCreature` (5) and `waterAmbient` (20).
`0` stops natural spawning of that category. Changes apply from the next
tick.

## Crowd mode

`tuff.mobs.crowd.enabled` (default `false`) is an opt-in for mob farms and
other dense crowds. A region (32x32 chunks) holding at least
`tuff.mobs.crowd.threshold` entities (default `1500`) then runs its mobs
simplified: pushing every other tick at double strength against the 8
nearest neighbours (cramming damage keeps its average rate), and full goal
selection every 4 ticks (every 8 when no player is within 32 blocks)
instead of every 2. Tempting, breeding, wandering and targeting still
work, with a few ticks more latency. The server logs once when a region
enters or leaves crowd mode. Off, behaviour is vanilla.

## Item conservation and save safety

Item stacks are move-only: a stack moves between players, containers,
entities and regions instead of being copied, and appears (loot,
crafting, `/give`) or disappears (eating, burning, `/clear`) only through
a few audited doors; plugins follow the same rules (see What the ABI
offers). Menu clicks and container and entity-container transactions
between a player and a region are checked; other transfers (dropped items,
hoppers and item ports across regions, hand-to-mob handovers) are not yet.
In release builds the checks are on unless
`tuff.items.conservationChecks` is `false`. An operation that would make
or lose items is refused instead of applied: the server logs it, writes a
`tuff:item_conservation` row to the audit log, and quarantines the region
and player involved. A quarantined region trades no items with players and
is not saved until it unloads; a quarantined player is not saved and,
while `tuff.items.conservationKick` is `true` (the default), is
disconnected so its client reloads the last consistent save. Debug builds
and tests always check, and a stack dropped without being stored or burned
panics there (release builds log it); every world save reports items lost
that way.

| Key | Default | Effect |
|-----|---------|--------|
| `tuff.items.conservationChecks` | `true` | Check item moves in release builds and refuse, audit and quarantine on a violation |
| `tuff.items.conservationKick` | `true` | Disconnect a player involved in a refused item operation |
| `tuff.blocks.tntDuplication` | `true` | Keep vanilla's TNT duplication: a TNT block a piston lifts can still light where it stood, once, making one extra primed TNT |
| `tuff.blocks.carpetRailDuplication` | `true` | Keep vanilla's carpet and rail duplication: a carpet or rail a piston lifts can still pop where it stood, once, making one extra drop |
| `tuff.blocks.gravityBlockPortalDuplication` | `true` | Keep vanilla's gravity block duplication through End portals (takes effect with falling blocks owning their block) |

Blocks are move-only like item stacks: a block exists once, in the world,
inside a moving piston, inside an entity, or as its drops. The three
`tuff.blocks.*Duplication` switches re-enable vanilla's own glitches
through one named, counted door each; with a switch off, the glitch cannot
happen. Tripwire hook duplication does not exist in 26.3 and has no switch.

A world save lands whole or not at all. Before a save's first write to a
chunk or player file, the previous contents go to a journal (a
`tuff-journal` directory in a world folder, two tables in a server file),
and the save commits once every player and chunk it covers is written. If
the server stops before that (a crash, an I/O error, a save that failed),
the next start restores the last complete save, so a player is never
loaded newer than the chests it traded with. A failed write stops commits
until the next start, which then restores the last complete save; the log
says so. A save whose snapshot takes longer than a second is completed by
the next save.
