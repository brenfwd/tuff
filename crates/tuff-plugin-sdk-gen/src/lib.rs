// SPDX-License-Identifier: MIT
//! Generates the plugin SDK bindings that cannot come from a build-time
//! macro: C (through `wit-bindgen-c`) and the typed surface Rust and C++
//! share ([`surface`]). The outputs are checked in; the
//! `bindings_are_current` tests fail when the WIT changed, and
//! `cargo run -p tuff-plugin-sdk-gen` rewrites them.

use std::path::{Path, PathBuf};

pub mod iface;
pub mod surface;

use anyhow::{Context, Result};
use wit_parser::{Resolve, WorldId};

/// The world the C bindings target.
pub const WORLD: &str = "plugin-worldgen";

pub fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

pub fn wit_dir() -> PathBuf {
    root().join("crates/tuff-plugin-wasm/wit")
}

pub fn load() -> Result<(Resolve, WorldId)> {
    let mut resolve = Resolve {
        all_features: true,
        ..Resolve::default()
    };
    let (package, _) = resolve.push_dir(wit_dir())?;
    let world = resolve.select_world(&[package], Some(WORLD))?;
    Ok((resolve, world))
}

/// A generated file: its path from the repository root and contents.
pub struct Output {
    pub path: PathBuf,
    pub contents: Vec<u8>,
}

/// `crates/tuff-plugin-sdk/c/bindings/plugin.{h,c}`.
pub fn c_bindings() -> Result<Vec<Output>> {
    let (mut resolve, world) = load()?;
    let opts = wit_bindgen_c::Opts {
        no_object_file: true,
        rename_world: Some("plugin".into()),
        ..Default::default()
    };
    let mut files = wit_bindgen_core::Files::default();
    opts.build().generate(&mut resolve, world, &mut files)?;
    Ok(files
        .iter()
        .map(|(name, contents)| Output {
            path: PathBuf::from("crates/tuff-plugin-sdk/c/bindings").join(name),
            contents: widen_flag_shifts(contents),
        })
        .collect())
}

fn widen_flag_shifts(contents: &[u8]) -> Vec<u8> {
    let Ok(text) = std::str::from_utf8(contents) else {
        return contents.to_vec();
    };
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let wide = line.starts_with("#define ")
            && line
                .rsplit_once("(1 << ")
                .and_then(|(_, n)| n.trim_end().strip_suffix(')'))
                .and_then(|n| n.parse::<u32>().ok())
                .is_some_and(|n| n >= 31);
        if wide {
            out.push_str(&line.replace("(1 << ", "(1ULL << "));
        } else {
            out.push_str(line);
        }
    }
    out.into_bytes()
}

pub fn fixture() -> Result<Output> {
    let dir = root().join("crates/tuff-plugin-sdk-gen/fixtures/demo");
    Ok(Output {
        path: PathBuf::from("crates/tuff-plugin-sdk/src/iface_fixture.rs"),
        contents: iface::generate_dir(&dir)?.into_bytes(),
    })
}

pub fn fixture_cpp() -> Result<Output> {
    let dir = root().join("crates/tuff-plugin-sdk-gen/fixtures/demo");
    Ok(Output {
        path: PathBuf::from("crates/tuff-plugin-sdk/c/fixtures/iface_demo.hpp"),
        contents: iface::generate_cpp_dir(&dir)?.into_bytes(),
    })
}

/// The C++ header of the messaging test provider's interface.
pub fn greeter_cpp() -> Result<Output> {
    let dir = root().join("crates/tuff-plugin-wasm/tests/guests/rust-messaging-provider/wit");
    Ok(Output {
        path: PathBuf::from("examples/cpp-messaging/greeter_api.hpp"),
        contents: iface::generate_cpp_dir(&dir)?.into_bytes(),
    })
}

/// The bots plugin's Rust API module.
pub fn bots_api() -> Result<Output> {
    let dir = root().join("examples/rust-bots/api/wit");
    Ok(Output {
        path: PathBuf::from("examples/rust-bots/api/src/api.rs"),
        contents: iface::generate_dir(&dir)?.into_bytes(),
    })
}

