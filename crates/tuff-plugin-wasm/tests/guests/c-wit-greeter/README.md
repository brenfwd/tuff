# c-wit-greeter

The `wgreet` plugin (see `../rust-wit-greeter`) in C on the C SDK in
`crates/tuff-plugin-sdk/c`, with the manifest written by `TUFF_MANIFEST`.

    ./build.sh            # or CC=/opt/homebrew/opt/llvm/bin/clang ./build.sh on macOS

The test `crates/tuff-plugin-wasm/tests/wit_guests.rs` loads
`build/wit-greeter.wasm` when it is built.
