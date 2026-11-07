---
title: Bots
description: "The bots plugin (examples/rust-bots, plugin id bots) adds server-side players that walk, mine, farm, build and fight on their own. Operators drive them..."
group: Guides
source: docs/guides/bots.md
---

The bots plugin (`examples/rust-bots`, plugin id `bots`) adds server-side
players that walk, mine, farm, build and fight on their own. Operators
drive them with `/bots`; other plugins drive them through its API (see
the plugin's README).

## Install

Build it and copy `bots.wasm` into the server's `plugins/` directory:

    rustup target add wasm32-unknown-unknown
    cargo build --release --target wasm32-unknown-unknown \
        --manifest-path examples/rust-bots/Cargo.toml

Then `/tuff plugin load bots.wasm`, or restart. `/bots build ... file`
also needs the file grant:

    /tuff plugin grant bots files all-requested

## Commands

Every command needs `bots.command.<command>` (operators of level 2 and up
by default). A bot answers only to the player who spawned it, unless the
sender holds `bots.command.admin`.

| Command | What it does |
|---|---|
| `/bots spawn <name>` | Spawn a bot where you stand |
| `/bots remove <name>` | Remove it |
| `/bots list` | Each bot, what it is doing and where; the last failure reason |
| `/bots stop <name>` | Stop what it is doing |
| `/bots come <name>` | Walk to you |
| `/bots goto <name> <y>`, `<x z>` or `<x y z>` | Walk to a level, a column or a block |
| `/bots follow <name> <player>` | Follow a player |
| `/bots fly <name> <x> <z>` | Fly there with an elytra, boosting with fireworks from its hotbar |
| `/bots find <name> <blocks>` | Walk to the nearest of these blocks |
| `/bots mine <name> <blocks> [count]` | Mine blocks within 48 blocks (default 16) |
| `/bots mine <name> <blocks> [count] [y <level>] [heading]` | Branch mine toward `north`, `south`, `east` or `west` (default east), at a level if given |
| `/bots collect <name> <item> <count>` | Mine until it holds `count` more of an item |
| `/bots farm <name> <x1 y1 z1 x2 y2 z2>` | Harvest and replant the crops in a box |
| `/bots build <name> box <dx> <dy> <dz> <block> [hollow]` | Build a box |
| `/bots build <name> line <x\|y\|z> <length> <block>` | Build a line |
| `/bots build <name> file <path>` | Build blocks listed in a file |
| `/bots skill <name> <novice\|average\|expert>` | Set reaction time, aim and click speed |

`<blocks>` is a comma-separated list of block names. Counts are 1 to
4096. A plugin has at most 64 bots.

Bots use what is in their inventory: give them tools, blocks, seeds, bone
meal, armor, an elytra and fireworks with `/give <name> ...`. They pick
the right tool, wear armor they carry, avoid lava and long falls, hit back
at whoever hurts them, and respawn on their own after dying.

## Building

A build starts 2 blocks east of where you stand. Blocks may carry a
state, such as `oak_stairs[facing=east,half=top]`. A shape or file holds
at most 4096 blocks. The bot places blocks from its inventory, builds
scaffolding where it needs to, and stops with `missing materials` when it
runs out.

A build file is a text file in `plugins/bots/files/`, named by its path
under that directory (for example `houses/hut.txt`). Each line is an
offset from the build origin and a block, and blank lines and `#`
comments are skipped:

    # a 2-block pillar with a lantern on top
    0 0 0 stone_bricks
    0 1 0 stone_bricks
    0 2 0 lantern

Offsets are within 1024, and the file is at most 512 KiB.
