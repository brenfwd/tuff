//! Block conservation: a piston moving a rail, carpet, sand or TNT together
//! with, or away from, the block it stands on, and falling blocks and
//! endermen holding blocks. No block may exist twice: on every tick no kind
//! of block in reach, counted where it stands, inside moving pistons, held
//! by a falling block or an enderman, as primed TNT and as dropped items,
//! outnumbers what was built, and once the piston settles each adds up to
//! it again. Across a
//! region border only the settled count is checked: the two regions are
//! read one after the other, a tick apart from the block's point of view.

use super::*;
use crate::world::messages::{BlockOp, BlockOpResult};
use std::collections::BTreeMap;
use tuff_nbt::Compound;
use tuff_vanilla_data::BlockState;
use tuff_vanilla_data::registry::EntityType;

const Y: i32 = 200;
const Z: i32 = 86;
const INSIDE: i32 = 84;
const ACROSS_BORDER: i32 = 510;

struct Bench {
    world: TestWorld,
    west: RegionHandle,
    east: RegionHandle,
    _player: PlayerHandle,
    _events: mpsc::UnboundedReceiver<PlayerEvent>,
}

const DUPE_KEYS: [&str; 4] = [
    tuff_core::config::BLOCKS_PERMANENT_BLOCK_BREAKING,
    tuff_core::config::BLOCKS_TNT_DUPLICATION,
    tuff_core::config::BLOCKS_CARPET_RAIL_DUPLICATION,
    tuff_core::config::BLOCKS_GRAVITY_PORTAL_DUPLICATION,
];

async fn bench(dir: &Path, dupes: bool) -> Bench {
    let (player, mut events) = PlayerHandle::new(100);
    let world = TestWorld::start(dir);
    for key in DUPE_KEYS {
        world
            .shared
            .config
            .set(key, tuff_core::config::Value::Bool(dupes))
            .unwrap();
    }
    for center in [
        ChunkPos::new(5, 5),
        ChunkPos::new(31, 5),
        ChunkPos::new(32, 5),
    ] {
        view_around(&world, &player, &mut events, center).await;
    }
    world.ticks(1).await;
    let west = world.region(ChunkPos::new(5, 5).region()).await;
    let east = world.region(ChunkPos::new(32, 5).region()).await;
    Bench {
        world,
        west,
        east,
        _player: player,
        _events: events,
    }
}

impl Bench {
    fn region(&self, pos: BlockPos) -> &RegionHandle {
        if pos.x >= 512 { &self.east } else { &self.west }
    }

    async fn blocks(&self, pos: BlockPos, op: BlockOp) -> BlockOpResult {
        let (reply, answer) = oneshot::channel();
        self.region(pos)
            .request(RegionRequest::Blocks { op, reply })
            .await;
        answer.await.unwrap()
    }

    async fn paste(&self, pos: BlockPos, state: &str) {
        self.paste_as(pos, state, false).await;
    }

    /// `strict` leaves out the placement reactions, so TNT can stand next
    /// to power without lighting.
    async fn paste_as(&self, pos: BlockPos, state: &str, strict: bool) {
        let state: BlockState = state.parse().unwrap();
        let changes = vec![(pos, state, None)];
        self.blocks(pos, BlockOp::Paste { changes, strict }).await;
    }

    /// Every block standing in `positions` or carried by a moving piston
    /// there, by id, into `tally`.
    async fn count_blocks(&self, positions: Vec<BlockPos>, tally: &mut Tally) {
        let Some(&first) = positions.first() else {
            return;
        };
        let BlockOpResult::Copied(found) = self.blocks(first, BlockOp::Copy { positions }).await
        else {
            panic!("copy failed");
        };
        for (state, nbt) in found.into_iter().flatten() {
            let name = match state.block() {
                Block::Air | Block::PistonHead => continue,
                Block::MovingPiston => match nbt.as_ref().and_then(carried) {
                    Some(name) => name,
                    None => continue,
                },
                block => block.name().to_owned(),
            };
            *tally.entry(name).or_default() += 1;
        }
    }

