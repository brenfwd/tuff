//! Optional void damage above a per-dimension height, applied to players
//! in survival and adventure only.

use crate::common::{TempDir, TestServer};

use tuff::Server;
use tuff_core::protocol::play::GameMode;
use tuff_core::world::fake::{FakeHandle, FakeSpec};

const NETHER: &str = "minecraft:the_nether";
const OVERWORLD: &str = "minecraft:overworld";

fn start(instance: &TempDir, settings: &[(&str, &str)]) -> Server {
    let overrides: Vec<(String, String)> = settings
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect();
    Server::builder()
        .features(tuff::vanilla_features::all())
        .instance_dir(instance.path().to_path_buf())
        .binds(vec!["127.0.0.1:0".into()])
        .online_mode(false)
        .whitelist(false)
        .view_distance(2)
        .seed(1)
        .config_overrides(overrides)
        .build()
}

async fn say(server: &TestServer, line: &str) -> String {
    server
        .handle
        .run_command(line)
        .await
        .unwrap()
        .iter()
        .map(|c| c.plain())
        .collect::<Vec<_>>()
        .join("\n")
}

async fn until<F: std::future::Future<Output = bool>>(mut check: impl FnMut() -> F) {
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        while !check().await {
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("condition not reached");
}

async fn health(server: &TestServer, name: &str) -> f64 {
    let text = say(server, &format!("data get entity {name} Health")).await;
    let after = text.split("data: ").nth(1).expect(&text);
    after
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect::<String>()
        .parse()
        .expect(&text)
}

struct Rig {
    server: TestServer,
    _fake: FakeHandle,
}

async fn rig(settings: &[(&str, &str)], dimension: &str, mode: GameMode, y: f64) -> Rig {
    let instance = TempDir::new();
    let server = start(&instance, settings);
    let handle = server.handle();
    let task = tokio::spawn(server.run());
    let addr = handle.local_addrs().await.unwrap()[0];
    let server = TestServer {
        handle,
        addr,
        task,
        _instance: instance,
    };
    let spec = FakeSpec {
        position: Some([0.5, y, 0.5]),
        dimension: Some(dimension.to_owned()),
        game_mode: Some(mode),
        ..FakeSpec::new("Roofer")
    };
    let fake = server
        .handle
        .spawn_fake("test", spec)
        .await
        .unwrap()
        .unwrap();
    fake.observed
        .clone()
        .wait_for(|o| o.teleports >= 1)
        .await
        .unwrap();
    for line in [
        "gamerule fall_damage false",
        "gamerule natural_health_regeneration false",
    ] {
        say(&server, line).await;
    }
    let floor = y as i32 - 1;
    until(|| async {
        say(
            &server,
            &format!("execute in {dimension} run fill -5 {floor} -5 5 {floor} 5 stone"),
        )
        .await
        .contains("Successfully")
    })
    .await;
    Rig {
        server,
        _fake: fake,
    }
}

async fn stop(rig: Rig) {
    rig.server.handle.shutdown().await.unwrap();
    rig.server.task.await.unwrap().unwrap();
}

const NETHER_HEIGHT: &str = "tuff.world.dimensions.nether.ceilingDamage.height";
const OVERWORLD_HEIGHT: &str = "tuff.world.dimensions.overworld.ceilingDamage.height";

async fn stays_unharmed(rig: &Rig) {
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    assert_eq!(health(&rig.server, "Roofer").await, 20.0);
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn nether_roof_is_off_by_default() {
    let rig = rig(&[], NETHER, GameMode::Survival, 200.0).await;
    stays_unharmed(&rig).await;
    stop(rig).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn nether_roof_hurts_a_survival_player_above_the_height() {
    let rig = rig(&[(NETHER_HEIGHT, "128")], NETHER, GameMode::Survival, 200.0).await;
    until(|| async { health(&rig.server, "Roofer").await < 20.0 }).await;
    let lost = 20.0 - health(&rig.server, "Roofer").await;
    assert!(lost >= 4.0 && lost % 4.0 == 0.0, "lost {lost}");
    stop(rig).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn nether_roof_amount_is_configurable() {
    let settings = [
        (NETHER_HEIGHT, "128"),
        ("tuff.world.dimensions.nether.ceilingDamage.amount", "2"),
    ];
    let rig = rig(&settings, NETHER, GameMode::Survival, 200.0).await;
    until(|| async { health(&rig.server, "Roofer").await < 20.0 }).await;
    let lost = 20.0 - health(&rig.server, "Roofer").await;
    assert!(lost >= 2.0 && lost % 2.0 == 0.0, "lost {lost}");
    stop(rig).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn nether_roof_spares_a_player_below_the_height() {
    let rig = rig(&[(NETHER_HEIGHT, "250")], NETHER, GameMode::Survival, 200.0).await;
    stays_unharmed(&rig).await;
    stop(rig).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn nether_roof_exempts_creative_players() {
    let rig = rig(&[(NETHER_HEIGHT, "128")], NETHER, GameMode::Creative, 200.0).await;
    stays_unharmed(&rig).await;
    stop(rig).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn nether_roof_height_is_per_dimension() {
    let rig = rig(
        &[(NETHER_HEIGHT, "100")],
        OVERWORLD,
        GameMode::Survival,
        150.0,
    )
    .await;
    stays_unharmed(&rig).await;
    stop(rig).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn nether_roof_overworld_key_hurts_in_the_overworld() {
    let rig = rig(
        &[(OVERWORLD_HEIGHT, "100")],
        OVERWORLD,
        GameMode::Survival,
        150.0,
    )
    .await;
    until(|| async { health(&rig.server, "Roofer").await < 20.0 }).await;
    stop(rig).await;
}
