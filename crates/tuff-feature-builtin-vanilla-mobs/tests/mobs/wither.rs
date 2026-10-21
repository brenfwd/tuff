//! The wither boss: block-assembly summon, birth invulnerability ending in
//! an explosion, and dying once vulnerable.

use super::*;
use crate::world::entity::EntityAttack;
use crate::world::messages::PlayerLooks;
use crate::world::nearby::{IndexedPlayer, PlayerIndex};
use tuff_vanilla_data::registry::EntityType;

/// A T of soul sand topped by three wither skeleton skulls, centered
/// (horizontally) on `head`.
fn t_of_soul_sand(head: BlockPos) -> Vec<(BlockPos, tuff_vanilla_data::BlockState)> {
    let base = Block::SoulSand.default_state();
    let skull = Block::WitherSkeletonSkull.default_state();
    vec![
        (BlockPos::new(head.x - 1, head.y, head.z), skull),
        (head, skull),
        (BlockPos::new(head.x + 1, head.y, head.z), skull),
        (BlockPos::new(head.x - 1, head.y - 1, head.z), base),
        (BlockPos::new(head.x, head.y - 1, head.z), base),
        (BlockPos::new(head.x + 1, head.y - 1, head.z), base),
        (BlockPos::new(head.x, head.y - 2, head.z), base),
    ]
}

pub(super) fn lethal_attack(player: &PlayerHandle, at: [f64; 3]) -> EntityAttack {
    EntityAttack {
        attacker: player.id,
        attacker_uuid: 77,
        attacker_position: at,
        attacker_yaw: 0.0,
        attacker_pitch: 0.0,
        damage: 10_000.0,
        knockback: 0.0,
        weapon: None,
        critical: false,
        magic: false,
        sweep: false,
        part: None,
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn three_skulls_on_a_soul_sand_t_assemble_an_invulnerable_wither_that_dies_once_born() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;

    let head = BlockPos::new(8, SKY + 1, 8);
    let mut structure = t_of_soul_sand(head);
    // The center skull is placed last, through the tested `Place` path,
    // like the golem tests place the completing pumpkin last.
    let center_skull = structure.remove(1);
    assert_eq!(center_skull.0, head);
    place_blocks(&world, &region, structure.clone()).await;

    let placement = place(&player, head, Item::WitherSkeletonSkull, 1);
    region.request(RegionRequest::Place(placement)).await;
    let outcome = expect(&mut events, |event| match event {
        PlayerEvent::ActionResult {
            sequence: 1,
            outcome,
        } => Some(outcome),
        _ => None,
    })
    .await;
    assert!(
        matches!(outcome, ActionOutcome::Placed { .. }),
        "{outcome:?}"
    );
    world.ticks(2).await;

    for (pos, _) in structure.iter().chain([&center_skull]) {
        assert_eq!(
            block_at(&region, *pos).await.map(|s| s.block()),
            Some(Block::Air),
            "{pos:?} is cleared"
        );
    }

    let withers = mobs(&region, EntityType::Wither).await;
    assert_eq!(withers.len(), 1, "{withers:?}");
    let wither = withers[0].id;
    let position = withers[0].position;

    region
        .request(RegionRequest::AttackEntity {
            attacker: player.clone(),
            target: wither,
            attack: lethal_attack(&player, position),
            sweep: None,
        })
        .await;
    world.ticks(5).await;
    assert_eq!(
        mobs(&region, EntityType::Wither).await.len(),
        1,
        "invulnerable during its birth"
    );

    // Past the birth window it detonates once, heals to full and can be
    // hurt again.
    world.ticks(230).await;

    region
        .request(RegionRequest::AttackEntity {
            attacker: player.clone(),
            target: wither,
            attack: lethal_attack(&player, position),
            sweep: None,
        })
        .await;
    // The death animation holds the corpse for a further DEATH_TICKS.
    world.ticks(25).await;
    assert!(
        mobs(&region, EntityType::Wither).await.is_empty(),
        "dies once vulnerable"
    );
    assert!(
        !mobs(&region, EntityType::Item).await.is_empty(),
        "drops its Nether Star (a code drop, not the empty jar loot table)"
    );

    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_summoned_wither_is_not_invulnerable() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, _events) = PlayerHandle::new(1000);
    place_blocks(
        &world,
        &region,
        [(BlockPos::new(8, SKY - 1, 8), Block::Stone.default_state())],
    )
    .await;

    summon(
        &region,
        EntityType::Wither,
        Compound::new(),
        [8.5, SKY as f64, 8.5],
    )
    .await;
    world.ticks(2).await;
    let withers = mobs(&region, EntityType::Wither).await;
    assert_eq!(withers.len(), 1, "{withers:?}");
    let (wither, position) = (withers[0].id, withers[0].position);

    region
        .request(RegionRequest::AttackEntity {
            attacker: player.clone(),
            target: wither,
            attack: lethal_attack(&player, position),
            sweep: None,
        })
        .await;
    world.ticks(25).await;
    assert!(
        mobs(&region, EntityType::Wither).await.is_empty(),
        "a /summon-ed wither takes damage immediately, unlike a block-assembled one"
    );

    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_wither_publishes_a_boss_bar_to_a_nearby_player() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    let overworld = world.dimension(crate::storage::level::OVERWORLD);
    let position = [8.5, SKY as f64, 8.5];
    world.players.send_replace(Arc::new(PlayerIndex::build([(
        overworld,
        IndexedPlayer {
            handle: player.clone(),
            uuid: 77,
            name: Arc::from("runner"),
            position,
            looks: PlayerLooks::default(),
            time_since_rest: 0,
        },
    )])));

    summon(
        &region,
        EntityType::Wither,
        Compound::new(),
        [position[0], position[1] + 3.0, position[2]],
    )
    .await;

    let boss = tuff_vanilla_data::packet::play::Clientbound::BossEvent.id();
    ticking_expect(&world, &mut events, 40, |event| match event {
        PlayerEvent::Packets(packets) => packets
            .iter()
            .any(|p| framed_packet_id(p) == boss)
            .then_some(()),
        _ => None,
    })
    .await;

    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_wither_shoots_skulls_that_hit_and_wither_a_player() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    // A plain, roofless pen: the wither is much taller than the undead
    // `roofed_pen` was built for, and would spawn with its head embedded
    // in that low ceiling.
    place_blocks(&world, &region, pen(2, 13)).await;
    summon(
        &region,
        EntityType::Wither,
        Compound::new(),
        [3.5, SKY as f64 + 1.0, 3.5],
    )
    .await;
    let position = [12.5, SKY as f64 + 1.0, 12.5];
    region
        .request(player_at(&player, position, PlayerLooks::default()))
        .await;
    let hit = next_hit(&world, &mut events, 600).await;
    assert_eq!(hit.projectile, Some(EntityType::WitherSkull));
    assert_eq!(hit.amount, 8.0);
    assert!(!hit.effects.is_empty(), "applies the Wither status effect");
    world.shutdown().await;
}
