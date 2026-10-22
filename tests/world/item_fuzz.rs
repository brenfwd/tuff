//! Seeded random sequences of menu clicks, hopper moves, piston pushes,
//! item entity merges and pickups and region handoffs, checking after
//! every step that no item was made or lost.

use super::*;
use crate::world::conservation::Subject;
use crate::world::messages::{BlockOp, BlockOpResult, MenuSource};
use std::collections::BTreeMap;
use tuff_item::item::conservation;
use tuff_item::item::{ItemSnapshot, ItemStack, MintReason};
use tuff_nbt::{Compound, Tag};
use tuff_vanilla_data::registry::EntityType;
use tuff_vanilla_data::{BlockState, Item};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn range(&mut self, low: i32, high: i32) -> i32 {
        low + self.below((high - low + 1) as u64) as i32
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }
}

const KINDS: [(Item, &str, u8); 3] = [
    (Item::Dirt, "minecraft:dirt", 64),
    (Item::Diamond, "minecraft:diamond", 64),
    (Item::EnderPearl, "minecraft:ender_pearl", 16),
];

fn kind(name: &str) -> (Item, u8) {
    KINDS
        .iter()
        .find(|(_, id, _)| *id == name)
        .map(|(item, _, max)| (*item, *max))
        .unwrap_or_else(|| panic!("unexpected item {name}"))
}

type Census = BTreeMap<String, i64>;

fn add(census: &mut Census, id: &str, count: i64) {
    *census.entry(id.to_owned()).or_default() += count;
    census.retain(|_, n| *n != 0);
}

fn slots_of(nbt: &Compound, census: &mut Census) {
    let Some(Tag::List(slots)) = nbt.get("Items") else {
        return;
    };
    for slot in slots.iter().filter_map(Tag::as_compound) {
        let id = slot.get_str("id").unwrap_or_default();
        add(census, id, i64::from(slot.get_int("count").unwrap_or(1)));
    }
}

fn random_items(rng: &mut Rng, slots: u8, filled: u64) -> Compound {
    let mut items = Vec::new();
    for slot in 0..slots {
        if !rng.chance(filled) {
            continue;
        }
        let (_, id, max) = KINDS[rng.below(KINDS.len() as u64) as usize];
        items.push(Tag::Compound(
            Compound::new()
                .with("Slot", Tag::Byte(slot as i8))
                .with("id", id)
                .with("count", Tag::Int(rng.range(1, i32::from(max)))),
        ));
    }
    Compound::new().with("Items", Tag::List(items))
}

async fn blocks(region: &RegionHandle, op: BlockOp) -> BlockOpResult {
    let (reply, answer) = oneshot::channel();
    region.request(RegionRequest::Blocks { op, reply }).await;
    answer.await.unwrap()
}

async fn paste(region: &RegionHandle, changes: Vec<(BlockPos, &str, Option<Compound>)>) {
    let changes = changes
        .into_iter()
        .map(|(pos, state, nbt)| (pos, state.parse::<BlockState>().unwrap(), nbt))
        .collect();
    blocks(
        region,
        BlockOp::Paste {
            changes,
            strict: false,
        },
    )
    .await;
}

async fn block_entities(region: &RegionHandle, positions: Vec<BlockPos>, census: &mut Census) {
    match blocks(region, BlockOp::Copy { positions }).await {
        BlockOpResult::Copied(found) => {
            for (_, nbt) in found.into_iter().flatten() {
                if let Some(nbt) = nbt {
                    slots_of(&nbt, census);
                }
            }
        }
        other => panic!("{other:?}"),
    }
}

async fn item_entities(region: &RegionHandle, census: &mut Census) {
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::EntitiesWithData { reply })
        .await;
    for entity in answer.await.unwrap() {
        if entity.kind != EntityType::Item {
            continue;
        }
        let item = entity
            .nbt
            .as_ref()
            .and_then(|nbt| nbt.get("Item"))
            .and_then(Tag::as_compound)
            .expect("an item entity saves its stack");
        add(
            census,
            item.get_str("id").unwrap_or_default(),
            i64::from(item.get_int("count").unwrap_or(1)),
        );
    }
}

fn lost_so_far() -> u64 {
    conservation::totals().lost
}

fn chest_pos() -> BlockPos {
    BlockPos::new(1, Generator::new(1).height(1, 1) + 1, 1)
}

