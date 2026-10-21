//! Eyes of ender, end crystals and the dragon fight driven through real
//! regions.

use super::*;
use crate::world::entity::EntityAttack;
use crate::world::messages::PlayerLooks;
use crate::world::messages::{BlockOp, BlockOpResult};
use crate::world::nearby::{IndexedPlayer, PlayerIndex};
use tuff_feature_builtin_vanilla_dimensions::dragon::fight::FightsExt;
use tuff_vanilla_data::registry::EntityType;

async fn snapshots(region: &RegionHandle) -> Vec<crate::world::messages::EntitySnapshot> {
    let (reply, answer) = oneshot::channel();
    region.request(RegionRequest::Entities { reply }).await;
    answer.await.unwrap()
}

async fn summon(region: &RegionHandle, id: &str, nbt: Compound, position: [f64; 3]) {
    let (reply, summoned) = oneshot::channel();
    region
        .request(RegionRequest::Summon {
            nbt: nbt.with("id", id),
            position,
            fresh: false,
            reply,
        })
        .await;
    assert!(summoned.await.unwrap(), "{id} loads from NBT");
}

fn locate_eye(
    player: &PlayerHandle,
    eye: [f64; 3],
    look: [f64; 3],
    off_hand: bool,
) -> RegionRequest {
    tuff_feature_builtin_vanilla_items::world::region::interact::locate_eye_request(
        player.clone(),
        eye,
        look,
        4.5,
        BlockPos::new(
            eye[0].floor() as i32,
            eye[1].floor() as i32,
            eye[2].floor() as i32,
        ),
        off_hand,
    )
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn eyes_of_ender_are_not_thrown_at_end_portal_frames() {
    let dir = TestDir::new();
    let (player, mut events) = PlayerHandle::new(1000);
    let world = TestWorld::start(dir.path());
    let region = world.region(RegionPos::new(0, 0)).await;
    subscribe(&region, &player, &mut events, &[ChunkPos::new(0, 0)]).await;
    let frame = BlockPos::new(8, 150, 10);
    place_blocks(
        &world,
        &region,
        [(frame, Block::EndPortalFrame.default_state())],
    )
    .await;
    let eye = [8.5, 150.5, 8.5];
    region
        .request(locate_eye(&player, eye, [0.0, 0.0, 1.0], true))
        .await;
    region
        .request(locate_eye(&player, eye, [0.0, 0.0, -1.0], false))
        .await;
    let (off_hand, target) = expect(&mut events, |event| match event {
        PlayerEvent::Run(task) => task
            .data::<tuff_feature_builtin_vanilla_items::world::region::interact::EyeTarget>()
            .map(|eye| (eye.off_hand, eye.target)),
        _ => None,
    })
    .await;
    assert!(!off_hand, "the throw at the frame was not refused");
    assert_eq!(target, None, "the test terrain has no strongholds");
    world.shutdown().await;
}

fn attack(
    player: &PlayerHandle,
    position: [f64; 3],
    damage: f32,
    part: Option<usize>,
) -> EntityAttack {
    EntityAttack {
        attacker: player.id,
        attacker_uuid: 77,
        attacker_position: position,
        attacker_yaw: 0.0,
        attacker_pitch: 0.0,
        damage,
        knockback: 0.0,
        weapon: None,
        critical: false,
        magic: false,
        sweep: false,
        part,
    }
}

fn explode_id() -> i32 {
    tuff_vanilla_data::packet::play::Clientbound::Explode.id()
}

async fn of_kind(
    region: &RegionHandle,
    kind: EntityType,
) -> Vec<crate::world::messages::EntitySnapshot> {
    snapshots(region)
        .await
        .into_iter()
        .filter(|e| e.kind == kind)
        .collect()
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn end_crystals_are_placed_on_obsidian_and_explode_when_hit() {
    let dir = TestDir::new();
    let (player, mut events) = PlayerHandle::new(1000);
    let world = TestWorld::start(dir.path());
    let region = world.region(RegionPos::new(0, 0)).await;
    subscribe(&region, &player, &mut events, &[ChunkPos::new(0, 0)]).await;
    let base = BlockPos::new(8, 150, 8);
    place_blocks(
        &world,
        &region,
        [
            (base, Block::Obsidian.default_state()),
            (BlockPos::new(3, 150, 3), Block::Stone.default_state()),
        ],
    )
    .await;

    let mut click = place(&player, BlockPos::new(3, 150, 3), Item::EndCrystal, 1);
    click.held = Some(Item::EndCrystal);
    region.request(RegionRequest::Place(click)).await;
    let mut click = place(&player, base, Item::EndCrystal, 2);
    click.held = Some(Item::EndCrystal);
    region.request(RegionRequest::Place(click)).await;
    let used = expect(&mut events, |event| match event {
        PlayerEvent::ActionResult {
            sequence: 2,
            outcome,
        } => Some(outcome),
        _ => None,
    })
    .await;
    assert!(
        matches!(
            used,
            ActionOutcome::Used {
                effect: crate::world::messages::HandEffect::Consume(1),
                ..
            }
        ),
        "{used:?}"
    );
    let crystals = of_kind(&region, EntityType::EndCrystal).await;
    assert_eq!(crystals.len(), 1, "only the obsidian takes a crystal");
    assert_eq!(crystals[0].position, [8.5, 151.0, 8.5]);

    let mut again = place(&player, base, Item::EndCrystal, 3);
    again.held = Some(Item::EndCrystal);
    region.request(RegionRequest::Place(again)).await;
    world.ticks(2).await;
    assert_eq!(
        of_kind(&region, EntityType::EndCrystal).await.len(),
        1,
        "one crystal per block"
    );

    region
        .request(RegionRequest::AttackEntity {
            attacker: player.clone(),
            target: crystals[0].id,
            attack: attack(&player, [8.5, 151.0, 11.0], 1.0, None),
            sweep: None,
        })
        .await;
    world.ticks(3).await;
    let id = explode_id();
    expect(&mut events, |event| match event {
        PlayerEvent::ChunkPackets { packets, .. } => packets
            .iter()
            .any(|p| framed_packet_id(p) == id)
            .then_some(()),
        _ => None,
    })
    .await;
    assert!(of_kind(&region, EntityType::EndCrystal).await.is_empty());
    assert_eq!(
        block_at(&region, base).await.map(|s| s.block()),
        Some(Block::Obsidian),
        "obsidian withstands the blast"
    );
    world.shutdown().await;
}

fn end_player(
    handle: &PlayerHandle,
    end: crate::world::dimension::DimensionId,
    position: [f64; 3],
) -> Arc<PlayerIndex> {
    Arc::new(PlayerIndex::build([(
        end,
        IndexedPlayer {
            handle: handle.clone(),
            uuid: 77,
            name: Arc::from("runner"),
            position,
            looks: PlayerLooks::default(),
            time_since_rest: 0,
        },
    )]))
}

fn boss_event_id() -> i32 {
    tuff_vanilla_data::packet::play::Clientbound::BossEvent.id()
}

/// Waits (ticking) until `check` holds, for up to `ticks` ticks.
async fn tick_until<F, Fut>(world: &TestWorld, ticks: u64, mut check: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    for _ in 0..ticks {
        if check().await {
            return true;
        }
        world.ticks(1).await;
    }
    check().await
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_dragon_spawns_dies_and_leaves_the_exit_portal_and_egg() {
    use tuff_feature_builtin_vanilla_dimensions::dragon::fight::{FightData, gateway_position};

    let dir = TestDir::new();
    let (player, mut events) = PlayerHandle::new(1000);
    let world = TestWorld::start_with_fights(dir.path());
    let end = world.dimension(crate::storage::level::THE_END);
    world
        .players
        .send_replace(end_player(&player, end, [0.5, 70.0, 20.5]));
    let origin_region = world
        .region_in(crate::storage::level::THE_END, RegionPos::new(0, 0))
        .await;

    let region = origin_region.clone();
    let spawned = tick_until(&world, 400, || {
        let region = region.clone();
        async move { !of_kind(&region, EntityType::EnderDragon).await.is_empty() }
    })
    .await;
    assert!(spawned, "the fight spawns a dragon");
    let boss = boss_event_id();
    expect(&mut events, |event| match event {
        PlayerEvent::Packets(packets) => packets
            .iter()
            .any(|p| framed_packet_id(p) == boss)
            .then_some(()),
        _ => None,
    })
    .await;
    let pillar = block_at(&origin_region, BlockPos::new(0, 64, 0)).await;
    assert_eq!(
        pillar.map(|s| s.block()),
        Some(Block::Bedrock),
        "the inactive podium is built"
    );
    assert_eq!(
        block_at(&origin_region, BlockPos::new(1, 64, 1))
            .await
            .map(|s| s.block()),
        Some(Block::Air)
    );

    let dragon = of_kind(&origin_region, EntityType::EnderDragon)
        .await
        .remove(0);
    let at = dragon.position;
    let owner = world
        .region_in(
            crate::storage::level::THE_END,
            ChunkPos::of_position(at).region(),
        )
        .await;
    owner
        .request(RegionRequest::AttackEntity {
            attacker: player.clone(),
            target: dragon.id,
            attack: attack(&player, [at[0], at[1] - 1.0, at[2]], 2000.0, Some(2)),
            sweep: None,
        })
        .await;

    let regions = [
        world
            .region_in(crate::storage::level::THE_END, RegionPos::new(0, 0))
            .await,
        world
            .region_in(crate::storage::level::THE_END, RegionPos::new(-1, 0))
            .await,
        world
            .region_in(crate::storage::level::THE_END, RegionPos::new(0, -1))
            .await,
        world
            .region_in(crate::storage::level::THE_END, RegionPos::new(-1, -1))
            .await,
    ];
    let portal_region = origin_region.clone();
    let opened = tick_until(&world, 1200, || {
        let region = portal_region.clone();
        async move {
            block_at(&region, BlockPos::new(1, 64, 1))
                .await
                .map(|s| s.block())
                == Some(Block::EndPortal)
        }
    })
    .await;
    assert!(opened, "the exit portal opens once the dragon is dead");
    let egg_region = origin_region.clone();
    let egg = tick_until(&world, 200, || {
        let region = egg_region.clone();
        async move {
            block_at(&region, BlockPos::new(0, 68, 0))
                .await
                .map(|s| s.block())
                == Some(Block::DragonEgg)
        }
    })
    .await;
    assert!(egg, "the egg sits on the pillar");
    for region in &regions {
        assert!(of_kind(region, EntityType::EnderDragon).await.is_empty());
    }
    assert_eq!(
        block_at(&origin_region, BlockPos::new(0, 68, 0))
            .await
            .map(|s| s.block()),
        Some(Block::DragonEgg),
        "the egg sits on the pillar"
    );
    let mut experience = 0;
    for region in &regions {
        experience += of_kind(region, EntityType::ExperienceOrb)
            .await
            .iter()
            .map(|o| o.experience)
            .sum::<i32>();
    }
    assert_eq!(experience, 12000, "a first kill drops 12000 experience");

    let mut data = FightData::default();
    data.init_gateways(world.shared.seed);
    let gateway = gateway_position(*data.gateways.last().unwrap());
    let gateway_region = world
        .region_in(
            crate::storage::level::THE_END,
            ChunkPos::of_block(gateway).region(),
        )
        .await;
    assert_eq!(
        block_at(&gateway_region, gateway).await.map(|s| s.block()),
        Some(Block::EndGateway)
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hovering_dragon_heals_from_a_nearby_crystal() {
    let dir = TestDir::new();
    let (player, mut events) = PlayerHandle::new(1000);
    let world = TestWorld::start(dir.path());
    let region = world.region(RegionPos::new(0, 0)).await;
    subscribe(&region, &player, &mut events, &[ChunkPos::new(1, 1)]).await;
    summon(
        &region,
        "minecraft:end_crystal",
        Compound::new(),
        [20.5, 150.0, 20.5],
    )
    .await;
    summon(
        &region,
        "minecraft:ender_dragon",
        Compound::new()
            .with("Health", 100.0f32)
            .with("DragonPhase", 10),
        [24.0, 150.0, 24.0],
    )
    .await;
    let dragon = of_kind(&region, EntityType::EnderDragon).await.remove(0);
    let health = |events: &mut mpsc::UnboundedReceiver<PlayerEvent>| {
        let mut latest = None;
        while let Ok(event) = events.try_recv() {
            if let PlayerEvent::EntityPackets { entity, packets } = event
                && entity == dragon.id
            {
                for packet in packets.iter() {
                    if let Some(value) = health_in(packet) {
                        latest = Some(value);
                    }
                }
            }
        }
        latest
    };
    world.ticks(200).await;
    let healed = health(&mut events).expect("its health changed");
    assert!(healed > 100.0, "healed to {healed}");
    world.shutdown().await;
}

/// The health a Set Entity Data packet carries, if any.
fn health_in(frame: &bytes::Bytes) -> Option<f32> {
    if framed_packet_id(frame) != tuff_vanilla_data::packet::play::Clientbound::SetEntityData.id() {
        return None;
    }
    let mut r = framed_payload(frame);
    r.read_var_int().ok()?;
    r.read_var_int().ok()?;
    loop {
        let index = r.read_unsigned_byte().ok()?;
        if index == 0xFF {
            return None;
        }
        let kind = r.read_var_int().ok()?;
        match (index, kind) {
            (9, 3) => return r.read_float().ok(),
            (_, 1) => {
                r.read_var_int().ok()?;
            }
            (_, 3) => {
                r.read_float().ok()?;
            }
            _ => return None,
        }
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn generated_spikes_carry_live_end_crystals() {
    let dir = TestDir::new();
    let world = TestWorld::start_generated(
        Storage::default().open(dir.path()).unwrap(),
        tuff_feature_builtin_vanilla_worldgen::chunk_generator::for_settings,
    );
    let spike =
        tuff_feature_builtin_vanilla_worldgen::feature::end::spikes_for_seed(world.shared.seed)[0];
    let top = spike.crystal();
    let region = world
        .region_in(
            crate::storage::level::THE_END,
            ChunkPos::of_block(top).region(),
        )
        .await;
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::Blocks {
            op: BlockOp::Get {
                positions: vec![top, BlockPos::new(top.x, top.y - 1, top.z)],
                load: true,
            },
            reply,
        })
        .await;
    let BlockOpResult::States(states) = tokio::time::timeout(TIMEOUT, answer)
        .await
        .expect("the spike generates")
        .unwrap()
    else {
        panic!("a block read answers states");
    };
    assert_eq!(states[0].map(|s| s.block()), Some(Block::Fire));
    assert_eq!(states[1].map(|s| s.block()), Some(Block::Bedrock));
    let crystals = of_kind(&region, EntityType::EndCrystal).await;
    let expected = [
        f64::from(top.x) + 0.5,
        f64::from(top.y),
        f64::from(top.z) + 0.5,
    ];
    assert!(
        crystals.iter().any(|c| c.position == expected),
        "{crystals:?}"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_player_arrow_hurts_the_dragon_through_a_part() {
    let dir = TestDir::new();
    let (player, mut events) = PlayerHandle::new(1000);
    let world = TestWorld::start(dir.path());
    let region = world.region(RegionPos::new(0, 0)).await;
    subscribe(
        &region,
        &player,
        &mut events,
        &[ChunkPos::new(1, 1), ChunkPos::new(1, 2)],
    )
    .await;
    region
        .request(update(&player, 1, [24.5, 150.0, 40.5]))
        .await;
    summon(
        &region,
        "minecraft:ender_dragon",
        Compound::new().with("DragonPhase", 10),
        [24.5, 150.0, 24.5],
    )
    .await;
    world.ticks(3).await;
    let dragon = of_kind(&region, EntityType::EnderDragon).await.remove(0);
    summon(
        &region,
        "minecraft:arrow",
        Compound::new()
            .with("Owner", Tag::uuid(u128::from(player.id as u32)))
            .with(
                "Motion",
                Tag::List(vec![Tag::Double(0.0), Tag::Double(0.0), Tag::Double(-2.5)]),
            ),
        [24.5, 151.5, 34.5],
    )
    .await;
    world.ticks(10).await;
    let mut lowest: Option<f32> = None;
    while let Ok(event) = events.try_recv() {
        if let PlayerEvent::EntityPackets { entity, packets } = event
            && entity == dragon.id
        {
            for packet in packets.iter() {
                if let Some(health) = health_in(packet) {
                    lowest = Some(lowest.map_or(health, |l: f32| l.min(health)));
                }
            }
        }
    }
    let health = lowest.expect("the arrow hurt the dragon");
    assert!(health < 200.0, "health {health}");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn releasing_the_arena_keeps_a_players_force_loaded_chunk() {
    use crate::world::messages::ChunkTicket;

    let dir = TestDir::new();
    let (player, _events) = PlayerHandle::new(1000);
    let world = TestWorld::start_with_fights(dir.path());
    let end = world.dimension(crate::storage::level::THE_END);
    world
        .players
        .send_replace(end_player(&player, end, [0.5, 70.0, 20.5]));
    let region = world
        .region_in(crate::storage::level::THE_END, RegionPos::new(0, 0))
        .await;
    let (kept, dropped) = (BlockPos::new(40, 10, 40), BlockPos::new(88, 10, 88));
    let arena = region.clone();
    let loaded = tick_until(&world, 400, || {
        let region = arena.clone();
        async move { block_at(&region, dropped).await.is_some() }
    })
    .await;
    assert!(loaded, "the fight loads its arena");

    let (reply, changed) = oneshot::channel();
    world
        .manager
        .send(ManagerMessage::ForceLoad {
            dimension: end,
            chunks: vec![ChunkPos::of_block(kept)],
            ticket: ChunkTicket::Forced,
            on: true,
            reply,
        })
        .await
        .unwrap();
    assert_eq!(changed.await.unwrap(), [true]);

    world.players.send_replace(Arc::new(PlayerIndex::default()));
    let released = region.clone();
    let unloaded = tick_until(&world, 600, || {
        let region = released.clone();
        async move { block_at(&region, dropped).await.is_none() }
    })
    .await;
    assert!(unloaded, "the arena unloads once nobody is near");
    assert!(
        block_at(&region, kept).await.is_some(),
        "the player's force-load outlives the fight's"
    );
    let (reply, forced) = oneshot::channel();
    world
        .manager
        .send(ManagerMessage::ForcedChunks {
            dimension: end,
            reply,
        })
        .await
        .unwrap();
    assert_eq!(forced.await.unwrap(), [ChunkPos::of_block(kept)]);
    world.shutdown().await;
}

async fn build_gateway(region: &RegionHandle, at: BlockPos) {
    let (reply, done) = oneshot::channel();
    region
        .request(RegionRequest::Blocks {
            op: BlockOp::Set {
                changes: tuff_feature_builtin_vanilla_dimensions::world::gateway::gateway_blocks(
                    at,
                ),
                destroy: false,
                keep: false,
                strict: false,
                quiet: false,
                data: None,
            },
            reply,
        })
        .await;
    done.await.unwrap();
}

/// Where the game builds the exit of a gateway at `entry` in an end whose
/// outer islands are void, and where travellers land beside it.
fn void_end_exit(entry: BlockPos) -> (BlockPos, [f64; 3]) {
    use tuff_feature_builtin_vanilla_dimensions::world::gateway::tallest;
    use tuff_item::random::LegacyRandom;

    let length = f64::from(entry.x * entry.x + entry.z * entry.z).sqrt();
    let reach = 1024.0 + 16.0 * 16.0;
    let (x, z) = (
        f64::from(entry.x) / length * reach,
        f64::from(entry.z) / length * reach,
    );
    let island = BlockPos::new((x + 0.5).floor() as i32, 75, (z + 0.5).floor() as i32);
    let mut tops = std::collections::HashMap::new();
    for pos in tuff_feature_builtin_vanilla_worldgen::feature::end::island_blocks(
        &mut LegacyRandom::new(island.pack()),
        island,
    ) {
        let top = tops.entry((pos.x, pos.z)).or_insert(pos.y);
        *top = (*top).max(pos.y);
    }
    let base = tallest(&tops, island, 16, true);
    let exit = BlockPos::new(base.x, base.y + 10, base.z);
    let land = tallest(&tops, BlockPos::new(exit.x, exit.y + 2, exit.z), 5, false);
    (
        exit,
        [
            f64::from(land.x) + 0.5,
            f64::from(land.y + 1),
            f64::from(land.z) + 0.5,
        ],
    )
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_item_in_a_gateway_raises_an_exit_island_and_comes_out_there() {
    use crate::storage::level::THE_END;

    let dir = TestDir::new();
    let (player, mut events) = PlayerHandle::new(1000);
    let world = TestWorld::start(dir.path());
    let entry = BlockPos::new(96, 75, 0);
    let region = world.region_in(THE_END, RegionPos::new(0, 0)).await;
    subscribe(&region, &player, &mut events, &[ChunkPos::of_block(entry)]).await;
    build_gateway(&region, entry).await;
    region
        .request(RegionRequest::SpawnItem {
            stack: ItemStack::new(Item::Stone, 1),
            position: [96.5, 75.1, 0.5],
            velocity: [0.0; 3],
            pickup_delay: 10_000,
        })
        .await;
    world.ticks(25).await;
    let block_event = tuff_vanilla_data::packet::play::Clientbound::BlockEvent.id();
    expect(&mut events, |event| match event {
        PlayerEvent::ChunkPackets { packets, .. } => packets
            .iter()
            .any(|p| framed_packet_id(p) == block_event)
            .then_some(()),
        _ => None,
    })
    .await;

    let (exit, arrival) = void_end_exit(entry);
    let there = world
        .region_in(THE_END, ChunkPos::of_position(arrival).region())
        .await;
    let landed = there.clone();
    let arrived = tick_until(&world, 400, || {
        let region = landed.clone();
        async move { !of_kind(&region, EntityType::Item).await.is_empty() }
    })
    .await;
    assert!(arrived, "the item comes out at the exit");
    let item = of_kind(&there, EntityType::Item).await.remove(0);
    assert!(
        (item.position[0] - arrival[0]).abs() < 1e-6
            && (item.position[2] - arrival[2]).abs() < 1e-6
            && (item.position[1] - arrival[1]).abs() < 0.1,
        "{:?} vs {arrival:?}",
        item.position
    );
    assert!(of_kind(&region, EntityType::Item).await.is_empty());
    let exit_region = world
        .region_in(THE_END, ChunkPos::of_block(exit).region())
        .await;
    assert_eq!(
        block_at(&exit_region, exit).await.map(|s| s.block()),
        Some(Block::EndGateway),
        "the exit gateway stands ten above the island"
    );
    assert_eq!(
        block_at(&exit_region, BlockPos::new(exit.x, exit.y + 2, exit.z))
            .await
            .map(|s| s.block()),
        Some(Block::Bedrock)
    );

    let destination = tuff_feature_builtin_vanilla_dimensions::world::gateway::destination(
        &world.shared,
        world.dimension(THE_END),
        exit,
    )
    .await;
    let back = destination.expect("the exit leads back");
    assert!(
        (back[0] - 96.5).abs() < 6.0 && (back[2] - 0.5).abs() < 6.0,
        "the exit gateway leads back to the entry: {back:?}"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn gateways_let_players_through_once_per_cooldown_and_exact_exits_are_kept() {
    use crate::storage::level::THE_END;
    use tuff_feature_builtin_vanilla_dimensions::world::gateway::GatewayRequest;
    use tuff_feature_builtin_vanilla_dimensions::world::portal::{PortalContact, PortalKind};

    let dir = TestDir::new();
    let (player, mut events) = PlayerHandle::new(1000);
    let world = TestWorld::start(dir.path());
    let entry = BlockPos::new(-60, 90, 40);
    let region = world.region_in(THE_END, RegionPos::new(-1, 0)).await;
    build_gateway(&region, entry).await;
    let inside = [-59.5, 89.5, 40.5];
    let contact = |seq| {
        let player = player.clone();
        let region = region.clone();
        async move {
            region.request(update(&player, seq, inside)).await;
        }
    };
    contact(1).await;
    contact(2).await;
    let mut seen = Vec::new();
    while seen.len() < 2 {
        let portal = expect(&mut events, |event| match event {
            PlayerEvent::Surroundings { portal, .. } => Some(portal.and_then(PortalContact::of)),
            _ => None,
        })
        .await;
        seen.push(portal);
    }
    assert_eq!(
        seen,
        [
            Some(PortalContact {
                kind: PortalKind::Gateway,
                pos: entry
            }),
            None
        ],
        "the second touch finds the gateway cooling down"
    );
    world.ticks(41).await;
    contact(3).await;
    let again = expect(&mut events, |event| match event {
        PlayerEvent::Surroundings { seq: 3, portal, .. } => Some(portal),
        _ => None,
    })
    .await;
    assert!(again.is_some(), "the cooldown is over");

    let exact = BlockPos::new(-300, 120, 900);
    region
        .request(
            tuff_feature_builtin_vanilla_dimensions::world::gateway::when_loaded(
                GatewayRequest::SetExit {
                    pos: entry,
                    exit: Some(exact),
                    exact: true,
                },
            ),
        )
        .await;
    let destination = tuff_feature_builtin_vanilla_dimensions::world::gateway::destination(
        &world.shared,
        world.dimension(THE_END),
        entry,
    )
    .await;
    assert_eq!(destination, Some([-299.5, 120.0, 900.5]));

    let overworld = world.region(RegionPos::new(0, 0)).await;
    let elsewhere = BlockPos::new(10, 100, 10);
    build_gateway(&overworld, elsewhere).await;
    let destination = tuff_feature_builtin_vanilla_dimensions::world::gateway::destination(
        &world.shared,
        world.dimension(OVERWORLD),
        elsewhere,
    )
    .await;
    assert_eq!(destination, None, "outside the end no exit is built");
    world.shutdown().await;
}

/// A world whose dragon is dead, with its end open and a player near the
/// island (whose events are subscribed to the origin chunk).
async fn after_the_dragon(
    dir: &TestDir,
) -> (
    TestWorld,
    PlayerHandle,
    mpsc::UnboundedReceiver<PlayerEvent>,
    RegionHandle,
) {
    use tuff_feature_builtin_vanilla_dimensions::dragon::fight::FightData;
    use tuff_storage::document::Document;

    {
        let backend = Storage::default().open(dir.path()).unwrap();
        let data = FightData {
            needs_state_scanning: false,
            dragon_killed: true,
            previously_killed: true,
            ..Default::default()
        };
        backend
            .documents()
            .write(
                Document::DimensionData {
                    dimension: crate::storage::level::THE_END,
                    id: "minecraft:ender_dragon_fight",
                },
                tuff_storage::document::saved(data.save()),
            )
            .unwrap();
    }
    let (player, mut events) = PlayerHandle::new(1000);
    let world = TestWorld::start_with_fights(dir.path());
    let end = world.dimension(crate::storage::level::THE_END);
    world
        .players
        .send_replace(end_player(&player, end, [0.5, 70.0, 20.5]));
    let origin = world
        .region_in(crate::storage::level::THE_END, RegionPos::new(0, 0))
        .await;
    subscribe(&origin, &player, &mut events, &[ChunkPos::new(0, 0)]).await;
    (world, player, events, origin)
}

/// Asks the fight to respawn until the exit portal is open, and answers
/// the portal's centre.
async fn open_exit_portal(world: &TestWorld, origin: &RegionHandle) -> BlockPos {
    use tuff_feature_builtin_vanilla_dimensions::dragon::fight::{FightMessage, FightRequest};

    let end = world.dimension(crate::storage::level::THE_END);
    world
        .shared
        .fight(end)
        .unwrap()
        .send(FightMessage::TryRespawn);
    for _ in 0..400 {
        let (reply, answer) = oneshot::channel();
        origin
            .request(
                FightRequest::FindBlocks {
                    block: Block::EndPortal,
                    min: BlockPos::new(-8, i32::MIN, -8),
                    max: BlockPos::new(8, i32::MAX, 8),
                    reply,
                }
                .request(),
            )
            .await;
        if let Some(y) = answer.await.unwrap().iter().map(|p| p.y).min() {
            return BlockPos::new(0, y, 0);
        }
        world.ticks(1).await;
    }
    panic!("a respawn attempt without a portal builds an open one");
}

/// Puts an end crystal on each side of the exit portal's rim.
async fn place_respawn_crystals(world: &TestWorld, portal: BlockPos) {
    for (dx, dz) in [(3, 0), (-3, 0), (0, 3), (0, -3)] {
        let at = BlockPos::new(portal.x + dx, portal.y + 1, portal.z + dz);
        let region = world
            .region_in(
                crate::storage::level::THE_END,
                ChunkPos::of_block(at).region(),
            )
            .await;
        summon(
            &region,
            "minecraft:end_crystal",
            Compound::new().with("ShowBottom", false),
            [
                f64::from(at.x) + 0.5,
                f64::from(at.y),
                f64::from(at.z) + 0.5,
            ],
        )
        .await;
    }
}

async fn arena_regions(world: &TestWorld) -> Vec<RegionHandle> {
    let low = ChunkPos::new(-8, -8).region();
    let high = ChunkPos::new(8, 8).region();
    let mut regions = Vec::new();
    for x in low.x..=high.x {
        for z in low.z..=high.z {
            regions.push(
                world
                    .region_in(crate::storage::level::THE_END, RegionPos::new(x, z))
                    .await,
            );
        }
    }
    regions
}

async fn in_arena(
    world: &TestWorld,
    kind: EntityType,
) -> Vec<crate::world::messages::EntitySnapshot> {
    let mut found = Vec::new();
    for region in arena_regions(world).await {
        found.extend(of_kind(&region, kind).await);
    }
    found
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn four_crystals_on_the_exit_portal_summon_a_new_dragon() {
    use tuff_feature_builtin_vanilla_dimensions::dragon::fight::{FightData, FightMessage};
    use tuff_storage::document::Document;

    let dir = TestDir::new();
    let (world, _player, mut events, origin) = after_the_dragon(&dir).await;
    let portal = open_exit_portal(&world, &origin).await;
    place_respawn_crystals(&world, portal).await;
    let end = world.dimension(crate::storage::level::THE_END);
    world
        .shared
        .fight(end)
        .unwrap()
        .send(FightMessage::TryRespawn);
    let inner = BlockPos::new(portal.x + 1, portal.y, portal.z + 1);
    let closed = origin.clone();
    let started = tick_until(&world, 100, || {
        let region = closed.clone();
        async move { block_at(&region, inner).await.map(|s| s.block()) == Some(Block::Air) }
    })
    .await;
    assert!(started, "summoning closes the exit portal");
    let roar = crate::protocol::play::level_event::ANIMATION_DRAGON_SUMMON_ROAR;
    let level_event = tuff_vanilla_data::packet::play::Clientbound::LevelEvent.id();
    world.ticks(5).await;
    expect(&mut events, |event| match event {
        PlayerEvent::ChunkPackets { packets, .. } => packets
            .iter()
            .any(|p| {
                framed_packet_id(p) == level_event && {
                    let mut body = framed_payload(p);
                    body.read_var_int().unwrap();
                    body.read_int().unwrap() == roar
                }
            })
            .then_some(()),
        _ => None,
    })
    .await;

    let dragon_arrived = tick_until(&world, 900, || {
        let world = &world;
        async move { !in_arena(world, EntityType::EnderDragon).await.is_empty() }
    })
    .await;
    assert!(dragon_arrived, "the summoning ends with a new dragon");
    world.ticks(2).await;
    let crystals = in_arena(&world, EntityType::EndCrystal).await;
    let spikes =
        tuff_feature_builtin_vanilla_worldgen::feature::end::spikes_for_seed(world.shared.seed);
    assert_eq!(crystals.len(), spikes.len(), "{crystals:?}");
    for spike in &spikes {
        let top = spike.crystal();
        assert!(
            crystals.iter().any(|c| c.position
                == [
                    f64::from(top.x) + 0.5,
                    f64::from(top.y),
                    f64::from(top.z) + 0.5
                ]),
            "the spike at {top:?} has its crystal back"
        );
        let column = world
            .region_in(
                crate::storage::level::THE_END,
                ChunkPos::of_block(top).region(),
            )
            .await;
        assert_eq!(
            block_at(&column, BlockPos::new(top.x, top.y - 2, top.z))
                .await
                .map(|s| s.block()),
            Some(Block::Obsidian),
            "the spike at {top:?} is rebuilt"
        );
    }
    let saved = world
        .shared
        .backend
        .documents()
        .read(Document::DimensionData {
            dimension: crate::storage::level::THE_END,
            id: "minecraft:ender_dragon_fight",
        })
        .unwrap();
    let data = FightData::load(tuff_storage::document::inner(saved));
    assert!(!data.dragon_killed && data.respawn_stage.is_none());
    assert!(data.dragon_uuid.is_some());
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn losing_a_summoning_crystal_reopens_the_portal() {
    use tuff_feature_builtin_vanilla_dimensions::dragon::fight::FightMessage;

    let dir = TestDir::new();
    let (world, player, _events, origin) = after_the_dragon(&dir).await;
    let portal = open_exit_portal(&world, &origin).await;
    place_respawn_crystals(&world, portal).await;
    let end = world.dimension(crate::storage::level::THE_END);
    world
        .shared
        .fight(end)
        .unwrap()
        .send(FightMessage::TryRespawn);
    let inner = BlockPos::new(portal.x + 1, portal.y, portal.z + 1);
    let closed = origin.clone();
    let started = tick_until(&world, 100, || {
        let region = closed.clone();
        async move { block_at(&region, inner).await.map(|s| s.block()) == Some(Block::Air) }
    })
    .await;
    assert!(started, "summoning closes the exit portal");
    let east = of_kind(&origin, EntityType::EndCrystal)
        .await
        .into_iter()
        .find(|c| c.position[0] > f64::from(portal.x + 2))
        .expect("the east crystal");
    origin
        .request(RegionRequest::AttackEntity {
            attacker: player.clone(),
            target: east.id,
            attack: attack(&player, east.position, 1.0, None),
            sweep: None,
        })
        .await;
    let reopened = origin.clone();
    let open = tick_until(&world, 100, || {
        let region = reopened.clone();
        async move { block_at(&region, inner).await.map(|s| s.block()) == Some(Block::EndPortal) }
    })
    .await;
    assert!(open, "the portal opens again");
    world.ticks(150).await;
    assert!(in_arena(&world, EntityType::EnderDragon).await.is_empty());
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_glass_bottle_scoops_the_dragons_breath_near_the_player() {
    use crate::world::entity::{Culprit, EntityBase};
    use rand::SeedableRng;
    use tuff_vanilla_common::effect_cloud::AreaEffectCloud;

    let dir = TestDir::new();
    let (player, _events) = PlayerHandle::new(1000);
    let world = TestWorld::start_with_fights(dir.path());
    let end = world.dimension(crate::storage::level::THE_END);
    world
        .players
        .send_replace(end_player(&player, end, [0.5, 70.0, 20.5]));
    let region = world
        .region_in(crate::storage::level::THE_END, RegionPos::new(0, 0))
        .await;
    let dragons = region.clone();
    let alive = tick_until(&world, 400, || {
        let region = dragons.clone();
        async move { !of_kind(&region, EntityType::EnderDragon).await.is_empty() }
    })
    .await;
    assert!(alive, "the fight spawns a dragon");
    let uuid = world
        .shared
        .fight(end)
        .and_then(|fight| fight.view().dragon)
        .expect("the live dragon's uuid");
    let dragon = EntityBase::new(EntityType::EnderDragon, 1, uuid, [0.0; 3], [0.0; 3]);
    let mut rng = rand::rngs::StdRng::seed_from_u64(3);
    let cloud = AreaEffectCloud::flame(
        world.shared.next_entity_id(),
        [8.5, 100.0, 8.5],
        Culprit::entity(&dragon),
        &mut rng,
    );
    region
        .request(RegionRequest::SpawnEntity(Box::new(cloud)))
        .await;
    let loading = region.clone();
    let spawned = tick_until(&world, 100, || {
        let region = loading.clone();
        async move {
            !of_kind(&region, EntityType::AreaEffectCloud)
                .await
                .is_empty()
        }
    })
    .await;
    assert!(spawned);
    let bottle = |bounds: [f64; 6]| {
        let region = region.clone();
        async move {
            let (reply, answer) = oneshot::channel();
            region
                .request(
                    tuff_feature_builtin_vanilla_dimensions::dragon::bottle::bottle(bounds, reply),
                )
                .await;
            answer.await.unwrap()
        }
    };
    let near = [11.5, 97.0, 6.7, 16.1, 102.8, 11.3];
    let far = [30.0, 98.0, 30.0, 32.6, 103.8, 32.6];
    assert!(bottle(near).await, "the flame reaches within two blocks");
    assert!(!bottle(far).await, "nothing to scoop out here");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_item_touching_a_gateway_in_the_neighbour_region_goes_through() {
    use crate::storage::level::THE_END;

    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let entry = BlockPos::new(-1, 75, 0);
    let gateway_region = world.region_in(THE_END, RegionPos::new(-1, 0)).await;
    let region = world.region_in(THE_END, RegionPos::new(0, 0)).await;
    build_gateway(&gateway_region, entry).await;
    world.ticks(25).await;
    region
        .request(RegionRequest::SpawnItem {
            stack: ItemStack::new(Item::Stone, 1),
            position: [0.1, 75.1, 0.5],
            velocity: [0.0; 3],
            pickup_delay: 10_000,
        })
        .await;

    let (_, arrival) = void_end_exit(entry);
    let there = world
        .region_in(THE_END, ChunkPos::of_position(arrival).region())
        .await;
    let landed = there.clone();
    let arrived = tick_until(&world, 400, || {
        let region = landed.clone();
        async move { !of_kind(&region, EntityType::Item).await.is_empty() }
    })
    .await;
    assert!(arrived, "the item crossed the region edge into the gateway");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn throwing_an_eye_of_ender_consumes_it_and_counts_its_use() {
    use crate::world::inventory::Inventory;
    use crate::world::stats::Stat;
    use tuff_feature_builtin_vanilla_items::world::session::item_use::ItemUseExt;
    use tuff_vanilla_data::registry::StatType;

    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, _outbound) = session_with_outbound(&world, 1000);
    player.stand_at([4.5, 151.0, 6.5], true);
    player.inventory_mut().set_slot(
        Inventory::hotbar_slot(0),
        Some(ItemStack::new(Item::EnderEye, 1)),
    );
    player.throw_eye(false, BlockPos::new(100, 64, 100)).await;
    assert!(
        player.inventory().hand(false).is_none(),
        "the thrown eye is consumed"
    );
    assert_eq!(player.stat(Stat::item(StatType::Used, Item::EnderEye)), 1);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_glass_bottle_scoops_water_sources_along_the_view_ray_only() {
    use tuff_core::world::game_event::Source;

    let dir = TestDir::new();
    let (player, mut events) = PlayerHandle::new(100);
    let chunk = ChunkPos::new(0, 0);
    let world = TestWorld::start(dir.path());
    let region = world.region(chunk.region()).await;
    subscribe(&region, &player, &mut events, &[chunk]).await;
    let floor = 150;
    let mut blocks = Vec::new();
    for x in 4..=12 {
        blocks.push((BlockPos::new(x, floor, 8), Block::Stone.default_state()));
    }
    blocks.push((BlockPos::new(8, floor, 8), Block::Water.default_state()));
    place_blocks(&world, &region, blocks).await;
    let scoop = |x: f64| {
        let region = region.clone();
        async move {
            let (reply, answer) = oneshot::channel();
            region
                .request(
                    tuff_feature_builtin_vanilla_dimensions::dragon::bottle::water_scoop(
                        [x, floor as f64 + 1.6, 8.5],
                        [0.0, -1.0, 0.0],
                        4.5,
                        Source::entity(1),
                        reply,
                    ),
                )
                .await;
            answer.await.unwrap()
        }
    };
    assert!(scoop(8.5).await, "looking down at a water source");
    assert!(!scoop(5.5).await, "looking down at stone");
    world.shutdown().await;
}
