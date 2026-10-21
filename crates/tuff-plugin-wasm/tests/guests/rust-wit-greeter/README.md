# rust-wit-greeter

The `wgreet` example plugin in Rust: `wit_bindgen::generate!` over
`crates/tuff-plugin-wasm/wit/plugin.wit`, built for `wasm32-unknown-unknown` as
a plain core module (no componentization, no WASI). It registers `/wgreet`
as a typed command tree (player, integer and double arguments, a
permission and a suggestion flag) and broadcasts joins and leaves.

    cargo build --release --target wasm32-unknown-unknown --manifest-path crates/tuff-plugin-wasm/tests/guests/rust-wit-greeter/Cargo.toml

The test `crates/tuff-plugin-wasm/tests/wit_guests.rs` loads
`target/wasm32-unknown-unknown/release/wit_greeter.wasm` when it is built.