    /// Every dropped item near `x0`, by id, into `tally`.
    async fn count_drops(&self, x0: i32, tally: &mut Tally) {
        for region in [&self.west, &self.east] {
            let (reply, answer) = oneshot::channel();
            region.request(RegionRequest::Entities { reply }).await;
            for entity in answer.await.unwrap() {
                if (entity.position[0] - f64::from(x0)).abs() > 16.0 {
                    continue;
                }
                if entity.kind == EntityType::Tnt {
                    *tally.entry("minecraft:tnt".to_owned()).or_default() += 1;
                    continue;
                }
                let held = match entity.kind {
                    EntityType::FallingBlock => Some("BlockState"),
                    EntityType::Enderman => Some("carriedBlockState"),
                    _ => None,
                };
                if let Some(key) = held {
                    let (reply, data) = oneshot::channel();
                    region
                        .request(RegionRequest::EntityData {
                            id: entity.id,
                            reply,
                        })
                        .await;
                    let state = data.await.unwrap().and_then(|d| {
                        d.get(key)
                            .map(tuff_core::storage::chunk_nbt::decode_block_state)
                    });
                    if let Some(state) = state.filter(|s| s.block() != Block::Air) {
                        *tally.entry(state.block().name().to_owned()).or_default() += 1;
                    }
                    continue;
                }
                if entity.kind != EntityType::Item {
                    continue;
                }
                let (reply, data) = oneshot::channel();
                region
                    .request(RegionRequest::EntityData {
                        id: entity.id,
                        reply,
                    })
                    .await;
                let Some(stack) = data
                    .await
                    .unwrap()
                    .and_then(|d| d.get_compound("Item").cloned())
                else {
                    continue;
                };
                let Some(id) = stack.get_str("id") else {
                    continue;
                };
                *tally.entry(id.to_owned()).or_default() +=
                    stack.get_int("count").unwrap_or(1) as u32;
            }
        }
    }

    /// Each kind of block the layout at `x0` can reach, placed, moving or
    /// dropped, leaving out the redstone block powering its piston.
    async fn tally(&self, x0: i32) -> Tally {
        let power = BlockPos::new(x0, Y - 1, Z);
        let (west, east): (Vec<_>, Vec<_>) = (x0 - 1..=x0 + 5)
            .flat_map(|x| (Y - 2..=Y + 3).map(move |y| (x, y)))
            .flat_map(|(x, y)| (Z - 2..=Z + 3).map(move |z| BlockPos::new(x, y, z)))
            .filter(|pos| *pos != power)
            .partition(|pos| pos.x < 512);
        let mut tally = Tally::new();
        self.count_blocks(west, &mut tally).await;
        self.count_blocks(east, &mut tally).await;
        self.count_drops(x0, &mut tally).await;
        tally
    }
}

type Tally = BTreeMap<String, u32>;

/// The id of the block a moving piston carries, unless it is a piston's
/// arm.
fn carried(nbt: &Compound) -> Option<String> {
    let moved = tuff_core::storage::chunk_nbt::decode_block_state(nbt.get("blockState")?);
    (!matches!(moved.block(), Block::Air | Block::PistonHead))
        .then(|| moved.block().name().to_owned())
}

/// A sticky piston facing east at `(x0, Y, Z)`, powered by a redstone
/// block under it, in front of `parts` (offsets from the piston).
type Layout = &'static [(i32, i32, i32, &'static str)];

const RIDING_A_SLIME_BLOCK: Layout = &[
    (1, 0, 0, "minecraft:slime_block"),
    (1, 1, 0, "minecraft:rail"),
];

const CARPET_RIDING_A_SLIME_BLOCK: Layout = &[
    (1, 0, 0, "minecraft:slime_block"),
    (1, 1, 0, "minecraft:white_carpet"),
];

