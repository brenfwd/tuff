//! Lets the test binaries find `libtuff_dylib.so` and the shared std when
//! run directly instead of through cargo, in both target directory layouts.

use std::process::Command;

fn main() {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let target = std::env::var("TARGET").unwrap();
    let sysroot = Command::new(rustc)
        .args(["--print", "sysroot"])
        .output()
        .expect("rustc --print sysroot");
    let sysroot = String::from_utf8(sysroot.stdout).unwrap();
    let sysroot = sysroot.trim();
    for path in [
        "$ORIGIN".to_owned(),
        "$ORIGIN/../../../..".to_owned(),
        format!("{sysroot}/lib/rustlib/{target}/lib"),
    ] {
        println!("cargo:rustc-link-arg-tests=-Wl,-rpath,{path}");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
