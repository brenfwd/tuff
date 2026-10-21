use crate::common;

use common::TempDir;
use serde_json::Value;
use tuff::Server;

struct TestServer {
    handle: tuff::ServerHandle,
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
    _instance: TempDir,
}

async fn start_server() -> TestServer {
    let instance = TempDir::new();
    let server = Server::builder()
        .features(tuff::vanilla_features::all())
        .instance_dir(instance.path().to_path_buf())
        .binds(vec!["127.0.0.1:0".into()])
        .online_mode(false)
        .whitelist(false)
        .seed(1)
        .build();
    let handle = server.handle();
    let task = tokio::spawn(server.run());
    handle.local_addrs().await.unwrap();
    TestServer {
        handle,
        task,
        _instance: instance,
    }
}

async fn run(server: &TestServer, line: &str) -> Vec<Value> {
    server
        .handle
        .run_command(line)
        .await
        .unwrap()
        .iter()
        .map(|line| line.to_value())
        .collect()
}

fn key(lines: &[Value]) -> &str {
    lines[0]["translate"].as_str().unwrap_or_default()
}

fn passes(lines: &[Value]) -> bool {
    key(lines).starts_with("commands.execute.conditional.pass")
}

fn with(lines: &[Value], index: usize) -> &str {
    lines[0]["with"][index]["text"].as_str().unwrap_or_default()
}

