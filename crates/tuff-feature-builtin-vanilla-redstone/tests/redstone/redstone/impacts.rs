//! Blocks reacting to the projectiles that strike them.

use super::*;
use tuff_feature_builtin_vanilla_combat::projectile::arrow::Arrow;

/// Fires an arrow (burning if `burning`) east along the bench, low enough
/// to strike a short block at `x`, from three blocks west of it.
async fn shoot_east(b: &Bench, x: i32, z: i32, burning: bool) {
    let mut arrow = Arrow::new(
        b.world.shared.next_entity_id(),
        [
            f64::from(x) - 2.5,
            f64::from(FLOOR_Y) + 1.2,
            f64::from(z) + 0.5,
        ],
        [1.5, 0.0, 0.0],
        Default::default(),
    );
    arrow.base.no_gravity = true;
    if burning {
        arrow.base.fire = 100;
    }
    b.region
        .request(RegionRequest::SpawnEntity(Box::new(arrow)))
        .await;
}

async fn wait_for_state(
    b: &Bench,
    pos: BlockPos,
    done: impl Fn(tuff_vanilla_data::BlockState) -> bool,
) -> bool {
    for _ in 0..20 {
        if done(state(&b.region, pos).await) {
            return true;
        }
        b.world.ticks(1).await;
    }
    done(state(&b.region, pos).await)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn burning_arrows_light_campfires_and_candles_but_cold_ones_do_not() {
    let dir = TestDir::new();
    let b = bench(dir.path(), &[ChunkPos::new(5, 5)], (80, 92), (80, 92)).await;
    let (campfire, candle, cold) = (at(86, 1, 84), at(86, 1, 86), at(86, 1, 88));
    set(
        &b.region,
        &[
            (campfire, "minecraft:campfire[lit=false]"),
            (candle, "minecraft:candle[lit=false]"),
            (cold, "minecraft:campfire[lit=false]"),
        ],
    )
    .await;
    shoot_east(&b, 86, 84, true).await;
    shoot_east(&b, 86, 86, true).await;
    shoot_east(&b, 86, 88, false).await;
    assert!(wait_for_state(&b, campfire, |s| s.lit() == Some(true)).await);
    assert!(wait_for_state(&b, candle, |s| s.lit() == Some(true)).await);
    b.world.ticks(5).await;
    assert_eq!(state(&b.region, cold).await.lit(), Some(false));
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_burning_arrow_primes_tnt() {
    let dir = TestDir::new();
    let b = bench(dir.path(), &[ChunkPos::new(5, 5)], (80, 92), (80, 92)).await;
    let tnt = at(86, 1, 86);
    set(&b.region, &[(tnt, "minecraft:tnt")]).await;
    shoot_east(&b, 86, 86, true).await;
    assert!(wait_for_state(&b, tnt, is_air).await, "the tnt is lit");
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_snowball_powers_a_target_briefly() {
    let dir = TestDir::new();
    let b = bench(dir.path(), &[ChunkPos::new(5, 5)], (80, 92), (80, 92)).await;
    let target = at(86, 1, 86);
    set(&b.region, &[(target, "minecraft:target")]).await;
    let snowball = tuff_feature_builtin_vanilla_combat::projectile::thrown::Thrown::new(
        tuff_vanilla_data::registry::EntityType::Snowball,
        b.world.shared.next_entity_id(),
        [83.5, f64::from(FLOOR_Y) + 1.5, 86.5],
        [1.5, 0.0, 0.0],
        ItemStack::mint(Item::Snowball, 1, MintReason::Test),
        Default::default(),
    );
    b.region
        .request(RegionRequest::SpawnEntity(Box::new(snowball)))
        .await;
    assert!(wait_for_state(&b, target, |s| s.power() > Some(0)).await);
    b.world.ticks(9).await;
    assert_eq!(
        state(&b.region, target).await.power(),
        Some(0),
        "a thrown item holds the signal for 8 ticks, not an arrow's 20"
    );
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_fast_trident_snaps_pointed_dripstone() {
    let dir = TestDir::new();
    let b = bench(dir.path(), &[ChunkPos::new(5, 5)], (80, 92), (80, 92)).await;
    let tip = at(86, 1, 86);
    set(
        &b.region,
        &[(
            tip,
            "minecraft:pointed_dripstone[vertical_direction=up,thickness=tip]",
        )],
    )
    .await;
    let mut trident = Arrow::trident(
        ItemStack::mint(Item::Trident, 1, MintReason::Test),
        b.world.shared.next_entity_id(),
        [83.5, f64::from(FLOOR_Y) + 1.3, 86.5],
        [2.5, 0.0, 0.0],
        Default::default(),
    );
    trident.base.no_gravity = true;
    b.region
        .request(RegionRequest::SpawnEntity(Box::new(trident)))
        .await;
    assert!(
        wait_for_state(&b, tip, is_air).await,
        "the dripstone breaks"
    );
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_players_snowball_on_a_target_reports_the_signal_it_gave() {
    let dir = TestDir::new();
    let mut b = bench(dir.path(), &[ChunkPos::new(5, 5)], (80, 92), (80, 92)).await;
    let target = at(86, 1, 86);
    set(&b.region, &[(target, "minecraft:target")]).await;
    let thrower = [82.5, f64::from(FLOOR_Y + 1), 86.5];
    b.region.request(update(&b.player, 1, thrower)).await;
    let owner = tuff_core::world::projectile::Owner::of(
        crate::world::entity::Culprit::player(b.player.id, u128::from(b.player.id as u32), "P"),
        Some(b.player.clone()),
    );
    let mut snowball = tuff_feature_builtin_vanilla_combat::projectile::thrown::Thrown::new(
        tuff_vanilla_data::registry::EntityType::Snowball,
        b.world.shared.next_entity_id(),
        [83.5, f64::from(FLOOR_Y) + 1.5, 86.5],
        [1.5, 0.0, 0.0],
        ItemStack::mint(Item::Snowball, 1, MintReason::Test),
        owner,
    );
    snowball.base.no_gravity = true;
    b.region
        .request(RegionRequest::SpawnEntity(Box::new(snowball)))
        .await;
    let (kind, signal) = super::ticking_expect(&b.world, &mut b.events, 20, |event| match event {
        PlayerEvent::Advancement(event) => match *event {
            crate::world::messages::AdvancementEvent::TargetHit { projectile, signal } => {
                Some((projectile.kind, signal))
            }
            _ => None,
        },
        _ => None,
    })
    .await;
    assert_eq!(
        kind,
        Some(tuff_vanilla_data::registry::EntityType::Snowball)
    );
    assert_eq!(signal, 15, "struck dead centre");
    b.world.shutdown().await;
}
