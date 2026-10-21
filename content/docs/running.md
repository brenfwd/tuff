---
title: Running
description: Starting the server, offline mode, proxies and restarting.
group: Getting started
source: README.md
---

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

## Offline mode and proxies

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

## Restarting

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
