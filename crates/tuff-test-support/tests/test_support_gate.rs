use std::process::Command;

fn cargo() -> Command {
    Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
}

const MANIFEST: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../Cargo.toml");

#[tuff_core::watchdog]
#[test]
fn only_dev_dependencies_enable_item_test_support() {
    let out = cargo()
        .args(["metadata", "--format-version", "1", "--no-deps", "--offline"])
        .args(["--manifest-path", MANIFEST])
        .output()
        .expect("run cargo metadata");
    assert!(out.status.success(), "cargo metadata failed");
    let meta: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    for package in meta["packages"].as_array().unwrap() {
        for dep in package["dependencies"].as_array().unwrap() {
            let enables = dep["features"]
                .as_array()
                .unwrap()
                .iter()
                .any(|f| f == "test-support");
            if dep["name"] == "tuff-item" && enables {
                assert_eq!(
                    dep["kind"], "dev",
                    "{} enables tuff-item/test-support outside dev-dependencies",
                    package["name"]
                );
            }
        }
    }
}

#[tuff_core::watchdog]
#[test]
fn server_build_has_no_item_test_support() {
    let out = cargo()
        .args(["tree", "--offline", "-p", "tuff", "-e", "normal,build,features"])
        .args(["-i", "tuff-item", "--manifest-path", MANIFEST])
        .output()
        .expect("run cargo tree");
    assert!(out.status.success(), "cargo tree failed");
    let tree = String::from_utf8(out.stdout).unwrap();
    assert!(tree.contains("tuff-item"), "tuff-item missing from the tree");
    assert!(
        !tree.contains("feature \"test-support\""),
        "the server build enables tuff-item/test-support:\n{tree}"
    );
}
