//! Fake players join through the real coordinator and session paths with
//! no socket; a bot on a real connection watches them.

#[path = "../common/mod.rs"]
pub(crate) mod common;

use common::{TempDir, TestServer};
use tuff::Server;
use tuff_bot::{Bot, BotConfig};
use tuff_core::world::fake::motion::Held;
use tuff_core::world::fake::{FakeHandle, FakeSpec};
use tuff_core::world::messages::PlayerId;
use tuff_vanilla_data::packet::play::Clientbound as C;

async fn start() -> TestServer {
    let instance = TempDir::new();
    let server = Server::builder()
        .features(tuff::vanilla_features::all())
        .instance_dir(instance.path().to_path_buf())
        .binds(vec!["127.0.0.1:0".into()])
        .online_mode(false)
        .whitelist(false)
        .view_distance(2)
        .seed(1)
        .build();
    let handle = server.handle();
    let task = tokio::spawn(server.run());
    let addr = handle.local_addrs().await.unwrap()[0];
    TestServer {
        handle,
        addr,
        task,
        _instance: instance,
    }
}

async fn stop(server: TestServer) {
    server.handle.shutdown().await.unwrap();
    server.task.await.unwrap().unwrap();
}

async fn watcher(server: &TestServer, name: &str) -> Bot {
    let bot = Bot::connect(
        server.addr,
        BotConfig {
            view_distance: 2,
            ..BotConfig::new(name)
        },
    )
    .await
    .unwrap();
    bot.wait_for(|state| state.chunks > 0).await.unwrap();
    bot
}

