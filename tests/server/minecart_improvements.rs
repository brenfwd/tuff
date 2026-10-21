//! The minecart improvements feature pack: a world created with it keeps
//! the feature flag, its pack and its game rule across a restart, the pack
//! cannot be disabled, and a world without it has no such rule.

use crate::common;

use std::path::Path;

use common::TempDir;
use tuff::Server;
use tuff_item::text::TextComponent;

fn server(instance: &Path) -> Server {
    Server::builder()
        .features(tuff::vanilla_features::all())
        .binds(vec!["127.0.0.1:0".to_string()])
        .instance_dir(instance.to_path_buf())
        .seed(7)
        .build()
}

async fn run(handle: &tuff::ServerHandle, line: &str) -> String {
    handle
        .run_command(line)
        .await
        .unwrap()
        .iter()
        .map(TextComponent::plain)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Runs a server on `instance` until it listens, runs `lines` in order
/// and stops it, returning their feedback.
async fn session(instance: &Path, lines: &[&str]) -> Vec<String> {
    session_after(instance, std::time::Duration::ZERO, lines).await
}

async fn session_after(
    instance: &Path,
    settle: std::time::Duration,
    lines: &[&str],
) -> Vec<String> {
    let server = server(instance);
    let handle = server.handle();
    let task = tokio::spawn(server.run());
    handle.local_addrs().await.unwrap();
    tokio::time::sleep(settle).await;
    let mut out = Vec::new();
    for line in lines {
        let mut result = run(&handle, line).await;
        if *line == CART {
            for _ in 0..100 {
                if result.contains("entity data") {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                result = run(&handle, line).await;
            }
        }
        out.push(result);
    }
    handle.shutdown().await.unwrap();
    task.await.unwrap().unwrap();
    out
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_feature_pack_its_flag_and_its_game_rule_survive_a_restart() {
    let instance = TempDir::new();
    std::fs::write(
        instance.path().join("server.properties"),
        "initial-enabled-packs=vanilla,minecart_improvements\n",
    )
    .unwrap();
    let first = session(
        instance.path(),
        &[
            "gamerule max_minecart_speed",
            "gamerule max_minecart_speed 30",
            "datapack disable minecart_improvements",
            "datapack list enabled",
        ],
    )
    .await;
    assert!(
        first[0].contains("max_minecart_speed") && first[0].contains('8'),
        "{first:?}"
    );
    assert!(first[1].contains("30"), "{first:?}");
    assert!(first[2].contains("cannot be disabled"), "{first:?}");
    assert!(first[3].contains("minecart_improvements"), "{first:?}");

    std::fs::write(
        instance.path().join("server.properties"),
        "initial-enabled-packs=vanilla\n",
    )
    .unwrap();
    let second = session(
        instance.path(),
        &["gamerule max_minecart_speed", "datapack list enabled"],
    )
    .await;
    assert!(second[0].contains("30"), "the saved value: {second:?}");
    assert!(second[1].contains("minecart_improvements"), "{second:?}");
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_world_without_the_feature_has_no_minecart_speed_rule() {
    let instance = TempDir::new();
    let out = session(
        instance.path(),
        &[
            "gamerule max_minecart_speed",
            "gamerule max_minecart_speed 30",
            "datapack enable minecart_improvements",
            "datapack list enabled",
        ],
    )
    .await;
    assert!(!out[0].contains("max_minecart_speed is"), "{out:?}");
    assert!(!out[1].contains("30"), "{out:?}");
    assert!(
        out[2].contains("minecraft:minecart_improvements"),
        "missing flag named: {out:?}"
    );
    assert!(!out[3].contains("minecart_improvements"), "{out:?}");
}

const CART: &str = "data get entity @e[type=minecart,limit=1] HasTicked";

fn feature_packs(instance: &Path, value: &str) {
    std::fs::write(
        instance.join("server.properties"),
        format!("feature-packs={value}\n"),
    )
    .unwrap();
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn carts_survive_the_startup_feature_pack_toggle_both_ways() {
    let instance = TempDir::new();
    std::fs::write(
        instance.path().join("server.properties"),
        "initial-enabled-packs=vanilla,minecart_improvements\n",
    )
    .unwrap();
    let first = session_after(
        instance.path(),
        std::time::Duration::from_millis(500),
        &[
            "forceload add 0 0",
            "fill 0 64 0 4 64 0 minecraft:rail",
            "summon minecraft:minecart 2.5 64.1 0.5",
            "gamerule max_minecart_speed",
            CART,
        ],
    )
    .await;
    assert!(first[4].contains("entity data"), "{first:?}");
    assert!(first[3].contains("max_minecart_speed"), "{first:?}");

    for (wanted, has_rule) in [("", false), ("minecart_improvements", true), ("", false)] {
        feature_packs(instance.path(), wanted);
        let out = session_after(
            instance.path(),
            std::time::Duration::from_millis(500),
            &[
                "forceload add 0 0",
                CART,
                "gamerule max_minecart_speed",
                "datapack list enabled",
            ],
        )
        .await;
        assert!(
            out[1].contains("entity data"),
            "the cart was lost or unreadable ({wanted:?}): {out:?}"
        );
        assert!(out[1].ends_with("0b"), "{wanted:?}: {out:?}");
        assert_eq!(
            out[2].contains("max_minecart_speed is"),
            has_rule,
            "{wanted:?}: {out:?}"
        );
        assert_eq!(
            out[3].contains("minecart_improvements"),
            has_rule,
            "{wanted:?}: {out:?}"
        );
    }
}
