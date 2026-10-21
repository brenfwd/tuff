# Your first tuff plugin in 5 minutes

Plugins are WebAssembly modules the server loads at runtime. Rust has the
fullest SDK and C++20 is close behind; both have the same high-level
surface.

## 1. Make a project

    tuff plugin new greeter                # Rust
    tuff plugin new greeter --lang cpp     # C++

This writes `./greeter`: a working plugin with a command, a setting, stored
data, event handlers and a repeating task. Rendered with id `hello`, the
templates are exactly `examples/rust-hello` and `examples/cpp-hello`, which
CI builds and tests (`crates/tuff-plugin-wasm/tests/hello_guests.rs`).

## 2. Build it

- Rust: `rustup target add wasm32-unknown-unknown`, then `cargo build --release`.
  The plugin is `target/wasm32-unknown-unknown/release/greeter.wasm`.
- C++: needs clang and wasm-ld (`apt install clang lld`, or
  `brew install llvm lld`), then `./build.sh`. The plugin is
  `build/greeter.wasm`.

## 3. Load it

Copy the `.wasm` into the server's `plugins/` directory and run
`/tuff plugin load greeter.wasm`. Try `/greeter`, `/greeter help` and
`/greeter later`.

## 4. Change it

| what | Rust | C++ |
|---|---|---|
| manifest | `plugin!(Greeter { id: "greeter", version: "0.1.0" })` | `TUFF_PLUGIN(Greeter, "greeter", "0.1.0", "...")` |
| a command | `command("x").about("..").executes(\|ctx\| ..).help().register()?` | `Tree t("x"); t.root().about("..").executes(..); t.help().register_command()` |
| an async command | `.executes_async(\|ctx\| async move { sleep(40).await; .. })` | `.executes_async([](Ctx ctx) -> Task { co_await sleep(40); .. })` |
| a permission | `.requires("x.admin", 2)` | `.require("x.admin", 2)` |
| an event | `on::<ev::Join>(\|p\| ..)?` | `on<ev::Join>([](const Player &p) { .. })` |
| a filtered event | `listen::<ev::BlockBreak>().in_dimension(..).block(..).run(..)?` | `listen<ev::BlockBreak>().in_dimension(..).block(..).run(..)` |
| a setting | `static S: Setting<String> = Setting::new("k", "v", "doc");` | `static constinit Setting<String> s("k", "v", "doc");` |
| stored data | `static V: StoredMap<u32> = StoredMap::new("visits");` | `static constinit StoredMap<uint32_t> v("visits");` |
| later / repeatedly | `after(ticks, f)`, `every(ticks, f)` | `after(ticks, f)`, `every(ticks, f)` |
| colors | `text!("hi".gold(), " ", name.aqua().bold())` | `Text() << gold("hi") << " " << aqua(name).bold()` |

Plugins run single-threaded inside the server (the host interpreter has no
wasm threads): anything that waits is an `async` task (Rust) or a coroutine
(C++), resumed by the server's timer and I/O callbacks.

The lower layers stay available: `tuff_plugin_sdk::host` (Rust) and
`tuff.h` (C) are the raw imports; `crates/tuff-plugin-wasm/wit/` is the ABI.
The whole ABI may change before 0.9. In the WIT, `@since(version = 0.1.0)`
marks the candidates for the 0.9 freeze (logging, commands, permissions,
settings, state and storage, timers, files, networking) and
`@unstable(feature = experimental)` marks parts expected to change shape
(menus, panel pages, block rules, runtime dimensions); both SDKs enable
`experimental`, so bindings you generate yourself need it too.
`tuff.hpp` does not expose `tuff.h`: C++ code that needs the raw layer
includes `tuff/raw.h` in place of it.

Dependency keys (comma separated ids, `id >= x.y.z` bounds allowed except in
`load-before`; Rust `plugin!` spells them `depends`, `soft_depends`,
`conflicts`, `load_before`):

- `depends`: required; refused if missing or too old.
- `soft-depends`: optional; loads after it when present, and without it when
  absent or below the bound (a warning is logged).
