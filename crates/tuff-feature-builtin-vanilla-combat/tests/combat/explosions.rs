//! Primed TNT, explosions, beds and respawn anchors driven through real
//! regions.

use super::*;
use crate::world::messages::{AnchorUse, BedUse, HandEffect};
use tuff_vanilla_data::BlockState;
use tuff_vanilla_data::block::{Facing, Part};
use tuff_vanilla_data::registry::EntityType;

const SKY: i32 = 150;

async fn outcome(events: &mut mpsc::UnboundedReceiver<PlayerEvent>, seq: i32) -> ActionOutcome {
    expect(events, |event| match event {
        PlayerEvent::ActionResult { sequence, outcome } if sequence == seq => Some(outcome),
        _ => None,
    })
    .await
}

fn ignite(player: &PlayerHandle, target: BlockPos, sequence: i32) -> Place {
    let mut click = place(player, target, Item::FlintAndSteel, sequence);
    click.item = None;
    click
}

/// A stone slab of the sky at `y`, from `min` to `max` in x and z.
fn platform(min: (i32, i32), max: (i32, i32), y: i32) -> Vec<(BlockPos, BlockState)> {
    let mut blocks = Vec::new();
    for x in min.0..=max.0 {
        for z in min.1..=max.1 {
            blocks.push((BlockPos::new(x, y, z), Block::Stone.default_state()));
        }
    }
    blocks
}

async fn state(region: &RegionHandle, pos: BlockPos) -> BlockState {
    block_at(region, pos).await.unwrap()
}

fn explode_id() -> i32 {
    tuff_vanilla_data::packet::play::Clientbound::Explode.id()
}

/// Waits for the next Explosion packet a viewer gets.
async fn explosion(events: &mut mpsc::UnboundedReceiver<PlayerEvent>) {
    let id = explode_id();
    expect(events, |event| match event {
        PlayerEvent::ChunkPackets { packets, .. } => packets
            .iter()
            .any(|p| framed_packet_id(p) == id)
            .then_some(()),
        _ => None,
    })
    .await;
}

