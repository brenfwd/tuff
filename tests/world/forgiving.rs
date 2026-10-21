//! Neutral mobs forgiving a player who dies, under the
//! `forgive_dead_players` rule, through real sessions dying and
//! respawning.

use super::*;
use crate::protocol::play::Serverbound;
use crate::world::game_rules::RuleValue;
use crate::world::session::PlayerSession;
use tuff_vanilla_data::damage_type;
use tuff_vanilla_data::registry::EntityType;
use tuff_world_model::pos::REGION_CHUNKS;

const PLAYER: i32 = 100;
const UUID: u128 = PLAYER as u128;

async fn pump(session: &mut PlayerSession) {
    while let Ok(event) = session.conn.events.try_recv() {
        session.on_event(event).await;
    }
}

async fn tick(world: &TestWorld, session: &mut PlayerSession) {
    world.ticks(1).await;
    pump(session).await;
    session.run_tick().await;
}

async fn grudge(region: &RegionHandle, id: i32) -> Option<u128> {
    entity_nbt(region, id)
        .await
        .expect("the mob is there")
        .get_uuid("angry_at")
}

fn forgive_dead_players(world: &TestWorld, on: bool) {
    world.game_rules.send_modify(|rules| {
        rules
            .set("forgive_dead_players", RuleValue::Bool(on))
            .expect("a boolean rule");
    });
}

/// A session standing in a pen of chunk (0, 0) next to an iron golem it
/// punched, the golem angry at it.
async fn punched_golem(world: &TestWorld) -> (RegionHandle, PlayerSession, i32) {
    punched(world, EntityType::IronGolem).await
}

async fn punched(world: &TestWorld, kind: EntityType) -> (RegionHandle, PlayerSession, i32) {
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (mut session, frames) = session_with_outbound(world, PLAYER);
    std::mem::forget(frames);
    session.view.subscribed.insert(chunk);
    region
        .request(RegionRequest::Subscribe {
            player: session.conn.handle.clone(),
            chunks: vec![chunk],
        })
        .await;
    for _ in 0..400 {
        tick(world, &mut session).await;
        if block_at(&region, BlockPos::new(8, 0, 8)).await.is_some() {
            break;
        }
    }
    place_blocks(world, &region, pen(2, 13)).await;
    session.stand_at([4.5, SKY as f64 + 1.0, 4.5], true);
    session.publish(true).await;
    summon(&region, kind, Compound::new(), [6.5, SKY as f64 + 1.0, 4.5]).await;
    let golem = mobs(&region, kind).await[0].id;
    tick(world, &mut session).await;
    session.skip_ticks(20);
    session.handle_packet(Serverbound::Attack(golem)).await;
    for _ in 0..40 {
        tick(world, &mut session).await;
        if grudge(&region, golem).await == Some(UUID) {
            return (region, session, golem);
        }
    }
    panic!("the punched golem never got angry");
}

async fn die(world: &TestWorld, session: &mut PlayerSession) {
    if !session.is_dead() {
        session
            .damage(1000.0, &damage_type::GENERIC_KILL, None)
            .await;
    }
    assert!(session.is_dead());
    for _ in 0..5 {
        tick(world, session).await;
    }
}

/// Respawns the session and walks it back beside the golem.
async fn respawn_beside_golem(session: &mut PlayerSession) {
    session.respawn().await;
    session.handle_packet(Serverbound::PlayerLoaded).await;
    assert!(!session.is_dead());
    session.stand_at([4.5, SKY as f64 + 1.0, 4.5], true);
    session.publish(true).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_iron_golem_forgives_a_player_who_died_and_stays_calm_after_respawn() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (region, mut session, golem) = punched_golem(&world).await;
    die(&world, &mut session).await;
    assert_eq!(grudge(&region, golem).await, None, "forgiven on death");
    respawn_beside_golem(&mut session).await;
    let full = session.health();
    for _ in 0..100 {
        tick(&world, &mut session).await;
    }
    assert_eq!(grudge(&region, golem).await, None, "still forgiven");
    assert_eq!(session.health(), full, "the golem left the player alone");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn without_forgive_dead_players_an_iron_golem_stays_angry_after_respawn() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    forgive_dead_players(&world, false);
    let (region, mut session, golem) = punched_golem(&world).await;
    die(&world, &mut session).await;
    assert_eq!(grudge(&region, golem).await, Some(UUID), "still angry");
    respawn_beside_golem(&mut session).await;
    let full = session.health();
    for _ in 0..100 {
        tick(&world, &mut session).await;
        if session.health() < full {
            world.shutdown().await;
            return;
        }
    }
    panic!("the angry golem never attacked the respawned player");
}