async fn chest_slots(region: &RegionHandle) -> Vec<Option<(Item, u8)>> {
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::BlockWithEntity {
            pos: chest_pos(),
            reply,
        })
        .await;
    let (_, nbt) = answer.await.unwrap().expect("the chest is loaded");
    let mut slots = vec![None; 27];
    if let Some(Tag::List(items)) = nbt.as_ref().and_then(|nbt| nbt.get("Items")) {
        for slot in items.iter().filter_map(Tag::as_compound) {
            let (item, _) = kind(slot.get_str("id").unwrap());
            let count = u8::try_from(slot.get_int("count").unwrap()).unwrap();
            slots[slot.get_byte("Slot").unwrap() as usize] = Some((item, count));
        }
    }
    slots
}

fn picture(slot: Option<(Item, u8)>) -> Option<ItemSnapshot> {
    slot.filter(|(_, count)| *count > 0)
        .map(|(item, count)| ItemSnapshot::of(&ItemStack::mint(item, count, MintReason::Test)))
}

fn change(slot: usize, expected: Option<(Item, u8)>, new: Option<(Item, u8)>) -> ContainerChange {
    ContainerChange {
        pos: chest_pos(),
        slot,
        expected: picture(expected),
        new: picture(new),
    }
}

fn name(item: Item) -> &'static str {
    KINDS.iter().find(|(i, ..)| *i == item).unwrap().1
}

fn chest_census(slots: &[Option<(Item, u8)>], hand: &[ItemStack]) -> Census {
    let mut census = Census::new();
    for (item, count) in slots.iter().flatten() {
        add(&mut census, name(*item), i64::from(*count));
    }
    for stack in hand {
        add(&mut census, name(stack.item), i64::from(stack.count()));
    }
    census
}

async fn click(
    region: &RegionHandle,
    player: &PlayerHandle,
    events: &mut mpsc::UnboundedReceiver<PlayerEvent>,
    changes: Vec<ContainerChange>,
    pool: Vec<ItemStack>,
) -> (bool, Vec<ItemStack>) {
    region
        .request(RegionRequest::ContainerTransaction {
            player: player.clone(),
            window_id: 1,
            changes,
            pool,
            took_result: false,
        })
        .await;
    expect(events, |event| match event {
        PlayerEvent::Transaction { accepted, pool, .. } => Some((accepted, pool)),
        _ => None,
    })
    .await
}

fn room(slot: Option<(Item, u8)>, item: Item) -> u8 {
    match slot {
        None => kind(name(item)).1,
        Some((held, count)) if held == item => kind(name(item)).1 - count,
        Some(_) => 0,
    }
}

