//! Simple rail and carpet dupers: a piston moving a rail or carpet
//! together with, or away from, the block it stands on. No block may exist as a block and as a
//! drop at once: on every tick no kind of block in reach, counted where it
//! stands, inside moving pistons and as dropped items, outnumbers what was
//! built, and once the piston settles each adds up to it again. Across a
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

const DUPE_KEYS: [&str; 2] = [
    tuff_core::config::BLOCKS_TNT_DUPLICATION,
    tuff_core::config::BLOCKS_CARPET_RAIL_DUPLICATION,
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

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_rail_riding_a_slime_block_duplicates_once_when_vanilla_does() {
    vanilla_dupe(RIDING_A_SLIME_BLOCK, &[("minecraft:rail", 1)]).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_linked_rails_duplicate_one_when_vanilla_does() {
    vanilla_dupe(
        TWO_LINKED_RAILS_ON_SLIME_AND_STONE,
        &[("minecraft:rail", 1)],
    )
    .await;
}