fn tnt_track(event: PlayerEvent) -> Option<i32> {
    match event {
        PlayerEvent::Track(track)
            if track.player.is_none()
                && spawned_entity_kind(&track) == EntityType::Tnt.id() as i32 =>
        {
            Some(track.entity)
        }
        _ => None,
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lit_tnt_falls_counts_down_and_blows_up_stone_but_not_obsidian() {
    let dir = TestDir::new();
    let (player, mut events) = PlayerHandle::new(100);
    let chunk = ChunkPos::new(0, 0);
    let world = TestWorld::start(dir.path());
    let region = world.region(chunk.region()).await;
    subscribe(&region, &player, &mut events, &[chunk]).await;
    present(&region, &player, [8.0, f64::from(SKY) + 1.0, 8.0]).await;
    let tnt = BlockPos::new(8, SKY + 1, 8);
    let obsidian = BlockPos::new(10, SKY + 1, 8);
    let mut blocks = platform((5, 5), (11, 11), SKY);
    blocks.push((tnt, Block::Tnt.default_state()));
    blocks.push((obsidian, Block::Obsidian.default_state()));
    place_blocks(&world, &region, blocks).await;

    region
        .request(RegionRequest::Place(ignite(&player, tnt, 1)))
        .await;
    let entity = expect(&mut events, tnt_track).await;
    assert!(matches!(
        outcome(&mut events, 1).await,
        ActionOutcome::Used {
            item: Item::FlintAndSteel,
            effect: HandEffect::Damage(1),
            ..
        }
    ));
    assert_eq!(state(&region, tnt).await.block(), Block::Air);
    world.ticks(60).await;
    assert_eq!(
        state(&region, BlockPos::new(8, SKY, 8)).await.block(),
        Block::Stone,
        "the fuse has not run out yet"
    );
    world.ticks(30).await;
    expect(&mut events, |event| match event {
        PlayerEvent::Untrack { entity: id, .. } if id == entity => Some(()),
        _ => None,
    })
    .await;
    explosion(&mut events).await;
    assert_eq!(
        state(&region, BlockPos::new(8, SKY, 8)).await.block(),
        Block::Air
    );
    assert_eq!(state(&region, obsidian).await.block(), Block::Obsidian);
    world.ticks(2).await;
    let (reply, answer) = oneshot::channel();
    region.request(RegionRequest::Entities { reply }).await;
    let drops = answer.await.unwrap();
    assert!(
        drops.iter().any(|e| e.name.contains("cobblestone")),
        "blown-up stone drops cobblestone: {:?}",
        drops.iter().map(|e| &e.name).collect::<Vec<_>>()
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn burning_tnt_movement_is_resent_only_on_its_update_interval() {
    let dir = TestDir::new();
    let (player, mut events) = PlayerHandle::new(100);
    let chunk = ChunkPos::new(0, 0);
    let world = TestWorld::start(dir.path());
    let region = world.region(chunk.region()).await;
    subscribe(&region, &player, &mut events, &[chunk]).await;
    let tnt = BlockPos::new(8, SKY + 1, 8);
    let mut blocks = platform((5, 5), (11, 11), SKY);
    blocks.push((tnt, Block::Tnt.default_state()));
    place_blocks(&world, &region, blocks).await;

    region
        .request(RegionRequest::Place(ignite(&player, tnt, 1)))
        .await;
    let entity = expect(&mut events, tnt_track).await;
    world.ticks(40).await;
    let mut updates = 0;
    while let Ok(event) = events.try_recv() {
        if matches!(event, PlayerEvent::Track(track) if track.entity == entity) {
            updates += 1;
        }
    }
    assert!(
        (1..=4).contains(&updates),
        "40 ticks of a ten-tick interval, got {updates} updates"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn explosions_prime_nearby_tnt_with_a_short_fuse() {
    let dir = TestDir::new();
    let (player, mut events) = PlayerHandle::new(100);
    let chunk = ChunkPos::new(0, 0);
    let world = TestWorld::start(dir.path());
    let region = world.region(chunk.region()).await;
    subscribe(&region, &player, &mut events, &[chunk]).await;
    present(&region, &player, [8.0, f64::from(SKY) + 1.0, 8.0]).await;
    let first = BlockPos::new(8, SKY + 1, 8);
    let second = BlockPos::new(10, SKY + 1, 8);
    let mut blocks = platform((4, 4), (12, 12), SKY);
    blocks.push((first, Block::Tnt.default_state()));
    blocks.push((second, Block::Tnt.default_state()));
    place_blocks(&world, &region, blocks).await;

    region
        .request(RegionRequest::Place(ignite(&player, first, 1)))
        .await;
    let lit = expect(&mut events, tnt_track).await;
    world.ticks(85).await;
    explosion(&mut events).await;
    let chained = expect(&mut events, |event| {
        tnt_track(event).filter(|id| *id != lit)
    })
    .await;
    assert_ne!(chained, lit);
    assert_eq!(state(&region, second).await.block(), Block::Air);
    world.ticks(35).await;
    explosion(&mut events).await;
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn explosions_reach_across_region_borders() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let west = world.region(ChunkPos::new(31, 0).region()).await;
    let east = world.region(ChunkPos::new(32, 0).region()).await;
    let (player, mut events) = PlayerHandle::new(100);
    let (west_chunk, east_chunk) = (ChunkPos::new(31, 0), ChunkPos::new(32, 0));
    subscribe(&west, &player, &mut events, &[west_chunk]).await;
    subscribe(&east, &player, &mut events, &[east_chunk]).await;
    world.ticks(3).await;
    let tnt = BlockPos::new(511, SKY + 1, 8);
    place_blocks(&world, &west, platform((508, 6), (511, 10), SKY)).await;
    place_blocks(&world, &east, platform((512, 6), (515, 10), SKY)).await;
    place_blocks(&world, &west, [(tnt, Block::Tnt.default_state())]).await;
    world.ticks(3).await;
    assert_eq!(
        state(&east, BlockPos::new(512, SKY, 8)).await.block(),
        Block::Stone
    );

    west.request(RegionRequest::Place(ignite(&player, tnt, 1)))
        .await;
    outcome(&mut events, 1).await;
    world.ticks(90).await;
    world.ticks(5).await;
    assert_eq!(
        state(&west, BlockPos::new(511, SKY, 8)).await.block(),
        Block::Air
    );
    assert_eq!(
        state(&east, BlockPos::new(512, SKY, 8)).await.block(),
        Block::Air,
        "the neighbour region breaks its own blocks"
    );
    world.shutdown().await;
}

fn bed(pos: BlockPos) -> Vec<(BlockPos, BlockState)> {
    let foot = Block::RedBed
        .default_state()
        .with_facing(Facing::South)
        .unwrap();
    let head = foot.with_part(Part::Head).unwrap();
    vec![(pos, foot), (BlockPos::new(pos.x, pos.y, pos.z + 1), head)]
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn beds_explode_in_the_nether_and_set_spawn_by_day_in_the_overworld() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (player, mut events) = PlayerHandle::new(100);
    let chunk = ChunkPos::new(0, 0);

    let nether = world.region_in(THE_NETHER, chunk.region()).await;
    subscribe(&nether, &player, &mut events, &[chunk]).await;
    present(&nether, &player, [8.0, 80.0, 8.0]).await;
    let foot = BlockPos::new(8, 60, 8);
    let mut blocks = platform((4, 4), (12, 12), 59);
    blocks.extend(bed(foot));
    place_blocks(&world, &nether, blocks).await;
    let mut click = place(&player, foot, Item::Stick, 1);
    click.item = None;
    click.held = None;
    click.position = [8.5, 61.0, 6.5];
    nether.request(RegionRequest::Place(click)).await;
    explosion(&mut events).await;
    assert!(matches!(
        outcome(&mut events, 1).await,
        ActionOutcome::Interacted { .. }
    ));
    assert_eq!(state(&nether, foot).await.block(), Block::Air);
    assert_eq!(
        state(&nether, BlockPos::new(8, 59, 9)).await.block(),
        Block::Air
    );

    let overworld = world.region(chunk.region()).await;
    subscribe(&overworld, &player, &mut events, &[chunk]).await;
    let foot = BlockPos::new(8, SKY + 1, 8);
    let mut blocks = platform((4, 4), (12, 12), SKY);
    blocks.extend(bed(foot));
    place_blocks(&world, &overworld, blocks).await;
    let mut click = place(&player, foot, Item::Stick, 2);
    click.item = None;
    click.held = None;
    click.position = [8.5, SKY as f64 + 1.0, 6.5];
    overworld.request(RegionRequest::Place(click)).await;
    let used: BedUse = expect(&mut events, |event| match event {
        PlayerEvent::Run(task) => task.data::<BedUse>().cloned(),
        _ => None,
    })
    .await;
    assert_eq!(
        used.pos,
        BlockPos::new(8, SKY + 1, 9),
        "the head of the bed"
    );
    assert!(used.set_spawn);
    assert!(
        matches!(&used.sleep, Err(Some(message))
            if format!("{message:?}").contains("block.minecraft.bed.no_sleep")),
        "no sleeping by day: {:?}",
        used.sleep
    );
    if let Err(Some(message)) = &used.sleep {
        assert!(
            message.events().is_none(),
            "vanilla's bed.no_sleep message carries no click or hover event: {message:?}"
        );
    }

    let (reply, answer) = oneshot::channel();
    overworld
        .request(RegionRequest::RespawnPoint {
            pos: used.pos,
            yaw: 0.0,
            pitch: 0.0,
            forced: false,
            consume: false,
            reply,
        })
        .await;
    let spot = answer.await.unwrap().expect("room beside the bed");
    assert_eq!(spot.position[1], SKY as f64 + 1.0);
    let (reply, answer) = oneshot::channel();
    overworld
        .request(RegionRequest::RespawnPoint {
            pos: BlockPos::new(4, SKY + 1, 4),
            yaw: 0.0,
            pitch: 0.0,
            forced: false,
            consume: false,
            reply,
        })
        .await;
    assert_eq!(answer.await.unwrap(), None, "no bed, no spawn");
    let (reply, answer) = oneshot::channel();
    overworld
        .request(RegionRequest::RespawnPoint {
            pos: BlockPos::new(4, SKY + 1, 4),
            yaw: 0.0,
            pitch: 0.0,
            forced: true,
            consume: false,
            reply,
        })
        .await;
    assert!(
        answer.await.unwrap().is_some(),
        "forced spawns need only room"
    );
    world.shutdown().await;
}

/// Vanilla's bed refusal messages (too far away, obstructed, occupied) are
/// plain translatable text with no click or hover event; this pins that
/// down so a future change does not add one that vanilla never had.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bed_error_overlay_messages_carry_no_click_or_hover_event() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (player, mut events) = PlayerHandle::new(100);
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    subscribe(&region, &player, &mut events, &[chunk]).await;

    async fn overlay(events: &mut mpsc::UnboundedReceiver<PlayerEvent>) -> TextComponent {
        expect(events, |event| match event {
            PlayerEvent::Overlay(message) => Some(message),
            _ => None,
        })
        .await
    }

    let mut blocks = platform((4, 4), (20, 20), SKY);

    let far_foot = BlockPos::new(8, SKY + 1, 8);
    blocks.extend(bed(far_foot));
    place_blocks(&world, &region, blocks).await;
    let mut click = place(&player, far_foot, Item::Stick, 1);
    click.item = None;
    click.held = None;
    click.position = [500.5, SKY as f64 + 1.0, 500.5];
    region.request(RegionRequest::Place(click)).await;
    let message = overlay(&mut events).await;
    assert!(
        message.plain().contains("too far away"),
        "expected too_far_away, got {message:?}"
    );
    assert!(
        message.events().is_none(),
        "vanilla's bed.too_far_away message carries no click or hover event: {message:?}"
    );

    let obstructed_foot = BlockPos::new(14, SKY + 1, 8);
    let mut obstructed = bed(obstructed_foot);
    obstructed.push((
        BlockPos::new(obstructed_foot.x, SKY + 2, obstructed_foot.z),
        Block::Stone.default_state(),
    ));
    obstructed.push((
        BlockPos::new(obstructed_foot.x, SKY + 2, obstructed_foot.z + 1),
        Block::Stone.default_state(),
    ));
    place_blocks(&world, &region, obstructed).await;
    let mut click = place(&player, obstructed_foot, Item::Stick, 2);
    click.item = None;
    click.held = None;
    region.request(RegionRequest::Place(click)).await;
    let message = overlay(&mut events).await;
    assert!(
        message.plain().contains("obstructed"),
        "expected obstructed, got {message:?}"
    );
    assert!(
        message.events().is_none(),
        "vanilla's bed.obstructed message carries no click or hover event: {message:?}"
    );

    let occupied_foot = BlockPos::new(8, SKY + 1, 14);
    let occupied = bed(occupied_foot)
        .into_iter()
        .map(|(pos, state)| (pos, state.with_occupied(true).unwrap()))
        .collect::<Vec<_>>();
    place_blocks(&world, &region, occupied).await;
    let mut click = place(&player, occupied_foot, Item::Stick, 3);
    click.item = None;
    click.held = None;
    region.request(RegionRequest::Place(click)).await;
    let message = overlay(&mut events).await;
    assert!(
        message.plain().contains("occupied"),
        "expected occupied, got {message:?}"
    );
    assert!(
        message.events().is_none(),
        "vanilla's bed.occupied message carries no click or hover event: {message:?}"
    );

    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_monster_near_a_bed_is_reported_to_the_sleeper() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (player, mut events) = PlayerHandle::new(100);
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    subscribe(&region, &player, &mut events, &[chunk]).await;
    let foot = BlockPos::new(8, SKY + 1, 8);
    let mut blocks = platform((4, 4), (12, 12), SKY);
    blocks.extend(bed(foot));
    place_blocks(&world, &region, blocks).await;

    async fn click(
        region: &RegionHandle,
        player: &PlayerHandle,
        events: &mut mpsc::UnboundedReceiver<PlayerEvent>,
        foot: BlockPos,
        sequence: i32,
    ) -> BedUse {
        let mut click = place(player, foot, Item::Stick, sequence);
        click.item = None;
        click.held = None;
        click.position = [8.5, SKY as f64 + 1.0, 6.5];
        region.request(RegionRequest::Place(click)).await;
        expect(events, |event| match event {
            PlayerEvent::Run(task) => task.data::<BedUse>().cloned(),
            _ => None,
        })
        .await
    }

    assert!(
        !click(&region, &player, &mut events, foot, 1)
            .await
            .monsters_near
    );

    let (reply, summoned) = oneshot::channel();
    region
        .request(RegionRequest::Summon {
            nbt: Compound::new()
                .with("id", "minecraft:zombie")
                .with("NoAI", 1i8),
            position: [10.5, SKY as f64 + 1.0, 10.5],
            fresh: false,
            reply,
        })
        .await;
    assert!(summoned.await.unwrap());
    assert!(
        click(&region, &player, &mut events, foot, 2)
            .await
            .monsters_near
    );

    world.shutdown().await;
}

/// The push in the next Explosion packet a viewer gets.
async fn explosion_push(events: &mut mpsc::UnboundedReceiver<PlayerEvent>) -> Option<[f64; 3]> {
    let id = explode_id();
    expect(events, |event| match event {
        PlayerEvent::ChunkPackets { packets, .. } => packets
            .iter()
            .find(|p| framed_packet_id(p) == id)
            .map(|packet| {
                let mut r = framed_payload(packet);
                r.read_var_int().unwrap();
                for _ in 0..3 {
                    r.read_double().unwrap();
                }
                r.read_float().unwrap();
                r.read_int().unwrap();
                r.read_bool()
                    .unwrap()
                    .then(|| [0; 3].map(|_| r.read_double().unwrap()))
            }),
        _ => None,
    })
    .await
}

async fn exploded_push(events: &mut mpsc::UnboundedReceiver<PlayerEvent>) -> [f64; 3] {
    expect(events, |event| match event {
        PlayerEvent::Exploded { push, .. } => Some(push),
        _ => None,
    })
    .await
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn blast_protection_softens_explosion_pushes_and_flying_creative_players_ignore_them() {
    let dir = TestDir::new();
    let chunk = ChunkPos::new(0, 0);
    let world = TestWorld::start(dir.path());
    let region = world.region(chunk.region()).await;
    let (igniter, mut igniter_events) = PlayerHandle::new(100);
    let (plain, mut plain_events) = PlayerHandle::new(101);
    let (resistant, mut resistant_events) = PlayerHandle::new(102);
    let (flying, mut flying_events) = PlayerHandle::new(103);
    for (player, events) in [
        (&igniter, &mut igniter_events),
        (&plain, &mut plain_events),
        (&resistant, &mut resistant_events),
        (&flying, &mut flying_events),
    ] {
        subscribe(&region, player, events, &[chunk]).await;
    }
    let tnt = BlockPos::new(8, SKY + 1, 8);
    let mut blocks = platform((2, 2), (14, 14), SKY);
    blocks.push((tnt, Block::Tnt.default_state()));
    place_blocks(&world, &region, blocks).await;
    let standing = |player: &PlayerHandle, position: [f64; 3], resistance, creative_flying| {
        RegionRequest::Player(PlayerUpdate {
            looks: Default::default(),
            explosion_knockback_resistance: resistance,
            creative_flying,
            ..match update(player, 1, position) {
                RegionRequest::Player(update) => update,
                _ => unreachable!(),
            }
        })
    };
    let y = (SKY + 1) as f64;
    region
        .request(standing(&plain, [11.5, y, 8.5], 0.0, false))
        .await;
    region
        .request(standing(&resistant, [5.5, y, 8.5], 0.6, false))
        .await;
    region
        .request(standing(&flying, [8.5, y, 11.5], 0.0, true))
        .await;
    region
        .request(RegionRequest::Place(ignite(&igniter, tnt, 1)))
        .await;
    outcome(&mut igniter_events, 1).await;
    world.ticks(90).await;

    let length = |v: [f64; 3]| v.iter().map(|c| c * c).sum::<f64>().sqrt();
    let plain_push = exploded_push(&mut plain_events).await;
    let resisted_push = exploded_push(&mut resistant_events).await;
    let ratio = length(resisted_push) / length(plain_push);
    assert!((ratio - 0.4).abs() < 0.05, "ratio {ratio}");
    assert!(plain_push[0] > 0.0 && resisted_push[0] < 0.0);
    let sent = explosion_push(&mut resistant_events).await.unwrap();
    assert!((length(sent) - length(resisted_push)).abs() < 1e-9);

    assert!(length(exploded_push(&mut flying_events).await) > 0.0);
    assert_eq!(
        explosion_push(&mut flying_events).await,
        None,
        "flying creative players are not pushed"
    );
    world.shutdown().await;
}

fn anchor(charges: u8) -> BlockState {
    Block::RespawnAnchor
        .default_state()
        .with_charges(charges)
        .unwrap()
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn respawn_anchors_charge_with_glowstone_and_grow_brighter() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (player, mut events) = PlayerHandle::new(100);
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    subscribe(&region, &player, &mut events, &[chunk]).await;
    present(&region, &player, [8.0, 80.0, 8.0]).await;
    let pos = BlockPos::new(8, 70, 8);
    place_blocks(&world, &region, [(pos, anchor(0))]).await;

    let mut last_light = state(&region, pos).await.light_emission();
    for seq in 1..=4 {
        region
            .request(RegionRequest::Place(place(
                &player,
                pos,
                Item::Glowstone,
                seq,
            )))
            .await;
        assert!(
            matches!(
                outcome(&mut events, seq).await,
                ActionOutcome::Used {
                    item: Item::Glowstone,
                    effect: HandEffect::Consume(1),
                    ..
                }
            ),
            "click {seq} should consume glowstone"
        );
        let charged = state(&region, pos).await;
        assert_eq!(charged.charges(), Some(seq as u8));
        let light = charged.light_emission();
        assert!(
            light > last_light,
            "charge {seq} should be brighter: {light} vs {last_light}"
        );
        last_light = light;
    }

    // A full anchor no longer charges: glowstone in hand falls through to
    // the anchor's other reaction (set spawn, here in the overworld, so it
    // explodes) and is not consumed.
    region
        .request(RegionRequest::Place(place(
            &player,
            pos,
            Item::Glowstone,
            5,
        )))
        .await;
    explosion(&mut events).await;
    assert!(matches!(
        outcome(&mut events, 5).await,
        ActionOutcome::Interacted { .. }
    ));
    assert_eq!(state(&region, pos).await.block(), Block::Air);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn respawn_anchors_set_the_spawn_in_the_nether_and_explode_elsewhere() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (player, mut events) = PlayerHandle::new(100);
    let chunk = ChunkPos::new(0, 0);

    let nether = world.region_in(THE_NETHER, chunk.region()).await;
    subscribe(&nether, &player, &mut events, &[chunk]).await;
    present(&nether, &player, [8.0, 70.0, 8.0]).await;
    let pos = BlockPos::new(8, 70, 8);
    let mut blocks = platform((6, 6), (10, 10), 69);
    blocks.push((pos, anchor(2)));
    place_blocks(&world, &nether, blocks).await;

    let mut click = place(&player, pos, Item::Stick, 1);
    click.item = None;
    nether.request(RegionRequest::Place(click)).await;
    // The region sends AnchorUse before the click's own ActionResult, so
    // look for it first: `outcome` discards whatever it skips past.
    let used: AnchorUse = expect(&mut events, |event| match event {
        PlayerEvent::Run(task) => task.data::<AnchorUse>().cloned(),
        _ => None,
    })
    .await;
    assert!(matches!(
        outcome(&mut events, 1).await,
        ActionOutcome::Interacted { .. }
    ));
    assert_eq!(used.pos, pos);
    assert_eq!(used.dimension, world.dimension(THE_NETHER));
    assert_eq!(
        state(&nether, pos).await.charges(),
        Some(2),
        "using the anchor does not spend a charge itself"
    );

    let (reply, answer) = oneshot::channel();
    nether
        .request(RegionRequest::RespawnPoint {
            pos,
            yaw: 0.0,
            pitch: 0.0,
            forced: false,
            consume: true,
            reply,
        })
        .await;
    let spot = answer.await.unwrap().expect("room to stand up");
    assert_eq!(spot.pitch, 0.0);
    assert!(spot.anchor);
    assert!(
        (spot.position[1] - pos.y as f64).abs() <= 1.0,
        "stands near the anchor: {:?}",
        spot.position
    );
    assert_eq!(
        state(&nether, pos).await.charges(),
        Some(1),
        "respawning consumes one charge"
    );

    let (reply, answer) = oneshot::channel();
    nether
        .request(RegionRequest::RespawnPoint {
            pos,
            yaw: 0.0,
            pitch: 0.0,
            forced: true,
            consume: true,
            reply,
        })
        .await;
    assert!(answer.await.unwrap().is_some());
    assert_eq!(
        state(&nether, pos).await.charges(),
        Some(1),
        "a forced respawn does not spend a charge"
    );

    let overworld = world.region(chunk.region()).await;
    subscribe(&overworld, &player, &mut events, &[chunk]).await;
    present(&overworld, &player, [8.0, 70.0, 8.0]).await;
    let elsewhere = BlockPos::new(2, 70, 2);
    let mut oblocks = platform((6, 6), (10, 10), 69);
    oblocks.extend(platform((0, 0), (4, 4), 69));
    oblocks.push((pos, anchor(2)));
    oblocks.push((elsewhere, anchor(2)));
    place_blocks(&world, &overworld, oblocks).await;

    // Merely resolving where a player would respawn never explodes the
    // anchor: only the click (tested above) does, in 26.3.
    let (reply, answer) = oneshot::channel();
    overworld
        .request(RegionRequest::RespawnPoint {
            pos: elsewhere,
            yaw: 0.0,
            pitch: 0.0,
            forced: false,
            consume: false,
            reply,
        })
        .await;
    assert_eq!(
        answer.await.unwrap(),
        None,
        "a spawn block that does not work here is just missing, not exploded"
    );
    assert_eq!(
        state(&overworld, elsewhere).await.block(),
        Block::RespawnAnchor,
        "resolving a respawn point does not touch the block"
    );

    let mut click = place(&player, pos, Item::Stick, 2);
    click.item = None;
    overworld.request(RegionRequest::Place(click)).await;
    explosion(&mut events).await;
    assert!(matches!(
        outcome(&mut events, 2).await,
        ActionOutcome::Interacted { .. }
    ));
    assert_eq!(
        state(&overworld, pos).await.block(),
        Block::Air,
        "using it outside the nether explodes it"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_wind_burst_flips_levers_across_a_region_border() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let west = world.region(ChunkPos::new(31, 0).region()).await;
    let east = world.region(ChunkPos::new(32, 0).region()).await;
    let (player, mut events) = PlayerHandle::new(100);
    subscribe(&west, &player, &mut events, &[ChunkPos::new(31, 0)]).await;
    subscribe(&east, &player, &mut events, &[ChunkPos::new(32, 0)]).await;
    world.ticks(3).await;
    let lever = Block::Lever.default_state().with("face", "floor").unwrap();
    let (near, far) = (
        BlockPos::new(510, SKY + 1, 8),
        BlockPos::new(513, SKY + 1, 8),
    );
    place_blocks(&world, &west, platform((508, 6), (511, 10), SKY)).await;
    place_blocks(&world, &east, platform((512, 6), (515, 10), SKY)).await;
    place_blocks(&world, &west, [(near, lever)]).await;
    place_blocks(&world, &east, [(far, lever)]).await;
    assert_eq!(state(&east, far).await.powered(), Some(false));

    west.request(RegionRequest::Run(
        crate::world::region_api::RegionTask::new(|world| {
            world.explode(crate::world::explosion::Explosion {
                center: [511.9, SKY as f64 + 1.5, 8.5],
                radius: 3.0,
                fire: false,
                blocks: crate::world::explosion::BlockInteraction::Trigger(
                    crate::world::explosion::WindBurst {
                        knockback: 1.0,
                        sound: tuff_vanilla_data::registry::SoundEvent::EntityBreezeWindBurst,
                        source: EntityType::BreezeWindCharge,
                        spares_fall: false,
                    },
                ),
                damage_type: &tuff_vanilla_data::damage_type::WIND_CHARGE,
                igniter: None,
                culprit: None,
                dangerous_wither_skull: false,
                spares_nether_portals: false,
            });
        }),
    ))
    .await;
    world.ticks(2).await;
    assert_eq!(state(&west, near).await.powered(), Some(true));
    assert_eq!(
        state(&east, far).await.powered(),
        Some(true),
        "the neighbour flips its own lever"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_portal_traveller_blast_spares_nether_portals_and_others_do_not() {
    let dir = TestDir::new();
    let (player, mut events) = PlayerHandle::new(100);
    let chunk = ChunkPos::new(0, 0);
    let world = TestWorld::start(dir.path());
    let region = world.region(chunk.region()).await;
    subscribe(&region, &player, &mut events, &[chunk]).await;
    let mut frame = Vec::new();
    for x in 7..=10 {
        for y in SKY..=SKY + 4 {
            let edge = x == 7 || x == 10 || y == SKY || y == SKY + 4;
            let block = if edge {
                Block::Obsidian
            } else {
                Block::NetherPortal
            };
            frame.push((BlockPos::new(x, y, 8), block.default_state()));
        }
    }
    frame.sort_by_key(|(_, state)| state.block() == Block::NetherPortal);
    let pane = BlockPos::new(8, SKY + 2, 8);
    for spares in [true, false] {
        place_blocks(&world, &region, frame.clone()).await;
        assert_eq!(state(&region, pane).await.block(), Block::NetherPortal);
        region
            .request(RegionRequest::Run(
                crate::world::region_api::RegionTask::new(move |world| {
                    world.explode(crate::world::explosion::Explosion {
                        center: [9.0, SKY as f64 + 2.0, 8.5],
                        radius: 4.0,
                        fire: false,
                        blocks: crate::world::explosion::BlockInteraction::Destroy,
                        damage_type: &tuff_vanilla_data::damage_type::EXPLOSION,
                        igniter: None,
                        culprit: None,
                        dangerous_wither_skull: false,
                        spares_nether_portals: spares,
                    });
                }),
            ))
            .await;
        world.ticks(2).await;
        let portal = state(&region, pane).await.block() == Block::NetherPortal;
        assert_eq!(portal, spares);
    }
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_blast_pushes_mobs_from_their_eyes_but_primed_tnt_from_its_feet() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(1, 1);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    view_around(&world, &viewer, &mut events, chunk).await;
    for (id, at) in [
        ("minecraft:pig", [26.5, 160.0, 24.5]),
        ("minecraft:tnt", [22.5, 160.0, 24.5]),
    ] {
        let (reply, summoned) = oneshot::channel();
        let nbt = Compound::new()
            .with("id", id)
            .with("NoGravity", Tag::Byte(1))
            .with("NoAI", Tag::Byte(1))
            .with("Invulnerable", Tag::Byte(1))
            .with("fuse", Tag::Short(200));
        region
            .request(RegionRequest::Summon {
                nbt,
                position: at,
                fresh: false,
                reply,
            })
            .await;
        assert!(summoned.await.unwrap());
    }
    region
        .request(RegionRequest::Run(
            crate::world::region_api::RegionTask::new(|world| {
                world.explode(crate::world::explosion::Explosion {
                    center: [24.5, 160.0, 24.5],
                    radius: 4.0,
                    fire: false,
                    blocks: crate::world::explosion::BlockInteraction::Keep,
                    damage_type: &tuff_vanilla_data::damage_type::EXPLOSION,
                    igniter: None,
                    culprit: None,
                    dangerous_wither_skull: false,
                    spares_nether_portals: false,
                });
            }),
        ))
        .await;
    let motion_y = async |kind: EntityType| {
        let id = mobs(&region, kind).await[0].id;
        let (reply, data) = oneshot::channel();
        region
            .request(RegionRequest::EntityData { id, reply })
            .await;
        let data = data.await.unwrap().unwrap();
        match data.get("Motion") {
            Some(Tag::List(motion)) => match motion[1] {
                Tag::Double(y) => y,
                _ => panic!("motion is doubles"),
            },
            _ => panic!("no motion"),
        }
    };
    assert!(
        motion_y(EntityType::Pig).await > 0.0,
        "a pig is lifted by its eyes"
    );
    assert!(
        motion_y(EntityType::Tnt).await.abs() < 1.0e-9,
        "primed tnt is pushed level from its feet"
    );
    world.shutdown().await;
}