async fn menu_clicks(seed: u64) {
    let mut rng = Rng(seed);
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let pos = ChunkPos::new(0, 0);
    let region = world.region(pos.region()).await;
    let (player, mut events) = PlayerHandle::new(100);
    subscribe(&region, &player, &mut events, &[pos]).await;
    paste(
        &region,
        vec![(
            chest_pos(),
            "minecraft:chest[facing=north]",
            Some(random_items(&mut rng, 27, 40).with("id", "minecraft:chest")),
        )],
    )
    .await;
    region
        .request(RegionRequest::Place(place(
            &player,
            chest_pos(),
            Item::Torch,
            1,
        )))
        .await;
    let open = expect(&mut events, container_menu).await;
    assert!(matches!(open.source, MenuSource::Blocks { .. }));
    let lost = lost_so_far();
    let mut hand: Vec<ItemStack> = Vec::new();
    let mut accepted_clicks = 0;
    let start = chest_census(&chest_slots(&region).await, &hand);

    for step in 0..80 {
        let slots = chest_slots(&region).await;
        let full: Vec<usize> = (0..27).filter(|s| slots[*s].is_some()).collect();
        let (changes, pool, honest) = match rng.below(6) {
            0 | 1 if !full.is_empty() => {
                let from = full[rng.below(full.len() as u64) as usize];
                let (item, count) = slots[from].unwrap();
                let to = rng.below(27) as usize;
                let n = (rng.range(1, i32::from(count)) as u8).min(room(slots[to], item));
                if to == from || n == 0 {
                    continue;
                }
                let landed = slots[to].map_or(n, |(_, held)| held + n);
                (
                    vec![
                        change(from, slots[from], Some((item, count - n))),
                        change(to, slots[to], Some((item, landed))),
                    ],
                    Vec::new(),
                    true,
                )
            }
            2 if !full.is_empty() => {
                let from = full[rng.below(full.len() as u64) as usize];
                let (item, count) = slots[from].unwrap();
                let n = rng.range(1, i32::from(count)) as u8;
                (
                    vec![change(from, slots[from], Some((item, count - n)))],
                    Vec::new(),
                    true,
                )
            }
            3 if !hand.is_empty() => {
                let stack = hand.swap_remove(rng.below(hand.len() as u64) as usize);
                let to = rng.below(27) as usize;
                let n = room(slots[to], stack.item).min(stack.count());
                if n == 0 {
                    hand.push(stack);
                    continue;
                }
                let landed = slots[to].map_or(n, |(_, held)| held + n);
                (
                    vec![change(to, slots[to], Some((stack.item, landed)))],
                    vec![stack],
                    true,
                )
            }
            4 => {
                let to = rng.below(27) as usize;
                let (item, ..) = KINDS[rng.below(KINDS.len() as u64) as usize];
                if room(slots[to], item) == 0 {
                    continue;
                }
                let landed = slots[to].map_or(1, |(_, held)| held + 1);
                let pool = hand
                    .iter()
                    .position(|stack| stack.item != item)
                    .map(|at| vec![hand.swap_remove(at)])
                    .unwrap_or_default();
                (
                    vec![change(to, slots[to], Some((item, landed)))],
                    pool,
                    false,
                )
            }
            5 if !full.is_empty() => {
                let from = full[rng.below(full.len() as u64) as usize];
                let (item, count) = slots[from].unwrap();
                let shown = if count > 1 { count - 1 } else { count + 1 };
                (
                    vec![change(from, Some((item, shown)), None)],
                    Vec::new(),
                    false,
                )
            }
            _ => continue,
        };
        let (accepted, back) = click(&region, &player, &mut events, changes, pool).await;
        assert_eq!(accepted, honest, "seed {seed} step {step}");
        accepted_clicks += usize::from(accepted);
        hand.extend(back);
        let now = chest_census(&chest_slots(&region).await, &hand);
        assert_eq!(now, start, "seed {seed} step {step}: items made or lost");
    }
    assert!(
        accepted_clicks >= 20,
        "seed {seed}: {accepted_clicks} clicks"
    );
    assert_eq!(lost_so_far() - lost, 0, "seed {seed}: a stack was dropped");
    let dimension = world.dimension(OVERWORLD);
    assert!(
        !world
            .shared
            .conservation
            .quarantined(Subject::Region(dimension, region.pos)),
        "seed {seed}: honest clicks never quarantine"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn random_menu_clicks_neither_make_nor_lose_items() {
    for seed in 1..=8 {
        menu_clicks(seed).await;
    }
}

const Y: i32 = 200;
const LANES: [i32; 5] = [81, 84, 87, 90, 93];
const BORDER: i32 = 512;

struct Lane {
    z: i32,
    x: i32,
    piston: bool,
}

async fn census(west: &RegionHandle, east: &RegionHandle) -> Census {
    let mut census = Census::new();
    let positions = |xs: std::ops::Range<i32>| -> Vec<BlockPos> {
        xs.flat_map(|x| (80..96).flat_map(move |z| (Y..Y + 3).map(move |y| BlockPos::new(x, y, z))))
            .collect()
    };
    block_entities(west, positions(500..BORDER), &mut census).await;
    block_entities(east, positions(BORDER..524), &mut census).await;
    item_entities(west, &mut census).await;
    item_entities(east, &mut census).await;
    census
}

async fn east_census(east: &RegionHandle) -> Census {
    let mut census = Census::new();
    let positions = (BORDER..524)
        .flat_map(|x| (80..96).flat_map(move |z| (Y..Y + 3).map(move |y| BlockPos::new(x, y, z))))
        .collect();
    block_entities(east, positions, &mut census).await;
    item_entities(east, &mut census).await;
    census
}

async fn drop_item(region: &RegionHandle, rng: &mut Rng, x: f64, z: f64, expected: &mut Census) {
    let (_, id, max) = KINDS[rng.below(KINDS.len() as u64) as usize];
    let count = rng.range(1, i32::from(max));
    let motion = (rng.range(-30, 30) as f64) / 100.0;
    let mut nbt = Compound::new()
        .with("id", "minecraft:item")
        .with("Item", Compound::new().with("id", id).with("count", count));
    nbt.insert(
        "Motion",
        Tag::List(vec![
            Tag::Double(motion),
            Tag::Double(0.0),
            Tag::Double(0.0),
        ]),
    );
    let (reply, summoned) = oneshot::channel();
    region
        .request(RegionRequest::Summon {
            nbt,
            position: [x, f64::from(Y + 1), z],
            fresh: false,
            reply,
        })
        .await;
    assert!(summoned.await.unwrap());
    add(expected, id, i64::from(count));
}

async fn machines(seed: u64) {
    let mut rng = Rng(seed);
    let dir = TestDir::new();
    let (player, mut events) = PlayerHandle::new(100);
    let world = TestWorld::start_with_features(
        dir.path(),
        vec![Box::new(
            tuff_feature_builtin_moving_block_entities::MovingBlockEntities,
        )],
    );
    view_around(&world, &player, &mut events, ChunkPos::new(31, 5)).await;
    view_around(&world, &player, &mut events, ChunkPos::new(32, 5)).await;
    world.ticks(1).await;
    let west = world.region(ChunkPos::new(31, 5).region()).await;
    let east = world.region(ChunkPos::new(32, 5).region()).await;
    let floor = |xs: std::ops::Range<i32>| -> Vec<_> {
        xs.flat_map(|x| (80..96).map(move |z| BlockPos::new(x, Y - 1, z)))
            .map(|pos| (pos, Block::Stone.default_state()))
            .collect()
    };
    place_blocks(&world, &west, floor(500..BORDER)).await;
    place_blocks(&world, &east, floor(BORDER..524)).await;

    let at = |x: i32, z: i32| BlockPos::new(x, Y, z);
    let owner = |x: i32| if x < BORDER { &west } else { &east };
    let mut lanes = Vec::new();
    for z in LANES {
        if rng.chance(50) {
            let x = rng.range(BORDER - 4, BORDER - 2);
            let piston = if rng.chance(50) {
                "minecraft:sticky_piston[facing=east]"
            } else {
                "minecraft:piston[facing=east]"
            };
            let (block, id, slots) = if rng.chance(70) {
                ("minecraft:chest[facing=north]", "minecraft:chest", 27)
            } else {
                ("minecraft:hopper[facing=east]", "minecraft:hopper", 5)
            };
            let items = random_items(&mut rng, slots, 30).with("id", id);
            paste(owner(x), vec![(at(x, z), piston, None)]).await;
            paste(owner(x + 1), vec![(at(x + 1, z), block, Some(items))]).await;
            lanes.push(Lane { z, x, piston: true });
        } else {
            let start = rng.range(BORDER - 4, BORDER - 1);
            let end = rng.range(BORDER, BORDER + 3);
            for x in start..end {
                let items = random_items(&mut rng, 5, 20).with("id", "minecraft:hopper");
                paste(
                    owner(x),
                    vec![(at(x, z), "minecraft:hopper[facing=east]", Some(items))],
                )
                .await;
            }
            let items = random_items(&mut rng, 27, 10).with("id", "minecraft:chest");
            paste(
                owner(end),
                vec![(at(end, z), "minecraft:chest[facing=north]", Some(items))],
            )
            .await;
            lanes.push(Lane {
                z,
                x: start,
                piston: false,
            });
        }
    }
    world.ticks(2).await;
    let lost = lost_so_far();
    let mut expected = census(&west, &east).await;
    let mut powered = vec![false; lanes.len()];
    let east_at_start = east_census(&east).await;
    let mut east_changed = false;

    for step in 0..30 {
        let lane = rng.below(lanes.len() as u64) as usize;
        let z = lanes[lane].z;
        if lanes[lane].piston && rng.chance(50) {
            powered[lane] = !powered[lane];
            let block = if powered[lane] {
                "minecraft:redstone_block"
            } else {
                "minecraft:air"
            };
            let power = BlockPos::new(lanes[lane].x, Y, z + 1);
            paste(&west, vec![(power, block, None)]).await;
        } else {
            let x = f64::from(rng.range((BORDER - 3) * 10, (BORDER + 3) * 10)) / 10.0;
            let region = if x < f64::from(BORDER) { &west } else { &east };
            drop_item(region, &mut rng, x, f64::from(z) + 0.5, &mut expected).await;
        }
        world.ticks(rng.range(1, 10) as u64).await;
        world.ticks(4).await;
        let now = census(&west, &east).await;
        assert_eq!(now, expected, "seed {seed} step {step}: items made or lost");
        east_changed |= east_census(&east).await != east_at_start;
    }
    assert!(east_changed, "seed {seed}: nothing crossed the border");
    assert_eq!(lost_so_far() - lost, 0, "seed {seed}: a stack was dropped");
    let dimension = world.dimension(OVERWORLD);
    for region in [&west, &east] {
        assert!(
            !world
                .shared
                .conservation
                .quarantined(Subject::Region(dimension, region.pos)),
            "seed {seed}: honest moves never quarantine"
        );
    }
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn random_hoppers_pistons_and_dropped_items_across_a_region_border_keep_every_item() {
    for seed in 11..=16 {
        machines(seed).await;
    }
}