async fn fake(server: &TestServer, name: &str, at: [f64; 3]) -> FakeHandle {
    let spec = FakeSpec {
        position: Some(at),
        ..FakeSpec::new(name)
    };
    server
        .handle
        .spawn_fake("test", spec)
        .await
        .unwrap()
        .unwrap()
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

fn sees(bot: &Bot, entity: PlayerId) -> bool {
    bot.with_state(|s| s.entities.contains_key(&entity))
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_fake_appears_to_others_and_vanishes_when_removed() {
    let server = start().await;
    let bot = watcher(&server, "Watcher").await;
    let bot_ref = &bot;
    let near = bot.state().position;
    let made = fake(&server, "Fakey", [near[0] + 2.0, near[1], near[2]]).await;
    until(|| async { sees(bot_ref, made.entity) }).await;
    until(|| async {
        bot_ref.with_state(|s| {
            s.received
                .get(&C::PlayerInfoUpdate.id())
                .copied()
                .unwrap_or(0)
                >= 2
        })
    })
    .await;
    until(|| async { say(&server, "list").await.contains("Fakey") }).await;

    assert_eq!(
        server
            .handle
            .remove_fakes("test", Some(made.uuid))
            .await
            .unwrap(),
        1
    );
    until(|| async { !sees(bot_ref, made.entity) }).await;
    until(|| async { bot_ref.with_state(|s| s.received.contains_key(&C::PlayerInfoRemove.id())) })
        .await;
    server.handle.remove_fakes("test", None).await.unwrap();
    drop(bot);
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_fake_confirms_teleports_acks_chunks_and_crosses_regions() {
    let server = start().await;
    let made = fake(&server, "Fakey", [0.5, 80.0, 0.5]).await;
    let mut observed = made.observed.clone();
    observed
        .wait_for(|o| o.chunk_batches >= 1 && o.teleports >= 1)
        .await
        .unwrap();
    let before = observed.borrow().teleports;
    say(&server, "tp Fakey 1200 80 1200").await;
    let o = observed
        .wait_for(|o| o.teleports > before && o.position[0] > 1000.0)
        .await
        .unwrap()
        .clone();
    assert_eq!(o.position[0], 1200.5);
    until(|| async { say(&server, "list").await.contains("Fakey") }).await;
    assert!(!made.packets.is_closed());
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fakes_do_not_count_in_the_status_ping_and_yield_to_a_real_player() {
    let server = start().await;
    let made = fake(&server, "Fakey", [0.5, 80.0, 0.5]).await;
    let mut observed = made.observed.clone();
    observed.wait_for(|o| o.teleports >= 1).await.unwrap();
    until(|| async { say(&server, "list").await.contains("Fakey") }).await;
    let status = common::ping(server.addr).await;
    assert_eq!(status["players"]["online"], 0);

    let real = watcher(&server, "Fakey").await;
    tokio::time::timeout(std::time::Duration::from_secs(30), made.packets.closed())
        .await
        .expect("the fake leaves");
    let status = common::ping(server.addr).await;
    assert_eq!(status["players"]["online"], 1);
    drop(real);
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_killed_fake_respawns_on_command() {
    let server = start().await;
    let made = fake(&server, "Fakey", [0.5, 80.0, 0.5]).await;
    let mut observed = made.observed.clone();
    observed.wait_for(|o| o.teleports >= 1).await.unwrap();
    until(|| async {
        say(&server, "data get entity Fakey Health")
            .await
            .contains("20")
    })
    .await;
    say(&server, "kill Fakey").await;
    until(|| async {
        !say(&server, "data get entity Fakey Health")
            .await
            .contains("20")
    })
    .await;
    made.packets
        .send(tuff_core::protocol::play::Serverbound::ClientCommand(0))
        .await
        .unwrap();
    until(|| async {
        say(&server, "data get entity Fakey Health")
            .await
            .contains("20")
    })
    .await;
    stop(server).await;
}

fn number_after(text: &str, key: &str) -> Vec<f64> {
    let start = text.find(key).map_or(0, |i| i + key.len());
    text[start..]
        .split(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-'))
        .filter_map(|part| part.parse().ok())
        .collect()
}

async fn position(server: &TestServer, name: &str) -> [f64; 3] {
    let text = say(server, &format!("data get entity {name} Pos")).await;
    let numbers = number_after(&text, "data: ");
    assert!(numbers.len() >= 3, "{text}");
    [numbers[0], numbers[1], numbers[2]]
}

async fn settled(server: &TestServer, name: &str) -> [f64; 3] {
    let mut last = position(server, name).await;
    let mut steady = 0;
    while steady < 5 {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let now = position(server, name).await;
        let grounded = say(server, &format!("data get entity {name} OnGround"))
            .await
            .contains("1b");
        steady = if grounded && now == last {
            steady + 1
        } else {
            0
        };
        last = now;
    }
    last
}

async fn no_violations(server: &TestServer) -> bool {
    say(server, "anticheat violations")
        .await
        .contains("No player has anti-cheat violations")
}

async fn build_course(server: &TestServer) {
    for line in [
        "fill -3 140 -3 3 149 14 stone",
        "fill -3 150 8 3 150 14 stone",
        "fill -3 146 15 3 146 40 stone",
        "fill -3 150 -3 3 160 40 air replace air",
    ] {
        say(server, line).await;
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_held_sprint_walks_a_course_takes_fall_damage_and_flags_nothing() {
    let server = start().await;
    let made = fake(&server, "Fakey", [0.5, 150.0, 0.5]).await;
    let mut observed = made.observed.clone();
    observed
        .wait_for(|o| o.teleports >= 1 && o.chunk_batches >= 1)
        .await
        .unwrap();
    until(|| async {
        say(&server, "fill -3 140 -3 3 149 14 stone")
            .await
            .contains("Successfully")
    })
    .await;
    say(&server, "gamerule natural_health_regeneration false").await;
    build_course(&server).await;
    settled(&server, "Fakey").await;

    made.hold(Held {
        forward: 1,
        sprint: true,
        jump: true,
        ..Held::default()
    });
    until(|| async {
        let at = position(&server, "Fakey").await;
        at[1] > 150.9 && at[2] > 9.0
    })
    .await;
    made.hold(Held {
        forward: 1,
        sprint: true,
        ..Held::default()
    });
    until(|| async { position(&server, "Fakey").await[2] > 24.0 }).await;
    made.hold(Held::default());
    let health = say(&server, "data get entity Fakey Health").await;
    assert!(
        health.contains("19.0f") || health.contains("18.0f"),
        "{health}"
    );
    let at = position(&server, "Fakey").await;
    assert!((at[1] - 147.0).abs() < 0.01, "{at:?}");
    assert!(at[2] > 15.0 && at[2] < 40.0, "{at:?}");
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!((at[0] - 0.5).abs() < 0.05, "{at:?}");
    let flags = say(&server, "anticheat violations Fakey").await;
    assert!(no_violations(&server).await, "{flags}");
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hit_knocks_a_fake_back_without_flags() {
    let server = start().await;
    let watcher = watcher(&server, "Hitter").await;
    let near = settled(&server, "Hitter").await;
    let (bx, by, bz) = (
        near[0].floor() as i32,
        near[1].floor() as i32,
        near[2].floor() as i32,
    );
    for line in [
        format!(
            "fill {} {} {} {} {} {} stone",
            bx - 1,
            by - 1,
            bz - 1,
            bx + 5,
            by - 1,
            bz + 1
        ),
        format!(
            "fill {} {} {} {} {} {} air",
            bx - 1,
            by,
            bz - 1,
            bx + 5,
            by + 3,
            bz + 1
        ),
    ] {
        if line.ends_with("stone") {
            until(|| async {
                let t = say(&server, &line).await;
                t.contains("Successfully") || t.contains("No blocks were filled")
            })
            .await;
        } else {
            say(&server, &line).await;
        }
    }
    let near = settled(&server, "Hitter").await;
    let at = [near[0] + 2.0, near[1], near[2]];
    let made = fake(&server, "Fakey", at).await;
    let mut observed = made.observed.clone();
    observed
        .wait_for(|o| o.teleports >= 1 && o.chunk_batches >= 1)
        .await
        .unwrap();
    let bot = &watcher;
    until(|| async { sees(bot, made.entity) }).await;
    let before = settled(&server, "Fakey").await;
    assert!((before[1] - near[1]).abs() < 0.01, "{near:?} {before:?}");
    let in_view = || {
        bot.with_state(|s| {
            s.entities.get(&made.entity).is_some_and(|seen| {
                (0..3).all(|axis| (seen.position[axis] - before[axis]).abs() < 0.05)
            })
        })
    };
    until(|| async { in_view() }).await;
    let baseline = observed.borrow().impulses;
    let eye = [near[0], near[1] + 1.62, near[2]];
    let rotation = tuff_bot::look_at(eye, [at[0], at[1] + 0.9, at[2]]);
    watcher.send(tuff_core::protocol::play::Serverbound::Move(
        tuff_core::protocol::play::Movement {
            position: None,
            rotation: Some(rotation),
            on_ground: true,
            horizontal_collision: false,
        },
    ));
    watcher.send(tuff_core::protocol::play::Serverbound::Attack(made.entity));
    watcher.send(tuff_core::protocol::play::Serverbound::Punch);
    tokio::time::timeout(
        std::time::Duration::from_secs(30),
        observed.wait_for(|o| o.impulses > baseline),
    )
    .await
    .expect("the hit lands")
    .unwrap();
    until(|| async { position(&server, "Fakey").await[0] - before[0] > 0.5 }).await;
    let after = settled(&server, "Fakey").await;
    let away = after[0] - before[0];
    assert!(away > 0.5 && away < 3.0, "{before:?} {after:?}");
    assert!(no_violations(&server).await);
    drop(watcher);
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_fake_is_in_the_tab_list_of_a_late_joiner_and_in_selectors() {
    let server = start().await;
    let made = fake(&server, "Fakey", [0.5, 80.0, 0.5]).await;
    let mut observed = made.observed.clone();
    observed.wait_for(|o| o.teleports >= 1).await.unwrap();
    until(|| async { say(&server, "list").await.contains("Fakey") }).await;
    let id = C::PlayerInfoUpdate.id();
    let late = Bot::connect(
        server.addr,
        BotConfig {
            view_distance: 2,
            keep: vec![id],
            ..BotConfig::new("Late")
        },
    )
    .await
    .unwrap();
    let uuid = made.uuid.to_be_bytes();
    until(|| async {
        late.with_state(|s| {
            s.last
                .get(&id)
                .is_some_and(|b| b.windows(16).any(|w| w == uuid))
        })
    })
    .await;
    assert!(
        say(&server, "tell @a[name=Fakey] hi")
            .await
            .contains("Fakey")
    );
    drop(late);
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_owner_may_have_at_most_sixty_four_fakes() {
    let server = start().await;
    let mut held = Vec::new();
    for n in 0..fake_limit() {
        held.push(fake(&server, &format!("F{n}"), [0.5, 80.0, 0.5]).await);
    }
    let spec = FakeSpec::new("OneTooMany");
    assert!(
        server
            .handle
            .spawn_fake("test", spec)
            .await
            .unwrap()
            .is_err()
    );
    server.handle.remove_fakes("test", None).await.unwrap();
    stop(server).await;
}

fn fake_limit() -> usize {
    tuff_core::world::fake::OWNER_LIMIT
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_fake_swims_a_pool_crosses_a_cobweb_and_climbs_and_descends_a_ladder() {
    let server = start().await;
    let made = fake(&server, "Swimmy", [0.5, 150.0, 0.5]).await;
    let mut observed = made.observed.clone();
    observed.wait_for(|o| o.teleports >= 1).await.unwrap();
    until(|| async {
        say(&server, "fill -6 140 -3 6 149 40 stone")
            .await
            .contains("Successfully")
    })
    .await;
    for line in [
        "gamerule natural_health_regeneration false",
        "fill -6 150 -3 6 170 40 air replace air",
        "fill -2 145 4 2 149 12 water",
        "setblock 0 150 14 cobweb",
        "setblock 0 151 14 cobweb",
        "setblock 0 150 15 cobweb",
        "setblock 0 151 15 cobweb",
        "fill 0 150 22 0 160 22 stone",
        "fill 0 150 21 0 157 21 ladder[facing=north]",
    ] {
        say(&server, line).await;
    }
    say(&server, "tp Swimmy 0.5 150 0.5").await;
    let feet = settled(&server, "Swimmy").await;
    assert_eq!(feet[1], 150.0, "{feet:?}");

    made.hold(Held {
        forward: 1,
        jump: true,
        ..Held::default()
    });
    until(|| async { position(&server, "Swimmy").await[2] > 13.0 }).await;
    made.hold(Held {
        forward: 1,
        ..Held::default()
    });
    until(|| async { position(&server, "Swimmy").await[2] > 16.5 }).await;
    until(|| async { (position(&server, "Swimmy").await[1] - 150.0).abs() < 0.01 }).await;
    made.hold(Held {
        forward: 1,
        jump: true,
        ..Held::default()
    });
    until(|| async { position(&server, "Swimmy").await[1] > 154.0 }).await;
    made.hold(Held {
        sneak: true,
        ..Held::default()
    });
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let held = position(&server, "Swimmy").await[1];
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert!((position(&server, "Swimmy").await[1] - held).abs() < 0.02);
    made.hold(Held::default());
    until(|| async { position(&server, "Swimmy").await[1] < 150.5 }).await;
    let flags = say(&server, "anticheat violations Swimmy").await;
    assert!(no_violations(&server).await, "{flags}");
    stop(server).await;
}

use tuff_core::world::fake::actions::FakeAction;
use tuff_world_model::{BlockPos, Face};

async fn arena(server: &TestServer, names: &[(&str, f64)]) -> Vec<FakeHandle> {
    let mut made = Vec::new();
    for (name, x) in names {
        made.push(fake(server, name, [*x, 150.0, 0.5]).await);
    }
    for fake in &made {
        fake.observed
            .clone()
            .wait_for(|o| o.teleports >= 1)
            .await
            .unwrap();
    }
    until(|| async {
        say(server, "fill -10 149 -10 10 149 10 stone")
            .await
            .contains("Successfully")
    })
    .await;
    for line in [
        "fill -10 150 -10 10 160 10 air replace air",
        "gamerule natural_health_regeneration false",
    ] {
        say(server, line).await;
    }
    for (name, x) in names {
        say(server, &format!("gamemode survival {name}")).await;
        say(server, &format!("clear {name}")).await;
        say(server, &format!("tp {name} {x} 150 0.5")).await;
    }
    for (name, _) in names {
        let at = settled(server, name).await;
        assert!((at[1] - 150.0).abs() < 1e-6, "{name} settled at {at:?}");
    }
    made
}

async fn health(server: &TestServer, name: &str) -> f64 {
    let text = say(server, &format!("data get entity {name} Health")).await;
    number_after(&text, "data: ")[0]
}

async fn block_is(server: &TestServer, pos: [i32; 3], block: &str) -> bool {
    say(
        server,
        &format!("execute if block {} {} {} {block}", pos[0], pos[1], pos[2]),
    )
    .await
    .contains("passed")
}

async fn timed_break(server: &TestServer, made: &FakeHandle, pos: [i32; 3]) -> std::time::Duration {
    let start = std::time::Instant::now();
    made.act(FakeAction::Dig {
        pos: BlockPos::new(pos[0], pos[1], pos[2]),
        face: Face::Up,
    });
    until(|| async { block_is(server, pos, "air").await }).await;
    start.elapsed()
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_fake_attacks_within_reach_with_cooldown_and_not_beyond() {
    let server = start().await;
    let made = arena(&server, &[("Hitter", 0.5), ("Near", 3.0), ("Far", -3.5)]).await;
    let (hitter, near, far) = (&made[0], &made[1], &made[2]);
    for name in ["Hitter", "Near", "Far"] {
        let at = settled(&server, name).await;
        assert!((at[1] - 150.0).abs() < 0.01, "{name} {at:?}");
    }
    say(
        &server,
        "item replace entity Hitter weapon.mainhand with diamond_sword",
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    hitter.act(FakeAction::Attack(far.entity));
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    assert_eq!(health(&server, "Far").await, 20.0);

    hitter.act(FakeAction::Attack(near.entity));
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    until(|| async { health(&server, "Near").await < 20.0 }).await;
    assert_eq!(health(&server, "Near").await, 13.0);
    assert_eq!(health(&server, "Far").await, 20.0);

    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    say(&server, "tp Near 3.0 150 0.5").await;
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    hitter.act(FakeAction::Swing);
    hitter.act(FakeAction::Attack(near.entity));
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    until(|| async { health(&server, "Near").await < 13.0 }).await;
    let spam = 13.0 - health(&server, "Near").await;
    assert!(spam > 0.5 && spam < 3.0, "{spam}");
    assert!(no_violations(&server).await);
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_fake_digs_at_vanilla_speed_and_the_right_tool_is_faster() {
    let server = start().await;
    let made = arena(&server, &[("Digger", 0.5)]).await;
    let digger = &made[0];
    say(&server, "tp Digger 0.5 150 0.5").await;
    let feet = settled(&server, "Digger").await;
    assert_eq!(feet[1], 150.0, "{feet:?}");
    for (x, block) in [(2, "dirt"), (3, "stone"), (4, "obsidian")] {
        say(&server, &format!("setblock {x} 150 0 {block}")).await;
    }
    let dirt = timed_break(&server, digger, [2, 150, 0]).await;
    assert!(dirt.as_millis() < 6000, "{dirt:?}");

    digger.act(FakeAction::Dig {
        pos: BlockPos::new(4, 150, 0),
        face: Face::Up,
    });
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    assert!(block_is(&server, [4, 150, 0], "obsidian").await);
    digger.act(FakeAction::AbortDig);

    say(
        &server,
        "item replace entity Digger weapon.mainhand with diamond_pickaxe",
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let stone = timed_break(&server, digger, [3, 150, 0]).await;
    assert!(stone < dirt, "{stone:?} {dirt:?}");
    let flags = say(&server, "anticheat violations Digger").await;
    assert!(no_violations(&server).await, "{flags}");
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_fake_places_a_block_and_loses_the_item() {
    let server = start().await;
    let made = arena(&server, &[("Builder", 0.5)]).await;
    say(
        &server,
        "item replace entity Builder weapon.mainhand with cobblestone 8",
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    made[0].act(FakeAction::UseItemOn {
        off_hand: false,
        pos: BlockPos::new(2, 149, 0),
        face: Face::Up,
        cursor: [0.5, 1.0, 0.5],
    });
    until(|| async { block_is(&server, [2, 150, 0], "cobblestone").await }).await;
    assert!(
        say(&server, "clear Builder cobblestone 0")
            .await
            .contains('7')
    );
    assert!(no_violations(&server).await);
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_fake_draws_and_releases_a_bow() {
    let server = start().await;
    let made = arena(&server, &[("Archer", 0.5)]).await;
    say(
        &server,
        "item replace entity Archer weapon.mainhand with bow",
    )
    .await;
    say(&server, "give Archer arrow 4").await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    made[0].act(FakeAction::UseItem { off_hand: false });
    tokio::time::sleep(std::time::Duration::from_millis(1300)).await;
    made[0].act(FakeAction::ReleaseUse);
    until(|| async {
        say(&server, "execute if entity @e[type=arrow]")
            .await
            .contains("passed")
    })
    .await;
    assert!(say(&server, "clear Archer arrow 0").await.contains('3'));
    assert!(no_violations(&server).await);
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_fake_selects_slots_swaps_hands_drops_and_respawns() {
    let server = start().await;
    let made = arena(&server, &[("Hands", 0.5)]).await;
    let hands = &made[0];
    say(&server, "item replace entity Hands hotbar.3 with apple 3").await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    hands.act(FakeAction::SelectSlot(3));
    hands.act(FakeAction::Drop { stack: false });
    until(|| async {
        say(&server, "execute if entity @e[type=item]")
            .await
            .contains("passed")
    })
    .await;
    assert!(say(&server, "clear Hands apple 0").await.contains('2'));
    hands.act(FakeAction::SwapHands);
    hands.act(FakeAction::Drop { stack: true });
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert!(say(&server, "clear Hands apple 0").await.contains('2'));
    say(&server, "kill Hands").await;
    until(|| async { health(&server, "Hands").await < 20.0 }).await;
    hands.act(FakeAction::Respawn);
    until(|| async { health(&server, "Hands").await == 20.0 }).await;
    assert!(no_violations(&server).await);
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_fakes_player_info_reaches_a_watcher_before_its_spawn() {
    let server = start().await;
    let info = C::PlayerInfoUpdate.id();
    let add = C::AddEntity.id();
    let watch = Bot::connect(
        server.addr,
        BotConfig {
            view_distance: 2,
            keep: vec![info, add],
            ..BotConfig::new("Watch")
        },
    )
    .await
    .unwrap();
    watch.wait_for(|state| state.chunks > 0).await.unwrap();
    let near = watch.state().position;
    let made = fake(&server, "Ordered", [near[0] + 2.0, near[1], near[2]]).await;
    let uuid = made.uuid.to_be_bytes();
    let has = |b: &bytes::Bytes| b.windows(16).any(|w| w == uuid);
    until(|| async { sees(&watch, made.entity) }).await;
    let order = watch.with_state(|s| s.kept_order.clone());
    let first_info = order.iter().position(|(id, b)| *id == info && has(b));
    let first_add = order.iter().position(|(id, b)| *id == add && has(b));
    assert!(first_info.is_some(), "no player info for the fake");
    assert!(first_add.is_some(), "no spawn for the fake");
    assert!(first_info < first_add, "spawn arrived before player info");
    drop(watch);
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lone_fake_stands_on_its_mirror_and_a_later_viewer_still_gets_chunks() {
    let server = start().await;
    let made = fake(&server, "Lonely", [0.5, 120.0, 0.5]).await;
    let mut observed = made.observed.clone();
    observed
        .wait_for(|o| o.chunk_batches >= 1 && o.body.on_ground)
        .await
        .unwrap();
    let _late = watcher(&server, "Late").await;
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_owner_removes_only_its_own_fakes() {
    let server = start().await;
    let mine = fake(&server, "MineA", [0.5, 80.0, 0.5]).await;
    let also = fake(&server, "MineB", [0.5, 80.0, 0.5]).await;
    let theirs = server
        .handle
        .spawn_fake("other", FakeSpec::new("Theirs"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(server.handle.remove_fakes("test", None).await.unwrap(), 2);
    until(|| async { mine.packets.is_closed() && also.packets.is_closed() }).await;
    assert!(!theirs.packets.is_closed());
    assert_eq!(server.handle.remove_fakes("other", None).await.unwrap(), 1);
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_fake_boosted_by_a_firework_glides_further_without_flags() {
    let server = start().await;
    say(&server, "forceload add -3 -3 24 60").await;
    until(|| async {
        say(&server, "fill -3 145 -3 24 149 60 stone")
            .await
            .contains("Successfully")
    })
    .await;
    let plain = fake(&server, "Plain", [0.5, 150.0, 0.5]).await;
    let rocket = fake(&server, "Rocket", [20.5, 150.0, 0.5]).await;
    for made in [&plain, &rocket] {
        let mut observed = made.observed.clone();
        observed
            .wait_for(|o| o.teleports >= 1 && o.chunk_batches >= 1)
            .await
            .unwrap();
    }
    say(&server, "gamerule natural_health_regeneration false").await;
    for name in ["Plain", "Rocket"] {
        let given = say(
            &server,
            &format!("item replace entity {name} armor.chest with elytra"),
        )
        .await;
        assert!(!given.to_lowercase().contains("unknown"), "{given}");
    }
    let given = say(
        &server,
        "item replace entity Rocket weapon.mainhand with firework_rocket[fireworks={flight_duration:1}] 4",
    )
    .await;
    assert!(!given.to_lowercase().contains("unknown"), "{given}");
    let level = Held {
        yaw: 0.0,
        pitch: 20.0,
        ..Held::default()
    };
    let before = [&plain, &rocket].map(|made| {
        made.hold(Held::default());
        made.observed.borrow().teleports
    });
    say(&server, "tp Plain 0.5 165 0.5").await;
    say(&server, "tp Rocket 20.5 165 0.5").await;
    for (made, before) in [&plain, &rocket].into_iter().zip(before) {
        let mut observed = made.observed.clone();
        observed
            .wait_for(|o| o.teleports > before && o.body.position[1] > 160.0)
            .await
            .unwrap();
    }
    for made in [&plain, &rocket] {
        made.hold(Held {
            jump: true,
            ..level
        });
    }
    let mut observed = rocket.observed.clone();
    observed
        .wait_for(|o| o.body.position[2] > 1.5)
        .await
        .unwrap();
    for made in [&plain, &rocket] {
        made.hold(level);
    }
    rocket.act(FakeAction::UseItem { off_hand: false });
    let plain_landed = settled(&server, "Plain").await;
    let rocket_landed = settled(&server, "Rocket").await;
    assert!(
        rocket_landed[2] > plain_landed[2] + 5.0,
        "{rocket_landed:?} {plain_landed:?}"
    );
    let left = say(&server, "data get entity Rocket SelectedItem.count").await;
    assert!(left.contains('3'), "{left}");
    let flags = say(&server, "anticheat violations Rocket").await;
    assert!(no_violations(&server).await, "{flags}");
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_fake_in_an_elytra_glides_down_a_dive_and_lands_without_flags() {
    let server = start().await;
    let made = fake(&server, "Glider", [0.5, 175.0, 0.5]).await;
    let mut observed = made.observed.clone();
    observed
        .wait_for(|o| o.teleports >= 1 && o.chunk_batches >= 1)
        .await
        .unwrap();
    until(|| async {
        say(&server, "fill -3 140 -3 3 149 60 stone")
            .await
            .contains("Successfully")
    })
    .await;
    say(&server, "fill -3 150 -3 3 180 60 air replace air").await;
    say(&server, "gamerule natural_health_regeneration false").await;
    let given = say(
        &server,
        "item replace entity Glider armor.chest with elytra",
    )
    .await;
    assert!(!given.to_lowercase().contains("unknown"), "{given}");
    made.hold(Held::default());
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    made.hold(Held {
        jump: true,
        yaw: 0.0,
        pitch: 40.0,
        ..Held::default()
    });
    until(|| async { position(&server, "Glider").await[2] > 3.0 }).await;
    made.hold(Held {
        yaw: 0.0,
        pitch: 40.0,
        ..Held::default()
    });
    let landed = settled(&server, "Glider").await;
    assert!((landed[1] - 150.0).abs() < 0.01, "{landed:?}");
    assert!(landed[2] > 8.0 && landed[2] < 60.0, "{landed:?}");
    assert!((landed[0] - 0.5).abs() < 0.5, "{landed:?}");
    let health = say(&server, "data get entity Glider Health").await;
    assert!(health.contains("20.0f"), "{health}");
    let flags = say(&server, "anticheat violations Glider").await;
    assert!(no_violations(&server).await, "{flags}");
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_attack_on_a_target_not_yet_in_view_waits_for_it() {
    let server = start().await;
    let made = arena(&server, &[("Hitter", 0.5), ("Near", 3.0)]).await;
    let (hitter, near) = (&made[0], &made[1]);
    for name in ["Hitter", "Near"] {
        settled(&server, name).await;
    }
    say(&server, "forceload add 0 192 15 207").await;
    say(&server, "tp Near 3.0 150 200").await;
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    hitter.act(FakeAction::Attack(near.entity));
    say(&server, "tp Near 3.0 150 0.5").await;
    until(|| async { health(&server, "Near").await < 20.0 }).await;
    assert!(no_violations(&server).await);
    stop(server).await;
}
