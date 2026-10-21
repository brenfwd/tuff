//! Manifest conflict refusal and soft dependencies at admission.

use super::*;

fn bare(id: &str, version: &str, extra: &str) -> Vec<u8> {
    module(
        &format!("id = {id}\nversion = {version}\napi = 0\n{extra}"),
        &format!("(func {} {})", export("register"), ok_unit()),
    )
}

async fn load(world: &TestWorld, file: &str) -> String {
    console(world, &format!("tuff plugin load {file}"))
        .await
        .remove(0)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_new_plugin_naming_a_loaded_one_in_conflicts_is_refused() {
    let dir = TestDir::new();
    let world = start(dir.path()).await;
    install(dir.path(), "base.wasm", &bare("base", "1.2.0", ""));
    install(
        dir.path(),
        "rival.wasm",
        &bare("rival", "1.0.0", "conflicts = base >= 1.0.0"),
    );
    install(
        dir.path(),
        "old.wasm",
        &bare("oldrival", "1.0.0", "conflicts = base >= 2.0.0"),
    );
    assert_eq!(load(&world, "base.wasm").await, "Loaded base 1.2.0");
    let refused = load(&world, "rival.wasm").await;
    assert!(
        refused.contains("rival conflicts with base >= 1.0.0, and base 1.2.0 is loaded"),
        "{refused}"
    );
    assert_eq!(load(&world, "old.wasm").await, "Loaded oldrival 1.0.0");
    assert_eq!(loaded(&world).await.len(), 2);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_loaded_plugin_naming_the_new_one_in_conflicts_refuses_it() {
    let dir = TestDir::new();
    let world = start(dir.path()).await;
    install(
        dir.path(),
        "picky.wasm",
        &bare("picky", "1.0.0", "conflicts = newcomer >= 2.0.0"),
    );
    install(dir.path(), "v1.wasm", &bare("newcomer", "1.0.0", ""));
    install(dir.path(), "v2.wasm", &bare("newcomer", "2.0.0", ""));
    assert_eq!(load(&world, "picky.wasm").await, "Loaded picky 1.0.0");
    let refused = load(&world, "v2.wasm").await;
    assert!(
        refused.contains("picky conflicts with newcomer >= 2.0.0, so newcomer 2.0.0 cannot load"),
        "{refused}"
    );
    assert_eq!(load(&world, "v1.wasm").await, "Loaded newcomer 1.0.0");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_soft_dependency_outside_its_bound_does_not_stop_the_load() {
    let dir = TestDir::new();
    let world = start(dir.path()).await;
    install(dir.path(), "base.wasm", &bare("base", "0.5.0", ""));
    install(
        dir.path(),
        "soft.wasm",
        &bare("soft", "1.0.0", "soft-depends = base >= 1.0.0"),
    );
    assert_eq!(load(&world, "base.wasm").await, "Loaded base 0.5.0");
    assert_eq!(load(&world, "soft.wasm").await, "Loaded soft 1.0.0");
    assert_eq!(loaded(&world).await.len(), 2);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reload_goes_through_the_conflict_check() {
    let dir = TestDir::new();
    let world = start(dir.path()).await;
    install(dir.path(), "a.wasm", &bare("a", "1.0.0", ""));
    install(dir.path(), "b.wasm", &bare("b", "1.0.0", ""));
    assert_eq!(load(&world, "a.wasm").await, "Loaded a 1.0.0");
    assert_eq!(load(&world, "b.wasm").await, "Loaded b 1.0.0");

    install(dir.path(), "b.wasm", &bare("b", "1.1.0", "conflicts = a"));
    let refused = console(&world, "tuff plugin reload b").await;
    assert!(refused[0].contains("b conflicts with a"), "{refused:?}");
    assert!(loaded(&world).await.contains(&("b".into(), "1.0.0".into())));

    install(dir.path(), "b.wasm", &bare("b", "1.2.0", ""));
    assert_eq!(
        console(&world, "tuff plugin reload b").await,
        ["Reloaded b 1.0.0 -> 1.2.0"]
    );
    world.shutdown().await;
}
