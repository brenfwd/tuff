//! Villagers living together: gossip beyond the bell, shared food, calling
//! golems, hiding from bells, beds that vanish and farmers at work.

use super::*;
use crate::world::region_api::RegionTask;
use tuff_vanilla_data::registry::EntityType;

const PLAYER: u128 = 77;
const DAY: i64 = 3000;
const NIGHT: i64 = 13000;
const DIMENSION: &str = "minecraft:overworld";

async fn data(region: &RegionHandle, id: i32) -> Compound {
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::EntityData { id, reply })
        .await;
    answer.await.unwrap().expect("the villager is there")
}

fn set_time(world: &TestWorld, ticks: i64) {
    let overworld = crate::world::clock::clock_id("minecraft:overworld").unwrap();
    world
        .clocks
        .send_modify(|clocks| clocks.get_mut(overworld).set(ticks));
}

fn memory_place(pos: BlockPos) -> Compound {
    Compound::new().with(
        "value",
        Compound::new()
            .with("dimension", DIMENSION)
            .with("pos", Tag::IntArray(vec![pos.x, pos.y, pos.z])),
    )
}

fn brain(memories: Vec<(&str, Compound)>) -> Compound {
    let mut saved = Compound::new();
    for (key, value) in memories {
        saved.insert(key, value);
    }
    Compound::new().with("memories", saved)
}

fn villager(profession: &str) -> Compound {
    Compound::new()
        .with(
            "VillagerData",
            Compound::new()
                .with("type", "minecraft:plains")
                .with("profession", profession)
                .with("level", 1),
        )
        .with("PersistenceRequired", true)
}

fn has_memory(nbt: &Compound, key: &str) -> bool {
    nbt.get_compound("Brain")
        .and_then(|b| b.get_compound("memories"))
        .is_some_and(|m| m.contains_key(key))
}

async fn wait_for_data(
    world: &TestWorld,
    region: &RegionHandle,
    id: i32,
    ticks: u64,
    mut check: impl FnMut(&Compound) -> bool,
) -> Compound {
    for _ in 0..ticks {
        let nbt = data(region, id).await;
        if check(&nbt) {
            return nbt;
        }
        world.ticks(1).await;
    }
    panic!(
        "never happened; the villager is {:#?}",
        data(region, id).await
    );
}

async fn villager_ids(region: &RegionHandle) -> Vec<i32> {
    let mut ids: Vec<i32> = mobs(region, EntityType::Villager)
        .await
        .iter()
        .map(|v| v.id)
        .collect();
    ids.sort_unstable();
    ids
}

async fn ring_bell(region: &RegionHandle, pos: BlockPos) {
    region
        .request(RegionRequest::WhenLoaded {
            min: pos,
            max: pos,
            run: RegionTask::new(move |world| world.bell_rung(pos)),
        })
        .await;
}

fn bed(head: BlockPos, foot: BlockPos) -> Vec<(BlockPos, tuff_vanilla_data::BlockState)> {
    let bed = Block::RedBed
        .default_state()
        .with("facing", "north")
        .unwrap();
    vec![
        (head, bed.with("part", "head").unwrap()),
        (foot, bed.with("part", "foot").unwrap()),
    ]
}

fn gossip_about_player() -> Tag {
    Tag::List(vec![Tag::Compound(
        Compound::new()
            .with("Target", Tag::uuid(PLAYER))
            .with("Type", "minor_negative")
            .with("Value", 30),
    )])
}

fn heard_about_player(nbt: &Compound) -> bool {
    nbt.get_list("Gossips").is_some_and(|list| {
        list.iter()
            .filter_map(Tag::as_compound)
            .any(|g| g.get_uuid("Target") == Some(PLAYER))
    })
}

