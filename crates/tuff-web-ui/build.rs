//! Builds the panel frontend in `app/` with bun (install, then `vite
//! build`) into `$OUT_DIR/web/index.html`, and records the sha256 of every
//! inline script and style so the server can send a strict CSP.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use base64::Engine;
use sha2::{Digest, Sha256};

const WATCHED: [&str; 8] = [
    "app/src",
    "app/index.html",
    "app/frame.html",
    "app/vite.frame.config.ts",
    "app/package.json",
    "app/bun.lock",
    "app/vite.config.ts",
    "app/tsconfig.json",
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
            "\n\nbuilding the web admin panel needs bun (https://bun.sh): \
             install it, put it on PATH or set BUN=/path/to/bun\n\n"
        ),
    }
}

fn run(bun: &Path, dir: &Path, args: &[&str], out: Option<&Path>) {
    let mut command = Command::new(bun);
    command.args(args).current_dir(dir);
    if let Some(out) = out {
        command.env("TUFF_WEB_OUT", out);
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

/// The contents of every `<tag ...>...</tag>` element, in order.
fn inline_blocks<'a>(html: &'a str, tag: &str) -> Vec<&'a str> {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(start) = rest.find(&open) {
        let after = &rest[start..];
        let Some(head_end) = after.find('>') else {
            break;
        };
        let body = &after[head_end + 1..];
        let Some(end) = body.find(&close) else { break };
        out.push(&body[..end]);
        rest = &body[end + close.len()..];
    }
    out
}

fn hashes(blocks: &[&str]) -> String {
    blocks
        .iter()
        .map(|block| {
            let digest = Sha256::digest(block.as_bytes());
            format!(
                "\"'sha256-{}'\"",
                base64::engine::general_purpose::STANDARD.encode(digest)
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

const PLACEHOLDER: &str = "<!doctype html><title>tuff</title><p>This build has no web admin panel (built without the panel feature).</p>";

fn placeholder(out_dir: &Path) {
    let web = out_dir.join("web");
    std::fs::create_dir_all(&web).unwrap();
    std::fs::write(web.join("index.html"), PLACEHOLDER).unwrap();
    std::fs::write(web.join("frame.html"), PLACEHOLDER).unwrap();
    let csp = "pub const SCRIPT_HASHES: &[&str] = &[];
pub const STYLE_HASHES: &[&str] = &[];
pub const FRAME_SCRIPT_HASHES: &[&str] = &[];
pub const FRAME_STYLE_HASHES: &[&str] = &[];
";
    std::fs::write(out_dir.join("csp.rs"), csp).unwrap();
}

fn main() {
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    if std::env::var_os("CARGO_FEATURE_PANEL").is_none() {
        placeholder(&out_dir);
        return;
    }
    let mut stdout = std::io::stdout();
    for path in WATCHED {
        writeln!(
            stdout,
            "cargo:rerun-if-changed={}",
            manifest.join(path).display()
        )
        .unwrap();
    }
    writeln!(stdout, "cargo:rerun-if-env-changed=BUN").unwrap();
    let app = manifest.join("app");
    let web = out_dir.join("web");
    let bun = bun();
    run(&bun, &app, &["install", "--frozen-lockfile"], None);
    run(&bun, &app, &["run", "build"], Some(&web));
    let html =
        std::fs::read_to_string(web.join("index.html")).expect("vite build produced no index.html");
    let scripts = inline_blocks(&html, "script");
    let styles = inline_blocks(&html, "style");
    assert!(!scripts.is_empty(), "the built panel has no inline script");
    let frame =
        std::fs::read_to_string(web.join("frame.html")).expect("vite build produced no frame.html");
    let frame_scripts = inline_blocks(&frame, "script");
    let frame_styles = inline_blocks(&frame, "style");
    assert!(
        !frame_scripts.is_empty(),
        "the built page frame has no inline script"
    );
    let csp = format!(
        "/// CSP source expressions for the inline scripts of [`INDEX_HTML`].\n\
         pub const SCRIPT_HASHES: &[&str] = &[{}];\n\
         /// CSP source expressions for the inline styles of [`INDEX_HTML`].\n\
         pub const STYLE_HASHES: &[&str] = &[{}];\n\
         /// CSP source expressions for the inline scripts of [`FRAME_HTML`].\n\
         pub const FRAME_SCRIPT_HASHES: &[&str] = &[{}];\n\
         /// CSP source expressions for the inline styles of [`FRAME_HTML`].\n\
         pub const FRAME_STYLE_HASHES: &[&str] = &[{}];\n",
        hashes(&scripts),
        hashes(&styles),
        hashes(&frame_scripts),
        hashes(&frame_styles)
    );
    std::fs::write(out_dir.join("csp.rs"), csp).unwrap();
}
