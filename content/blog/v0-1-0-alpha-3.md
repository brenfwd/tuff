---
title: tuff v0.1.0-alpha.3
description: Release notes for v0.1.0-alpha.3.
date: 2026-11-07
author: Brenden Forward
ai: "Drafted with Claude Fable 5.1 from the project's commit history and edited by the author."
tags: [release]
draft: false
---

## Fixes

- Teleports wait for the destination chunk, so you no longer fall through the floor.
- Refused digs and places in anti-xray cells no longer leave ghost blocks.
- Stopped worlds free their memory (about 2.5 MB leaked per world start).
- Player-only commands work under `execute as <player>` from the console.
- UUID selectors find entities crossing between regions.
- Fewer anti-cheat false flags with real clients at high latency.

## SQL console

- Region-file worlds can edit `chunks`, `entities` and `pois`. Writes are all-or-nothing.
- Idle loaded chunks are unloaded before a write instead of refused.

## Plugins

- Item stacks carry their enchantments.
- New `fake-hurt` event for fake players.
- The plugin interface is not stable: regenerate bindings.

## Bots

See the [bots guide](/docs/guides/bots).

- `/bots build`: boxes, lines, or a block list from a file.
- Branch mining, collect mode, farming with bone meal.
- Danger-aware routing, tool swapping, armor, fighting back.
- Bedwars: bed rushing and defense, void-edge retreat.

## Mobs

- Baby happy ghast, copper golem, piglin brute and villager behavior closer to vanilla.
