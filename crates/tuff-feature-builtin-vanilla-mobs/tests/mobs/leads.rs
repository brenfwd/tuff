//! Leads: tying mobs and boats to players, fence knots and other mobs,
//! the pull and the snap, the saved `leash`, and leads whose holder stands
//! in a neighbouring region.

use super::*;
use crate::world::messages::{HandEffect, PlayerLooks};
use tuff_vanilla_data::registry::EntityType;

const PLAYER: u128 = 77;
const FLOOR: f64 = SKY as f64 + 1.0;

fn held_by(uuid: u128) -> Compound {
    Compound::new().with("leash", Compound::new().with("UUID", Tag::uuid(uuid)))
}

fn tied_to(pos: BlockPos) -> Compound {
    Compound::new().with("leash", Tag::IntArray(vec![pos.x, pos.y, pos.z]))
}

async fn leash_of(region: &RegionHandle, id: i32) -> Option<Tag> {
    entity_nbt(region, id).await?.get("leash").cloned()
}

fn uuid_leash(uuid: u128) -> Option<Tag> {
    Some(Tag::Compound(Compound::new().with("UUID", Tag::uuid(uuid))))
}

async fn click(
    region: &RegionHandle,
    player: &PlayerHandle,
    events: &mut mpsc::UnboundedReceiver<PlayerEvent>,
    target: i32,
    item: Option<Item>,
    at: [f64; 3],
    sneaking: bool,
) -> HandEffect {
    let mut interaction = interaction(player, item);
    interaction.position = at;
    interaction.sneaking = sneaking;
    region
        .request(RegionRequest::InteractEntity {
            player: player.clone(),
            target,
            interaction,
        })
        .await;
    expect(events, |event| match event {
        PlayerEvent::EntityInteracted { effect, .. } => Some(effect),
        _ => None,
    })
    .await
}

/// Lead items lying in the region.
async fn loose_leads(region: &RegionHandle) -> usize {
    let mut count = 0;
    for item in snapshots(region)
        .await
        .into_iter()
        .filter(|s| s.kind == EntityType::Item)
    {
        let lead = entity_nbt(region, item.id).await.is_some_and(|nbt| {
            nbt.get_compound("Item")
                .and_then(|i| i.get_str("id").map(str::to_owned))
                .as_deref()
                == Some("minecraft:lead")
        });
        count += usize::from(lead);
    }
    count
}

