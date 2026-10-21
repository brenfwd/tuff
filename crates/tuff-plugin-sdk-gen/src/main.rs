// SPDX-License-Identifier: MIT
use std::path::PathBuf;

use anyhow::{Context, Result, bail};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("interface") => interface(&args[1..]),
        None => regenerate(),
        Some(other) => bail!(
            "unknown mode `{other}`; use no arguments or `interface <wit dir> --rust <out.rs>`"
        ),
    }
}

fn interface(args: &[String]) -> Result<()> {
    const USAGE: &str = "usage: interface <wit dir> [--rust <out.rs>] [--cpp <out.hpp>]";
    let (dir, flags) = args.split_first().context(USAGE)?;
    if flags.is_empty() || flags.len() % 2 != 0 {
        bail!(USAGE);
    }
    let dir = PathBuf::from(dir);
    for pair in flags.chunks(2) {
        let code = match pair[0].as_str() {
            "--rust" => tuff_plugin_sdk_gen::iface::generate_dir(&dir)?,
            "--cpp" => tuff_plugin_sdk_gen::iface::generate_cpp_dir(&dir)?,
            _ => bail!(USAGE),
        };
        std::fs::write(&pair[1], code).with_context(|| format!("writing {}", pair[1]))?;
        println!("wrote {}", pair[1]);
    }
    Ok(())
}

fn regenerate() -> Result<()> {
    for output in tuff_plugin_sdk_gen::all()? {
        let path = tuff_plugin_sdk_gen::root().join(&output.path);
        if std::fs::read(&path).ok().as_deref() != Some(&output.contents[..]) {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(&path, &output.contents)?;
            println!("wrote {}", output.path.display());
        }
    }
    Ok(())
}