- `conflicts`: refused while a matching plugin is loaded, in both directions.
- `load-before`: ordering only, as if the named plugin soft-depended on this one.

Cycles of soft or load-before edges are dropped with a warning; a cycle of
`depends` refuses every plugin in it. An id may not appear in `depends` or
`soft-depends` and in `conflicts`. All keys together hold at most 32 entries.

## Cancel model

A plugin cancels vanilla behavior by registering a filter. The server
evaluates filters before acting, cancels on a match, and (once the plugin
subscribes) reports the action as an event whose `cancelled` flag says
whether a filter matched. Plugins never veto after the fact; an event is
always a report.

| Registration | Filter fields | Report |
| --- | --- | --- |
| `intercept-click` | kind, item, tag, block | `left-click` / `right-click` |
| `intercept-item` | kind, item, tag | `item-drop`, `item-pickup`, `item-swap`, `item-craft` |
| `intercept-inventory` | container, slot range, item, click (a bare container bars opening it; click `close` with only a container keeps that window open) | `inventory-click` |
| `intercept-entity-interact` | kind, tag | `entity-interact` |
| `intercept-attack` | kind, tag, held-item, attacker, dimension, area | `player-attack` |
| `intercept-damage` | see the WIT | damage events |
| `intercept-command` / `intercept-chat` | prefix, contains, action (cancel with an optional message, or rewrite) | none |
| `intercept-state` | state transitions | state events |
| `block-rule` (protected box) | dimension, box, place, break-placed, breakable, explosion options | none (blocks are resent) |

Conventions shared by all filters:

- Every set field must match; an unset field (`none`) matches anything.
  A filter with no fields set matches every action of its kind.
- Several filters combine as a union: an action is cancelled if any
  filter of the plugin (or of any loaded plugin) matches.
- Item selectors are an item id plus an optional top-level
  `custom_data` key (`tag`). Entity selectors are an entity type id
  plus an optional scoreboard tag. Block selectors are block ids.
- Spatial scoping (dimension, box) exists on `intercept-attack` and
  `block-rule`; the other filters are global, so scope them in the
  plugin by checking the reported event's player and position.
- Feedback to the player is the plugin's job: a cancelling filter is
  silent, so subscribe to the report event and send a message or action
  bar from the handler. Only `intercept-command` and `intercept-chat`
  carry an inline cancel message.
- Cancelling never changes server-side state; blocks and inventories
  affected by a cancelled action are resent to the client.

Click, item and entity-interact filters carry a `filter-scope`
(`events::intercept_click_scoped`, `intercept_item_scoped`,
`menu::intercept_entity_interact_scoped`): a dimension, an area (the
player's position, or the entity's for entity filters) and a `bypass`
permission node whose holders are never matched.

## Messaging between plugins

A provider declares an interface and routes method keys (u64) to
handlers in `register`; callers `request` (awaitable) or `notify`
(one way). Delivery happens in the same host turn, and a handler may
answer later.

```rust
// Provider plugin
fn register() -> Result<(), String> {
    messaging::provide("greeter", vec![
        (1, messaging::handler(|msg, reply| {
            reply.ok([b"hello ", msg.payload.as_slice()].concat());
        })),
        (2, messaging::handler(|_, reply| {
            spawn(async move {
                sleep(20).await;
                reply.ok(b"slow");
            });
        })),
    ]).map_err(|e| format!("{e:?}"))
}

// Caller plugin
spawn(async {
    match messaging::request("greeter-plugin", "greeter", 1, b"bob".to_vec())
        .timeout(100)
        .await
    {
        Ok(bytes) => log(Level::Info, &String::from_utf8_lossy(&bytes)),
        Err(e) => log(Level::Warn, &format!("greeter unavailable: {e}")),
    }
});
```

- `Target::any()` (or a plugin id as `&str`) picks the provider.
  A request future sends on its first poll; dropping it ignores a
  late reply.