/// A world with a pen over chunk (0, 0), a viewer and the player standing
/// at `at`.
async fn pen_world(
    dir: &TestDir,
    at: [f64; 3],
) -> (
    TestWorld,
    RegionHandle,
    PlayerHandle,
    mpsc::UnboundedReceiver<PlayerEvent>,
) {
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(0, 14)).await;
    region
        .request(player_at(&player, at, PlayerLooks::default()))
        .await;
    (world, region, player, events)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lead_ties_a_cow_to_the_player_and_it_follows() {
    let dir = TestDir::new();
    let at = [3.5, FLOOR, 3.5];
    let (world, region, player, mut events) = pen_world(&dir, at).await;
    summon(
        &region,
        EntityType::Cow,
        Compound::new(),
        [10.5, FLOOR, 10.5],
    )
    .await;
    world.ticks(2).await;
    let cow = mobs(&region, EntityType::Cow).await[0].clone();
    let effect = click(
        &region,
        &player,
        &mut events,
        cow.id,
        Some(Item::Lead),
        at,
        false,
    )
    .await;
    assert_eq!(effect, HandEffect::Consume(1));
    assert_eq!(leash_of(&region, cow.id).await, uuid_leash(PLAYER));
    world.ticks(2).await;
    let nbt = entity_nbt(&region, cow.id).await.unwrap();
    assert_eq!(
        nbt.get_int("home_radius"),
        Some(5),
        "the holder is its home"
    );
    wait_for(&world, &region, 300, |found| {
        found
            .iter()
            .any(|s| s.id == cow.id && crate::world::distance_sq(s.position, at) < 4.0 * 4.0)
    })
    .await;
    assert_eq!(leash_of(&region, cow.id).await, uuid_leash(PLAYER));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lead_pulled_too_far_snaps_and_drops() {
    let dir = TestDir::new();
    let at = [3.5, FLOOR, 3.5];
    let (world, region, player, _events) = pen_world(&dir, at).await;
    summon(&region, EntityType::Cow, held_by(PLAYER), [6.5, FLOOR, 6.5]).await;
    world.ticks(3).await;
    let cow = mobs(&region, EntityType::Cow).await[0].clone();
    assert_eq!(leash_of(&region, cow.id).await, uuid_leash(PLAYER));
    region
        .request(player_at(
            &player,
            [60.5, FLOOR, 3.5],
            PlayerLooks::default(),
        ))
        .await;
    world.ticks(3).await;
    assert_eq!(leash_of(&region, cow.id).await, None, "it snapped");
    assert_eq!(loose_leads(&region).await, 1);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn clicking_your_leashed_mob_unties_it_and_shears_cut_a_lead() {
    let dir = TestDir::new();
    let at = [3.5, FLOOR, 3.5];
    let (world, region, player, mut events) = pen_world(&dir, at).await;
    summon(&region, EntityType::Cow, held_by(PLAYER), [6.5, FLOOR, 6.5]).await;
    summon(&region, EntityType::Pig, held_by(PLAYER), [6.5, FLOOR, 3.5]).await;
    world.ticks(3).await;
    let cow = mobs(&region, EntityType::Cow).await[0].clone();
    let pig = mobs(&region, EntityType::Pig).await[0].clone();
    let effect = click(&region, &player, &mut events, cow.id, None, at, false).await;
    assert_eq!(effect, HandEffect::None);
    assert_eq!(leash_of(&region, cow.id).await, None);
    let effect = click(
        &region,
        &player,
        &mut events,
        pig.id,
        Some(Item::Shears),
        at,
        false,
    )
    .await;
    assert_eq!(effect, HandEffect::Damage(1), "the shears wear");
    assert_eq!(leash_of(&region, pig.id).await, None);
    assert_eq!(loose_leads(&region).await, 2, "both leads dropped");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sneak_clicking_a_mob_ties_your_leashed_mobs_to_it() {
    let dir = TestDir::new();
    let at = [3.5, FLOOR, 3.5];
    let (world, region, player, mut events) = pen_world(&dir, at).await;
    summon(&region, EntityType::Cow, held_by(PLAYER), [6.5, FLOOR, 6.5]).await;
    summon(
        &region,
        EntityType::Horse,
        Compound::new().with("NoAI", true),
        [4.5, FLOOR, 8.5],
    )
    .await;
    world.ticks(3).await;
    let cow = mobs(&region, EntityType::Cow).await[0].clone();
    let horse = mobs(&region, EntityType::Horse).await[0].clone();
    let effect = click(&region, &player, &mut events, horse.id, None, at, true).await;
    assert_eq!(effect, HandEffect::None);
    assert_eq!(leash_of(&region, cow.id).await, uuid_leash(horse.uuid));
    world.ticks(20).await;
    assert_eq!(
        leash_of(&region, cow.id).await,
        uuid_leash(horse.uuid),
        "the horse holds it"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lead_tied_to_a_fence_saves_as_the_fence_and_reties_its_knot() {
    let dir = TestDir::new();
    let chunk = ChunkPos::new(0, 0);
    let fence = BlockPos::new(6, SKY + 1, 6);
    let cow_id;
    {
        let at = [6.5, FLOOR, 8.5];
        let (world, region, player, mut events) = pen_world(&dir, at).await;
        place_blocks(&world, &region, [(fence, Block::OakFence.default_state())]).await;
        summon(&region, EntityType::Cow, held_by(PLAYER), [9.5, FLOOR, 9.5]).await;
        world.ticks(3).await;
        cow_id = mobs(&region, EntityType::Cow).await[0].id;
        let mut use_fence = place(&player, fence, Item::Lead, 5);
        use_fence.uuid = PLAYER;
        use_fence.position = at;
        region.request(RegionRequest::Place(use_fence)).await;
        let outcome = expect(&mut events, |event| match event {
            PlayerEvent::ActionResult {
                sequence: 5,
                outcome,
            } => Some(outcome),
            _ => None,
        })
        .await;
        assert!(
            matches!(outcome, ActionOutcome::Interacted { .. }),
            "{outcome:?}"
        );
        assert_eq!(
            leash_of(&region, cow_id).await,
            Some(Tag::IntArray(vec![fence.x, fence.y, fence.z]))
        );
        let knots = mobs(&region, EntityType::LeashKnot).await;
        assert_eq!(knots.len(), 1);
        assert_eq!(
            [knots[0].position[0], knots[0].position[2]],
            [6.5, 6.5],
            "on the fence"
        );
        world.ticks(20).await;
        assert_eq!(mobs(&region, EntityType::LeashKnot).await.len(), 1);
        world.shutdown().await;
    }
    let saved = stored_entities(dir.path(), chunk);
    let cow = saved
        .iter()
        .find(|e| e.get_str("id") == Some("minecraft:cow"))
        .expect("the cow saved");
    assert_eq!(
        cow.get("leash"),
        Some(&Tag::IntArray(vec![fence.x, fence.y, fence.z]))
    );
    assert!(
        !saved
            .iter()
            .any(|e| e.get_str("id") == Some("minecraft:leash_knot")),
        "knots are not saved"
    );
    let world = TestWorld::start(dir.path());
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    let found = wait_for(&world, &region, 40, |found| {
        found.iter().any(|s| s.kind == EntityType::LeashKnot)
    })
    .await;
    let cow = found
        .iter()
        .find(|s| s.kind == EntityType::Cow)
        .expect("the cow loaded");
    world.ticks(120).await;
    assert_eq!(
        leash_of(&region, cow.id).await,
        Some(Tag::IntArray(vec![fence.x, fence.y, fence.z])),
        "still tied after the wait a lost holder gets"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn breaking_the_fence_frees_the_mob_and_drops_the_lead() {
    let dir = TestDir::new();
    let at = [3.5, FLOOR, 3.5];
    let (world, region, _player, _events) = pen_world(&dir, at).await;
    let fence = BlockPos::new(6, SKY + 1, 6);
    place_blocks(&world, &region, [(fence, Block::OakFence.default_state())]).await;
    summon(&region, EntityType::Cow, tied_to(fence), [8.5, FLOOR, 8.5]).await;
    wait_for(&world, &region, 20, |found| {
        found.iter().any(|s| s.kind == EntityType::LeashKnot)
    })
    .await;
    let cow = mobs(&region, EntityType::Cow).await[0].clone();
    place_blocks(&world, &region, [(fence, Block::Air.default_state())]).await;
    wait_for(&world, &region, 120, |found| {
        !found.iter().any(|s| s.kind == EntityType::LeashKnot)
    })
    .await;
    world.ticks(2).await;
    assert_eq!(leash_of(&region, cow.id).await, None);
    assert_eq!(loose_leads(&region).await, 1);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn execute_on_leasher_runs_as_the_knot_holding_the_lead() {
    let dir = TestDir::new();
    let at = [3.5, FLOOR, 3.5];
    let (world, region, _player, _events) = pen_world(&dir, at).await;
    let fence = BlockPos::new(6, SKY + 1, 6);
    place_blocks(&world, &region, [(fence, Block::OakFence.default_state())]).await;
    summon(&region, EntityType::Cow, tied_to(fence), [8.5, FLOOR, 8.5]).await;
    wait_for(&world, &region, 20, |found| {
        found.iter().any(|s| s.kind == EntityType::LeashKnot)
    })
    .await;
    crate::world::commands::console::run(
        world.shared.clone(),
        "execute as @e[type=cow] on leasher run tag @s add holder",
    )
    .await;
    world.ticks(2).await;
    let knot = snapshots(&region)
        .await
        .into_iter()
        .find(|s| s.kind == EntityType::LeashKnot)
        .unwrap();
    let nbt = entity_nbt(&region, knot.id).await.unwrap();
    assert!(format!("{:?}", nbt.get("Tags")).contains("holder"));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn execute_on_relations_without_a_target_run_nothing() {
    let dir = TestDir::new();
    let at = [3.5, FLOOR, 3.5];
    let (world, region, _player, _events) = pen_world(&dir, at).await;
    summon(&region, EntityType::Cow, held_by(4242), [8.5, FLOOR, 8.5]).await;
    for relation in [
        "vehicle",
        "passengers",
        "owner",
        "attacker",
        "target",
        "controller",
        "origin",
    ] {
        crate::world::commands::console::run(
            world.shared.clone(),
            &format!("execute as @e[type=cow] on {relation} run tag @s add related"),
        )
        .await;
    }
    world.ticks(2).await;
    let cow = mobs(&region, EntityType::Cow).await[0].clone();
    let nbt = entity_nbt(&region, cow.id).await.unwrap();
    assert!(!format!("{:?}", nbt.get("Tags")).contains("related"));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lead_whose_holder_never_turns_up_drops_after_a_while() {
    let dir = TestDir::new();
    let at = [3.5, FLOOR, 3.5];
    let (world, region, _player, _events) = pen_world(&dir, at).await;
    summon(&region, EntityType::Cow, held_by(4242), [8.5, FLOOR, 8.5]).await;
    world.ticks(50).await;
    let cow = mobs(&region, EntityType::Cow).await[0].clone();
    assert_eq!(leash_of(&region, cow.id).await, uuid_leash(4242));
    world.ticks(60).await;
    assert_eq!(leash_of(&region, cow.id).await, None);
    assert_eq!(loose_leads(&region).await, 1);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn boats_take_a_lead_and_save_it() {
    let dir = TestDir::new();
    let at = [3.5, FLOOR, 3.5];
    let (world, region, player, mut events) = pen_world(&dir, at).await;
    summon(
        &region,
        EntityType::OakBoat,
        Compound::new(),
        [6.5, FLOOR, 6.5],
    )
    .await;
    world.ticks(2).await;
    let boat = mobs(&region, EntityType::OakBoat).await[0].clone();
    let effect = click(
        &region,
        &player,
        &mut events,
        boat.id,
        Some(Item::Lead),
        at,
        false,
    )
    .await;
    assert_eq!(effect, HandEffect::Consume(1));
    assert_eq!(leash_of(&region, boat.id).await, uuid_leash(PLAYER));
    world.ticks(10).await;
    assert_eq!(leash_of(&region, boat.id).await, uuid_leash(PLAYER));
    world.shutdown().await;
}

/// Two regions meeting at x = 0 with a floor across the border.
async fn border_world(dir: &TestDir) -> (TestWorld, RegionHandle, RegionHandle, PlayerHandle) {
    let world = TestWorld::start(dir.path());
    let west_chunk = ChunkPos::new(-1, 0);
    let east_chunk = ChunkPos::new(0, 0);
    let west = world.region(west_chunk.region()).await;
    let east = world.region(east_chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    subscribe(&west, &viewer, &mut events, &[west_chunk]).await;
    subscribe(&east, &viewer, &mut events, &[east_chunk]).await;
    let mut floor = Vec::new();
    for x in -16..16 {
        for z in 0..16 {
            floor.push((BlockPos::new(x, SKY, z), Block::GrassBlock.default_state()));
        }
    }
    place_blocks(&world, &east, floor.clone()).await;
    place_blocks(&world, &west, floor).await;
    world.ticks(3).await;
    (world, west, east, viewer)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_holder_across_a_region_border_keeps_and_pulls_its_lead() {
    let dir = TestDir::new();
    let (world, west, east, _viewer) = border_world(&dir).await;
    let still = Compound::new().with("NoAI", true);
    summon(&east, EntityType::Cow, still.clone(), [6.5, FLOOR, 8.5]).await;
    let holder = mobs(&east, EntityType::Cow).await[0].clone();
    let mut tied = held_by(holder.uuid);
    tied.insert("NoAI", true);
    summon(&west, EntityType::Cow, tied, [-4.5, FLOOR, 8.5]).await;
    let cow = mobs(&west, EntityType::Cow).await[0].clone();
    world.ticks(150).await;
    let mut seen = None;
    for region in [&west, &east] {
        for s in mobs(region, EntityType::Cow).await {
            if s.id == cow.id || (s.uuid == cow.uuid && s.id != holder.id) {
                seen = Some((s.position, leash_of(region, s.id).await));
            }
        }
    }
    let (position, leash) = seen.expect("the cow is somewhere");
    assert_eq!(leash, uuid_leash(holder.uuid));
    assert!(position[0] > -4.4, "pulled east: {position:?}");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_fence_across_a_region_border_ties_its_knot_there() {
    let dir = TestDir::new();
    let (world, west, east, _viewer) = border_world(&dir).await;
    let fence = BlockPos::new(2, SKY + 1, 8);
    place_blocks(&world, &east, [(fence, Block::OakFence.default_state())]).await;
    let mut tied = tied_to(fence);
    tied.insert("NoAI", true);
    summon(&west, EntityType::Cow, tied, [-3.5, FLOOR, 8.5]).await;
    let cow = mobs(&west, EntityType::Cow).await[0].clone();
    world.ticks(150).await;
    assert_eq!(mobs(&east, EntityType::LeashKnot).await.len(), 1);
    assert!(mobs(&west, EntityType::LeashKnot).await.is_empty());
    assert_eq!(
        leash_of(&west, cow.id).await,
        Some(Tag::IntArray(vec![fence.x, fence.y, fence.z]))
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn elder_guardians_keep_to_where_they_started() {
    let dir = TestDir::new();
    let at = [3.5, FLOOR, 3.5];
    let (world, region, _player, _events) = pen_world(&dir, at).await;
    summon(
        &region,
        EntityType::ElderGuardian,
        Compound::new(),
        [8.5, FLOOR, 8.5],
    )
    .await;
    world.ticks(3).await;
    let elder = mobs(&region, EntityType::ElderGuardian).await[0].clone();
    let nbt = entity_nbt(&region, elder.id).await.unwrap();
    assert_eq!(nbt.get_int("home_radius"), Some(16));
    assert!(nbt.get("home_pos").is_some());
    world.shutdown().await;
}
