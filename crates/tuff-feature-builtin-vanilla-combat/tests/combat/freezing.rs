//! Powder snow freezing players and mobs.

use super::*;
use crate::world::block_contact::Contact;
use crate::world::freezing::{self, TICKS_TO_FREEZE};
use crate::world::inventory::Inventory;
use crate::world::session::PlayerSession;
use tuff_vanilla_data::item::EquipmentSlot;
use tuff_vanilla_data::registry::EntityType;

fn session(world: &TestWorld, id: i32) -> PlayerSession {
    let (session, outbound) = session_with_outbound(world, id);
    std::mem::forget(outbound);
    session
}

fn in_powder_snow(player: &PlayerSession, powder_snow: bool) -> PlayerEvent {
    PlayerEvent::Surroundings {
        dimension: player.dimension_id(),
        seq: 0,
        in_water: false,
        submersion: Default::default(),
        in_lava: false,
        in_fire: None,
        climbable: false,
        climb_location: None,
        resets_fall: false,
        landed_on: None,
        portal: None,
        contact: Contact {
            freezing: freezing::Inside {
                powder_snow,
                thaws: false,
                powder_snow_at: None,
            },
            ..Contact::default()
        },
        biome: None,
        stepping_on: None,
    }
}

async fn run_ticks(player: &mut PlayerSession, ticks: usize) {
    for _ in 0..ticks {
        player.run_tick().await;
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn powder_snow_freezes_a_player_hurts_it_and_thaws_outside() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut player = session(&world, 1000);
    player.make_hungry();
    player.handle_event(in_powder_snow(&player, true)).await;
    run_ticks(&mut player, TICKS_TO_FREEZE as usize - 1).await;
    assert_eq!(player.body.ticks_frozen, TICKS_TO_FREEZE - 1);
    assert_eq!(player.health(), 20.0, "not hurt before fully frozen");
    run_ticks(&mut player, 50).await;
    assert_eq!(player.body.ticks_frozen, TICKS_TO_FREEZE);
    assert_eq!(player.health(), 19.0, "one freeze hit within 40 ticks");

    player.handle_event(in_powder_snow(&player, false)).await;
    run_ticks(&mut player, 10).await;
    assert_eq!(player.body.ticks_frozen, TICKS_TO_FREEZE - 20);
    run_ticks(&mut player, 100).await;
    assert_eq!(player.body.ticks_frozen, 0);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn leather_armor_keeps_a_player_from_freezing() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut player = session(&world, 1000);
    let feet = Inventory::armor_slot(EquipmentSlot::Feet).unwrap();
    player.inventory_mut().set_slot(
        feet,
        Some(ItemStack::mint(Item::LeatherBoots, 1, MintReason::Test)),
    );
    player.handle_event(in_powder_snow(&player, true)).await;
    run_ticks(&mut player, 200).await;
    assert_eq!(player.body.ticks_frozen, 0);
    assert_eq!(player.health(), 20.0);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_freeze_damage_rule_spares_frozen_players() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    world.game_rules.send_modify(|rules| {
        rules
            .set(
                "freeze_damage",
                crate::world::game_rules::RuleValue::Bool(false),
            )
            .expect("a boolean rule");
    });
    let mut player = session(&world, 1000);
    player.make_hungry();
    player.handle_event(in_powder_snow(&player, true)).await;
    run_ticks(&mut player, 300).await;
    assert_eq!(player.body.ticks_frozen, TICKS_TO_FREEZE);
    assert_eq!(player.health(), 20.0);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn home_region_reports_powder_snow_around_a_player() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let pos = ChunkPos::new(0, 0);
    let region = world.region(pos.region()).await;
    let (player, mut events) = PlayerHandle::new(100);
    subscribe(&region, &player, &mut events, &[pos]).await;
    place_blocks(
        &world,
        &region,
        [
            (BlockPos::new(2, SKY, 2), Block::PowderSnow.default_state()),
            (BlockPos::new(6, SKY, 2), Block::Fire.default_state()),
        ],
    )
    .await;
    for (seq, position, powder_snow, thaws) in [
        (1, [2.5, SKY as f64, 2.5], true, false),
        (2, [6.5, SKY as f64, 2.5], false, true),
        (3, [10.5, SKY as f64, 2.5], false, false),
    ] {
        region.request(update(&player, seq, position)).await;
        let contact = expect(&mut events, |event| match event {
            PlayerEvent::Surroundings {
                seq: at, contact, ..
            } if at == seq => Some(contact),
            _ => None,
        })
        .await;
        assert_eq!(
            (contact.freezing.powder_snow, contact.freezing.thaws),
            (powder_snow, thaws),
            "at {position:?}"
        );
    }
    world.shutdown().await;
}

async fn entity_nbt(region: &RegionHandle, id: i32) -> Compound {
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::EntityData { id, reply })
        .await;
    answer.await.unwrap().expect("entity exists")
}

/// A two-deep powder snow pit on stone at `(x, z)`, walled with stone.
fn pit(x: i32, z: i32) -> Vec<(BlockPos, tuff_vanilla_data::BlockState)> {
    let mut blocks = Vec::new();
    for dx in -1..=1 {
        for dz in -1..=1 {
            let (bx, bz) = (x + dx, z + dz);
            blocks.push((BlockPos::new(bx, SKY - 1, bz), Block::Stone.default_state()));
            let wall = dx != 0 || dz != 0;
            for y in SKY..SKY + 2 {
                let state = if wall {
                    Block::Stone.default_state()
                } else {
                    Block::PowderSnow.default_state()
                };
                blocks.push((BlockPos::new(bx, y, bz), state));
            }
        }
    }
    blocks
}

