//! The C++ interface codec (`tuff/wire.hpp` and the generated header of the
//! shared fixture) against the Rust codec: the guest `cpp-iface` encodes the
//! same values as `crates/tuff-plugin-sdk/c/fixtures/iface_demo.golden` byte for
//! byte, and refuses malformed payloads. Skipped when the guest is not
//! built, unless `TUFF_EXAMPLE_PLUGINS=1` requires it.

use std::path::{Path, PathBuf};

use tuff_plugin_wasm::sandbox::{Limits, Player, Sandbox, engine};

fn built() -> Option<Vec<u8>> {
    let path: PathBuf =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/guests/cpp-iface/build/iface.wasm");
    match std::fs::read(&path) {
        Ok(wasm) => Some(wasm),
        Err(_) if std::env::var("TUFF_EXAMPLE_PLUGINS").as_deref() == Ok("1") => {
            panic!("{} is not built", path.display())
        }
        Err(_) => {
            eprintln!("skipping: {} is not built", path.display());
            None
        }
    }
}

fn golden() -> Vec<(String, String)> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../tuff-plugin-sdk/c/fixtures/iface_demo.golden");
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| {
            let (name, hex) = line.split_once(' ').unwrap();
            (name.to_owned(), hex.to_owned())
        })
        .collect()
}

fn plugin(wasm: &[u8]) -> Sandbox {
    let mut plugin = Sandbox::new(&engine(), wasm, "iface", Limits::default()).unwrap();
    plugin.register().unwrap();
    plugin.enable().unwrap();
    plugin
}

fn run(plugin: &mut Sandbox, line: &str) -> Result<Vec<String>, String> {
    match plugin.command(Player::new(1, "Alex"), line) {
        Ok((Ok(_), effects)) => Ok(effects.replies),
        Ok((Err(err), _)) => Err(format!("{err:?}")),
        Err(err) => Err(format!("{err:?}")),
    }
}

#[tuff_core::watchdog]
#[test]
fn cpp_encodes_the_fixture_like_rust() {
    let Some(wasm) = built() else { return };
    let mut plugin = plugin(&wasm);
    for (name, hex) in golden() {
        let replies = run(&mut plugin, &format!("iface encode {name}")).unwrap();
        assert_eq!(replies, [hex], "{name}");
    }
}

#[tuff_core::watchdog]
#[test]
fn cpp_decodes_the_golden_bytes_and_encodes_them_again() {
    let Some(wasm) = built() else { return };
    let mut plugin = plugin(&wasm);
    for (name, hex) in golden() {
        let ty = match name.as_str() {
            "outcome-err" | "outcome-ok" => "outcome",
            "pick-some" | "pick-none" | "find-err" => continue,
            other => other,
        };
        let replies = run(&mut plugin, &format!("iface decode {ty} {hex}")).unwrap();
        assert_eq!(replies, [format!("ok {hex}")], "{name}");
    }
}

#[tuff_core::watchdog]
#[test]
fn cpp_refuses_malformed_payloads() {
    let Some(wasm) = built() else { return };
    let mut plugin = plugin(&wasm);
    let named = golden()
        .into_iter()
        .find(|(name, _)| name == "named")
        .unwrap()
        .1;
    let cases = [
        ("named", named[..named.len() - 2].to_owned()),
        ("named", format!("{named}00")),
        ("point", "0300".to_owned()),
        ("color", "03".to_owned()),
        ("shapes", "0109".to_owned()),
        ("perms", "0800000000000000".to_owned()),
        ("ids", "ffffffff0f".to_owned()),
        ("ids", "80808080".to_owned()),
        ("outcome", "02".to_owned()),
    ];
    for (ty, hex) in cases {
        let replies = run(&mut plugin, &format!("iface decode {ty} {hex}")).unwrap();
        assert_eq!(replies, ["bad"], "{ty} {hex}");
    }
    let every = golden()
        .into_iter()
        .find(|(name, _)| name == "every")
        .unwrap()
        .1;
    let patch = |byte_offset: usize, with: &str| {
        let at = byte_offset * 2;
        let mut out = every.clone();
        out.replace_range(at..at + with.len(), with);
        out
    };
    let utf8_at = every.len() / 2 - 5;
    for hex in [
        patch(42, "02"),
        patch(43, "00d80000"),
        patch(utf8_at, "c328"),
    ] {
        let replies = run(&mut plugin, &format!("iface decode every {hex}")).unwrap();
        assert_eq!(replies, ["bad"], "{hex}");
    }
}
