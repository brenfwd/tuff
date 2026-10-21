//! The C++ SDK keeps the raw binding layer out of `tuff.hpp`: a plugin on
//! the high-level API compiles with `-Werror=deprecated`, raw functions,
//! binding types, export helpers and binding constants do not resolve
//! through `tuff.hpp` (not even after `using namespace tuff`), and
//! `tuff/raw.h` still reaches them. Needs a clang++ with the wasm32 target
//! (`CXX` picks it); skipped without one unless `TUFF_EXAMPLE_PLUGINS=1`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const PLUGIN: &str = r#"
#include "tuff.hpp"
struct Probe {
    static tuff::Status on_register() {
        tuff::Tree tree("probe");
        tree.root().executes([](tuff::Call &call) -> tuff::Result {
            call.reply(tuff::Text() << tuff::gold("hi"));
            return 1;
        });
        tuff::log(tuff::Level::Info, "registered");
        return tree.register_command();
    }
};
TUFF_PLUGIN(Probe, "probe", "0.1.0", "visibility probe");
"#;

const HIDDEN: &[(&str, &str)] = &[
    ("tuff_reply", "tuff_reply(\"x\");"),
    ("tuff_event_t", "tuff_event_t *ev = nullptr; (void)ev;"),
    ("plugin_string_t", "plugin_string_t s{}; (void)s;"),
    (
        "tuff_plugin_types_player_t",
        "tuff_plugin_types_player_t p{}; (void)p;",
    ),
    (
        "tuff_plugin_host_broadcast",
        "(void)&tuff_plugin_host_broadcast;",
    ),
    (
        "exports_tuff_plugin_guest_event_free",
        "exports_tuff_plugin_guest_event_free(nullptr);",
    ),
    ("TUFF_MANIFEST", "TUFF_MANIFEST(\"id = x\");"),
    ("tuff_subscribe", "using namespace tuff; tuff_subscribe(0);"),
    (
        "TUFF_PLUGIN_TYPES_LOG_LEVEL_INFO",
        "int level = TUFF_PLUGIN_TYPES_LOG_LEVEL_INFO; (void)level;",
    ),
    (
        "TUFF_PLUGIN_TYPES_EVENT_JOIN",
        "int tag = TUFF_PLUGIN_TYPES_EVENT_JOIN; (void)tag;",
    ),
];

const RAW: &str = r#"
#include "tuff/raw.h"
extern "C" void exports_tuff_plugin_guest_on_event(tuff_event_t *ev) {
    if (ev->tag == TUFF_PLUGIN_TYPES_EVENT_JOIN) {
        plugin_string_t name = ev->val.join.name;
        tuff_reply_str(name);
    }
    tuff_log(TUFF_PLUGIN_TYPES_LOG_LEVEL_INFO, "raw");
    exports_tuff_plugin_guest_event_free(ev);
}
"#;

fn sdk() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../tuff-plugin-sdk/c")
}

/// Compiles `source` as a C++ plugin file the way `build.sh` does, syntax
/// only; the compiler's stderr on failure.
fn compile(source: &str) -> Result<(), String> {
    let sdk = sdk();
    let cxx = std::env::var("CXX").unwrap_or_else(|_| "clang++".into());
    let mut child = Command::new(cxx)
        .args([
            "--target=wasm32",
            "-ffreestanding",
            "-nostdlib",
            "-fno-builtin",
            "-std=c++20",
            "-fno-exceptions",
            "-fno-rtti",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-Werror=deprecated",
            "-fsyntax-only",
        ])
        .arg(format!("-I{}", sdk.join("include").display()))
        .arg(format!("-I{}", sdk.join("bindings").display()))
        .arg(format!("-I{}", sdk.display()))
        .args(["-x", "c++", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run the C++ compiler: {e}"))?;
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(source.as_bytes())
        .map_err(|e| e.to_string())?;
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).into_owned())
    }
}

