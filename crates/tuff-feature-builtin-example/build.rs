//! Builds the example's panel page in `panel/` with bun (install, then
//! `vite build`) into `$OUT_DIR/panel/gold.js` and `gold.css`, the way
//! `crates/tuff-web-ui` builds the shell. A feature page is a React app built
//! against the shell's SDK in `crates/tuff-web-ui/app` (see `panel/vite.config.ts`).

use std::path::{Path, PathBuf};
use std::process::Command;

const WATCHED: [&str; 8] = [
    "panel/src",
    "panel/package.json",
    "panel/bun.lock",
    "panel/vite.config.ts",
    "panel/tsconfig.json",
    "../tuff-web-ui/app/sdk",
    "../tuff-web-ui/app/src/sdk",
    "build.rs",
];

fn bun() -> PathBuf {
    if let Some(path) = std::env::var_os("BUN") {
        return PathBuf::from(path);
    }
    let found = std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join("bun"))
            .find(|candidate| candidate.is_file())
    });
    if let Some(path) = found {
        return path;
    }
    let home = std::env::var_os("HOME").map(|home| Path::new(&home).join(".bun/bin/bun"));
    match home.filter(|p| p.is_file()) {
        Some(path) => path,
        None => panic!(
            "\n\nbuilding the example feature's panel page needs bun (https://bun.sh): \
             install it, put it on PATH or set BUN=/path/to/bun\n\n"
        ),
    }
}

fn run(bun: &Path, dir: &Path, args: &[&str], out: Option<&Path>) {
    let mut command = Command::new(bun);
    command.args(args).current_dir(dir);
    if let Some(out) = out {
        command.env("TUFF_PANEL_OUT", out);
    }
    let status = command
        .status()
        .unwrap_or_else(|err| panic!("\n\ncould not run {}: {err}\n\n", bun.display()));
    assert!(
        status.success(),
        "\n\n`bun {}` failed in {} ({status})\n\n",
        args.join(" "),
        dir.display()
    );
}

fn main() {
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    for path in WATCHED {
        println!("cargo:rerun-if-changed={}", manifest.join(path).display());
    }
    println!("cargo:rerun-if-env-changed=BUN");
    let app = manifest.join("panel");
    let out = out_dir.join("panel");
    let bun = bun();
    run(&bun, &app, &["install", "--frozen-lockfile"], None);
    run(&bun, &app, &["run", "build"], Some(&out));
    for file in ["gold.js", "gold.css"] {
        assert!(
            out.join(file).is_file(),
            "vite build produced no {file} in {}",
            out.display()
        );
    }
}