const PUSHED_OFF_ITS_SUPPORT: Layout =
    &[(1, -1, 0, "minecraft:stone"), (1, 0, 0, "minecraft:rail")];

const DRAGGED_SIDEWAYS_BY_A_SLIME_BLOCK: Layout = &[
    (1, 0, 0, "minecraft:slime_block"),
    (1, -1, 1, "minecraft:stone"),
    (1, 0, 1, "minecraft:rail[shape=east_west]"),
];

const TWO_LINKED_RAILS_ON_SLIME_AND_STONE: Layout = &[
    (1, 0, 0, "minecraft:slime_block"),
    (1, 1, 0, "minecraft:rail[shape=east_west]"),
    (2, 0, 0, "minecraft:stone"),
    (2, 1, 0, "minecraft:rail[shape=east_west]"),
];

const SAND_RIDING_A_SLIME_BLOCK: Layout = &[
    (1, 0, 0, "minecraft:slime_block"),
    (1, 1, 0, "minecraft:sand"),
];

const SAND_LOSING_ITS_SUPPORT: Layout = &[
    (1, -2, 0, "minecraft:stone"),
    (1, 0, 0, "minecraft:stone"),
    (1, 1, 0, "minecraft:sand"),
];

const POWERED_TNT_RIDING_A_SLIME_BLOCK: Layout = &[
    (1, 0, 0, "minecraft:slime_block"),
    (1, 2, 0, "minecraft:redstone_block"),
    (1, 1, 0, "minecraft:tnt"),
];

const POWERED_RAIL_ONTO_AIR_BESIDE_A_POWERED_ONE: Layout = &[
    (1, -1, 0, "minecraft:stone"),
    (1, 0, 0, "minecraft:powered_rail[shape=east_west]"),
    (3, -1, 0, "minecraft:redstone_block"),
    (
        3,
        0,
        0,
        "minecraft:powered_rail[shape=east_west,powered=true]",
    ),
];

/// Builds `layout` at `x0`, then cycles the piston twice, checking the
/// tally on every tick.
async fn cycle(b: &Bench, x0: i32, layout: Layout, every_tick: bool, extra: &[(&str, u32)]) {
    let power = BlockPos::new(x0, Y - 1, Z);
    b.paste(
        BlockPos::new(x0, Y, Z),
        "minecraft:sticky_piston[facing=east]",
    )
    .await;
    for &(dx, dy, dz, state) in layout {
        b.paste_as(
            BlockPos::new(x0 + dx, Y + dy, Z + dz),
            state,
            state == "minecraft:tnt",
        )
        .await;
    }
    b.world.ticks(2).await;
    let built = b.tally(x0).await;
    assert!(
        built.len() > 1,
        "the layout at x={x0} holds more than its piston: {built:?}"
    );
    let mut allowed = built.clone();
    for &(id, n) in extra {
        *allowed.entry(id.to_owned()).or_default() += n;
    }
    for round in 0..2 {
        for (on, step) in [(true, "push"), (false, "pull")] {
            let state = if on {
                "minecraft:redstone_block"
            } else {
                "minecraft:air"
            };
            b.paste(power, state).await;
            for tick in 1..=6 {
                b.world.ticks(1).await;
                if !every_tick {
                    continue;
                }
                let now = b.tally(x0).await;
                for (id, &n) in &now {
                    let was = allowed.get(id).copied().unwrap_or(0);
                    assert!(
                        n <= was,
                        "{step} {round}, tick {tick}, at x={x0}: {n} {id} from {was}; \
                         a block exists as a block, inside a moving piston or as a drop, \
                         never twice\n now: {now:?}\n allowed: {allowed:?}"
                    );
                }
            }
            assert_eq!(
                b.tally(x0).await,
                allowed,
                "{step} {round} at x={x0}: every block settled once, as a block or a drop"
            );
        }
    }
}