/// Every generated file.
pub fn all() -> Result<Vec<Output>> {
    let mut out = c_bindings()?;
    let plugin_h = out
        .iter()
        .find(|o| o.path.ends_with("plugin.h"))
        .context("wit-bindgen-c wrote no plugin.h")?;
    let unraw = surface::unraw(std::str::from_utf8(&plugin_h.contents)?)?;
    out.push(unraw);
    let (resolve, _) = load()?;
    out.extend(surface::generate(&resolve)?);
    out.push(fixture()?);
    out.push(fixture_cpp()?);
    out.push(greeter_cpp()?);
    out.push(bots_api()?);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn bindings_are_current() {
        let mut stale = Vec::new();
        for output in all().unwrap() {
            let path = root().join(&output.path);
            if std::fs::read(&path).ok().as_deref() != Some(&output.contents[..]) {
                stale.push(output.path.display().to_string());
            }
        }
        assert!(
            stale.is_empty(),
            "stale SDK bindings {stale:?}: run `cargo run -p tuff-plugin-sdk-gen`"
        );
    }

    fn gen_src(body: &str) -> Result<String> {
        iface::generate_source(
            "t.wit",
            &format!("package a:b;\ninterface i {{\n{body}\n}}"),
        )
    }

    fn key_line(code: &str, name: &str) -> String {
        code.lines()
            .find(|l| l.contains(&format!("pub const {name}_KEY")))
            .unwrap()
            .to_owned()
    }

    #[tuff_core::watchdog]
    #[test]
    fn method_keys_follow_the_signature() {
        let base = gen_src("record r { x: u8 }\nf: func(a: r) -> u8;").unwrap();
        let again = gen_src("record r { x: u8 }\nf: func(a: r) -> u8;").unwrap();
        assert_eq!(key_line(&base, "F"), key_line(&again, "F"));
        let renamed_param = gen_src("record r { x: u8 }\nf: func(b: r) -> u8;").unwrap();
        assert_eq!(key_line(&base, "F"), key_line(&renamed_param, "F"));
        for changed in [
            "record r { x: u16 }\nf: func(a: r) -> u8;",
            "record r { y: u8 }\nf: func(a: r) -> u8;",
            "record r { x: u8 }\nf: func(a: r) -> u16;",
            "record r { x: u8 }\nf: func(a: r, b: u8) -> u8;",
        ] {
            let other = gen_src(changed).unwrap();
            assert_ne!(key_line(&base, "F"), key_line(&other, "F"), "{changed}");
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn unsupported_shapes_are_refused() {
        for body in [
            "resource r { constructor(); }",
            "f: func(x: future<u8>);",
            "f: func(x: stream<u8>);",
            "f: func(x: map<u8, u8>);",
            "f: func(x: error-context);",
        ] {
            assert!(gen_src(body).is_err(), "{body}");
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn cpp_and_rust_headers_share_method_keys() {
        let dir = root().join("crates/tuff-plugin-sdk-gen/fixtures/demo");
        let rust = iface::generate_dir(&dir).unwrap();
        let cpp = iface::generate_cpp_dir(&dir).unwrap();
        let mut keys = 0;
        for line in rust
            .lines()
            .filter(|l| l.contains("pub const") && l.contains("_KEY"))
        {
            let name = line
                .split("pub const ")
                .nth(1)
                .unwrap()
                .split(':')
                .next()
                .unwrap();
            let value = line.split("= ").nth(1).unwrap().trim_end_matches(';');
            assert!(
                cpp.contains(&format!("inline constexpr uint64_t {name} = {value}ull;")),
                "{name} {value}"
            );
            keys += 1;
        }
        assert!(keys >= 8);
    }

    #[tuff_core::watchdog]
    #[test]
    fn cpp_refuses_what_the_wire_cannot_carry() {
        for body in [
            "resource r { constructor(); }",
            "f: func(x: stream<u8>);",
            "f: func(x: tuple<u8, u8, u8, u8, u8>);",
            "f: func(a: u8, b: u8, c: u8, d: u8, e: u8, f: u8, g: u8);",
        ] {
            let source = format!("package a:b;\ninterface i {{\n{body}\n}}");
            assert!(
                iface::generate_cpp_source("t.wit", &source).is_err(),
                "{body}"
            );
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn cpp_keywords_are_escaped_and_forward_types_ordered() {
        let source = "package a:b;\ninterface i {\nrecord outer { inner: inner, new: u8 }\nrecord inner { x: u8 }\nf: func(class: outer);\n}";
        let cpp = iface::generate_cpp_source("t.wit", source).unwrap();
        assert!(cpp.find("struct Inner").unwrap() < cpp.find("struct Outer").unwrap());
        assert!(cpp.contains("new_{}"));
        assert!(cpp.contains("::a::b") || cpp.contains("namespace i"));
    }
}
