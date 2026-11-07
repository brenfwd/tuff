---
title: tuff v0.1.0-alpha.2
description: Release notes for v0.1.0-alpha.2.
date: 2026-10-28
author: Brenden Forward
ai: "Drafted with Claude Fable 5.1 from the project's release notes and edited by the author."
tags: [release]
draft: false
---

## Before upgrading

- Packet flood limits are on: `rate-limit` defaults to `400` packets per second. Excess recipe, command suggestion, book and sign packets are dropped without a kick. `0` turns it off.
- Per-player chunk budget: `tuff.world.chunkBudgetPerPlayer` defaults to `64` (`0` is unlimited).
- Item data shown to other players is stripped of container, bundle, book and template contents and any component over 4 KiB. Oversized block entity data in chunk packets is replaced.
- Per-chunk entity caps on save and load: `tuff.world.entityCaps.item` (1024), `.experience_orb`, `.arrow` (256), `.snowball` (128).
- Saves that would break item conservation are refused.
- Animals, water mobs, horses, donkeys and llamas despawn like vanilla.

## Security

- Item and block duplication prevention across inventories, menus, hoppers, dispensers, projectiles, mobs, plugins and chunk unloads.
- Each player's data is held by one session at a time; a crash while leaving no longer loses items.
- Block corrections no longer leak the true block.
- Anti-cheat baseline checks always run; `tuff.anticheat.kickOperators` (default `false`) keeps operators from being kicked.

## Settings

- `tuff.hidePlayerDetails.health` and `.items` hide other players' health and items.
- `tuff.mobs.spawnCap.perPlayer` and `tuff.mobs.collisionCap`.
- Despawn, merge radius and lifetime settings, void damage height, scheduled tick caps, fire delay and map cursor interval.
- Vanilla dupes stay on and can be turned off: `tuff.blocks.tntDuplication`, `.carpetRailDuplication`, `.gravityBlockPortalDuplication`, `.permanentBlockBreaking`.

## Fixes

- Piston and rail dupers; rail support on neighbor updates; candle drops.
- Stale station menus close; replaced results no longer leak items.
- `/tp` of a mob into an unloaded region; sound and global events scoped like vanilla.
- Grouped save journal syncs and batched chunk writes.
- Correct dylib naming on macOS.
- Stalled region actors are reported.

## Plugins

- `spawn-custom-entity` and custom entity interaction and damage events.
- Stashes: items a plugin can move and use up but never copy.
- The plugin interface is not stable: regenerate bindings.

## Bots

- No hopping on farmland, capped parkour take-offs, elytra landings before `goto`.
