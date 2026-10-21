//! Compares the plugin WIT files against checked-in snapshots. Ignored until
//! the 0.9 ABI freeze: before then the ABI changes freely. At freeze, run
//! `cargo test -p tuff-plugin-wasm --test abi_golden -- --ignored`; to
//! re-snapshot a deliberate change, set `TUFF_UPDATE_ABI_GOLDEN=1`.

use std::path::Path;

const FILES: [&str; 2] = ["plugin.wit", "capabilities.wit"];

#[tuff_core::watchdog]
#[test]
#[ignore = "ABI is unstable until 0.9"]
fn wit_matches_golden_snapshot() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for name in FILES {
        let live = root.join("wit").join(name);
        let golden = root.join("tests/golden").join(name);
        if std::env::var("TUFF_UPDATE_ABI_GOLDEN").as_deref() == Ok("1") {
            std::fs::copy(&live, &golden).unwrap();
            continue;
        }
        let live = std::fs::read_to_string(live).unwrap().replace("\r\n", "\n");
        let golden = std::fs::read_to_string(golden)
            .unwrap()
            .replace("\r\n", "\n");
        assert!(
            live == golden,
            "{name} changed from the golden ABI snapshot"
        );
    }
}