/// A player dying just west of a region border: an angry zombified piglin
/// across the border within 32 blocks forgives them, one further away does
/// not, and a piglin forgets its grudge wherever it is in the dimension.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn neutral_mobs_across_a_region_border_forgive_within_reach_and_piglins_anywhere() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let border = REGION_CHUNKS * 16;
    let east_chunks: Vec<ChunkPos> = (REGION_CHUNKS..REGION_CHUNKS + 4)
        .map(|x| ChunkPos::new(x, 0))
        .collect();
    let east = world.region(east_chunks[0].region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    subscribe(&east, &viewer, &mut events, &east_chunks).await;
    let floor: Vec<_> = (border..border + 64)
        .map(|x| (BlockPos::new(x, SKY, 8), Block::Stone.default_state()))
        .collect();
    place_blocks(&world, &east, floor).await;
    let angry = || {
        Compound::new()
            .with("NoAI", true)
            .with("anger_end_time", 1_000_000i64)
            .with("angry_at", Tag::uuid(UUID))
    };
    let y = SKY as f64 + 1.0;
    let near = [f64::from(border) + 8.5, y, 8.5];
    let far = [f64::from(border) + 48.5, y, 8.5];
    summon(&east, EntityType::ZombifiedPiglin, angry(), near).await;
    summon(&east, EntityType::ZombifiedPiglin, angry(), far).await;
    summon(&east, EntityType::Piglin, angry(), far).await;
    world.ticks(2).await;
    let zombified = mobs(&east, EntityType::ZombifiedPiglin).await;
    let at = |x: f64| {
        zombified
            .iter()
            .find(|m| (m.position[0] - x).abs() < 1.0)
            .unwrap()
            .id
    };
    let (near, far_zombified) = (at(near[0]), at(far[0]));
    let piglin = mobs(&east, EntityType::Piglin).await[0].id;
    for id in [near, far_zombified, piglin] {
        assert_eq!(grudge(&east, id).await, Some(UUID));
    }

    let (mut session, frames) = session_with_outbound(&world, PLAYER);
    std::mem::forget(frames);
    session.stand_at([f64::from(border) - 7.5, y, 8.5], true);
    die(&world, &mut session).await;
    world.ticks(2).await;
    assert_eq!(grudge(&east, near).await, None, "forgiven within reach");
    assert_eq!(
        grudge(&east, far_zombified).await,
        Some(UUID),
        "out of reach of the death"
    );
    assert_eq!(grudge(&east, piglin).await, None, "piglins forget anywhere");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_iron_golem_drops_its_grudge_when_the_player_turns_creative() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (region, mut session, golem) = punched_golem(&world).await;
    session
        .set_game_mode(crate::protocol::play::GameMode::Creative)
        .await;
    for _ in 0..5 {
        tick(&world, &mut session).await;
    }
    assert_eq!(grudge(&region, golem).await, None, "forgotten in creative");
    session
        .set_game_mode(crate::protocol::play::GameMode::Survival)
        .await;
    let full = session.health();
    for _ in 0..100 {
        tick(&world, &mut session).await;
    }
    assert_eq!(grudge(&region, golem).await, None, "does not come back");
    assert_eq!(session.health(), full);
    world.shutdown().await;
}

fn universal_anger(world: &TestWorld, on: bool) {
    world.game_rules.send_modify(|rules| {
        rules
            .set("universal_anger", RuleValue::Bool(on))
            .expect("a boolean rule");
    });
}

async fn bystander_hurt_after_punch(on: bool) -> bool {
    bystander_hurt_after_punching(on, EntityType::IronGolem).await
}

async fn bystander_hurt_after_punching(on: bool, kind: EntityType) -> bool {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    universal_anger(&world, on);
    let (_region, mut puncher, _golem) = punched(&world, kind).await;
    let (mut other, frames) = session_with_outbound(&world, PLAYER + 1);
    std::mem::forget(frames);
    other.view.subscribed.insert(ChunkPos::new(0, 0));
    other.stand_at([7.5, SKY as f64 + 1.0, 4.5], true);
    other.publish(true).await;
    puncher.stand_at([12.5, SKY as f64 + 1.0, 12.5], true);
    puncher.publish(true).await;
    let full = other.health();
    let mut hurt = false;
    for _ in 0..200 {
        world.ticks(1).await;
        pump(&mut other).await;
        other.run_tick().await;
        pump(&mut puncher).await;
        puncher.run_tick().await;
        if other.health() < full {
            hurt = true;
            break;
        }
    }
    world.shutdown().await;
    hurt
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn with_universal_anger_a_punched_iron_golem_turns_on_another_player() {
    assert!(bystander_hurt_after_punch(true).await);
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn without_universal_anger_a_punched_iron_golem_leaves_another_player_alone() {
    assert!(!bystander_hurt_after_punch(false).await);
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn with_universal_anger_a_punched_bee_turns_on_another_player() {
    assert!(bystander_hurt_after_punching(true, EntityType::Bee).await);
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn without_universal_anger_a_punched_bee_leaves_another_player_alone() {
    assert!(!bystander_hurt_after_punching(false, EntityType::Bee).await);
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn with_universal_anger_a_punched_enderman_turns_on_another_player() {
    assert!(bystander_hurt_after_punching(true, EntityType::Enderman).await);
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn without_universal_anger_a_punched_enderman_leaves_another_player_alone() {
    assert!(!bystander_hurt_after_punching(false, EntityType::Enderman).await);
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn with_universal_anger_a_punched_zombified_piglin_turns_on_another_player() {
    assert!(bystander_hurt_after_punching(true, EntityType::ZombifiedPiglin).await);
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn without_universal_anger_a_punched_zombified_piglin_leaves_another_player_alone() {
    assert!(!bystander_hurt_after_punching(false, EntityType::ZombifiedPiglin).await);
}
