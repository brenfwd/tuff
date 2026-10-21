# cpp-greeter

The `wgreet` plugin (see `../rust-wit-greeter`) in C++ on `tuff.hpp` from
`crates/tuff-plugin-sdk/c`: freestanding (no exceptions, RTTI or standard
library), written on the high-level SDK (`TUFF_PLUGIN`, `Tree`/`Node`,
`on<ev::Join>`). A version on the raw binding layer is a conformance fixture in
`crates/tuff-plugin-wasm/tests/guests/cpp-raw-greeter`.

    ./build.sh            # or CC=/opt/homebrew/opt/llvm/bin/clang CXX=/opt/homebrew/opt/llvm/bin/clang++ ./build.sh

The test `crates/tuff-plugin-wasm/tests/wit_guests.rs` loads
`build/wit-greeter.wasm` when it is built.