async fn load(server: &TestServer, from: (i32, i32), to: (i32, i32)) {
    run(
        server,
        &format!("forceload add {} {} {} {}", from.0, from.1, to.0, to.1),
    )
    .await;
    for z in [from.1, to.1] {
        for x in [from.0, to.0] {
            let probe = format!("execute if block {x} 300 {z} minecraft:air");
            let mut ready = false;
            for _ in 0..300 {
                if passes(&run(server, &probe).await) {
                    ready = true;
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            assert!(ready, "chunk at {x} {z} never loaded");
        }
    }
}

async fn count(
    server: &TestServer,
    x: (i32, i32),
    y: (i32, i32),
    z: (i32, i32),
    filter: &str,
) -> i32 {
    let mut total = 0;
    for tx in (x.0..x.1).step_by(32) {
        for tz in (z.0..z.1).step_by(32) {
            for ty in (y.0..y.1).step_by(16) {
                let answer = run(
                    server,
                    &format!(
                        "fill {tx} {ty} {tz} {} {} {} minecraft:air replace {filter}",
                        tx + 31,
                        ty + 15,
                        tz + 31
                    ),
                )
                .await;
                if key(&answer) == "commands.fill.success" {
                    total += with(&answer, 0).parse::<i32>().unwrap();
                }
            }
        }
    }
    total
}

#[tuff_core::watchdog]
#[tokio::test]
async fn place_feature_grows_a_tree_and_fails_like_the_game() {
    let server = start_server().await;
    load(&server, (-32, -32), (48, 48)).await;

    run(&server, "fill 0 200 0 16 200 16 minecraft:grass_block").await;
    let placed = run(&server, "place feature minecraft:oak 8 201 8").await;
    assert_eq!(key(&placed), "commands.place.feature.success");
    assert_eq!(with(&placed, 0), "minecraft:oak");
    assert_eq!(with(&placed, 1), "8");
    assert_eq!(with(&placed, 2), "201");
    let log = run(&server, "execute if block 8 201 8 minecraft:oak_log").await;
    assert!(passes(&log));

    run(&server, "fill 0 210 0 16 230 16 minecraft:stone").await;
    let blocked = run(&server, "place feature minecraft:oak 8 215 8").await;
    assert_eq!(key(&blocked), "commands.place.feature.failed");
    let unknown = run(&server, "place feature minecraft:no_such_feature 8 201 8").await;
    assert_eq!(key(&unknown), "argument.resource_or_id.no_such_element");
    let unloaded = run(&server, "place feature minecraft:oak 5000 100 5000").await;
    assert_eq!(key(&unloaded), "argument.pos.unloaded");

    server.handle.shutdown().await.unwrap();
    server.task.await.unwrap().unwrap();
}

#[tuff_core::watchdog]
#[tokio::test]
async fn place_template_matches_across_a_region_border() {
    let server = start_server().await;
    load(&server, (-32, -32), (80, 48)).await;
    load(&server, (480, -32), (560, 48)).await;

    let template = "minecraft:igloo/top";
    let near = run(&server, &format!("place template {template} 8 200 8")).await;
    assert_eq!(key(&near), "commands.place.template.success");
    assert_eq!(with(&near, 1), "8");
    let empty = run(&server, "execute if blocks 8 200 8 22 215 22 8 260 8 all").await;
    assert!(!passes(&empty));

    let split = run(&server, &format!("place template {template} 510 200 8")).await;
    assert_eq!(key(&split), "commands.place.template.success");
    let same = run(&server, "execute if blocks 8 200 8 22 215 22 510 200 8 all").await;
    assert!(passes(&same));

    let missing = run(&server, "place template minecraft:no/such 8 200 8").await;
    assert_eq!(key(&missing), "commands.place.template.invalid");
    let unloaded = run(&server, &format!("place template {template} 5000 100 5000")).await;
    assert_eq!(key(&unloaded), "argument.pos.unloaded");

    server.handle.shutdown().await.unwrap();
    server.task.await.unwrap().unwrap();
}

#[tuff_core::watchdog]
#[tokio::test]
async fn place_template_rotation_and_integrity_change_the_result() {
    let server = start_server().await;
    load(&server, (-32, -32), (112, 48)).await;

    let template = "minecraft:igloo/top";
    run(&server, &format!("place template {template} 8 200 8")).await;
    run(
        &server,
        &format!("place template {template} 72 200 8 clockwise_90"),
    )
    .await;
    let rotated = run(&server, "execute if blocks 8 200 8 22 215 22 72 200 8 all").await;
    assert!(!passes(&rotated));

    run(
        &server,
        &format!("place template {template} 8 230 8 none none 0.5 7"),
    )
    .await;
    run(
        &server,
        &format!("place template {template} 72 230 8 none none 0.5 7"),
    )
    .await;
    let same = run(&server, "execute if blocks 8 230 8 22 245 22 72 230 8 all").await;
    assert!(passes(&same));
    let full = run(&server, "execute if blocks 8 230 8 22 245 22 8 200 8 all").await;
    assert!(!passes(&full));

    server.handle.shutdown().await.unwrap();
    server.task.await.unwrap().unwrap();
}

#[tuff_core::watchdog]
#[tokio::test]
async fn place_structure_builds_it_where_asked() {
    let server = start_server().await;
    load(&server, (-48, -48), (176, 96)).await;

    let mut placed = false;
    for step in 0..6 {
        let x = 8 + 16 * step;
        let line = format!("place structure minecraft:ruined_portal {x} 70 8");
        let answer = run(&server, &line).await;
        if key(&answer) == "commands.place.structure.success" {
            assert_eq!(with(&answer, 0), "minecraft:ruined_portal");
            placed = true;
            break;
        }
        assert_eq!(key(&answer), "commands.place.structure.failed");
    }
    assert!(placed, "no start was found");
    let obsidian = count(
        &server,
        (-32, 160),
        (-64, 192),
        (-32, 64),
        "minecraft:obsidian",
    )
    .await;
    assert!(obsidian > 0, "the portal was not placed");

    let unloaded = run(
        &server,
        "place structure minecraft:ruined_portal 5000 70 5000",
    )
    .await;
    assert_eq!(key(&unloaded), "argument.pos.unloaded");

    server.handle.shutdown().await.unwrap();
    server.task.await.unwrap().unwrap();
}

#[tuff_core::watchdog]
#[tokio::test]
async fn place_jigsaw_assembles_pieces_from_a_pool() {
    let server = start_server().await;
    load(&server, (-96, -96), (112, 112)).await;

    let line = "place jigsaw minecraft:village/plains/town_centers minecraft:bottom 3 8 100 8";
    let placed = run(&server, line).await;
    assert_eq!(key(&placed), "commands.place.jigsaw.success");
    assert_eq!(with(&placed, 1), "100");
    let planks = count(
        &server,
        (-80, 112),
        (60, 140),
        (-80, 112),
        "#minecraft:planks",
    )
    .await;
    assert!(planks > 0, "no pieces were placed");

    let no_target = run(
        &server,
        "place jigsaw minecraft:village/plains/town_centers minecraft:no_such 3 8 100 8",
    )
    .await;
    assert_eq!(key(&no_target), "commands.place.jigsaw.failed");
    let unloaded = run(
        &server,
        "place jigsaw minecraft:village/plains/town_centers minecraft:bottom 3 5000 100 5000",
    )
    .await;
    assert_eq!(key(&unloaded), "argument.pos.unloaded");
    let depth = run(
        &server,
        "place jigsaw minecraft:village/plains/town_centers minecraft:bottom 21 8 100 8",
    )
    .await;
    assert_eq!(key(&depth), "argument.integer.big");

    server.handle.shutdown().await.unwrap();
    server.task.await.unwrap().unwrap();
}

#[tuff_core::watchdog]
#[tokio::test]
async fn place_jigsaw_across_a_region_border_places_both_sides() {
    let server = start_server().await;
    load(&server, (416, -96), (608, 96)).await;

    let line = "place jigsaw minecraft:village/plains/town_centers minecraft:bottom 3 508 100 8";
    let placed = run(&server, line).await;
    assert_eq!(key(&placed), "commands.place.jigsaw.success");
    let mut sides = [0, 0];
    for filter in ["minecraft:cobblestone", "minecraft:dirt_path"] {
        sides[0] += count(&server, (440, 512), (60, 140), (-64, 96), filter).await;
        sides[1] += count(&server, (512, 584), (60, 140), (-64, 96), filter).await;
    }
    let [west, east] = sides;
    assert!(west > 0 && east > 0, "west {west} east {east}");

    server.handle.shutdown().await.unwrap();
    server.task.await.unwrap().unwrap();
}