fn toolchain() -> bool {
    match compile(PLUGIN) {
        Ok(()) => true,
        Err(err) if std::env::var("TUFF_EXAMPLE_PLUGINS").as_deref() == Ok("1") => {
            panic!("a high-level C++ plugin does not compile:\n{err}")
        }
        Err(err) => {
            eprintln!("skipping: no usable wasm32 clang++ (set CXX):\n{err}");
            false
        }
    }
}

#[tuff_core::watchdog]
#[test]
fn typed_manifest_macros_compile() {
    if !toolchain() {
        return;
    }
    let source = format!(
        "{PLUGIN}\nTUFF_DEPENDS(\"a >= 1.0.0\");\nTUFF_CAP_HTTP(\"example.com\", \"why\");\nTUFF_CAP_FILES(\"1 MiB\", \"notes\");\n"
    );
    compile(&source).unwrap_or_else(|err| panic!("typed manifest macros failed:\n{err}"));
}

#[tuff_core::watchdog]
#[test]
fn raw_names_are_not_visible_from_tuff_hpp() {
    if !toolchain() {
        return;
    }
    for (name, statement) in HIDDEN {
        let probe = format!("{PLUGIN}\nvoid probe() {{ {statement} }}\n");
        match compile(&probe) {
            Ok(()) => panic!("{name} is visible through tuff.hpp"),
            Err(err) => assert!(
                err.contains(name),
                "{name} probe failed for another reason:\n{err}"
            ),
        }
    }
}

#[tuff_core::watchdog]
#[test]
fn raw_header_reaches_the_binding_layer() {
    if !toolchain() {
        return;
    }
    if let Err(err) = compile(RAW) {
        panic!("tuff/raw.h does not expose the raw layer:\n{err}");
    }
    let after = format!("{PLUGIN}\n#include \"tuff/raw.h\"\n");
    assert!(
        compile(&after).is_err_and(|err| err.contains("in place of tuff.hpp")),
        "including tuff/raw.h after tuff.hpp must be refused"
    );
    let c_header = "#include \"tuff.h\"\n#include \"tuff.hpp\"\n";
    assert!(
        compile(c_header).is_err_and(|err| err.contains("not tuff.h")),
        "including tuff.h in C++ must be refused"
    );
}

const WRAPPERS: &str = r#"
#include "tuff.hpp"
struct Probe {
    static tuff::Status on_register() {
        tuff::on<tuff::ev::MenuClick>([](const auto &c) {
            tuff::Menu m("shop", 3, "Shop");
            m.item(13, "minecraft:diamond", 1).name("Buy").lore("x");
            (void)m.open(c.player.uuid);
            tuff::menu::clear_slot(c.player.uuid, 1);
            tuff::menu::close(c.player.uuid);
            tuff::BlockPos a{0, 0, 0};
            auto id = tuff::BlockRule("minecraft:overworld", a, a).place(true).breakable("minecraft:dirt").add();
            if (id) tuff::rules::remove(*id);
            tuff::rules::set_team(c.player.uuid, "red");
            for (auto &p : tuff::online::all()) { tuff::online::kill(p.uuid); }
            auto st = tuff::online::state(c.player.uuid);
            if (st) tuff::online::set_game_mode(c.player.uuid, tuff::GameMode::Creative);
            tuff::online::set_vitals(c.player.uuid, 20.0f, -1);
            tuff::online::add_effect(c.player.uuid, "minecraft:speed", 100);
            tuff::dimensions::create("g:a", "minecraft:overworld");
            tuff::dimensions::discard("g:a");
            (void)tuff::dimensions::list();
            (void)tuff::acting::damage();
            (void)tuff::acting::hit();
            (void)tuff::acting::holds("minecraft:bread");
        });
        return tuff::Status();
    }
};
TUFF_PLUGIN(Probe, "probe", "0.1.0", "p");
"#;

#[tuff_core::watchdog]
#[test]
fn menu_rules_players_and_dimension_wrappers_compile() {
    if !toolchain() {
        return;
    }
    if let Err(err) = compile(WRAPPERS) {
        panic!("the menu, rules, players and dimensions wrappers do not compile:\n{err}");
    }
}
