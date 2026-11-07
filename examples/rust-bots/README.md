# Bots

A Baritone-like bots plugin for tuff (WebAssembly, Rust on
`crates/tuff-plugin-sdk`). Plugin id `bots`. Bots are fake players that
path, mine, farm, build, fly and fight. Operators use `/bots`; see
[the bots guide](../../docs/guides/bots.md) for the commands.

## Build and install

    rustup target add wasm32-unknown-unknown
    cargo build --release --target wasm32-unknown-unknown \
        --manifest-path examples/rust-bots/Cargo.toml

Copy `examples/rust-bots/target/wasm32-unknown-unknown/release/bots.wasm`
into the server's `plugins/` directory and run `/tuff plugin load bots.wasm`.
`/bots build ... file` reads from `plugins/bots/files/` and needs
`/tuff plugin grant bots files all-requested`.

## Using bots from another plugin

`api/` is the `tuff-bots-api` crate, generated from `api/wit/bots.wit`.
A plugin depends on it, calls `control::Control` to spawn bots, set their
policy and skill, and start a process (goto, follow, mine, farm, build,
attack, fly and others), and serves `events::Serve` to hear when a
process finishes or fails and when a bot spawns, dies, respawns or is
removed. Each owner has
at most 64 bots, and only the owner can command its bots.
`examples/rust-bedwars` is a full consumer.

## Layout

- `src/core.rs`: bot registry and process manager, free of host calls.
- `src/lib.rs`: binds the core to the SDK.
- `src/commands.rs`: `/bots`.
- `src/path/`, `src/exec/`: pathfinding and movement.
- `src/mine.rs`, `src/farm.rs`, `src/build.rs`, `src/flight.rs`,
  `src/combat/`: the processes.

`cargo test --manifest-path examples/rust-bots/Cargo.toml` runs the unit
tests; `tests/world/bots.rs` runs bots on a real server.
