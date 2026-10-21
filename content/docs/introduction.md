---
title: Introduction
description: What tuff is and what the first public alpha supports.
group: Getting started
source: README.md
---

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
