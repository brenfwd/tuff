# Example runtime plugins

Plugins are core WebAssembly modules built against the WIT world in
`crates/tuff-plugin-wasm/wit/`, with a `tuff-plugin` manifest custom section.
The Rust examples use the SDK prelude (`tuff_plugin_sdk::prelude::*`,
see `crates/tuff-plugin-sdk/GUIDE.md`). To try one, build it, copy the `.wasm`
into the server's `plugins/` directory and run `/tuff plugin load <file>`.

| you want to | read |
|---|---|
| say hello, keep per-player state, use a setting | `rust-hello/`, or `cpp-hello/` for C++ |
| write a command with typed arguments and permissions | `rust-worldedit/` |
| provide or call a typed interface between plugins | `rust-messaging-client/` (optional consumer; its provider is the test guest `crates/tuff-plugin-wasm/tests/guests/rust-messaging-provider/`) |
| call or serve a typed interface from C++ | `cpp-messaging/` (calls the same greeter provider) |
| serve a typed API other plugins drive (bots: fake players, owners, events) | `rust-bots/` (API crate `rust-bots/api/`, test consumer `crates/tuff-plugin-wasm/tests/guests/rust-bots-consumer/`) |
| call HTTP and keep files, async | `rust-discord-relay/`, or `cpp-io/` for C++ |
| add a dimension | `rust-tuffer/` |
| see a large plugin | `rust-worldedit/`, `rust-bedwars/` |

C++ plugins: read `crates/tuff-plugin-sdk/c/README.md` (toolchain, constraints, guide).

The raw host imports are only reachable through the SDK's `raw-abi` cargo feature (only the Rust conformance guest enables it). Other languages and the raw ABI are covered by conformance guests, not
examples: `crates/tuff-plugin-wasm/tests/guests/` holds the `wgreet` plugin in
Rust, C and C++ (`wit_guests.rs` drives every build; CI sets
`TUFF_EXAMPLE_PLUGINS=1` so a missing build fails). The hand-written WAT
greeter is `crates/tuff-plugin-wasm/examples/greeter.wat`. Other toolchains can
add the manifest with `add-manifest.py`.

## rust-worldedit

`rust-worldedit/` (plugin id `worldedit`, Rust on `crates/tuff-plugin-sdk`) is a
WorldEdit-style plugin: `/we pos1|pos2 [x y z]`, `set <state>`,
`replace <from> <to>`, `setblock <x> <y> <z> <state>`, `copy`, `paste`,
`clone <x> <y> <z>`, `undo`, `redo`, `limit [n]` and `status`, plus a
wooden-axe wand (left click pos1, right click pos2). Edits load and write
chunks nobody has loaded, split into 4096-block slices applied one per
tick with progress broadcasts, bounded by a per-player block limit and
recorded for undo and redo. Players need `worldedit.use` (operator level 2).
Build it with `cargo build --release --target wasm32-unknown-unknown
--manifest-path examples/rust-worldedit/Cargo.toml`. Biome edits wait on a
core biome write operation.

## rust-discord-relay

`rust-discord-relay/` (plugin id `discord-relay`, Rust on `crates/tuff-plugin-sdk`)
relays chat, joins and leaves to a Discord webhook, written on the SDK
prelude: typed `on::<ev::Chat/Join/Leave>` handlers, the `plugin!` manifest,
a secret `Setting` for the URL and the async wrappers: `net::Request` for the
POST (retrying on 429 after the `retry-after` delay via `sleep`) and `files`
for a `failed.log` of messages that could not be delivered. The owner sets
the URL with the `plugins.discord-relay.webhook` setting and grants
`cap.http = discord.com/api/webhooks/` and `cap.files`. Build it with `cargo
build --release --target wasm32-unknown-unknown --manifest-path
examples/rust-discord-relay/Cargo.toml`.

## rust-tuffer

`rust-tuffer/` (plugin id `tuffer`, Rust on `crates/tuff-plugin-sdk`) adds the
`tuffer:tuffer` dimension: floating islands with occasional pale oak trees
under a fixed pale sky, no natural spawning, no sleeping and water that
stays, all from registry data so the vanilla client needs no resource pack.
A glowstone frame (nether portal shape rules) lit by using a water bucket on
it fills with end gateways and links the overworld and the tuffer at 1:128,
finding the matching portal on the other side or building one. The owner must
grant `cap.dimension = tuffer:tuffer`. While it is loaded it claims every
end gateway as a plugin portal. Build it with `cargo build --release --target
wasm32-unknown-unknown --manifest-path examples/rust-tuffer/Cargo.toml`.

## rust-bedwars

`rust-bedwars/` (plugin id `bedwars`, Rust on `crates/tuff-plugin-sdk`) is a
Hypixel-style Bed Wars game: a lobby dimension and four arena dimensions
(`cap.dimension = bedwars:*`), maps as JSON in its `maps` setting, teams
with beds and forges, diamond and emerald generators on a timeline, bed and
block rules, final kills and `/bedwars` (`/bw`) to queue and manage games.
Its README says how to play and set it up. Build it with `cargo build
--release --target wasm32-unknown-unknown --manifest-path
examples/rust-bedwars/Cargo.toml`; its game logic has native unit tests
(`cargo test --manifest-path examples/rust-bedwars/Cargo.toml`) and
`tests/world/bedwars.rs` plays a game with the module loaded.
