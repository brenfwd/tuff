# tuff-client-harness

Opt-in tests that launch the real vanilla client. Never part of `cargo t`.

    env TUFF_CLIENT_TESTS=1 TUFF_ACCEPT_MOJANG_EULA=1 cargo test -p tuff-client-harness --test client -- --ignored --test-threads=1


Needs a desktop session; the first run downloads the client into `VANILLA_CACHE_DIR`
(`TUFF_CLIENT_ASSETS=minimal` skips sounds and lang). At most four clients machine-wide via slot locks (`CLIENT_SLOTS`).
