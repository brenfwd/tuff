//! Illagers and raid mobs: evoker spellcasting (fangs, vex summoning) and
//! vex charging, driven through a region's channels. Raids are not covered
//! here, so these only check standalone hostile behavior.

use super::*;
use crate::world::messages::PlayerLooks;
use tuff_vanilla_data::Item;
use tuff_vanilla_data::registry::EntityType;

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn evokers_cast_fangs_at_a_close_target() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(0, 10)).await;
    summon(
        &region,
        EntityType::Evoker,
        Compound::new(),
        [3.5, SKY as f64 + 1.0, 3.5],
    )
    .await;
    // Two blocks away: within the evoker's close-range fang ring.
    let position = [5.5, SKY as f64 + 1.0, 3.5];
    region
        .request(player_at(&player, position, PlayerLooks::default()))
        .await;
    let hit = next_hit(&world, &mut events, 400).await;
    assert_eq!(hit.kind.name, "minecraft:indirect_magic");
    assert_eq!(hit.amount, 6.0);
    let attacker = hit.attacker.expect("the evoker is to blame");
    assert_eq!(attacker.kind, EntityType::Evoker);
    let fangs = mobs(&region, EntityType::EvokerFangs).await;
    assert!(!fangs.is_empty(), "the spell spawned fang entities");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn evokers_summon_vexes() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(0, 20)).await;
    summon(
        &region,
        EntityType::Evoker,
        Compound::new(),
        [3.5, SKY as f64 + 1.0, 3.5],
    )
    .await;
    // Within the evoker's 12-block follow range, so it keeps a target and
    // eventually casts either spell; a vex appearing can only be the
    // summon spell (the only thing that creates one).
    let position = [10.5, SKY as f64 + 1.0, 3.5];
    region
        .request(player_at(&player, position, PlayerLooks::default()))
        .await;
    let found = wait_for(&world, &region, 600, |found| {
        found.iter().any(|s| s.kind == EntityType::Vex)
    })
    .await;
    assert!(found.iter().any(|s| s.kind == EntityType::Vex));
    // Drain any hurt events the fangs line might still have produced;
    // this test only cares that a vex was summoned.
    while events.try_recv().is_ok() {}
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vexes_charge_and_hit_their_target() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(0, 10)).await;
    summon(
        &region,
        EntityType::Vex,
        Compound::new(),
        [3.5, SKY as f64 + 2.0, 3.5],
    )
    .await;
    let position = [7.5, SKY as f64 + 2.0, 3.5];
    region
        .request(player_at(&player, position, PlayerLooks::default()))
        .await;
    let hit = next_hit(&world, &mut events, 600).await;
    assert_eq!(hit.kind.name, "minecraft:mob_attack");
    assert_eq!(hit.amount, 4.0, "a vex's attack damage");
    let attacker = hit.attacker.expect("the vex is to blame");
    assert_eq!(attacker.kind, EntityType::Vex);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[test]
