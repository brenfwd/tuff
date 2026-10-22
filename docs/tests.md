# Tests

cargo t

`cargo t` is an alias for `cargo test --workspace --lib --bins --tests
--no-fail-fast`. Never run `cargo test --all-targets`, `--benches` or
`--bench`: those select the benchmarks, which are load runs, not tests.

The benchmarks in `benches/` are load runs. Run one at a time, in release,
on a quiet machine, for example `cargo bench --bench village`.

The real-client tests in `crates/tuff-client-harness` are opt-in and never part
of `cargo t`. They run the vanilla Minecraft client, which means accepting
the Minecraft EULA (https://aka.ms/MinecraftEULA), so they need
`TUFF_ACCEPT_MOJANG_EULA=1` as well as `TUFF_CLIENT_TESTS=1`; without both
they skip and report `ok`. At most four clients run at once on a machine:

    env TUFF_CLIENT_TESTS=1 TUFF_ACCEPT_MOJANG_EULA=1 cargo test -p tuff-client-harness --test client -- --ignored --test-threads=1

The vanilla drops parity test (`tests/server/vanilla_drops.rs`) starts a real
vanilla server and is ignored by default. Narrow it with
`TUFF_DROPS_FILTER`, a comma-separated list of substrings matched against
"fixture | command". A full run takes about 40 minutes, so run it in
chunks of about 8 fixtures:

    env TUFF_VANILLA_TESTS=1 TUFF_ACCEPT_MOJANG_EULA=1 TUFF_DROPS_FILTER=kelp,cactus cargo test --test server vanilla_drops -- --ignored --nocapture

The repository scripts run on Linux and macOS. Their logic is tested with
plain Python 3:

    for d in scripts scripts/tests; do python3 -m unittest discover -s "$d" -p 'test_*.py'; done
