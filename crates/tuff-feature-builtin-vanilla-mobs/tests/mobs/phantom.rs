//! Phantoms: they fly and circle when idle, and swoop down to bite once
//! they take a player as a target. Natural (insomnia-driven) spawning is
//! covered separately, driving the region actor directly with an injected
//! player index rather than a real session's statistics.

use super::*;
use crate::world::messages::PlayerLooks;
use crate::world::nearby::{IndexedPlayer, PlayerIndex};
use tuff_vanilla_data::registry::EntityType;

/// A tall hollow room, walled and floored with stone, with plenty of
/// headroom for a phantom to circle in. `roofed` caps it, for a room a
/// phantom must stay inside; without a roof the shaft stays open to the
/// sky, for a natural spawn's line-of-sight check.
fn air_room(
    min: i32,
    max: i32,
    height: i32,
    roofed: bool,
) -> Vec<(BlockPos, tuff_vanilla_data::BlockState)> {
    let mut blocks = Vec::new();
    for x in min - 1..=max + 1 {
        for z in min - 1..=max + 1 {
            let edge = x < min || x > max || z < min || z > max;
            blocks.push((BlockPos::new(x, SKY, z), Block::Stone.default_state()));
            if edge {
                for y in SKY + 1..=SKY + height {
                    blocks.push((BlockPos::new(x, y, z), Block::Stone.default_state()));
                }
            }
            if roofed {
                blocks.push((
                    BlockPos::new(x, SKY + height + 1, z),
                    Block::Stone.default_state(),
                ));
            }
        }
    }
    blocks
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn phantoms_fly_and_circle_when_idle() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    place_blocks(&world, &region, air_room(1, 12, 20, true)).await;
    let start = [6.5, SKY as f64 + 10.0, 6.5];
    summon(&region, EntityType::Phantom, Compound::new(), start).await;
    wait_for(&world, &region, 300, |found| {
        found.iter().any(|s| {
            s.kind == EntityType::Phantom && crate::world::distance_sq(s.position, start) > 1.0
        })
    })
    .await;
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn phantoms_swoop_down_and_bite_their_target() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, air_room(1, 12, 20, true)).await;
    summon(
        &region,
        EntityType::Phantom,
        Compound::new(),
        [6.5, SKY as f64 + 15.0, 6.5],
    )
    .await;
    let position = [6.5, SKY as f64 + 1.0, 6.5];
    region
        .request(player_at(&player, position, PlayerLooks::default()))
        .await;
    let hit = next_hit(&world, &mut events, 600).await;
    assert_eq!(hit.kind.name, "minecraft:mob_attack");
    assert_eq!(
        hit.attacker.map(|a| a.kind),
        Some(EntityType::Phantom),
        "the phantom bit its target"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn insomniac_players_spawn_a_batch_of_phantoms_above_them() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    // Well clear of any chunk edge, since a phantom's spawn position can
    // land up to 10 blocks away on either axis: subscribing a 5x5 chunk
    // area around it (the placeholder terrain's natural air well above
    // it is enough; nothing needs to be built) keeps every candidate
    // position loaded and readable.
    let center = ChunkPos::new(5, 5);
    let region = world.region(center.region()).await;
    let (viewer, mut events) = PlayerHandle::new(2000);
    let chunks: Vec<ChunkPos> = (3..=7)
        .flat_map(|x| (3..=7).map(move |z| ChunkPos::new(x, z)))
        .collect();
    subscribe(&region, &viewer, &mut events, &chunks).await;
    world.game_rules.send_modify(|rules| {
        rules
            .set(
                "spawn_phantoms",
                crate::world::game_rules::RuleValue::Bool(true),
            )
            .unwrap();
    });
    // Phantoms only spawn once it is dark enough out.
    world.clocks.send_modify(|clocks| {
        clocks
            .get_mut(crate::world::clock::clock_id("minecraft:overworld").unwrap())
            .set(18_000);
    });
    let (handle, _events) = PlayerHandle::new(1000);
    let position = [87.5, SKY as f64 + 1.0, 87.5];
    world
        .players
        .send_replace(std::sync::Arc::new(PlayerIndex::build([(
            world.dimension(crate::storage::level::OVERWORLD),
            IndexedPlayer {
                handle,
                uuid: 77,
                name: std::sync::Arc::from("Insomniac"),
                position,
                looks: PlayerLooks::default(),
                time_since_rest: i32::MAX,
            },
        )])));
    wait_for(&world, &region, 5000, |found| {
        found.iter().any(|s| s.kind == EntityType::Phantom)
    })
    .await;
    world.shutdown().await;
}
