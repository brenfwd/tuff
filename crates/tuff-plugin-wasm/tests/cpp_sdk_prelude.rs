//! C++ SDK prelude pieces run in a sandbox: typed argument binding
//! (`executes_with`) and the `Text` overflow flag. Builds a probe plugin with
//! `crates/tuff-plugin-sdk/c/build.sh`; skipped without a wasm32 clang toolchain
//! unless `TUFF_EXAMPLE_PLUGINS=1` requires it.

use std::path::{Path, PathBuf};
use std::process::Command;

use tuff_plugin_wasm::sandbox::{Limits, Player, Sandbox, engine};

const PLUGIN: &str = r#"
#include "tuff.hpp"
using namespace tuff;

struct Sum {
    int32_t a;
    int32_t b;
    static const char *bind(const Call &call, Sum &out) {
        if (!call.get("a", out.a)) return "a is missing";
        if (!call.get("b", out.b)) return "b is missing";
        if (out.b == 0) return "b must not be zero";
        return nullptr;
    }
};

struct Probe {
    static Status on_register() {
        Tree tree("probe");
        Node root = tree.root();
        Node add = root.literal("add");
        add.argument("a", parser::integer())
            .argument("b", parser::integer())
            .executes_with<Sum>([](Call &call, Sum &s) -> Result {
                call.reply(Text() << s.a + s.b);
                return s.a + s.b;
            });
        root.literal("long").executes([](Call &call) -> Result {
            Text t;
            for (int i = 0; i < 2000; i++) t << "x";
            call.reply(Text() << (t.truncated() ? "truncated" : "fits"));
            Text short_text;
            short_text << "y";
            call.reply(Text() << (short_text.truncated() ? "truncated" : "fits"));
            return 1;
        });
        return tree.register_command();
    }
};
void io_probe() {
    files::Usage u = files::usage();
    (void)u.used;
    (void)u.quota;
    (void)http::get("https://example.com").json("{}").timeout(5).decompress().redirects(2);
}

TUFF_PLUGIN(Probe, "probe", "0.1.0", "prelude probe");
"#;

fn build() -> Option<Vec<u8>> {
    let sdk = Path::new(env!("CARGO_MANIFEST_DIR")).join("../tuff-plugin-sdk/c");
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("cpp-sdk-prelude-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("probe.cpp");
    let out = dir.join("probe.wasm");
    std::fs::write(&source, PLUGIN).unwrap();
    let result = Command::new("sh")
        .arg(sdk.join("build.sh"))
        .arg(&out)
        .arg(&source)
        .output();
    let wasm = match result {
        Ok(o) if o.status.success() => Ok(std::fs::read(&out).unwrap()),
        Ok(o) => Err(String::from_utf8_lossy(&o.stderr).into_owned()),
        Err(e) => Err(e.to_string()),
    };
    let _ = std::fs::remove_dir_all(&dir);
    match wasm {
        Ok(wasm) => Some(wasm),
        Err(err) if std::env::var("TUFF_EXAMPLE_PLUGINS").as_deref() == Ok("1") => {
            panic!("the probe plugin does not build:\n{err}")
        }
        Err(err) => {
            eprintln!("skipping: no usable wasm32 clang toolchain:\n{err}");
            None
        }
    }
}

fn sandbox(wasm: &[u8]) -> Sandbox {
    let mut plugin = Sandbox::new(&engine(), wasm, "probe", Limits::default()).unwrap();
    plugin.register().unwrap();
    plugin.enable().unwrap();
    plugin
}

#[tuff_core::watchdog]
#[test]
fn typed_args_bind_and_text_reports_truncation() {
    let Some(wasm) = build() else { return };
    let mut plugin = sandbox(&wasm);
    let alex = || Player::new(0xa1e8, "Alex");

    let (result, effects) = plugin.command(alex(), "probe add 2 3").unwrap();
    assert_eq!(result, Ok(5));
    assert_eq!(effects.replies, ["5"]);

    let (result, _) = plugin.command(alex(), "probe add 2 0").unwrap();
    assert_eq!(result, Err("b must not be zero".into()));

    let (result, effects) = plugin.command(alex(), "probe long").unwrap();
    assert_eq!(result, Ok(1));
    assert_eq!(effects.replies, ["truncated", "fits"]);
}
