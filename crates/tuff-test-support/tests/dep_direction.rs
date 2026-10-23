use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;

type Graph = BTreeMap<String, BTreeSet<String>>;

fn normal_graph() -> Graph {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let manifest = concat!(env!("CARGO_MANIFEST_DIR"), "/../../Cargo.toml");
    let out = Command::new(cargo)
        .args([
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--offline",
            "--manifest-path",
            manifest,
        ])
        .output()
        .expect("run cargo metadata");
    assert!(out.status.success(), "cargo metadata failed");
    let meta: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let packages = meta["packages"].as_array().unwrap();
    let names: BTreeSet<String> = packages
        .iter()
        .map(|p| p["name"].as_str().unwrap().to_string())
        .collect();
    let mut graph = Graph::new();
    for p in packages {
        let deps = p["dependencies"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|d| d["kind"].is_null())
            .map(|d| d["name"].as_str().unwrap().to_string())
            .filter(|n| names.contains(n))
            .collect();
        graph.insert(p["name"].as_str().unwrap().to_string(), deps);
    }
    graph
}

fn transitive(graph: &Graph, from: &str) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    let mut stack = vec![from.to_string()];
    while let Some(n) = stack.pop() {
        for d in graph.get(&n).into_iter().flatten() {
            if seen.insert(d.clone()) {
                stack.push(d.clone());
            }
        }
    }
    seen
}

#[tuff_core::watchdog]
#[test]
fn normal_dependency_graph_is_acyclic() {
    let graph = normal_graph();
    for name in graph.keys() {
        assert!(
            !transitive(&graph, name).contains(name),
            "{name} depends on itself through normal dependencies"
        );
    }
}

#[tuff_core::watchdog]
#[test]
fn only_the_binary_test_support_and_the_dylib_facade_sit_above_core() {
    let graph = normal_graph();
    for (name, deps) in &graph {
        if name == "tuff-dylib" {
            continue;
        }
        for forbidden in ["tuff", "tuff-test-support"] {
            assert!(
                !deps.contains(forbidden),
                "{name} has a normal dependency on {forbidden}"
            );
        }
    }
}

#[tuff_core::watchdog]
#[test]
fn foundation_crates_do_not_reach_into_vanilla_or_core() {
    let graph = normal_graph();
    let foundation = [
        "tuff-vanilla-data",
        "tuff-http",
        "tuff-macros",
        "tuff-nbt",
        "tuff-storage",
        "tuff-zip",
        "tuff-datapack",
        "tuff-web-ui",
        "tuff-timings",
        "tuff-trace",
        "tuff-wire",
        "tuff-world-model",
        "tuff-item",
    ];
    for name in foundation {
        for dep in transitive(&graph, name) {
            assert!(
                dep == "tuff-vanilla-data"
                    || dep.starts_with("tuff-")
                        && !dep.starts_with("tuff-feature-builtin-")
                        && dep != "tuff-core"
                        && dep != "tuff-test-support",
                "{name} must not depend on {dep}"
            );
        }
    }
}

#[tuff_core::watchdog]
#[test]
fn core_does_not_depend_on_vanilla_content_or_extensions() {
    let graph = normal_graph();
    for dep in transitive(&graph, "tuff-core") {
        assert!(
            dep == "tuff-vanilla-data"
                || [
                    "tuff-datapack",
                    "tuff-http",
                    "tuff-item",
                    "tuff-macros",
                    "tuff-nbt",
                    "tuff-supervise",
                    "tuff-sqlite-nbt",
                    "tuff-storage",
                    "tuff-timings",
                    "tuff-trace",
                    "tuff-wire",
                    "tuff-world-model",
                    "tuff-web-ui",
                    "tuff-zip"
                ]
                .contains(&dep.as_str()),
            "tuff-core must not depend on {dep}"
        );
    }
}

#[tuff_core::watchdog]
#[test]
fn leaf_crates_do_not_depend_on_each_other() {
    let graph = normal_graph();
    let leaves: BTreeSet<&str> = [
        "tuff-feature-builtin-core-commands",
        "tuff-plugin-wasm",
        "tuff-replay",
        "tuff-feature-builtin-audit-log",
        "tuff-feature-builtin-world-map",
        "tuff-bot",
        "tuff-client-harness",
        "tuff-feature-builtin-example",
        "tuff-feature-builtin-chunk-progress",
        "tuff-feature-builtin-moving-block-entities",
    ]
    .into();
    for leaf in &leaves {
        for dep in graph.get(*leaf).into_iter().flatten() {
            assert!(
                !dep.starts_with("tuff-feature-builtin-vanilla-") || dep == "tuff-vanilla-data",
                "{leaf} must not depend on {dep}"
            );
            assert!(
                !leaves.contains(dep.as_str()),
                "{leaf} must not depend on leaf crate {dep}"
            );
        }
    }
}

#[tuff_core::watchdog]
#[test]
fn vanilla_crates_depend_on_no_other_vanilla_crate_but_data() {
    let graph = normal_graph();
    let vanilla = graph.keys().filter(|name| {
        name.starts_with("tuff-feature-builtin-vanilla-") && *name != "tuff-vanilla-data"
    });
    let mut checked = 0;
    for from in vanilla {
        checked += 1;
        for dep in transitive(&graph, from) {
            assert!(
                !dep.starts_with("tuff-feature-builtin-vanilla-"),
                "{from} must not depend on {dep}"
            );
        }
    }
    assert!(checked >= 14, "only {checked} vanilla crates found");
}

#[tuff_core::watchdog]
#[test]
fn vanilla_common_depends_only_on_core_and_data() {
    let graph = normal_graph();
    for dep in graph
        .get("tuff-vanilla-common")
        .expect("tuff-vanilla-common exists")
    {
        assert!(
            !dep.starts_with("tuff-feature-builtin-"),
            "tuff-vanilla-common must not depend on {dep}"
        );
    }
}

#[tuff_core::watchdog]
#[test]
fn core_world_keeps_no_moved_rule_tables() {
    let world = concat!(env!("CARGO_MANIFEST_DIR"), "/../tuff-core/src/world");
    for module in [
        "block_support.rs",
        "brushable.rs",
        "combat.rs",
        "effect_cloud.rs",
        "falling.rs",
        "jukebox.rs",
        "scaffolding.rs",
        "shadow.rs",
        "side_chain.rs",
        "spawn_equipment.rs",
        "spawn_search.rs",
        "speleothem.rs",
        "placement/shape.rs",
    ] {
        assert!(
            !std::path::Path::new(world).join(module).exists(),
            "rule module world/{module} is back in core; it belongs to a vanilla crate"
        );
    }
}