async fn freezing_world() -> (TestDir, TestWorld, RegionHandle, PlayerHandle) {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    view_around(&world, &player, &mut events, chunk).await;
    std::mem::forget(events);
    (dir, world, region, player)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn powder_snow_freezes_and_hurts_a_mob_and_saves_its_frozen_ticks() {
    let (_dir, world, region, _player) = freezing_world().await;
    place_blocks(&world, &region, pit(4, 4)).await;
    place_blocks(&world, &region, pit(10, 4)).await;
    summon(
        &region,
        EntityType::Pig,
        Compound::new().with("NoAI", true),
        [4.5, SKY as f64, 4.5],
    )
    .await;
    let boots = ItemStack::mint(Item::LeatherBoots, 1, MintReason::Test).to_nbt();
    summon(
        &region,
        EntityType::Pig,
        Compound::new()
            .with("NoAI", true)
            .with("equipment", Compound::new().with("feet", boots)),
        [10.5, SKY as f64, 4.5],
    )
    .await;
    world.ticks(TICKS_TO_FREEZE as u64 + 45).await;
    let pigs = mobs(&region, EntityType::Pig).await;
    assert_eq!(pigs.len(), 2);
    let (bare, booted) = if pigs[0].position[0] < 7.0 {
        (pigs[0].id, pigs[1].id)
    } else {
        (pigs[1].id, pigs[0].id)
    };
    let nbt = entity_nbt(&region, bare).await;
    assert_eq!(nbt.get_int("TicksFrozen"), Some(TICKS_TO_FREEZE));
    assert!(
        nbt.get_float("Health").unwrap() < 10.0,
        "{:?}",
        nbt.get_float("Health")
    );
    let nbt = entity_nbt(&region, booted).await;
    assert_eq!(
        nbt.get_int("TicksFrozen"),
        None,
        "leather boots keep it warm"
    );
    assert_eq!(nbt.get_float("Health"), Some(10.0));

    place_blocks(
        &world,
        &region,
        [
            (BlockPos::new(4, SKY, 4), Block::Air.default_state()),
            (BlockPos::new(4, SKY + 1, 4), Block::Air.default_state()),
        ],
    )
    .await;
    world.ticks(80).await;
    let nbt = entity_nbt(&region, bare).await;
    assert_eq!(
        nbt.get_int("TicksFrozen"),
        None,
        "thawed outside powder snow"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rabbits_stand_on_powder_snow_that_pigs_sink_into() {
    let (_dir, world, region, _player) = freezing_world().await;
    place_blocks(&world, &region, pit(4, 4)).await;
    place_blocks(&world, &region, pit(10, 4)).await;
    let top = (SKY + 2) as f64;
    summon(
        &region,
        EntityType::Rabbit,
        Compound::new().with("NoAI", true),
        [4.5, top + 0.5, 4.5],
    )
    .await;
    summon(
        &region,
        EntityType::Pig,
        Compound::new().with("NoAI", true),
        [10.5, top + 0.5, 4.5],
    )
    .await;
    world.ticks(40).await;
    let rabbit = &mobs(&region, EntityType::Rabbit).await[0];
    assert!(
        (rabbit.position[1] - top).abs() < 1.0e-6,
        "{:?}",
        rabbit.position
    );
    let pig = &mobs(&region, EntityType::Pig).await[0];
    assert!(
        (pig.position[1] - SKY as f64).abs() < 1.0e-6,
        "{:?}",
        pig.position
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_burning_mob_melts_the_powder_snow_it_is_in_and_goes_out() {
    let (_dir, world, region, _player) = freezing_world().await;
    place_blocks(&world, &region, pit(4, 4)).await;
    summon(
        &region,
        EntityType::Pig,
        Compound::new().with("NoAI", true).with("Fire", 200i16),
        [4.5, SKY as f64, 4.5],
    )
    .await;
    world.ticks(5).await;
    assert_eq!(
        block_at(&region, BlockPos::new(4, SKY, 4))
            .await
            .map(|s| s.block()),
        Some(Block::Air),
        "the snow around it melted"
    );
    let pig = mobs(&region, EntityType::Pig).await[0].id;
    let fire = entity_nbt(&region, pig).await.get("Fire").cloned();
    assert!(
        fire.as_ref()
            .and_then(crate::world::entity::number)
            .is_none_or(|f| f <= 0.0),
        "put out: {fire:?}"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn powder_snow_puts_out_a_burning_player() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut player = session(&world, 1000);
    player.make_hungry();
    let PlayerEvent::Surroundings {
        dimension,
        mut contact,
        ..
    } = in_powder_snow(&player, true)
    else {
        unreachable!()
    };
    contact.freezing.powder_snow_at = Some(BlockPos::new(0, 64, 0));
    player.ignite(8.0);
    assert!(player.fire_ticks() > 0);
    player
        .handle_event(PlayerEvent::Surroundings {
            dimension,
            seq: 0,
            in_water: false,
            submersion: Default::default(),
            in_lava: false,
            in_fire: None,
            climbable: false,
            climb_location: None,
            resets_fall: false,
            landed_on: None,
            portal: None,
            contact,
            biome: None,
            stepping_on: None,
        })
        .await;
    player.run_tick().await;
    assert!(player.fire_ticks() <= 0, "powder snow put it out");
    world.shutdown().await;
}
