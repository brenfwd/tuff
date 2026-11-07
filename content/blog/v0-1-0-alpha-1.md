---
title: tuff v0.1.0-alpha.1
description: Release notes for v0.1.0-alpha.1, the first public alpha.
date: 2026-10-21
author: Brenden Forward
ai: "Drafted with Claude Fable 5.1 from the project's release notes and edited by the author."
tags: [release]
draft: false
---

The first public alpha: a drop-in survival server for 26.3 with sandboxed WASM plugins.

## Support

- No prebuilt binaries. Build from source (see [Installing](/docs/installing)); the build downloads Mojang's server jar and embeds data generated from it.
- Worlds from Minecraft 1.13 (data version 1519) or newer load and are upgraded. Older worlds are refused.
- Linux and macOS are supported. Windows is best effort: use WSL or Docker.
- Anti-cheat only logs by default.
- The admin panel has no TLS or remote hardening. Keep it on localhost, or put a reverse proxy with TLS and authentication in front.
- Report bugs with the server log and, if possible, steps to reproduce.