- Failures are values, never panics: `NoProvider`, `Ambiguous`,
  `NoMethod`, `TargetGone`, `Faulted`, `TimedOut`, `Busy`, `TooLarge`,
  `Refused(text)` (`CallError` implements `Display`). An absent
  provider fails the request at once.
- A `Reply` answers once (`ok` or `err`). Dropping it unanswered
  refuses the request with "dropped" so the caller does not wait for
  its timeout. A request for an unrouted method is refused; an
  unrouted notify reaches `Plugin::message`.
- `Plugin::plugin_change(id, version, loaded)` reports declared
  dependencies loading or unloading.

### Typed interfaces from WIT

A provider owns a WIT package (for example `package tuff-bots:api;`
with `interface control { ... }`) in a small api crate and generates
Rust from it; consumers depend on that crate. The host never reads the
WIT, it routes bytes.

```sh
cargo run -p tuff-plugin-sdk-gen -- interface path/to/wit --rust src/api.rs
```

Check the output in next to the WIT and add a test that regenerating
gives the same bytes. Per WIT interface the output has a module with
the types (records, variants, enums, flags, options, results, tuples,
lists; resources, futures and streams are refused), a client struct
(`Control::any()` or `Control::at("plugin-id")`, one async fn per func,
plus `notify_*` for funcs without a result), a `Serve` trait and
`serve(impl)` to call in `register`. A func returning `result<T, E>`
gives `Result<T, wire::Error<E>>` (`Error::Call` or `Error::App`);
otherwise `Result<T, CallError>`. Method keys are FNV-1a 64 of the
interface name, func name and structural signature, so a changed
signature is `NoMethod` for old callers instead of a misdecode. Event
interfaces work the same way: consumers `serve` them, the provider
calls the client.

`crates/tuff-plugin-wasm/tests/guests/rust-messaging-provider` (test provider, `wit/greeter.wit` and the checked-in
`src/api.rs`) and `examples/rust-messaging-client` (soft-depends on it, falls back on
`NoProvider`, implements the `counted` event interface and reacts to
`plugin_change`) are a working pair; `tests/world/wasm_messaging.rs` loads,
unloads and reloads them. Both request `cap.cross-plugin = all`.

`tuff_plugin_sdk::wire` is the payload codec (little-endian ints, LEB128
lengths and discriminants, flags as u64); decoding checks every length
against the remaining bytes and never panics.

## Particles

`World::particles` shows a particle effect to the players near it (32 blocks,
512 with `.force()`), and `particles_for` to one player. Options are typed:
`Particle::dust(0xff0000, 1.0, at)`, `Particle::dust_transition`,
`Particle::block("minecraft:falling_dust", "minecraft:sand", at)`,
`Particle::item("minecraft:apple", at)`, or `Particle::new(id, at)` with
`.options(ParticleOptions::...)` for the rest (entity effect and other ARGB
colours, vibration, shriek, sculk charge, trail, geyser, dragon breath,
spell). Options the particle rejects, as `/particle` would, return
`invalid-state`. The call needs no grant; one call may queue 256 effects.

## Call groups that need a grant

Five groups of host calls are closed until the plugin requests them in its
manifest and the server owner grants them (`tuff plugin grant <id> <group> all`,
or `all-requested`): `cap.world-write = all` (block-set, spawn-entity,
remove-entity, entity-merge, entity-set-name, entity-strike, glide-entities,
teleport-entity, add-block-rule, add-change-rule), `cap.player-write = all`
(teleports, inventory, vitals, xp, effects, game mode, kill/respawn, team and
advancement changes), `cap.player-read = all` (online-players, player-info,
player-location, acting-inventory) and `cap.cross-plugin = all` (request and
notify) and `cap.panel = all` (register-panel-page, register-panel-api and
register-panel-card; a panel page's script runs in the admin's browser, so grant
it only to plugins you trust with the panel). A denied write returns `not-granted`; denied reads return nothing.
Every other call is open; `caps::classes::HOST_CALL_GROUPS` lists all of them.
