<h1>
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/assets/logo-dark.svg">
    <img src="docs/assets/logo-light.svg" alt="" width="40" height="40" align="top">
  </picture>
  tuff
</h1>

A Minecraft Java Edition server written in Rust, for game version 26.3.
The vanilla game runs as a set of plugins on an inert core, and external
plugins are sandboxed WebAssembly modules. See `CONTRIBUTING.md` for
development guidelines.

## Release notes: v0.1.0-alpha.1

The first public alpha is a drop-in survival server for 26.3 with
sandboxed WASM plugins. The tag is not cut yet; this is the support
statement it will ship with:

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

## Installing

Clone the repository and run the install script from the checkout:

    git clone https://github.com/brenfwd/tuff.git tuff && cd tuff && scripts/install.sh

The script builds the server and installs the binary at
`./tuff-server/tuff`. Run it from that directory (for offline-mode
accounts add `--offline` and `server-ip=127.0.0.1`, or see "Offline mode
and proxies"), set `white-list=false` in `server.properties` or add
players with `whitelist add <name>` on the console, then join
`localhost:25565` with a 26.3 client.

### The Minecraft EULA

The build downloads the official Minecraft server jar from Mojang and
generates game data from it, so the script asks you to accept the
[Minecraft EULA](https://www.minecraft.net/eula) before it starts
building. Answer `y` at the prompt, or pass `--accept-eula` (or set
`TUFF_ACCEPT_MOJANG_EULA=1`) for unattended installs. Without a terminal
to prompt on and without one of those, the script stops.

### Requirements

The script needs git, curl and a C compiler (`cc`, `gcc` or `clang`) and
checks for them first. It runs on bash 4 or newer on Linux and macOS. If
rustup or [bun](https://bun.sh) is missing, it installs them for your
user only (no root needed); rustup then installs the toolchain pinned in
`rust-toolchain.toml`.

### Build cost

A release build on a 16-vCPU machine, with the Mojang download already
cached, took about 6.5 minutes, peaked at about 1.3 GiB of memory in one
process and left about 1.2 GiB in `target/`. A first build on a fresh
machine is slower and uses more disk. On a host with little memory, limit
parallel jobs with `--jobs 2` (or 4). Pass `--clean` to delete `target/`
after installing; the next update then rebuilds from scratch.

Pass `--opt` to build with the `release-opt` profile (fat LTO, one codegen
unit). The server runs faster, but the build takes several times longer
and needs more memory, so leave it off on small hosts.

Other flags: `--prefix DIR` installs somewhere other than
`./tuff-server`, `--no-update` skips the `git pull`, `--dry-run` prints
the steps without running them, and `--help` lists everything.

### Building on another machine

You can run the install script on a stronger machine and copy the binary
to the server host, as long as both have the same target triple: the same
CPU architecture, operating system and C library. Check it on both with:

    rustc -vV | grep '^host:'

For example, `x86_64-unknown-linux-gnu` on both. With a `gnu` triple the
build machine's glibc must not be newer than the host's, or the binary
will not start there. If the host's glibc is older, build a musl static
binary instead.

#### Cross-building

Another way to get a binary for a different machine, for example a
Raspberry Pi or an ARM VPS, is to cross-build on an x86_64 Linux host:

    scripts/install.sh --accept-eula --target aarch64-unknown-linux-musl

With a `--target` that differs from the host, the script runs
`rustup target add` and builds with `cargo zigbuild` (it installs
`cargo-zigbuild` user-locally if missing). `zig` must already be on your
`PATH`. `--opt` and `--jobs` work as usual. The `musl` triple gives a static
binary with no glibc dependency (`file` reports it as statically linked).
The output is
`target/<triple>/release/tuff`, installed to the prefix. The aarch64 musl
target has not been tested yet. The no-redistribution rule below applies
unchanged.

The binary lands in `--prefix DIR` if you passed one (default
`./tuff-server/tuff`). If you build with cargo directly, it is
`target/release/tuff`, or `target/release-opt/tuff` with `--opt`. Copy it
to the host with `scp` or `rsync`.

You still may not redistribute the binary. It embeds data generated from
Mojang's server jar, so you may only run it on machines that you (or your
organisation) operate for your own server. Redistributing means giving or
making the binary available to anyone else in any form: uploading it to a
website, release page or file share, publishing a container image that
contains it, including it in a modpack, hosting panel or installer, or
handing it to another server operator. Each operator builds their own
copy; the install script does this and asks them to accept the
[Minecraft EULA](https://www.minecraft.net/eula). This is a plain
description of how the project works, not legal advice. The same rule is
why Docker images stay off public registries.

### Updating

Run `scripts/install.sh` again. It pulls the latest commits with
`git pull --ff-only`, rebuilds and replaces the binary. Stop the server
before replacing a running binary.

### Docker

The `Dockerfile` runs the same script inside the image build. Build the
image locally and never push it to a public registry, since it contains
data generated from Mojang's server jar:

    docker build --build-arg ACCEPT_MOJANG_EULA=1 -t tuff .
    docker run -p 25565:25565 -v tuff-data:/data tuff

The build fails without `ACCEPT_MOJANG_EULA=1`. Add
`--build-arg JOBS=4` to limit parallel jobs, and `--build-arg OPT=1` to
build with the `release-opt` profile, like `--opt` above. The world and
settings live in the `tuff-data` volume, mounted at `/data`. To update, pull the
repository and build the image again.

### Windows

Windows is best effort. Run the install script inside WSL, or use the
Docker image.

## Building

The install script wraps the steps below; use them directly when working
on tuff. The build needs the Rust toolchain pinned in
`rust-toolchain.toml` (the nightly `nightly-2026-09-11`, with rustfmt,
clippy and the `wasm32-unknown-unknown` target; rustup installs it on
first use) and [bun](https://bun.sh), which builds the web admin panel
and the example plugins. The build looks for bun in `BUN`, then on
`PATH`, then at `$HOME/.bun/bin/bun`, and runs
`bun install --frozen-lockfile`; tested with bun 1.3.14.

    cargo build --release

The first build downloads the vanilla server jar and a Java runtime from
Mojang to generate game data. Set `VANILLA_JAVA` to use your own Java
instead of the downloaded one. Both are cached under `target/vanilla` (or
`VANILLA_CACHE_DIR`), and later builds need no network while that cache is
complete; if parts of it are deleted, the next build downloads them again.

## Running

    cargo run --release -- --offline -Dtuff.allowPublicOffline=true

The server listens on port 25565 on every IPv4 and IPv6 interface, or only
on `server-ip` when that is set. Without `--offline`, players sign in with
their Microsoft accounts. Type commands on the console (`op Alice`,
`whitelist add Bob`); Ctrl-C stops the server.

A new server has `white-list=true`, so add players to the whitelist or set
`white-list=false` in `server.properties`.

| Flag | Effect |
|------|--------|
| `--offline` | Skip Microsoft authentication |
| `--instance_dir <dir>` | Where the server keeps its files (default: the current directory) |
| `--world_backend <vanilla\|sqlite>` | `vanilla` (default) keeps the world in `world/`; `sqlite` keeps the whole server in `world.mcserver` |
| `--server_file <file>` | Use this server file (implies `sqlite`) |
| `--sqlite_locking <exclusive\|normal>` | SQLite locking mode, default normal (sqlite backend only) |
| `--seed <seed>` | Seed for a new world |
| `--config <key> <value>` | Override a setting for this run (repeatable; `--config=key=value` also works). See Settings |
| `-D<key>=<value>` | Shorthand for `--config <key> <value>` (`-D key=value` also works; repeatable, the last one for a key wins) |
| `--execute <file>` | Run the commands in a file at start-up (repeatable); a failing line makes the process exit with status 1 after shutdown |
| `--no-serve` | Open no game or query port; the HTTP server still starts. With `--execute`, the server stops once the files have run |
| `--no-save` | For testing: read the world and configuration but write nothing to disk (chunks, entities, player data, `level.dat`, stats, advancements, maps, config files, `session.lock`, backups, logs, crash reports). The run sees its own changes in memory; `/save-all` and shutdown succeed without writing. With no world, the whole world lives in an in-memory server file and nothing is created in the instance directory. Plugin data folders and the audit-log database are not covered. Cannot be combined with `--convert_to` or `--force_upgrade` |
| `--tick_lockstep <sprint\|always>` | Tick lockstep mode |
| `--pid_file <path>` | Write the process id to this file |
| `--convert_to <vanilla\|sqlite> --convert_output <path>` | Convert the server to a backend at `<path>` and exit |
| `--force_upgrade` | Upgrade every chunk to the current version (with `--convert_output`) |
| `--erase_cache` | With `--force_upgrade`, drop cached lighting and heightmaps |

An unknown argument prints the full usage line, and argument errors exit
with status -5.

### Offline mode and proxies

Offline mode trusts the name each client claims. The server therefore
refuses to start in offline mode on a non-loopback address unless
`tuff.allowPublicOffline=true` is set or Velocity forwarding (below) is on,
ignores `ops.json` (permissions, whitelist and player-limit bypass, low-disk notices) for unverified names unless `tuff.offlineOps=true`, and
keeps vanilla's duplicate login rule (the new login kicks the online
player) unless `tuff.refuseDuplicateLogin=true`, which turns the new login
away instead.

To run behind a [Velocity](https://papermc.io/software/velocity) proxy:

1. In `velocity.toml`, set `player-info-forwarding-mode = "modern"` and
   note the secret in `forwarding.secret` (the `forwarding-secret-file`).
2. On this server, set `online-mode=false`, `tuff.proxy.velocity.enabled=true`
   and either `tuff.proxy.velocity.secret` to the same secret or
   `tuff.proxy.velocity.secretFile` to a copy of `forwarding.secret`
   (relative to the instance directory). These are read at startup.
3. Keep the server's port reachable only from the proxy (bind
   `server-ip` to a private address or firewall it).

The proxy then authenticates players and the server takes each player's
UUID, name, skin and address from the signed forwarding data, so bans, the
whitelist, operators, plugins and the web panel see the real identity and
address. A connection without valid forwarding data (a direct client, a
proxy with a different secret or another forwarding mode) is refused.

### Restarting

`/tuff restart [seconds|cancel]` (permission `tuff.command.restart`, ops by
default) saves and stops the server, then starts it again according to
`tuff.restart.mode`:

- `self` (default): the server re-executes its own binary with the same
  arguments and environment. On Unix this is `exec`, so the PID and
  terminal stay the same and a replaced binary on disk is picked up. On
  Windows it starts a new process and exits with status 0, so the PID and
  console differ. If the re-exec fails, the server logs `restart failed`
  and exits with status 75.
- `exit`: the server exits with status 75 for a supervisor (systemd
  `Restart=on-failure`, a container, a shell loop) to start again. Use
  this mode under a supervisor.

The delay is 0 to 3600 seconds and counts down with notices at 600, 300,
120, 60, 30, 15 and 10 seconds and each of the last five;
`/tuff restart cancel` aborts it. The countdown and the cancel notice go
to every online player while `tuff.restart.broadcast_countdown` is true
(the default); false sends them only to the source and operators. While
`tuff.restart.disconnect_message` is true (the default), players are
disconnected with `tuff.restart.disconnect_text` (default "Server is
restarting, please reconnect shortly") instead of the generic text.
`/stop` is unchanged.

## Settings

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

### Settings and vanilla entries

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

### Dimensions

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

### Mob spawn caps

`tuff.mobs.spawnCap.<category>` sets how many mobs of a category spawn
naturally per 17x17 chunks around players, scaled like vanilla's cap by the
chunks players have loaded. The categories and their vanilla defaults are
`monster` (70), `creature` (10), `ambient` (15), `axolotls` (5),
`undergroundWaterCreature` (5), `waterCreature` (5) and `waterAmbient` (20).
`0` stops natural spawning of that category. Changes apply from the next
tick.

### Crowd mode

`tuff.mobs.crowd.enabled` (default `false`) is an opt-in for mob farms and
other dense crowds. A region (32x32 chunks) holding at least
`tuff.mobs.crowd.threshold` entities (default `1500`) then runs its mobs
simplified: pushing every other tick at double strength against the 8
nearest neighbours (cramming damage keeps its average rate), and full goal
selection every 4 ticks (every 8 when no player is within 32 blocks)
instead of every 2. Tempting, breeding, wandering and targeting still
work, with a few ticks more latency. The server logs once when a region
enters or leaves crowd mode. Off, behaviour is vanilla.

### Item conservation and save safety

Items are never copied: a stack moves between players, containers,
entities and regions, and only appears (loot, crafting, `/give`) or
disappears (eating, burning, `/clear`) through a few audited doors;
plugins follow the same rules (see What the ABI offers). Moves
between actors are checked; in release builds the checks are on unless
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

## Built-in features

The vanilla game is 16 built-in features loaded on an inert core: `worldgen`,
`redstone`, `items`, `maps`, `recipes`, `loot`, `blocks`, `combat`,
`players`, `entities`, `mobs`, `survival`, `dimensions`, `tuff-feature-builtin-core-commands`,
`tuff-feature-builtin-vanilla-commands` and `anticheat`. They load by default. Leave one out
(for example on a minigame server) with
`tuff.vanilla.<name>.enabled=false`, where `<name>` is the feature id
without `vanilla-` (`tuff-feature-builtin-core-commands` keeps its full name). If a feature that
stays needs one you left out, the server refuses to start and names both.
The loaded set is logged at start-up.

Optional built-in features change vanilla behaviour and are off by default:

| Id | Effect |
|----|--------|
| `tuff-feature-builtin-moving-block-entities` | Pistons move blocks with block entities (chests, furnaces and others) and their data |
| `world-map` | A live, zoomable world map page in the web panel, with chunk pre-generation (permissions `map.view`, level 2, and `map.generate`, level 4) |
| `tuff-feature-builtin-example` | The built-in feature API example (`crates/tuff-feature-builtin-example`): `/gold`, extra recipes, a custom item, block and entity, and a Gold panel page |
| `tuff-feature-builtin-example-chunk-progress` | A "Loading Chunks" boss bar showing each player how many chunks in view have been sent |
| `audit-log` | A world audit log in its own SQLite file beside the world (`<world>.audit.sqlite`, or `tuff.features.audit-log.database`): block breaks and placements, explosions, chat, commands and sessions, with `/tuff audit lookup`, `rollback`, `restore`, `purge` and `status`. Retention is set by `tuff.features.audit-log.retention` and old entries are purged in the background |

Turn one on with `--config tuff.features.<id>.enabled true` or by setting
`tuff.features.<id>.enabled=true` in the saved settings (`tuff.properties`
for a folder world, the server file for sqlite); `--config` wins. A
feature's own settings live under `tuff.features.<id>.*`. Feature and
vanilla feature switches are read only at start-up.

## Custom content pack

The server builds a resource pack from the custom items, blocks and assets
that plugins register, and every client must accept it: a player who
declines it, or whose download fails, is disconnected. When no plugin
registers any assets there is no pack, and clients are not asked for one.

Clients download the pack from the HTTP server, so its address must be
reachable from their machines. Fix `tuff.http.port` and set
`tuff.http.resourcePathBaseUrl` to a URL players can reach, or set
`tuff.http.detectPublicIp` to `true` (default `false`), which learns the
server's public address with a STUN request over UDP to
`stun.cloudflare.com` and `stun.l.google.com`. Both apply without a
restart, and the log warns, naming both keys, when a pack URL is one
clients cannot reach. `tuff.customContent.prompt` sets the text shown with
the offer. `tuff.resourcePacks.enabled` turns off other plugin packs but
not this one.

## Runtime plugins (WebAssembly)

External plugins are WebAssembly modules that the server loads, unloads
and reloads while it runs, in a sandbox (`crates/tuff-plugin-wasm`, on the
pure-Rust wasmi interpreter). They are separate from the built-in features
above: a feature is compiled into the server and switched on with
`tuff.features.<id>.enabled`, while a runtime plugin is a `.wasm` file and
needs no setting of its own. The plugin host is always part of the `tuff`
binary and is on by default.

The plugin ABI is not stable. `api = 0` is the only version, and the ABI
may change in any release until it is frozen, which is planned for 0.9.

### Where plugins live

The host reads modules from the `plugins/` directory, relative to the
directory the server was started from (`--instance_dir` does not move it).
At start-up it loads every file in that directory whose name ends in
`.wasm` and does not start with `.`, in file-name order, each after the
plugins it depends on; subdirectories are not searched. A module is at
most 16 MiB and must carry a manifest (see below), or it is skipped with
an error in the log.

| Setting | Default | Effect |
|---------|---------|--------|
| `tuff.plugins.wasm.enabled` | `true` | `false` unloads every runtime plugin and refuses to load more; `true` again rescans the directory |
| `tuff.plugins.wasm.directory` | `plugins` | The directory modules load from |

The host reads both again before each request, so
`/tuff config tuff.plugins.wasm.enabled false` takes effect without a
restart.

### Managing plugins

`/tuff plugin` needs the permission `tuff.command.plugin`; without it the
command reports itself as unknown.

| Command | Effect |
|---------|--------|
| `/tuff plugin list` | Each loaded plugin's id, version, commands, declared permission nodes and description |
| `/tuff plugin load <file>` | Loads `<file>` from the plugin directory; `.wasm` may be left off |
| `/tuff plugin unload <id>` | Calls the plugin's disable hook and removes everything it registered |
| `/tuff plugin reload <id>` | Reads the plugin's file again and swaps in the new module |
| `/tuff plugin purge <id>` | Deletes everything plugin `<id>` stored, whether it is loaded or not |

`load` takes a file name; `unload` and `reload` take the plugin id from
the manifest. A load fails when the id is already loaded, when a
dependency is missing or too old, when the manifest's `api` is not `0`,
when its stored data cannot be read, or when the plugin claims a command
the server or another plugin already has. A plugin that others depend on
cannot be unloaded until they are. `reload` refuses a file whose manifest
now declares a different id.

### Checking that a plugin loaded

At start-up the log has `Loaded <id> <version>` for each module that
loaded and `wasm plugin not loaded: <reason>` for each that did not.
`/tuff plugin load` replies with the same text. `/tuff plugin list` prints
`<n> runtime plugins loaded` and then one line per plugin, for example
`hello 0.1.0 /hello: Greets players and counts their visits`.

### Plugin grants

A plugin requests capabilities in its manifest (`cap.<kind> = entries`);
nothing is allowed until the owner grants it. Kinds: `files`, `http`,
`websocket`, `tcp`, `udp`, `route`, `listen-tcp`, `listen-udp` and
`dimension` (a dimension id such as `myplugin:mine`, or `myplugin:*`).
Grants live only in the SQLite server file; a folder world cannot hold
them.

In game, `/tuff plugin grants <id>` shows what is requested and granted,
and `/tuff plugin grant <id> <cap> <entry|all-requested>` and
`/tuff plugin revoke <id> <cap> <entry|all>` change them.
`all-requested` never grants `private` network entries or `top` routes;
name those explicitly. A dimension grant made in game applies on the next
restart.

To grant before the world is first generated, run the same commands
offline against the server file while no server is running on it:

```
tuff plugin grants myplugin --server_file world.mcserver --plugins plugins
tuff plugin grant myplugin dimension all-requested --server_file world.mcserver
```

`--server_file` defaults to `./world.mcserver` (created if missing) and
`--plugins` to `./plugins`. The plugin must be in that directory, and the
subcommand refuses a folder world. Start the server on the same file with
`--world_backend sqlite`.

A plugin that traps, runs out of fuel or misuses the ABI is unloaded on
the spot, with `wasm plugin <id> faulted during <phase> and was unloaded`
in the log; plugins that depended on it are unloaded with it, and the
server carries on.

### Writing a plugin in Rust

`crates/tuff-plugin-sdk` (`tuff-plugin-sdk`) wraps the ABI; its only dependency
is `wit-bindgen`. `crates/tuff-plugin-sdk/GUIDE.md` is the SDK guide.
`examples/rust-hello` is a complete plugin to copy: its `Cargo.toml`
builds a `cdylib` named `hello`, depends on the SDK by path, and is its
own workspace so the server's build never compiles it for the host.

A plugin implements the `Plugin` trait, whose hooks (`register`, `enable`,
`disable`, `command`, `on_event`, `timer` and others) all have default
bodies, and declares itself with `plugin!`, which writes the manifest into
the module. The SDK's prelude has a command builder, typed events, timers,
async tasks, settings and per-plugin storage. Part of `rust-hello`:

```rust
use tuff_plugin_sdk::prelude::*;

static VISITS: StoredMap<u32> = StoredMap::new("visits");

struct Hello;

impl Plugin for Hello {
    fn register() -> Result<(), String> {
        command("hello")
            .about("Greets you")
            .executes(|ctx| {
                let player = ctx.player()?;
                let visits = VISITS.update(player.uuid, |n| n.unwrap_or(0) + 1)?;
                ctx.reply(text!("Hello, ", player.name.aqua(), "! ", format!("(visit {visits})").gray()))?;
                Ok(visits as i32)
            })
            .help()
            .register()?;
        Ok(())
    }
}

plugin!(Hello {
    id: "hello",
    version: "0.1.0",
    description: "Greets players and counts their visits",
});
```

The full example also has `/hello later` (an async reply after two
seconds), `/hello reset` (gated on its own `hello.reset` node at level 2),
a join welcome, a diamond-ore break listener, a five-minute broadcast
timer and a `plugins.hello.greeting` setting.

The pinned toolchain already includes the `wasm32-unknown-unknown`
target, so from the repository root:

```sh
cargo build --release --target wasm32-unknown-unknown --manifest-path examples/rust-hello/Cargo.toml
mkdir -p plugins
cp examples/rust-hello/target/wasm32-unknown-unknown/release/hello.wasm plugins/
```

Then run `/tuff plugin load hello` on a server started from the repository
root, or restart it. The plugin id is `hello`, so it is unloaded with
`/tuff plugin unload hello`.

The manifest is a `tuff-plugin` custom section of `key = value` lines:
`id` (1 to 64 of `a-z 0-9 - _`), `version` (`x.y.z`) and `api` are
required; `depends` (for example `economy >= 2.0.0, chat-format`),
`description`, `spawn_dimension` (a dimension the plugin adds and was
granted, claimed as the spawn dimension; see Dimensions) and `cap.*`
requests are optional, and unknown keys are ignored.

### Other examples and languages

`examples/README.md` covers building each example. Besides `rust-hello`,
`examples/` has more Rust plugins (`rust-worldedit`, `rust-tuffer` with a
custom dimension, and `rust-messaging-client` for plugin-to-plugin
messaging) and C++ plugins (`cpp-hello`, `cpp-io`, `cpp-messaging`) built
on the C/C++ SDK in `crates/tuff-plugin-sdk/c`, whose bindings are generated by
wit-bindgen. `crates/tuff-plugin-wasm/examples/greeter.wat` is a hand-written
plugin in WebAssembly text.

### The WorldEdit example plugin

`examples/rust-worldedit` is a runtime plugin whose commands live under
`/tuff we <name>` and also answer to `//<name>` (for example
`//set stone`); there is no bare `/we`. Navigation commands (`ascend`,
`descend`, `ceil`, `thru`, `jumpto`, `up`, `unstuck`) register their bare
names instead. `//help [-a] [command]` lists every command. They include
selection (`pos1`, `pos2`, `sel`, `desel`), editing (`set`, `replace`,
`setblock`), the clipboard (`copy`, `cut`, `paste`, `clone`, `rotate`,
`flip`), history (`undo`, `redo`), schematics, brushes, generation, biome
and entity commands, and `limit`, `status`, `wand`, `toggleeditwand` and
`version`.

Every command needs `worldedit.use` plus its own node (for example
`worldedit.selection.pos` for `pos1` and `pos2`, `worldedit.wand` for
`wand`), each held by operators at level 2 by default. With the wand
toggled on, a wooden axe selects: left click sets pos1, right click pos2.
The plugin requests the `world-write`, `player-write` and `player-read`
capabilities.

### What the ABI offers

The ABI is the WIT world in `crates/tuff-plugin-wasm/wit/plugin.wit` under the
canonical ABI (limits in `crates/tuff-plugin-wasm/src/abi.rs`); any wit-bindgen
backend that emits a core module can target it.

- Logging to the server log at error, warn, info or debug level.
- Commands: up to 32 per plugin, each 1 to 32 of `a-z 0-9 - _`. The
  plugin receives the whole command line without the slash, can reply to
  the player who ran it, and returns the command's result.
- Events: player join and leave and chat.
- Broadcasting a message to every player.
- Timers: a callback with a plugin-chosen token after a delay of up to a
  day of server ticks (1,728,000), with at most 1024 pending per plugin.
- Per-player state: byte values under string keys, up to 64 keys per
  player, for the player a command or event acts for. A timer acts for no
  player.
- Permissions: a plugin declares nodes named `<plugin id>.<name>` in
  `register`, each held by every player (level 0) or by operators from a
  chosen level (1 to 4), and checks whether the acting player holds a node
  (its own or any server node). `/tuff perm` grants and roles apply as
  for built-in nodes. The nodes go away with the plugin.
- Settings: a plugin declares its own config keys, which appear in
  `/tuff config`.
- Storage: byte values under string keys (up to 1024 keys and 1 MiB per
  plugin), kept by plugin id in `<plugin directory>/<id>/storage.bin`.
  They survive unloads, reloads and restarts until
  `/tuff plugin purge <id>`; the file is written at most every 100 ticks,
  on unload and at shutdown.
- The acting player's position, dimension and (for commands) look.
- Blocks: read one block or a box of up to 32,768 as a palette (both load
  the chunks they touch), and queue block changes, with or without
  neighbour updates, which are applied in order on the owning regions
  once the call returns. A call may read 1,048,576 blocks and queue
  65,536 changes.
- Block events: a player broke or placed a block (payload: `x y z
  dimension state`). They arrive after the change and cannot cancel it.
- Items: a plugin holds items only in host-owned stashes (`stash-new`,
  `stash-mint`, `stash-split`, `stash-merge`, `stash-burn`) and moves them
  with `give-stash` and `take-into-stash`, so it can make, move and use up
  items but never copy one. Up to 1024 stashes at once; a stash still
  holding items when the plugin unloads or faults is used up.
- Messages to another loaded plugin, delivered after the call to its
  `tuff_message` with the sender's id, followed at most 8 deep when
  handlers answer each other.
- Web panel pages (experimental, behind an unstable feature), served at
  `/admin/pages/<id>/` and gated by a permission scope.
- Capability-gated I/O (files, HTTP, WebSocket, TCP, UDP, listeners) and
  plugin dimensions, each only after a grant (see Plugin grants).

Commands and event subscriptions can only be claimed in `register`. Each
call into a plugin may burn 50,000,000 units of fuel (roughly wasm
instructions), send 64 messages and schedule 64 timers; a module's memory
is capped at 64 MiB, and a string or value crossing the boundary at
32 KiB. A refused call, such as a reply with no player to reply to,
returns a negative result; a call made in the wrong phase, or with an
out-of-bounds or oversized string, traps and unloads the plugin.

Plugin commands are in the client's command tree, which is sent again
whenever a plugin loads, unloads or reloads, so their names tab-complete.
Any player can run them; a plugin gates its commands by checking its own
permission nodes.

Per-player state is dropped when the player leaves, plugins cannot cancel
block events, and HTTP routes are not finished.

## Pre-generating chunks

`/tuff chunk generate <x> <z> <radius> circle|square [--force-regenerate]`
generates chunks in the source's dimension; run it with
`/execute in <dimension> run ...` to target another dimension. `x`, `z`
and `radius` are in blocks, and `radius` is 0 to 100000. A chunk is
included when any of its blocks is within the radius: for `circle` that
means a straight-line distance, for `square` the larger of the two axis
distances. Chunks beyond the 30,000,000-block world limit are skipped.

Chunks are generated in a spiral out from the centre, each one fully (the
same as a player loading it), then saved and unloaded. Chunks a player is
loading take priority. `tuff.pregen.parallel` sets how many chunks
generate at once (0 to 1024); `0`, the default, means twice the number of
worker threads, at least 4 and at most 128. `tuff.pregen.maxPriority`
(default `false`) raises that to eight times the worker threads (32 to
1024) for an otherwise idle server, at the cost of slower world ticks.

`--force-regenerate` first deletes everything in every chunk in the shape
(blocks, entities, block entities and points of interest), shown as a
"Removing" phase, then generates them again. Chunks just outside the shape
are left as they are, so a feature that crosses the edge may no longer
match on both sides.

`/tuff chunk status` reports progress, the oldest chunks still in flight
and what each is waiting on, and a summary of the last job.
`/tuff chunk cancel` stops the job after the chunks already in flight
finish. Only one job runs at a time. A chunk that has not finished after
300 seconds counts as failed, so a job always ends.

A player with the `tuff.command.chunk` permission (operator level 4 by
default) sees the job's progress on a boss bar: chunks done and total, the
percentage, the rate and an ETA.

With the `world-map` feature on, the panel's World map page can draw the
same shape, as a circle or a square, by dragging its centre or its handles
or by typing a radius. It shows the chunk count and starts the same job
with the Generate button; force-regenerate asks for confirmation first.
Starting or cancelling a job needs `map.generate` (level 4); `map.view`
(level 2) is enough to watch progress. New chunks appear on the map as
they are saved.

## Web admin panel

The panel is served under `/admin` on the HTTP server and is off by
default. To log in:

1. Run `/tuff config tuff.web.enabled true`.
2. Run `/tuff web login`. It prints a single-use 16-digit code
   (`XXXX-XXXX-XXXX-XXXX`), valid for 120 seconds, and the panel URL.
3. Open the URL and enter the code.

Code exchanges are rate limited per address and overall. `/tuff web
logout` ends your own panel sessions and `/tuff web logout-all` ends
everyone's.

Scripts use API tokens instead of a session.
`/tuff web token create <name> <level> [expiry]` prints a token of the
form `tuff_<id>_<secret>` once; only its hash is stored. The name is 1 to
64 letters, digits, `_`, `-` or `.`, a token cannot outrank its creator,
and the expiry is `never` or a count with `m`, `h`, `d`, `w` or `y`, like
`30d`, up to ten years. Send it as `Authorization: Bearer <token>`; such
requests need no CSRF header. `/tuff web token list` shows tokens and
`/tuff web token revoke <id or name>` deletes one. The Access page in the
panel does the same. Logins, logouts, token changes and refused tokens go
to the panel's audit log.

The built-in pages are Access, World, Config, Audit, Players, Logs,
Metrics, Timings, Backups, Datapacks, Resource packs and Anticheat. Each
area has its own permission node (for example `tuff.web.config.write`,
`tuff.web.backups.restore`), held by operators at level 4 by default.

| Key | Default | Effect |
|-----|---------|--------|
| `tuff.web.enabled` | `false` | Serve the panel |
| `tuff.web.trustOfflineNames` | `false` | With `online-mode=false` player names are unauthenticated, so by default players get no panel login codes, tokens or panel sessions (the console still can); set true to accept that anyone can claim an operator's name |
| `tuff.web.allowRemote` | `false` | Accept clients other than loopback; refused (with an error in the log) unless `tuff.web.tlsTerminated` or `tuff.web.acknowledgePlaintext` is also true |
| `tuff.web.tlsTerminated` | `false` | Declare that a TLS proxy terminates HTTPS in front of the panel |
| `tuff.web.acknowledgePlaintext` | `false` | Accept serving the panel to remote clients over plain HTTP |
| `tuff.web.hostnames` | empty | Extra host names the panel answers to, comma separated |
| `tuff.web.publicUrl` | empty | Base URL shown in login messages (empty or an `http(s)` URL) |
| `tuff.web.trustForwarded` | `false` | Take the client address and host from `X-Forwarded-For`/`-Host` |
| `tuff.web.minOpLevel` | `4` | Lowest operator level (1 to 4) that may log in; the panel refuses a raise that would lock you out |

Behind a reverse proxy that rewrites the `Host` header, set
`tuff.web.publicUrl` to the proxy's public address (and
`tuff.web.trustForwarded` if the proxy sets `X-Forwarded-Host`/`-For`), or
plugin pages will not load. A request whose host is not allowed is
answered with 421. While `publicUrl` is empty and `trustForwarded` is
false, the server logs one warning per process the first time an allowed
panel request names a host (in `Host` or `X-Forwarded-Host`) that is
neither a bound address nor in `tuff.web.hostnames`.

Plugins can add pages and JSON APIs to the panel. The example plugin (the
`tuff-feature-builtin-example` feature) adds a Gold page that shows, live, how many gold
blocks have been placed in the world, with a button to reset the count.
Pages are React apps built with the Vite helper `panelPage()` in
`crates/tuff-web-ui/app/sdk/vite.ts`; `crates/tuff-feature-builtin-example/README.md` shows
how to write one. A plugin page is gated by a permission scope that
defaults to operator level 4; a plugin can set a lower level, as the world
map does for `map.view`.

## Anti-cheat

The anti-cheat plugin checks player packets: movement, reach, rates, item
and inventory integrity, and vehicles. It runs by default, but only logs:
checks count violations and alert, and never set a player back or kick
unless `tuff.anticheat.enforce` is `true`. With enforcement on, a failed
check sets the action back, and a player whose violations pass a check's
kick limit is kicked only when `tuff.anticheat.kickThreshold` is above 0.

| Key | Default | Effect |
|-----|---------|--------|
| `tuff.anticheat.enabled` | `true` | Run the checks |
| `tuff.anticheat.enforce` | `false` | `false`: checks run, count and alert but never set back or kick. `true` enables setbacks (and kicks with `kickThreshold`) |
| `tuff.anticheat.exemptOperators` | `true` | Skip operators |
| `tuff.anticheat.exemptCreative` | `false` | Skip players in creative mode |
| `tuff.anticheat.exemptSpectators` | `false` | Skip spectators |
| `tuff.anticheat.alert` | `true` | Log violations, setbacks and kicks to the console |
| `tuff.anticheat.alertOperators` | `true` | Tell online players with `tuff.anticheat.alerts` in chat, at most once per check and player per second |
| `tuff.anticheat.kickThreshold` | `0` | Multiplier on every kick limit; `0` never kicks and only sets back, `1.0` is the designed limit, larger is more lenient |
| `tuff.anticheat.setbackFallDamage` | `true` | A setback drop counts as falling |
| `tuff.anticheat.kickMessage` | "Kicked for suspicious activity" | The kick text |

Each check also has `tuff.anticheat.check.<name>.enabled`, `.kickAbove`
(its kick limit, multiplied by `kickThreshold`), `.decay` and `.action`
(`setback` or `log`), for example `tuff.anticheat.check.reach.kickAbove`;
`/tuff config` completes the names. The vehicle checks are `vehicle_fly`
(a boat rising or hovering without water or support under it),
`vehicle_speed` (boats and ridden mobs), `vehicle_phase` (a vehicle moving
through blocks), `vehicle_timer` (vehicle moves sent too often) and
`vehicle_control` (moves for a vehicle the player does not steer). Each
answers with a setback and can kick like any other check, except
`vehicle_control`, which only resends the vehicle's position and in
practice never reaches its kick limit. The `/anticheat` command and the
panel's Anticheat page show flags and kicks.

## Anti-xray

Anti-xray sends chunks with valuable blocks nobody can see replaced, and
sends the true blocks once they are exposed or mined towards. It is off by
default; turn it on with `/tuff config tuff.antiXray.enabled true`.

| Key | Default | Effect |
|-----|---------|--------|
| `tuff.antiXray.mode` | `hide` | `hide` sends hidden blocks as a nearby stone-like block; `fill` sends every enclosed block as a random hidden one |
| `tuff.antiXray.maxY` | `64` | Highest y at which blocks are hidden |
| `tuff.antiXray.updateRadius` | `2` | Blocks (1 to 4) around a change whose true blocks are resent |
| `tuff.antiXray.<dimension>.hiddenBlocks` | ores and similar | Blocks to hide; empty turns anti-xray off in that dimension |
| `tuff.antiXray.<dimension>.replacementBlocks` | stone-like blocks | Blocks sent in their place |

`<dimension>` is `overworld`, `the_nether` or `the_end`; other dimensions
use the overworld's lists.

## Data packs

A world loads its own data packs from `datapacks/`. Functions, tags,
recipes, loot tables, predicates and advancements are applied; other
content, such as worldgen, is not. With the server stopped, the `datapack`
subcommand adds, lists and removes packs for a world folder or a server
file alike:

    tuff datapack add <world> <pack.zip|folder> [--name <name>] [--enable] [--replace]
    tuff datapack list <world>
    tuff datapack remove <world> <name>

`<world>` is a world folder holding `level.dat` (for example `./world`) or
a server file (for example `./world.mcserver`). The command locks the
world, so it refuses a world a server is running. `add` checks the pack (a
`pack.mcmeta` at its root, at most 16 MiB per file, 256 MiB in total and
65,535 entries, no paths that leave the pack) before storing it as
`datapacks/<name>.zip` or `datapacks/<name>/`. A name is up to 64 ASCII
letters, digits, `_`, `-`, `.` or `+`; without `--name` it comes from the
file or folder name. An existing pack is refused unless `--replace` is
given. `add` and `list` note a pack made for another game version.

New packs load on the next start; `--enable` also re-enables a pack the
world's saved selection had disabled. `remove` deletes the pack and leaves
the saved selection alone; the server drops the missing pack on its next
start. `tuff datapack help` prints the details. While the server runs, the
panel's Datapacks page and the `/datapack` command manage packs.

Without the subcommand, copy packs into `<world>/datapacks/` for a folder
world. A server file keeps them in its `sqlar` table under
`extra/datapacks/`, which the `sqlite3` shell can update while the server
is stopped:

    mkdir -p stage/extra/datapacks && cp pack.zip stage/extra/datapacks/
    sqlite3 world.mcserver -A -u -C stage extra/datapacks/pack.zip

## Performance

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

## Tests

    cargo t

`cargo t` is an alias for `cargo test --workspace --lib --bins --tests
--no-fail-fast`. Never run `cargo test --all-targets`, `--benches` or
`--bench`: those select the benchmarks, which are load runs, not tests.

The benchmarks in `benches/` are load runs. Run one at a time, in release,
on a quiet machine, for example `cargo bench --bench village`.

The real-client tests in `crates/tuff-client-harness` are opt-in and never part
of `cargo t`. They run the vanilla Minecraft client, which means accepting
the Minecraft EULA (https://aka.ms/MinecraftEULA), so they need
`TUFF_ACCEPT_MOJANG_EULA=1` as well as `TUFF_CLIENT_TESTS=1`; without both
they skip and report `ok`. At most four clients run at once on a machine:

    env TUFF_CLIENT_TESTS=1 TUFF_ACCEPT_MOJANG_EULA=1 cargo test -p tuff-client-harness --test client -- --ignored --test-threads=1

The vanilla drops parity test (`tests/server/vanilla_drops.rs`) starts a real
vanilla server and is ignored by default. Narrow it with
`TUFF_DROPS_FILTER`, a comma-separated list of substrings matched against
"fixture | command". A full run takes about 40 minutes, so run it in
chunks of about 8 fixtures:

    env TUFF_VANILLA_TESTS=1 TUFF_ACCEPT_MOJANG_EULA=1 TUFF_DROPS_FILTER=kelp,cactus cargo test --test server vanilla_drops -- --ignored --nocapture

The repository scripts run on Linux and macOS. Their logic is tested with
plain Python 3:

    for d in scripts scripts/tests; do python3 -m unittest discover -s "$d" -p 'test_*.py'; done

## License

tuff is split between two licenses:

- The server (everything not listed below) is licensed under the
  [GNU Affero General Public License v3.0 only](LICENSE) (`AGPL-3.0-only`).
- The plugin-facing code is licensed under the [MIT License](examples/LICENSE-MIT):
  the plugin SDK (`crates/tuff-plugin-sdk`, including its C and C++ headers),
  the SDK generator (`crates/tuff-plugin-sdk-gen`), the plugin ABI WIT files
  (`crates/tuff-plugin-wasm/wit`) and the example plugins (`examples/`). Each
  of these directories carries its own `LICENSE-MIT`, and the source files
  carry `SPDX-License-Identifier: MIT` headers.

What this means in practice:

- Server operators: running an unmodified tuff server places no obligations
  on you. If you run a modified server, the AGPL network-use clause (section
  13) requires you to offer the complete corresponding source of your
  modified version to everyone who interacts with it over the network,
  including players.
- Plugin authors: plugins written against the MIT SDK and ABI are your own
  work. You may license them however you like, including closed source, and
  the AGPL does not apply to them.

Contributions require agreeing to the [Contributor License Agreement](CLA.md);
see [CONTRIBUTING.md](CONTRIBUTING.md).