fn evoker_fangs_have_no_extra_metadata() {
    assert_eq!(
        EntityType::EvokerFangs.data_layout().len(),
        8,
        "no metadata beyond the shared entity fields"
    );
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vindicators_chase_and_hit_players_with_their_axe() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, roofed_pen(0, 12)).await;
    summon(
        &region,
        EntityType::Vindicator,
        Compound::new(),
        [1.5, SKY as f64 + 1.0, 1.5],
    )
    .await;
    let position = [10.5, SKY as f64 + 1.0, 1.5];
    region
        .request(player_at(&player, position, PlayerLooks::default()))
        .await;
    let hit = next_hit(&world, &mut events, 600).await;
    assert_eq!(hit.kind.name, "minecraft:mob_attack");
    let attacker = hit.attacker.expect("the vindicator is to blame");
    assert_eq!(attacker.kind, EntityType::Vindicator);
    let vindicator = &mobs(&region, EntityType::Vindicator).await[0];
    assert!(
        crate::world::distance_sq(vindicator.position, position) < 3.0 * 3.0,
        "it walked over: {:?}",
        vindicator.position
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pillagers_load_and_fire_their_crossbow() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(0, 12)).await;
    // `summon` loads from saved NBT rather than a fresh spawn, so it skips
    // `finalize_spawn`'s default equipment; give it a crossbow directly.
    let equipped = Compound::new().with(
        "equipment",
        Compound::new().with(
            "mainhand",
            tuff_item::item::ItemStack::mint(Item::Crossbow, 1, tuff_item::item::MintReason::Test)
                .to_nbt(),
        ),
    );
    summon(
        &region,
        EntityType::Pillager,
        equipped,
        [1.5, SKY as f64 + 1.0, 1.5],
    )
    .await;
    // Six blocks away: within its eight-block crossbow range.
    let position = [7.5, SKY as f64 + 1.0, 1.5];
    region
        .request(player_at(&player, position, PlayerLooks::default()))
        .await;
    let hit = next_hit(&world, &mut events, 600).await;
    assert_eq!(hit.kind.name, "minecraft:arrow");
    let attacker = hit.attacker.expect("the pillager is to blame");
    assert_eq!(attacker.kind, EntityType::Pillager);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ravagers_chase_and_hit_hard() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, roofed_pen(0, 14)).await;
    summon(
        &region,
        EntityType::Ravager,
        Compound::new(),
        [1.5, SKY as f64 + 1.0, 1.5],
    )
    .await;
    let position = [12.5, SKY as f64 + 1.0, 1.5];
    region
        .request(player_at(&player, position, PlayerLooks::default()))
        .await;
    let hit = next_hit(&world, &mut events, 600).await;
    assert_eq!(hit.kind.name, "minecraft:mob_attack");
    assert_eq!(hit.amount, 12.0, "a ravager's attack damage");
    let attacker = hit.attacker.expect("the ravager is to blame");
    assert_eq!(attacker.kind, EntityType::Ravager);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn illusioners_shoot_arrows() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(0, 12)).await;
    let equipped = Compound::new().with(
        "equipment",
        Compound::new().with(
            "mainhand",
            tuff_item::item::ItemStack::mint(Item::Bow, 1, tuff_item::item::MintReason::Test)
                .to_nbt(),
        ),
    );
    summon(
        &region,
        EntityType::Illusioner,
        equipped,
        [1.5, SKY as f64 + 1.0, 1.5],
    )
    .await;
    let position = [9.5, SKY as f64 + 1.0, 1.5];
    region
        .request(player_at(&player, position, PlayerLooks::default()))
        .await;
    let hit = next_hit(&world, &mut events, 600).await;
    assert_eq!(hit.kind.name, "minecraft:arrow");
    let attacker = hit.attacker.expect("the illusioner is to blame");
    assert_eq!(attacker.kind, EntityType::Illusioner);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn illusioners_blind_their_target_on_hard() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    world
        .difficulty
        .send_replace(crate::world::player::Difficulty::Hard);
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(0, 12)).await;
    summon(
        &region,
        EntityType::Illusioner,
        Compound::new(),
        [1.5, SKY as f64 + 1.0, 1.5],
    )
    .await;
    let position = [3.5, SKY as f64 + 1.0, 1.5];
    region
        .request(player_at(&player, position, PlayerLooks::default()))
        .await;
    let hit = ticking_expect(&world, &mut events, 400, |event| match event {
        PlayerEvent::Potion(hit) => Some(hit),
        _ => None,
    })
    .await;
    assert_eq!(hit.effects.len(), 1);
    assert_eq!(
        hit.effects[0].effect,
        tuff_vanilla_data::registry::MobEffect::Blindness
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_raised_shield_stuns_or_throws_back_the_ravager_biting_it() {
    use crate::world::entity::{Culprit, EntityBase, EntityHit};
    use crate::world::inventory::Inventory;
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (mut defender, _outbound) = session_with_outbound(&world, 1);
    let stand = [5.5, SKY as f64 + 1.0, 3.5];
    defender.pose.position = stand;
    defender.pose.yaw = 90.0;
    let handle = defender.conn.handle.clone();
    subscribe(&region, &handle, &mut defender.conn.events, &[chunk]).await;
    place_blocks(&world, &region, roofed_pen(0, 10)).await;
    let at = [3.5, SKY as f64 + 1.0, 3.5];
    summon(
        &region,
        EntityType::Ravager,
        Compound::new().with("NoAI", true),
        at,
    )
    .await;
    region
        .request(player_at(&handle, stand, PlayerLooks::default()))
        .await;
    let id = mobs(&region, EntityType::Ravager).await[0].id;
    defender.inventory_mut().set_slot(
        Inventory::OFFHAND_SLOT,
        Some(tuff_item::item::ItemStack::mint(
            Item::Shield,
            1,
            tuff_item::item::MintReason::Test,
        )),
    );
    defender.hold_up(true, 10);
    let mut hit = EntityHit::new(12.0, &tuff_vanilla_data::damage_type::MOB_ATTACK);
    hit.attacker = Some(Culprit::entity(&EntityBase::new(
        EntityType::Ravager,
        id,
        id as u128,
        at,
        [0.0; 3],
    )));
    hit.position = Some(at);
    hit.report_blocked = true;
    defender
        .handle_event(PlayerEvent::EntityHurt(Box::new(hit)))
        .await;
    assert_eq!(defender.health(), 20.0, "the shield took the bite");

    let push = ticking_expect(&world, &mut defender.conn.events, 40, |event| match event {
        PlayerEvent::Pushed(push) => Some(push),
        _ => None,
    })
    .await;
    let stunned = entity_nbt(&region, id)
        .await
        .and_then(|nbt| match nbt.get("StunTick") {
            Some(Tag::Int(v)) => Some(*v),
            _ => None,
        })
        .unwrap_or(0);
    if stunned > 0 {
        assert!(push[0] > 0.0 && push[0] < 0.06, "nudged off: {push:?}");
    } else {
        assert_eq!(push, [2.0, 0.2, 0.0], "thrown back");
    }
    world.shutdown().await;
}
