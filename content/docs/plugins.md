---
title: WebAssembly plugins
description: Sandboxed runtime plugins: managing, grants, writing one in Rust and the ABI.
group: Plugins
source: docs/plugins.md
---

External plugins are WebAssembly modules that the server loads, unloads
and reloads while it runs, in a sandbox (`crates/tuff-plugin-wasm`, on the
pure-Rust wasmi interpreter). They are separate from the built-in features
above: a feature is compiled into the server and switched on with
`tuff.features.<id>.enabled`, while a runtime plugin is a `.wasm` file and
needs no setting of its own. The plugin host is always part of the `tuff`
binary and is on by default.

The plugin ABI is not stable. `api = 0` is the only version, and the ABI
may change in any release until it is frozen, which is planned for 0.9.

## Where plugins live

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

## Managing plugins

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

## Checking that a plugin loaded

At start-up the log has `Loaded <id> <version>` for each module that
loaded and `wasm plugin not loaded: <reason>` for each that did not.
`/tuff plugin load` replies with the same text. `/tuff plugin list` prints
`<n> runtime plugins loaded` and then one line per plugin, for example
`hello 0.1.0 /hello: Greets players and counts their visits`.

## Plugin grants

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

## Writing a plugin in Rust

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

## Other examples and languages

`examples/README.md` covers building each example. Besides `rust-hello`,
`examples/` has more Rust plugins (`rust-worldedit`, `rust-tuffer` with a
custom dimension, and `rust-messaging-client` for plugin-to-plugin
messaging) and C++ plugins (`cpp-hello`, `cpp-io`, `cpp-messaging`) built
on the C/C++ SDK in `crates/tuff-plugin-sdk/c`, whose bindings are generated by
wit-bindgen. `crates/tuff-plugin-wasm/examples/greeter.wat` is a hand-written
plugin in WebAssembly text.

## The WorldEdit example plugin

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

## What the ABI offers

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