async fn no_dupe(layout: Layout) {
    let dir = TestDir::new();
    let b = bench(dir.path(), false).await;
    cycle(&b, INSIDE, layout, true, &[]).await;
    cycle(&b, ACROSS_BORDER, layout, false, &[]).await;
    b.world.shutdown().await;
}

/// With the vanilla glitches on, `extra` more of a kind exist than were
/// built, and they come from the duplication door.
async fn vanilla_dupe(layout: Layout, extra: &[(&str, u32)]) {
    let dir = TestDir::new();
    let b = bench(dir.path(), true).await;
    let before = tuff_core::world::block_ledger::totals();
    cycle(&b, INSIDE, layout, true, extra).await;
    cycle(&b, ACROSS_BORDER, layout, false, extra).await;
    let doored = tuff_core::world::block_ledger::totals()
        .since(before)
        .duplicated;
    b.world.shutdown().await;
    let wanted: u64 = extra.iter().map(|&(_, n)| u64::from(n)).sum::<u64>() * 2;
    assert!(
        doored >= wanted,
        "the extras came from {doored} door uses, wanted {wanted}"
    );
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_rail_riding_a_slime_block_is_not_duplicated() {
    no_dupe(RIDING_A_SLIME_BLOCK).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_rail_pushed_off_its_support_drops_once() {
    no_dupe(PUSHED_OFF_ITS_SUPPORT).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_rail_dragged_off_its_support_by_a_slime_block_drops_once() {
    no_dupe(DRAGGED_SIDEWAYS_BY_A_SLIME_BLOCK).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_linked_rails_moved_on_slime_and_stone_are_not_duplicated() {
    no_dupe(TWO_LINKED_RAILS_ON_SLIME_AND_STONE).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_powered_rail_pushed_onto_air_beside_a_powered_one_drops_once() {
    no_dupe(POWERED_RAIL_ONTO_AIR_BESIDE_A_POWERED_ONE).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_carpet_riding_a_slime_block_is_not_duplicated() {
    no_dupe(CARPET_RIDING_A_SLIME_BLOCK).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn powered_tnt_riding_a_slime_block_primes_once() {
    no_dupe(POWERED_TNT_RIDING_A_SLIME_BLOCK).await;
}

const EVERY_LAYOUT: &[Layout] = &[
    RIDING_A_SLIME_BLOCK,
    CARPET_RIDING_A_SLIME_BLOCK,
    PUSHED_OFF_ITS_SUPPORT,
    DRAGGED_SIDEWAYS_BY_A_SLIME_BLOCK,
    TWO_LINKED_RAILS_ON_SLIME_AND_STONE,
    POWERED_TNT_RIDING_A_SLIME_BLOCK,
    POWERED_RAIL_ONTO_AIR_BESIDE_A_POWERED_ONE,
    SAND_RIDING_A_SLIME_BLOCK,
    SAND_LOSING_ITS_SUPPORT,
];

/// Vanilla's piston movement clears every moved block's old place before
/// any update runs, so no layout of these duplicates a block even with the
/// duplication settings on.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_layout_duplicates_with_the_vanilla_settings_on() {
    for layout in EVERY_LAYOUT {
        vanilla_dupe(layout, &[]).await;
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sand_riding_a_slime_block_is_not_duplicated() {
    no_dupe(SAND_RIDING_A_SLIME_BLOCK).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sand_whose_support_a_piston_moves_away_falls_once() {
    no_dupe(SAND_LOSING_ITS_SUPPORT).await;
}

const HELD_AT: i32 = INSIDE + 3;

impl Bench {
    async fn entities_of(&self, kind: EntityType) -> Vec<i32> {
        let (reply, answer) = oneshot::channel();
        self.west.request(RegionRequest::Entities { reply }).await;
        answer
            .await
            .unwrap()
            .into_iter()
            .filter(|e| e.kind == kind)
            .map(|e| e.id)
            .collect()
    }

    async fn drop_falling(&self, state: &str, at: BlockPos) {
        let (reply, summoned) = oneshot::channel();
        self.region(at)
            .request(RegionRequest::Summon {
                nbt: Compound::new()
                    .with("id", "minecraft:falling_block")
                    .with("BlockState", Compound::new().with("Name", state))
                    .with("Time", 1),
                position: [
                    f64::from(at.x) + 0.5,
                    f64::from(at.y),
                    f64::from(at.z) + 0.5,
                ],
                fresh: false,
                reply,
            })
            .await;
        assert!(summoned.await.unwrap());
    }

    async fn kill(&self, id: i32) {
        self.west
            .request(RegionRequest::Run(
                tuff_core::world::region_api::RegionTask::new(move |world| {
                    world.kill_entity(id);
                }),
            ))
            .await;
    }

    async fn held_tally(&self, blocks: Vec<BlockPos>) -> Tally {
        let mut tally = Tally::new();
        self.count_blocks(blocks, &mut tally).await;
        self.count_drops(HELD_AT, &mut tally).await;
        tally
    }
}

fn count_of(tally: &Tally, id: &str) -> u32 {
    tally.get(id).copied().unwrap_or(0)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_falling_block_killed_mid_fall_is_gone_without_a_drop() {
    let dir = TestDir::new();
    let b = bench(dir.path(), false).await;
    let at = BlockPos::new(HELD_AT, Y, Z + 10);
    b.drop_falling("minecraft:sand", at).await;
    b.world.ticks(4).await;
    let falling = b.entities_of(EntityType::FallingBlock).await;
    assert_eq!(
        falling.len(),
        1,
        "the sand fell: {:?} {:?}",
        block_at(&b.west, at).await,
        block_at(&b.west, BlockPos::new(at.x, at.y - 1, at.z)).await
    );
    assert_eq!(count_of(&b.held_tally(vec![at]).await, "minecraft:sand"), 1);
    let before = tuff_core::world::block_ledger::totals();
    b.kill(falling[0]).await;
    b.world.ticks(2).await;
    let after = b.held_tally(vec![at]).await;
    assert_eq!(count_of(&after, "minecraft:sand"), 0, "{after:?}");
    assert!(
        tuff_core::world::block_ledger::totals()
            .since(before)
            .burned
            >= 1
    );
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_falling_block_saved_and_reloaded_mid_fall_lands_once() {
    let dir = TestDir::new();
    let b = bench(dir.path(), false).await;
    let top = BlockPos::new(HELD_AT, Y, Z + 10);
    let landing = BlockPos::new(HELD_AT, Y - 9, Z + 10);
    b.paste(BlockPos::new(HELD_AT, Y - 10, Z + 10), "minecraft:stone")
        .await;
    b.drop_falling("minecraft:sand", top).await;
    b.world.ticks(4).await;
    assert_eq!(b.entities_of(EntityType::FallingBlock).await.len(), 1);
    b.world.shutdown().await;
    let b = bench(dir.path(), false).await;
    let column: Vec<BlockPos> = (Y - 9..=Y)
        .map(|y| BlockPos::new(HELD_AT, y, top.z))
        .collect();
    b.world.ticks(60).await;
    let tally = b.held_tally(column).await;
    assert_eq!(count_of(&tally, "minecraft:sand"), 1, "{tally:?}");
    assert!(b.entities_of(EntityType::FallingBlock).await.is_empty());
    let landed = block_at(&b.west, landing).await.unwrap();
    assert_eq!(landed.block(), Block::Sand);
    b.world.shutdown().await;
}

async fn enderman_holding_dirt(dir: &Path) -> Bench {
    let b = bench(dir, false).await;
    b.world.game_rules.send_modify(|rules| {
        rules
            .set(
                "mob_griefing",
                crate::world::game_rules::RuleValue::Bool(false),
            )
            .expect("a boolean rule");
    });
    b
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_enderman_holding_a_block_saved_reloaded_and_killed_drops_it_once() {
    let dir = TestDir::new();
    let b = enderman_holding_dirt(dir.path()).await;
    let floor = BlockPos::new(HELD_AT, Y - 1, Z + 20);
    b.paste(floor, "minecraft:stone").await;
    let (reply, summoned) = oneshot::channel();
    b.west
        .request(RegionRequest::Summon {
            nbt: Compound::new()
                .with("id", "minecraft:enderman")
                .with(
                    "carriedBlockState",
                    Compound::new().with("Name", "minecraft:dirt"),
                )
                .with("PersistenceRequired", true),
            position: [
                f64::from(HELD_AT) + 0.5,
                f64::from(Y),
                f64::from(Z + 20) + 0.5,
            ],
            fresh: false,
            reply,
        })
        .await;
    assert!(summoned.await.unwrap());
    b.world.ticks(3).await;
    assert_eq!(count_of(&b.held_tally(vec![]).await, "minecraft:dirt"), 1);
    b.world.shutdown().await;

    let b = enderman_holding_dirt(dir.path()).await;
    b.world.ticks(3).await;
    let tally = b.held_tally(vec![]).await;
    assert_eq!(count_of(&tally, "minecraft:dirt"), 1, "{tally:?}");
    let enderman = b.entities_of(EntityType::Enderman).await;
    assert_eq!(enderman.len(), 1);
    b.kill(enderman[0]).await;
    b.world.ticks(40).await;
    let tally = b.held_tally(vec![]).await;
    assert!(b.entities_of(EntityType::Enderman).await.is_empty());
    assert_eq!(count_of(&tally, "minecraft:dirt"), 1, "{tally:?}");
    b.world.shutdown().await;
}

const PORTAL_Z: i32 = Z + 20;

async fn sand_into_an_end_portal(dupes: bool) -> (u64, u32) {
    let dir = TestDir::new();
    let b = bench(dir.path(), dupes).await;
    let portal = BlockPos::new(HELD_AT, Y - 2, PORTAL_Z);
    b.paste(BlockPos::new(HELD_AT, Y - 3, PORTAL_Z), "minecraft:stone")
        .await;
    b.paste(portal, "minecraft:end_portal").await;
    let before = tuff_core::world::block_ledger::totals();
    b.drop_falling("minecraft:sand", portal).await;
    let platform = BlockPos::new(100, 49, 0);
    let end = b
        .world
        .region_in(
            crate::storage::level::THE_END,
            ChunkPos::of_block(platform).region(),
        )
        .await;
    let mut sand = 0;
    for _ in 0..120 {
        b.world.ticks(2).await;
        tokio::time::sleep(Duration::from_millis(10)).await;
        sand = 0;
        if block_at(&end, platform).await.map(|s| s.block()) == Some(Block::Sand) {
            sand += 1;
        }
        let (reply, answer) = oneshot::channel();
        end.request(RegionRequest::Entities { reply }).await;
        for entity in answer.await.unwrap() {
            if entity.kind != EntityType::Item {
                continue;
            }
            let (reply, data) = oneshot::channel();
            end.request(RegionRequest::EntityData {
                id: entity.id,
                reply,
            })
            .await;
            let item = data
                .await
                .unwrap()
                .and_then(|d| d.get_compound("Item").cloned());
            if item
                .and_then(|i| i.get_str("id").map(str::to_owned))
                .as_deref()
                == Some("minecraft:sand")
            {
                sand += 1;
            }
        }
        if sand >= 1 && !dupes || sand >= 2 {
            break;
        }
    }
    b.world.ticks(10).await;
    let duplicated = tuff_core::world::block_ledger::totals()
        .since(before)
        .duplicated;
    b.world.shutdown().await;
    (duplicated, sand)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sand_landing_in_an_end_portal_comes_through_twice_when_vanilla_does() {
    let (duplicated, sand) = sand_into_an_end_portal(true).await;
    assert!(duplicated >= 1, "no copy was made");
    assert_eq!(sand, 2, "the original and the copy reached the end");
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sand_landing_in_an_end_portal_comes_through_once_with_the_setting_off() {
    let (_, sand) = sand_into_an_end_portal(false).await;
    assert_eq!(sand, 1);
}

mod designs {
    use super::*;
    use crate::dupe_designs::{COUNT_BOX, DESIGNS, Design, ORIGIN};

    impl Bench {
        async fn design_tally(&self, design: &Design) -> Tally {
            let (lo, hi) = COUNT_BOX;
            let positions: Vec<BlockPos> = (lo.0..=hi.0)
                .flat_map(|x| (lo.1..=hi.1).map(move |y| (x, y)))
                .flat_map(|(x, y)| (lo.2..=hi.2).map(move |z| (x, y, z)))
                .map(|(x, y, z)| BlockPos::new(ORIGIN.0 + x, ORIGIN.1 + y, ORIGIN.2 + z))
                .collect();
            let mut tally = Tally::new();
            self.count_blocks(positions, &mut tally).await;
            self.count_drops(ORIGIN.0, &mut tally).await;
            let primed = self.primed_tnt().await;
            if primed > 0 {
                let tnt = tally.entry("minecraft:tnt".to_owned()).or_default();
                *tnt = tnt.saturating_sub(primed);
                tally.insert("primed_tnt".to_owned(), primed);
            }
            tally.retain(|id, n| {
                *n > 0 && (id == "primed_tnt" || design.kinds.contains(&id.as_str()))
            });
            tally
        }

        async fn end_tally(&self, design: &Design) -> Tally {
            let platform = BlockPos::new(100, 49, 0);
            let end = self
                .world
                .region_in(
                    crate::storage::level::THE_END,
                    ChunkPos::of_block(platform).region(),
                )
                .await;
            let positions: Vec<BlockPos> = (94..=106)
                .flat_map(|x| (44..=56).map(move |y| (x, y)))
                .flat_map(|(x, y)| (-8..=8).map(move |z| BlockPos::new(x, y, z)))
                .collect();
            let (reply, answer) = oneshot::channel();
            end.request(RegionRequest::Blocks {
                op: BlockOp::Copy { positions },
                reply,
            })
            .await;
            let BlockOpResult::Copied(found) = answer.await.unwrap() else {
                panic!("copy failed");
            };
            let mut tally = Tally::new();
            for (state, _) in found.into_iter().flatten() {
                if state.block() != Block::Air {
                    *tally
                        .entry(format!("end:{}", state.block().name()))
                        .or_default() += 1;
                }
            }
            let (reply, answer) = oneshot::channel();
            end.request(RegionRequest::Entities { reply }).await;
            for entity in answer.await.unwrap() {
                if entity.kind != EntityType::Item {
                    continue;
                }
                let (reply, data) = oneshot::channel();
                end.request(RegionRequest::EntityData {
                    id: entity.id,
                    reply,
                })
                .await;
                let item = data
                    .await
                    .unwrap()
                    .and_then(|d| d.get_compound("Item").cloned());
                if let Some(id) = item.as_ref().and_then(|i| i.get_str("id")) {
                    *tally.entry(id.to_owned()).or_default() +=
                        item.as_ref().and_then(|i| i.get_int("count")).unwrap_or(1) as u32;
                }
            }
            tally
                .into_iter()
                .filter(|(id, _)| design.kinds.contains(&id.trim_start_matches("end:")))
                .collect()
        }

        async fn primed_tnt(&self) -> u32 {
            let (reply, answer) = oneshot::channel();
            self.west.request(RegionRequest::Entities { reply }).await;
            answer
                .await
                .unwrap()
                .into_iter()
                .filter(|e| e.kind == EntityType::Tnt)
                .count() as u32
        }
    }

    /// Builds and fires `design`, returning the counts afterwards.
    pub async fn run(design: &Design, dupes: bool) -> (Tally, u64) {
        let dir = TestDir::new();
        let b = bench(dir.path(), dupes).await;
        let at = |(x, y, z, _): (i32, i32, i32, &str)| {
            BlockPos::new(ORIGIN.0 + x, ORIGIN.1 + y, ORIGIN.2 + z)
        };
        for &part in design.build {
            b.paste_as(at(part), part.3, true).await;
        }
        b.world.ticks(4).await;
        let before = tuff_core::world::block_ledger::totals();
        let mut end_seen = Tally::new();
        for &part in design.trigger {
            match part.3.strip_prefix("@step:") {
                Some(n) if design.end => {
                    for _ in 0..n.parse::<u64>().unwrap() {
                        b.world.ticks(1).await;
                        tokio::time::sleep(Duration::from_millis(5)).await;
                        for (id, n) in b.end_tally(design).await {
                            let seen = end_seen.entry(id).or_default();
                            *seen = (*seen).max(n);
                        }
                    }
                }
                Some(n) => b.world.ticks(n.parse::<u64>().unwrap()).await,
                None if part.3.starts_with("@fall:") => {
                    b.drop_falling(&part.3["@fall:".len()..], at(part)).await;
                }
                None => b.paste(at(part), part.3).await,
            }
        }
        b.world.ticks(u64::from(design.steps.max(1))).await;
        let mut after = b.design_tally(design).await;
        if design.end {
            for _ in 0..60 {
                b.world.ticks(2).await;
                tokio::time::sleep(Duration::from_millis(10)).await;
                for (id, n) in b.end_tally(design).await {
                    let seen = end_seen.entry(id).or_default();
                    *seen = (*seen).max(n);
                }
            }
            after = b.design_tally(design).await;
            for (id, n) in end_seen {
                *after.entry(id).or_default() += n;
            }
        }
        let doored = tuff_core::world::block_ledger::totals()
            .since(before)
            .duplicated;
        b.world.shutdown().await;
        (after, doored)
    }

    fn recorded(design: &Design) -> Tally {
        design
            .vanilla
            .iter()
            .map(|&(id, n)| (id.to_owned(), n))
            .collect()
    }

    #[tuff_core::watchdog]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn tuff_matches_vanilla_with_the_settings_on_and_never_exceeds_the_build_with_them_off() {
        let mut report = Vec::new();
        for design in DESIGNS {
            if std::env::var("TUFF_ONLY").is_ok_and(|n| n != design.name) {
                continue;
            }
            let (on, doored) = run(design, true).await;
            let (off, off_doored) = run(design, false).await;
            let wanted = recorded(design);
            eprintln!(
                "{}: vanilla {wanted:?}\n  on {on:?} (door {doored})\n  off {off:?} (door {off_doored})",
                design.name
            );
            if on != wanted {
                report.push(format!("{} on: {on:?} vs vanilla {wanted:?}", design.name));
            }
            let built = crate::vanilla_dupers::built_counts(design);
            let mut summed = Tally::new();
            for (id, n) in &off {
                *summed
                    .entry(id.trim_start_matches("end:").to_owned())
                    .or_default() += n;
            }
            for (id, n) in &summed {
                if id != "primed_tnt" && *n > built.get(id).copied().unwrap_or(0) {
                    report.push(format!(
                        "{} off: {off:?} exceeds the build {built:?}",
                        design.name
                    ));
                }
            }
            if off.contains_key("primed_tnt") {
                report.push(format!("{} off: primed TNT {off:?}", design.name));
            }
        }
        assert!(report.is_empty(), "{}", report.join("\n"));
    }
}