fn items(entries: &[(&str, i32)]) -> Tag {
    Tag::List(
        entries
            .iter()
            .enumerate()
            .map(|(slot, (id, count))| {
                Tag::Compound(
                    Compound::new()
                        .with("Slot", slot as i8)
                        .with("id", *id)
                        .with("count", *count),
                )
            })
            .collect(),
    )
}

fn count_of(nbt: &Compound, item: &str) -> i32 {
    nbt.get_list("Inventory")
        .map(|items| {
            items
                .iter()
                .filter_map(Tag::as_compound)
                .filter(|i| i.get_str("id") == Some(item))
                .filter_map(|i| i.get_int("count"))
                .sum()
        })
        .unwrap_or(0)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn villagers_swap_gossip_wherever_they_meet() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    set_time(&world, DAY);
    place_blocks(&world, &region, pen(4, 11)).await;
    let mut talker = villager("minecraft:none");
    talker.insert("Gossips", gossip_about_player());
    summon(
        &region,
        EntityType::Villager,
        villager("minecraft:none"),
        [5.5, SKY as f64 + 1.0, 5.5],
    )
    .await;
    summon(
        &region,
        EntityType::Villager,
        talker,
        [9.5, SKY as f64 + 1.0, 9.5],
    )
    .await;
    world.ticks(1).await;
    let listener = villager_ids(&region).await[0];
    assert!(!heard_about_player(&data(&region, listener).await));
    let nbt = wait_for_data(&world, &region, listener, 3000, heard_about_player).await;
    let gossips = nbt.get_list("Gossips").unwrap();
    let value = gossips[0].as_compound().unwrap().get_int("Value").unwrap();
    assert!(
        value < 30 && value > 0,
        "gossip weakens as it spreads: {value}"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn villagers_hand_spare_food_to_a_hungry_neighbour() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    set_time(&world, DAY);
    place_blocks(&world, &region, pen(4, 11)).await;
    let mut generous = villager("minecraft:none");
    generous.insert("Inventory", items(&[("minecraft:bread", 60)]));
    generous.insert("CanPickUpLoot", true);
    let mut hungry = villager("minecraft:none");
    hungry.insert("CanPickUpLoot", true);
    summon(
        &region,
        EntityType::Villager,
        generous,
        [5.5, SKY as f64 + 1.0, 5.5],
    )
    .await;
    summon(
        &region,
        EntityType::Villager,
        hungry,
        [8.5, SKY as f64 + 1.0, 8.5],
    )
    .await;
    world.ticks(1).await;
    let mut receiver = None;
    for id in villager_ids(&region).await {
        if count_of(&data(&region, id).await, "minecraft:bread") == 0 {
            receiver = Some(id);
        }
    }
    let receiver = receiver.expect("one has no bread");
    let fed = wait_for_data(&world, &region, receiver, 1500, |nbt| {
        count_of(nbt, "minecraft:bread") > 0
    })
    .await;
    assert!(count_of(&fed, "minecraft:bread") >= 30, "half the stack");
    world.shutdown().await;
}

async fn villagers_who_slept(world: &TestWorld, region: &RegionHandle, slept: usize, total: usize) {
    set_time(world, DAY);
    place_blocks(world, region, pen(4, 11)).await;
    for i in 0..total {
        let mut nbt = villager("minecraft:none");
        if i < slept {
            nbt.insert(
                "Brain",
                brain(vec![(
                    "minecraft:last_slept",
                    Compound::new().with("value", 1i64),
                )]),
            );
        }
        let x = 5.5 + (i % 3) as f64 * 2.0;
        let z = 5.5 + (i / 3) as f64 * 2.0;
        summon(region, EntityType::Villager, nbt, [x, SKY as f64 + 1.0, z]).await;
    }
    world.ticks(1).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn five_rested_villagers_who_talk_call_one_iron_golem() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    villagers_who_slept(&world, &region, 5, 5).await;
    wait_for(&world, &region, 3000, |found| {
        found.iter().any(|s| s.kind == EntityType::IronGolem)
    })
    .await;
    world.ticks(400).await;
    assert_eq!(
        mobs(&region, EntityType::IronGolem).await.len(),
        1,
        "the golem keeps the others from asking for more"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn three_rested_villagers_panicking_near_a_zombie_call_an_iron_golem() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    villagers_who_slept(&world, &region, 3, 3).await;
    summon(
        &region,
        EntityType::Zombie,
        Compound::new().with("NoAI", true),
        [9.5, SKY as f64 + 1.0, 9.5],
    )
    .await;
    wait_for(&world, &region, 3000, |found| {
        found.iter().any(|s| s.kind == EntityType::IronGolem)
    })
    .await;
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn four_rested_villagers_and_a_sleepless_fifth_call_no_golem() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    villagers_who_slept(&world, &region, 4, 5).await;
    world.ticks(2500).await;
    assert!(mobs(&region, EntityType::IronGolem).await.is_empty());
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn villagers_run_for_their_beds_when_a_bell_rings_and_come_out_later() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    set_time(&world, DAY);
    let head = BlockPos::new(10, SKY + 1, 7);
    let foot = BlockPos::new(10, SKY + 1, 8);
    let mut blocks = pen(4, 11);
    blocks.extend(bed(head, foot));
    place_blocks(&world, &region, blocks).await;
    summon(
        &region,
        EntityType::Villager,
        villager("minecraft:none"),
        [5.5, SKY as f64 + 1.0, 5.5],
    )
    .await;
    world.ticks(1).await;
    let id = villager_ids(&region).await[0];
    wait_for_data(&world, &region, id, 600, |nbt| {
        has_memory(nbt, "minecraft:home")
    })
    .await;

    ring_bell(&region, BlockPos::new(60, SKY + 1, 60)).await;
    world.ticks(2).await;
    assert!(
        !has_memory(&data(&region, id).await, "minecraft:heard_bell_time"),
        "too far to hear"
    );

    ring_bell(&region, BlockPos::new(8, SKY + 1, 8)).await;
    wait_for_data(&world, &region, id, 5, |nbt| {
        has_memory(nbt, "minecraft:heard_bell_time")
    })
    .await;
    wait_for(&world, &region, 200, |found| {
        found.iter().any(|s| {
            s.kind == EntityType::Villager
                && crate::world::distance_sq(s.position, [10.5, SKY as f64 + 1.0, 7.5]) < 4.0
        })
    })
    .await;
    world.ticks(320).await;
    assert!(
        !has_memory(&data(&region, id).await, "minecraft:heard_bell_time"),
        "the alarm passes"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sleeping_villager_wakes_when_its_bed_is_broken() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    set_time(&world, NIGHT);
    let head = BlockPos::new(10, SKY + 1, 7);
    let foot = BlockPos::new(10, SKY + 1, 8);
    let mut blocks = pen(4, 11);
    blocks.extend(bed(head, foot));
    place_blocks(&world, &region, blocks).await;
    summon(
        &region,
        EntityType::Villager,
        villager("minecraft:none"),
        [5.5, SKY as f64 + 1.0, 5.5],
    )
    .await;
    world.ticks(1).await;
    let id = villager_ids(&region).await[0];
    wait_for_data(&world, &region, id, 800, |nbt| {
        has_memory(nbt, "minecraft:last_slept")
    })
    .await;
    place_blocks(
        &world,
        &region,
        [
            (head, Block::Air.default_state()),
            (foot, Block::Air.default_state()),
        ],
    )
    .await;
    wait_for_data(&world, &region, id, 40, |nbt| {
        has_memory(nbt, "minecraft:last_woken")
    })
    .await;
    world.shutdown().await;
}

fn crop_age(state: Option<tuff_vanilla_data::BlockState>) -> Option<u8> {
    state.and_then(|s| s.age())
}

async fn farm(
    world: &TestWorld,
    region: &RegionHandle,
    inventory: &[(&str, i32)],
    age: u8,
) -> (i32, Vec<BlockPos>) {
    set_time(world, DAY);
    let composter = BlockPos::new(8, SKY + 1, 8);
    let mut blocks = pen(4, 11);
    blocks.push((composter, Block::Composter.default_state()));
    let farmland = Block::Farmland
        .default_state()
        .with("moisture", "7")
        .unwrap();
    let wheat = Block::Wheat
        .default_state()
        .with("age", &age.to_string())
        .unwrap();
    let mut crops = Vec::new();
    for x in [6, 7] {
        blocks.push((BlockPos::new(x, SKY, 6), farmland));
        blocks.push((BlockPos::new(x, SKY + 1, 6), wheat));
        crops.push(BlockPos::new(x, SKY + 1, 6));
    }
    place_blocks(world, region, blocks).await;
    let mut nbt = villager("minecraft:farmer");
    nbt.insert("Xp", 5);
    nbt.insert("CanPickUpLoot", true);
    nbt.insert("Inventory", items(inventory));
    nbt.insert(
        "Brain",
        brain(vec![("minecraft:job_site", memory_place(composter))]),
    );
    summon(
        region,
        EntityType::Villager,
        nbt,
        [7.5, SKY as f64 + 1.0, 7.5],
    )
    .await;
    world.ticks(1).await;
    (villager_ids(region).await[0], crops)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn farmers_harvest_ripe_crops_and_replant_them() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    let (id, crops) = farm(&world, &region, &[("minecraft:wheat_seeds", 4)], 7).await;
    let mut replanted = false;
    for _ in 0..3000 {
        let mut young = 0;
        for crop in &crops {
            if crop_age(block_at(&region, *crop).await).is_some_and(|age| age < 7) {
                young += 1;
            }
        }
        if young == crops.len() {
            replanted = true;
            break;
        }
        world.ticks(1).await;
    }
    assert!(replanted, "both crops were harvested and sown again");
    let _ = id;
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn farmers_with_bone_meal_grow_young_crops() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    let (id, crops) = farm(&world, &region, &[("minecraft:bone_meal", 8)], 0).await;
    let mut grown = false;
    let mut held_bone_meal = false;
    for _ in 0..4000 {
        for crop in &crops {
            if crop_age(block_at(&region, *crop).await).is_some_and(|age| age >= 2) {
                grown = true;
            }
        }
        let held = data(&region, id).await;
        held_bone_meal |= held
            .get_compound("equipment")
            .and_then(|equipment| equipment.get_compound("mainhand"))
            .is_some_and(|stack| stack.get_str("id") == Some("minecraft:bone_meal"));
        if grown {
            break;
        }
        world.ticks(1).await;
    }
    assert!(grown, "bone meal advanced a crop by at least two stages");
    assert!(
        held_bone_meal,
        "it held bone meal in its main hand while working"
    );
    let nbt = data(&region, id).await;
    assert!(count_of(&nbt, "minecraft:bone_meal") < 8);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn farmers_bake_wheat_into_bread_at_their_composter() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    let (id, _) = farm(&world, &region, &[("minecraft:wheat", 10)], 0).await;
    let baked = wait_for_data(&world, &region, id, 4000, |nbt| {
        count_of(nbt, "minecraft:bread") > 0
    })
    .await;
    assert_eq!(count_of(&baked, "minecraft:bread"), 3);
    assert_eq!(count_of(&baked, "minecraft:wheat"), 1);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_farmer_with_a_full_inventory_drops_the_bread_it_cannot_carry() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    let mut inventory = vec![("minecraft:wheat", 10)];
    inventory.extend([("minecraft:stone", 64); 7]);
    let (id, _) = farm(&world, &region, &inventory, 0).await;
    let mut dropped = 0;
    for _ in 0..4000 {
        world.ticks(1).await;
        dropped = snapshots(&region)
            .await
            .into_iter()
            .filter(|s| s.kind == EntityType::Item)
            .count();
        if dropped > 0 {
            break;
        }
    }
    assert!(dropped > 0, "the bread landed in the world");
    assert_eq!(count_of(&data(&region, id).await, "minecraft:bread"), 0);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_jobless_villager_yields_a_workstation_to_one_who_has_the_trade() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    set_time(&world, DAY);
    let composter = BlockPos::new(8, SKY + 1, 8);
    let mut blocks = pen(4, 11);
    blocks.push((composter, Block::Composter.default_state()));
    place_blocks(&world, &region, blocks).await;
    summon(
        &region,
        EntityType::Villager,
        villager("minecraft:none"),
        [8.5, SKY as f64 + 1.0, 10.5],
    )
    .await;
    let mut farmer = villager("minecraft:farmer");
    farmer.insert("Xp", 5);
    summon(
        &region,
        EntityType::Villager,
        farmer,
        [5.5, SKY as f64 + 1.0, 5.5],
    )
    .await;
    world.ticks(1).await;
    let ids = villager_ids(&region).await;
    let (jobless, farmer) = (ids[0], ids[1]);
    wait_for_data(&world, &region, farmer, 800, |nbt| {
        has_memory(nbt, "minecraft:job_site")
    })
    .await;
    world.ticks(100).await;
    let nbt = data(&region, jobless).await;
    assert!(!has_memory(&nbt, "minecraft:job_site"));
    assert_eq!(
        nbt.get_compound("VillagerData")
            .and_then(|d| d.get_str("profession")),
        Some("minecraft:none")
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn baby_villagers_bounce_on_beds() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    set_time(&world, DAY);
    let head = BlockPos::new(10, SKY + 1, 7);
    let foot = BlockPos::new(10, SKY + 1, 8);
    let mut blocks = pen(4, 11);
    blocks.extend(bed(head, foot));
    place_blocks(&world, &region, blocks).await;
    summon(
        &region,
        EntityType::Villager,
        villager("minecraft:none").with("Age", -24000),
        [5.5, SKY as f64 + 1.0, 5.5],
    )
    .await;
    world.ticks(1).await;
    let bed_top = SKY as f64 + 1.0 + 0.5625;
    let mut jumped = false;
    for _ in 0..3000 {
        let kids = mobs(&region, EntityType::Villager).await;
        let baby = &kids[0];
        let near_bed = (baby.position[0] - 10.5).abs() < 0.6
            && (baby.position[2] > 6.9 && baby.position[2] < 9.1);
        if near_bed && baby.position[1] > bed_top + 0.25 {
            jumped = true;
            break;
        }
        world.ticks(1).await;
    }
    assert!(jumped, "the baby hopped on the bed");
    world.shutdown().await;
}

async fn raid_phase(
    region: &RegionHandle,
    center: BlockPos,
    phase: Option<tuff_feature_builtin_vanilla_mobs::raid::Phase>,
) {
    region
        .request(RegionRequest::WhenLoaded {
            min: center,
            max: center,
            run: RegionTask::new(move |world| {
                let raids = world
                    .state()
                    .get::<tuff_feature_builtin_vanilla_mobs::raid::Raids>();
                if let Some(phase) = phase {
                    let id = raids.start(center);
                    raids.set_phase(id, phase);
                }
            }),
        })
        .await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn villagers_across_a_region_border_hear_a_bell() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (west_chunk, east_chunk) = (ChunkPos::new(31, 0), ChunkPos::new(32, 0));
    let west = world.region(west_chunk.region()).await;
    let east = world.region(east_chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    subscribe(&west, &viewer, &mut events, &[west_chunk]).await;
    subscribe(&east, &viewer, &mut events, &[east_chunk]).await;
    set_time(&world, DAY);
    let pen: Vec<_> = pen(4, 11)
        .into_iter()
        .map(|(pos, state)| (BlockPos::new(pos.x + 512, pos.y, pos.z), state))
        .collect();
    place_blocks(&world, &east, pen).await;
    let bell = BlockPos::new(509, SKY + 1, 8);
    place_blocks(
        &world,
        &west,
        [
            (BlockPos::new(509, SKY, 8), Block::Stone.default_state()),
            (bell, Block::Bell.default_state()),
        ],
    )
    .await;
    summon(
        &east,
        EntityType::Villager,
        villager("minecraft:none"),
        [519.5, SKY as f64 + 1.0, 7.5],
    )
    .await;
    world.ticks(1).await;
    let id = villager_ids(&east).await[0];
    assert!(!has_memory(
        &data(&east, id).await,
        "minecraft:heard_bell_time"
    ));

    let mut click = place(&viewer, bell, Item::Stick, 1);
    click.item = None;
    click.held = None;
    click.face = tuff_world_model::Face::North;
    click.cursor = [0.5, 0.5, 0.0];
    west.request(RegionRequest::Place(click)).await;
    wait_for_data(&world, &east, id, 5, |nbt| {
        has_memory(nbt, "minecraft:heard_bell_time")
    })
    .await;
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn villagers_walk_to_the_bell_and_ring_it_before_a_raid() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    set_time(&world, DAY);
    let bell = BlockPos::new(10, SKY + 1, 10);
    let mut blocks = pen(4, 13);
    blocks.push((bell, Block::Bell.default_state()));
    place_blocks(&world, &region, blocks).await;
    let mut nbt = villager("minecraft:none");
    nbt.insert(
        "Brain",
        brain(vec![("minecraft:meeting_point", memory_place(bell))]),
    );
    summon(
        &region,
        EntityType::Villager,
        nbt,
        [5.5, SKY as f64 + 1.0, 5.5],
    )
    .await;
    world.ticks(1).await;
    let id = villager_ids(&region).await[0];
    world.ticks(100).await;
    assert!(
        !has_memory(&data(&region, id).await, "minecraft:heard_bell_time"),
        "nothing to ring for"
    );
    raid_phase(
        &region,
        bell,
        Some(tuff_feature_builtin_vanilla_mobs::raid::Phase::Preparing),
    )
    .await;
    wait_for_data(&world, &region, id, 800, |nbt| {
        has_memory(nbt, "minecraft:heard_bell_time")
    })
    .await;
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn villagers_hide_in_a_bed_while_raiders_are_out() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    set_time(&world, DAY);
    let head = BlockPos::new(10, SKY + 1, 7);
    let foot = BlockPos::new(10, SKY + 1, 8);
    let mut blocks = pen(4, 13);
    blocks.extend(bed(head, foot));
    place_blocks(&world, &region, blocks).await;
    summon(
        &region,
        EntityType::Villager,
        villager("minecraft:none"),
        [5.5, SKY as f64 + 1.0, 5.5],
    )
    .await;
    raid_phase(
        &region,
        head,
        Some(tuff_feature_builtin_vanilla_mobs::raid::Phase::Active),
    )
    .await;
    wait_for(&world, &region, 800, |found| {
        found.iter().any(|s| {
            s.kind == EntityType::Villager
                && crate::world::distance_sq(s.position, [10.5, SKY as f64 + 1.0, 7.5]) < 4.0
        })
    })
    .await;
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn farmers_compost_their_spare_seeds_and_keep_ten() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    let (id, _) = farm(&world, &region, &[("minecraft:wheat_seeds", 25)], 0).await;
    let composted = wait_for_data(&world, &region, id, 4000, |nbt| {
        count_of(nbt, "minecraft:wheat_seeds") < 25
    })
    .await;
    let seeds = count_of(&composted, "minecraft:wheat_seeds");
    assert!((10..25).contains(&seeds), "{seeds} seeds left");
    let composter = block_at(&region, BlockPos::new(8, SKY + 1, 8))
        .await
        .unwrap();
    let level = tuff_vanilla_common::composter::level(composter).unwrap();
    assert!(
        seeds == 10 || level == 7,
        "{seeds} seeds left at level {level}"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn villagers_claim_a_bed_in_a_neighbour_region_beyond_its_halo() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let west = ChunkPos::new(31, 0);
    let east = [ChunkPos::new(32, 0), ChunkPos::new(33, 0)];
    let west_region = world.region(west.region()).await;
    let east_region = world.region(east[0].region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    subscribe(&west_region, &viewer, &mut events, &[west]).await;
    subscribe(&east_region, &viewer, &mut events, &east).await;
    set_time(&world, DAY);
    let floor = |xs: std::ops::RangeInclusive<i32>| {
        xs.flat_map(|x| {
            (2..=6).map(move |z| (BlockPos::new(x, SKY, z), Block::Stone.default_state()))
        })
        .collect::<Vec<_>>()
    };
    place_blocks(&world, &west_region, floor(500..=511)).await;
    let head = BlockPos::new(532, SKY + 1, 3);
    let foot = BlockPos::new(532, SKY + 1, 4);
    let mut blocks = floor(512..=535);
    blocks.extend(bed(head, foot));
    place_blocks(&world, &east_region, blocks).await;
    summon(
        &west_region,
        EntityType::Villager,
        villager("minecraft:none"),
        [501.5, SKY as f64 + 1.0, 4.5],
    )
    .await;
    world.ticks(1).await;
    let id = villager_ids(&west_region).await[0];
    let nbt = wait_for_data(&world, &west_region, id, 400, |nbt| {
        has_memory(nbt, "minecraft:home")
    })
    .await;
    let pos = nbt
        .get_compound("Brain")
        .and_then(|b| b.get_compound("memories"))
        .and_then(|m| m.get_compound("minecraft:home"))
        .and_then(|h| h.get_compound("value"))
        .and_then(|v| match v.get("pos") {
            Some(Tag::IntArray(p)) => Some(p.clone()),
            _ => None,
        });
    assert_eq!(pos, Some(vec![head.x, head.y, head.z]));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_resonating_bell_makes_raiders_across_a_region_border_glow() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (west_chunk, east_chunk) = (ChunkPos::new(31, 0), ChunkPos::new(32, 0));
    let west = world.region(west_chunk.region()).await;
    let east = world.region(east_chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    subscribe(&west, &viewer, &mut events, &[west_chunk]).await;
    subscribe(&east, &viewer, &mut events, &[east_chunk]).await;
    let floor: Vec<_> = (500..530)
        .map(|x| (BlockPos::new(x, SKY, 8), Block::Stone.default_state()))
        .collect();
    place_blocks(&world, &west, floor.clone()).await;
    place_blocks(&world, &east, floor).await;
    let bell = BlockPos::new(509, SKY + 1, 8);
    place_blocks(&world, &west, [(bell, Block::Bell.default_state())]).await;
    let raider = || {
        Compound::new()
            .with("NoAI", true)
            .with("PersistenceRequired", true)
    };
    let y = SKY as f64 + 1.0;
    summon(&west, EntityType::Pillager, raider(), [505.5, y, 8.5]).await;
    summon(&east, EntityType::Pillager, raider(), [519.5, y, 8.5]).await;
    world.ticks(2).await;
    let far = mobs(&east, EntityType::Pillager).await[0].id;
    let mut click = place(&viewer, bell, Item::Stick, 1);
    click.item = None;
    click.held = None;
    click.face = tuff_world_model::Face::North;
    click.cursor = [0.5, 0.5, 0.0];
    west.request(RegionRequest::Place(click)).await;
    world.ticks(70).await;
    let nbt = data(&east, far).await;
    assert!(
        format!("{nbt:?}").contains("glowing"),
        "the raider east of the border glows"
    );
    world.shutdown().await;
}
