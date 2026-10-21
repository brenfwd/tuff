//! Commands that change blocks and spawn entities: setblock, fill, clone
//! and summon. Block reads and writes go to the regions that own the
//! chunks, split per region and summed.

use std::collections::BTreeMap;
use tuff_item::item::DupUnchecked;

use tokio::sync::oneshot;
use tuff_vanilla_data::registry::EntityType;
use tuff_vanilla_data::{Block, BlockState};

use tuff_core::command::block::BlockPredicate;
use tuff_core::command::{CommandError, Value};
use tuff_core::world::dimension::DimensionId;
use tuff_core::world::messages::{
    BlockOp, BlockOpResult, ChunkTicket, CoordinatorMessage, CopiedTick, DifficultyOutcome,
    ManagerMessage, RegionRequest,
};
use tuff_core::world::player::Difficulty;
use tuff_item::text::TextComponent;
use tuff_nbt::{Compound, Tag};
use tuff_world_model::BlockPos;
use tuff_world_model::pos::{ChunkPos, RegionPos};

use super::{Context, Executor, executor_future, text};

const MAX_HORIZONTAL: i32 = 30_000_000;

/// An inclusive box of block positions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockBox {
    pub min: [i32; 3],
    pub max: [i32; 3],
}

impl BlockBox {
    pub fn from_corners(a: [i32; 3], b: [i32; 3]) -> Self {
        let mut min = [0; 3];
        let mut max = [0; 3];
        for axis in 0..3 {
            min[axis] = a[axis].min(b[axis]);
            max[axis] = a[axis].max(b[axis]);
        }
        Self { min, max }
    }

    pub fn volume(&self) -> i64 {
        (0..3)
            .map(|axis| i64::from(self.max[axis] - self.min[axis]) + 1)
            .product()
    }

    pub fn intersects(&self, other: &BlockBox) -> bool {
        (0..3).all(|axis| self.min[axis] <= other.max[axis] && self.max[axis] >= other.min[axis])
    }

    pub fn contains(&self, pos: [i32; 3]) -> bool {
        (0..3).all(|axis| (self.min[axis]..=self.max[axis]).contains(&pos[axis]))
    }

    pub fn on_surface(&self, pos: [i32; 3]) -> bool {
        (0..3).any(|axis| pos[axis] == self.min[axis] || pos[axis] == self.max[axis])
    }

    /// Positions in the game's iteration order: x fastest, then y, then z.
    pub fn positions(&self) -> impl Iterator<Item = [i32; 3]> + '_ {
        (self.min[2]..=self.max[2]).flat_map(move |z| {
            (self.min[1]..=self.max[1])
                .flat_map(move |y| (self.min[0]..=self.max[0]).map(move |x| [x, y, z]))
        })
    }
}

pub(crate) fn block_pos_arg(ctx: &Context<'_>, name: &str) -> Result<[i32; 3], CommandError> {
    match ctx.arg(name) {
        Some(Value::Coordinates(coordinates)) => Ok(coordinates.block(&ctx.source.pose())),
        _ => Err(CommandError::simple("command.unknown.argument")),
    }
}

fn to_block_pos(pos: [i32; 3]) -> BlockPos {
    BlockPos::new(pos[0], pos[1], pos[2])
}

/// Runs a block operation over positions that may span regions, returning
/// each region's result in the order of `groups`.
async fn per_region(
    ctx: &mut Context<'_>,
    dimension: DimensionId,
    groups: BTreeMap<(i32, i32), Vec<usize>>,
    make: impl Fn(&[usize]) -> BlockOp,
) -> Result<Vec<(Vec<usize>, BlockOpResult)>, CommandError> {
    let mut results = Vec::new();
    for ((x, z), indices) in groups {
        let Some(region) = ctx.actor.region(dimension, RegionPos { x, z }).await else {
            return Err(CommandError::simple("argument.pos.unloaded"));
        };
        let (reply, answer) = oneshot::channel();
        if !region
            .request(RegionRequest::Blocks {
                op: make(&indices),
                reply,
            })
            .await
        {
            return Err(CommandError::simple("argument.pos.unloaded"));
        }
        let result = answer
            .await
            .map_err(|_| CommandError::simple("argument.pos.unloaded"))?;
        results.push((indices, result));
    }
    Ok(results)
}

fn group_by_region(positions: &[[i32; 3]]) -> BTreeMap<(i32, i32), Vec<usize>> {
    let mut groups: BTreeMap<(i32, i32), Vec<usize>> = BTreeMap::new();
    for (index, pos) in positions.iter().enumerate() {
        let region = ChunkPos::of_block(to_block_pos(*pos)).region();
        groups.entry((region.x, region.z)).or_default().push(index);
    }
    groups
}

/// The states at `positions`, loading chunks when `load`.
pub async fn read_blocks(
    ctx: &mut Context<'_>,
    dimension: DimensionId,
    positions: &[[i32; 3]],
    load: bool,
) -> Result<Vec<Option<BlockState>>, CommandError> {
    let groups = group_by_region(positions);
    let results = per_region(ctx, dimension, groups, |indices| BlockOp::Get {
        positions: indices
            .iter()
            .map(|i| to_block_pos(positions[*i]))
            .collect(),
        load,
    })
    .await?;
    let mut states = vec![None; positions.len()];
    for (indices, result) in results {
        if let BlockOpResult::States(found) = result {
            for (index, state) in indices.into_iter().zip(found) {
                states[index] = state;
            }
        }
    }
    Ok(states)
}

/// The biome name at a position, `None` where the chunk is not loaded.
pub async fn read_biome(
    ctx: &mut Context<'_>,
    dimension: DimensionId,
    pos: [i32; 3],
) -> Result<Option<&'static str>, CommandError> {
    let groups = group_by_region(&[pos]);
    let results = per_region(ctx, dimension, groups, |_| BlockOp::Biomes {
        positions: vec![to_block_pos(pos)],
        load: false,
    })
    .await?;
    Ok(results.into_iter().find_map(|(_, result)| match result {
        BlockOpResult::Biomes(found) => found.into_iter().next().flatten(),
        _ => None,
    }))
}

/// Waits until each chunk is loaded. The forced-chunk mark reaches a region
/// on its internal channel, so a block request sent right after the command
/// answers could otherwise overtake it and see the chunk as unloaded.
async fn await_forced_chunks(ctx: &mut Context<'_>, dimension: DimensionId, chunks: &[ChunkPos]) {
    for pos in chunks {
        let Some(region) = ctx.actor.region(dimension, pos.region()).await else {
            continue;
        };
        let (reply, answer) = oneshot::channel();
        if region
            .request(RegionRequest::AwaitChunk {
                pos: BlockPos::new(pos.x * 16, 0, pos.z * 16),
                reply,
            })
            .await
        {
            let _ = answer.await;
        }
    }
}

/// Whether every position's chunk is loaded, as the game checks its
/// corner arguments.
pub async fn ensure_loaded(
    ctx: &mut Context<'_>,
    dimension: DimensionId,
    positions: &[[i32; 3]],
) -> Result<(), CommandError> {
    let states = read_blocks(ctx, dimension, positions, false).await?;
    if states.iter().any(Option::is_none) {
        return Err(CommandError::simple("argument.pos.unloaded"));
    }
    Ok(())
}

/// Sets blocks across regions as the block commands do, replacing block
/// entities without their side effects and loading `data` into each;
/// returns how many changed.
pub async fn write_blocks(
    ctx: &mut Context<'_>,
    dimension: DimensionId,
    changes: &[([i32; 3], BlockState)],
    data: Option<&Compound>,
    [destroy, keep, strict]: [bool; 3],
) -> Result<usize, CommandError> {
    let positions: Vec<[i32; 3]> = changes.iter().map(|(pos, _)| *pos).collect();
    let groups = group_by_region(&positions);
    let results = per_region(ctx, dimension, groups, |indices| BlockOp::Set {
        changes: indices
            .iter()
            .map(|i| (to_block_pos(changes[*i].0), changes[*i].1))
            .collect(),
        destroy,
        keep,
        strict,
        quiet: true,
        data: data.cloned(),
    })
    .await?;
    Ok(results
        .into_iter()
        .map(|(_, result)| match result {
            BlockOpResult::Set(count) => count,
            _ => 0,
        })
        .sum())
}

/// The states and saved block entities at `positions`, loading chunks.
pub async fn copy_blocks(
    ctx: &mut Context<'_>,
    dimension: DimensionId,
    positions: &[[i32; 3]],
) -> Result<Vec<Option<(BlockState, Option<Compound>)>>, CommandError> {
    let groups = group_by_region(positions);
    let results = per_region(ctx, dimension, groups, |indices| BlockOp::Copy {
        positions: indices
            .iter()
            .map(|i| to_block_pos(positions[*i]))
            .collect(),
    })
    .await?;
    let mut copied = vec![None; positions.len()];
    for (indices, result) in results {
        if let BlockOpResult::Copied(found) = result {
            for (index, entry) in indices.into_iter().zip(found) {
                copied[index] = entry;
            }
        }
    }
    Ok(copied)
}

/// The test instance blocks within `radius` (x and z) of `center` whose
/// chunks are loaded, closest first.
pub async fn find_test_instances(
    ctx: &mut Context<'_>,
    dimension: DimensionId,
    center: [i32; 3],
    radius: i32,
) -> Result<Vec<[i32; 3]>, CommandError> {
    let low =
        ChunkPos::of_block(to_block_pos([center[0] - radius, 0, center[2] - radius])).region();
    let high =
        ChunkPos::of_block(to_block_pos([center[0] + radius, 0, center[2] + radius])).region();
    let mut groups: BTreeMap<(i32, i32), Vec<usize>> = BTreeMap::new();
    for x in low.x..=high.x {
        for z in low.z..=high.z {
            groups.insert((x, z), Vec::new());
        }
    }
    let center_pos = to_block_pos(center);
    let results = per_region(ctx, dimension, groups, |_| BlockOp::TestInstances {
        center: center_pos,
        radius,
    })
    .await?;
    let mut found: Vec<[i32; 3]> = results
        .into_iter()
        .filter_map(|(_, result)| match result {
            BlockOpResult::Positions(positions) => Some(positions),
            _ => None,
        })
        .flatten()
        .map(|pos| [pos.x, pos.y, pos.z])
        .collect();
    let distance =
        |p: &[i32; 3]| -> i64 { (0..3).map(|i| i64::from(p[i] - center[i]).pow(2)).sum() };
    found.sort_by_key(|p| (distance(p), *p));
    found.dedup();
    Ok(found)
}

/// Triggers the test blocks at `positions` (`trigger`), or reads whether
/// each was triggered, resetting those that were when `reset`; see
/// [`BlockOp::TestBlocks`].
pub async fn test_blocks(
    ctx: &mut Context<'_>,
    dimension: DimensionId,
    positions: &[[i32; 3]],
    trigger: bool,
    reset: bool,
) -> Result<Vec<Option<bool>>, CommandError> {
    let groups = group_by_region(positions);
    let results = per_region(ctx, dimension, groups, |indices| BlockOp::TestBlocks {
        positions: indices
            .iter()
            .map(|i| to_block_pos(positions[*i]))
            .collect(),
        trigger,
        reset,
    })
    .await?;
    let mut flags = vec![None; positions.len()];
    for (indices, result) in results {
        if let BlockOpResult::Triggered(found) = result {
            for (index, entry) in indices.into_iter().zip(found) {
                flags[index] = entry;
            }
        }
    }
    Ok(flags)
}

/// Sets blocks across regions without block entity side effects, giving
/// each block its copied block entity data; returns how many changed.
pub async fn paste_blocks(
    ctx: &mut Context<'_>,
    dimension: DimensionId,
    changes: &[([i32; 3], BlockState, Option<Compound>)],
    strict: bool,
) -> Result<usize, CommandError> {
    let positions: Vec<[i32; 3]> = changes.iter().map(|(pos, ..)| *pos).collect();
    let groups = group_by_region(&positions);
    let results = per_region(ctx, dimension, groups, |indices| BlockOp::Paste {
        changes: indices
            .iter()
            .map(|i| {
                let (pos, state, data) = &changes[*i];
                (to_block_pos(*pos), *state, data.clone())
            })
            .collect(),
        strict,
    })
    .await?;
    Ok(results
        .into_iter()
        .map(|(_, result)| match result {
            BlockOpResult::Set(count) => count,
            _ => 0,
        })
        .sum())
}

/// Hands `stack` to the item port block at `pos` (a jukebox) as `/item`
/// does; false when the block there has no port that takes it.
pub async fn replace_port_item(
    ctx: &mut Context<'_>,
    dimension: DimensionId,
    pos: [i32; 3],
    stack: Option<tuff_item::item::ItemStack>,
) -> Result<bool, CommandError> {
    let groups = group_by_region(&[pos]);
    let results = per_region(ctx, dimension, groups, |_| BlockOp::ReplacePortItem {
        pos: to_block_pos(pos),
        stack: stack.dup_unchecked(),
    })
    .await?;
    Ok(results
        .iter()
        .any(|(_, result)| matches!(result, BlockOpResult::Set(1))))
}

/// The pending block ticks inside `area`, in the order they run relative to
/// each other, asking every region that holds one of `positions`.
async fn copy_ticks(
    ctx: &mut Context<'_>,
    dimension: DimensionId,
    positions: &[[i32; 3]],
    area: BlockBox,
) -> Result<Vec<CopiedTick>, CommandError> {
    let groups = group_by_region(positions);
    let (min, max) = (to_block_pos(area.min), to_block_pos(area.max));
    let results = per_region(ctx, dimension, groups, |_| BlockOp::CopyTicks { min, max }).await?;
    let mut ticks: Vec<CopiedTick> = results
        .into_iter()
        .flat_map(|(_, result)| match result {
            BlockOpResult::Ticks(ticks) => ticks,
            _ => Vec::new(),
        })
        .collect();
    ticks.sort_by_key(|tick| (tick.delay, tick.priority, tick.order));
    Ok(ticks)
}

async fn paste_ticks(
    ctx: &mut Context<'_>,
    dimension: DimensionId,
    ticks: &[CopiedTick],
) -> Result<(), CommandError> {
    let positions: Vec<[i32; 3]> = ticks
        .iter()
        .map(|tick| [tick.pos.x, tick.pos.y, tick.pos.z])
        .collect();
    let groups = group_by_region(&positions);
    per_region(ctx, dimension, groups, |indices| BlockOp::PasteTicks {
        ticks: indices.iter().map(|i| ticks[*i]).collect(),
    })
    .await?;
    Ok(())
}

/// Whether `pos` is inside the world's fixed horizontal limits; the world
/// border does not bound commands.
pub(crate) fn in_world(pos: [i32; 3]) -> bool {
    (-MAX_HORIZONTAL..MAX_HORIZONTAL).contains(&pos[0])
        && (-MAX_HORIZONTAL..MAX_HORIZONTAL).contains(&pos[2])
}

pub struct SetBlock;

impl Executor for SetBlock {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            let pos = block_pos_arg(ctx, "pos")?;
            let Some(Value::BlockState(block)) = ctx.arg("block").cloned() else {
                return Err(CommandError::simple("command.unknown.argument"));
            };
            let dimension = ctx.source.dimension;
            if !in_world(pos) {
                return Err(CommandError::simple("argument.pos.unloaded"));
            }
            ensure_loaded(ctx, dimension, &[pos]).await?;
            let literals = ctx.literals();
            let mode = literals.get(3).copied().unwrap_or("replace");
            let (destroy, keep, strict) = match mode {
                "destroy" => (true, false, false),
                "keep" => (false, true, false),
                "strict" => (false, false, true),
                _ => (false, false, false),
            };
            let count = write_blocks(
                ctx,
                dimension,
                &[(pos, block.state)],
                block.nbt.as_ref(),
                [destroy, keep, strict],
            )
            .await?;
            if count == 0 {
                return Err(CommandError::simple("commands.setblock.failed"));
            }
            ctx.success(
                TextComponent::translate(
                    "commands.setblock.success",
                    [text(pos[0]), text(pos[1]), text(pos[2])],
                ),
                true,
            )
            .await;
            Ok(1)
        })
    }
}

/// Blocks per slab a region part is applied in, bounding the position
/// lists held for a huge fill.
const FILL_SLAB: i64 = 1 << 16;
/// Region parts applied concurrently.
const FILL_REGIONS_IN_FLIGHT: usize = 8;

/// Edits larger than the vanilla default limit show a progress bar, which only
/// happens once the game rule is raised above it.
const BAR_ABOVE: i64 = 32768;

/// The block limit of one fill: the `max_block_modifications` game rule.
fn fill_limit(ctx: &Context<'_>) -> Option<i64> {
    Some(i64::from(ctx.shared().rule_int("max_block_modifications")))
}

struct FillPlan {
    region: BlockBox,
    block: tuff_core::command::block::BlockInput,
    filter: Option<BlockPredicate>,
    mode: &'static str,
    flags: [bool; 3],
    bar: bool,
}

const FILL_BAR_UUID: u128 = 0x7F1D_26B5_0C49_4E83_9A57_D1E8_3B60_C2A4;

fn fill_bar(ctx: &mut Context<'_>, action: Option<(f32, i64, i64)>, first: bool) {
    bar_titled(ctx, "Filling blocks", action, first);
}

fn bar_titled(ctx: &mut Context<'_>, label: &str, action: Option<(f32, i64, i64)>, first: bool) {
    use tuff_core::protocol::play::{BossAction, BossEvent};
    let Some(session) = ctx.actor.player() else {
        return;
    };
    let Some((progress, done, total)) = action else {
        session.send(&BossEvent {
            uuid: FILL_BAR_UUID,
            action: BossAction::Remove,
        });
        return;
    };
    let title = TextComponent::new(format!(
        "{label}: {:.0}% ({done} / {total})",
        f64::from(progress) * 100.0
    ));
    if first {
        session.send(&BossEvent {
            uuid: FILL_BAR_UUID,
            action: BossAction::Add {
                title: &title,
                progress,
                color: 1,
                overlay: 0,
                flags: 0,
            },
        });
    } else {
        session.send(&BossEvent {
            uuid: FILL_BAR_UUID,
            action: BossAction::Title(&title),
        });
        session.send(&BossEvent {
            uuid: FILL_BAR_UUID,
            action: BossAction::Progress(progress),
        });
    }
}

/// The parts of `region` per owning region actor, in region order.
fn split_by_region(region: &BlockBox) -> Vec<(RegionPos, BlockBox)> {
    let span = tuff_world_model::pos::REGION_CHUNKS * 16;
    let lo = ChunkPos::of_block(to_block_pos(region.min)).region();
    let hi = ChunkPos::of_block(to_block_pos(region.max)).region();
    let mut parts = Vec::new();
    for rz in lo.z..=hi.z {
        for rx in lo.x..=hi.x {
            let min = [
                region.min[0].max(rx * span),
                region.min[1],
                region.min[2].max(rz * span),
            ];
            let max = [
                region.max[0].min(rx * span + span - 1),
                region.max[1],
                region.max[2].min(rz * span + span - 1),
            ];
            parts.push((RegionPos { x: rx, z: rz }, BlockBox { min, max }));
        }
    }
    parts
}

/// Slabs of a part along z holding at most `FILL_SLAB` blocks each
/// (one row when a row is already larger).
fn slabs(part: &BlockBox) -> Vec<BlockBox> {
    let row = i64::from(part.max[0] - part.min[0] + 1) * i64::from(part.max[1] - part.min[1] + 1);
    let rows = (FILL_SLAB / row).max(1) as i32;
    let mut out = Vec::new();
    let mut z = part.min[2];
    while z <= part.max[2] {
        let end = z.saturating_add(rows - 1).min(part.max[2]);
        out.push(BlockBox {
            min: [part.min[0], part.min[1], z],
            max: [part.max[0], part.max[1], end],
        });
        z = end + 1;
    }
    out
}

/// Sends each op to its region at once and collects the answers in order,
/// so the regions work in parallel.
async fn dispatch(
    handles: &[tuff_core::world::messages::RegionHandle],
    ops: Vec<(usize, BlockOp)>,
) -> Result<Vec<BlockOpResult>, CommandError> {
    let mut answers = Vec::with_capacity(ops.len());
    for (handle, op) in ops {
        let (reply, answer) = oneshot::channel();
        if !handles[handle]
            .request(RegionRequest::Blocks { op, reply })
            .await
        {
            return Err(CommandError::simple("argument.pos.unloaded"));
        }
        answers.push(answer);
    }
    let mut results = Vec::with_capacity(answers.len());
    for answer in answers {
        results.push(
            answer
                .await
                .map_err(|_| CommandError::simple("argument.pos.unloaded"))?,
        );
    }
    Ok(results)
}

/// Applies a fill region by region, a window of regions at a time and a
/// slab at a time per region. Chunks that are not loaded are loaded by the
/// owning region, which applies the slab once they arrive.
async fn fill_parts(
    ctx: &mut Context<'_>,
    dimension: DimensionId,
    plan: &FillPlan,
) -> Result<usize, CommandError> {
    let air = Block::Air.default_state();
    let parts = split_by_region(&plan.region);
    let mut total = 0;
    let volume = plan.region.volume();
    let mut visited: i64 = 0;
    if plan.bar {
        fill_bar(ctx, Some((0.0, 0, volume)), true);
    }
    for window in parts.chunks(FILL_REGIONS_IN_FLIGHT) {
        let mut handles = Vec::new();
        let mut queues = Vec::new();
        for (pos, part) in window {
            let Some(handle) = ctx.actor.region(dimension, *pos).await else {
                return Err(CommandError::simple("argument.pos.unloaded"));
            };
            handles.push(handle);
            let mut slab_list = slabs(part);
            slab_list.reverse();
            queues.push(slab_list);
        }
        loop {
            let round: Vec<(usize, Vec<[i32; 3]>)> = queues
                .iter_mut()
                .enumerate()
                .filter_map(|(i, queue)| Some((i, queue.pop()?.positions().collect())))
                .collect();
            if round.is_empty() {
                break;
            }
            let current: Vec<Vec<Option<BlockState>>> = if plan.filter.is_some() {
                let ops = round
                    .iter()
                    .map(|(i, positions)| {
                        (
                            *i,
                            BlockOp::Get {
                                positions: positions.iter().map(|p| to_block_pos(*p)).collect(),
                                load: true,
                            },
                        )
                    })
                    .collect();
                dispatch(&handles, ops)
                    .await?
                    .into_iter()
                    .map(|result| match result {
                        BlockOpResult::States(states) => states,
                        _ => Vec::new(),
                    })
                    .collect()
            } else {
                Vec::new()
            };
            let mut ops = Vec::new();
            for (n, (i, positions)) in round.iter().enumerate() {
                let mut changes = Vec::with_capacity(positions.len());
                for (index, pos) in positions.iter().enumerate() {
                    if let Some(filter) = &plan.filter
                        && !current[n][index].is_some_and(|state| filter.test(state))
                    {
                        continue;
                    }
                    let state = match plan.mode {
                        "outline" if !plan.region.on_surface(*pos) => continue,
                        "hollow" if !plan.region.on_surface(*pos) => air,
                        _ => plan.block.state,
                    };
                    changes.push((to_block_pos(*pos), state));
                }
                let [destroy, keep, strict] = plan.flags;
                ops.push((
                    *i,
                    BlockOp::Set {
                        changes,
                        destroy,
                        keep,
                        strict,
                        quiet: true,
                        data: plan.block.nbt.clone(),
                    },
                ));
            }
            for result in dispatch(&handles, ops).await? {
                if let BlockOpResult::Set(count) = result {
                    total += count;
                }
            }
            visited += round.iter().map(|(_, p)| p.len() as i64).sum::<i64>();
            if plan.bar {
                let fraction = (visited as f64 / volume as f64) as f32;
                fill_bar(ctx, Some((fraction, visited, volume)), false);
            }
        }
    }
    Ok(total)
}

pub struct Fill;

impl Executor for Fill {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            let from = block_pos_arg(ctx, "from")?;
            let to = block_pos_arg(ctx, "to")?;
            let Some(Value::BlockState(block)) = ctx.arg("block").cloned() else {
                return Err(CommandError::simple("command.unknown.argument"));
            };
            let filter = match ctx.arg("filter") {
                Some(Value::BlockPredicate(predicate)) => Some(
                    predicate
                        .clone()
                        .resolved(&ctx.shared().datapacks.contents().tags)?,
                ),
                _ => None,
            };
            let region = BlockBox::from_corners(from, to);
            let limit = fill_limit(ctx);
            if limit.is_some_and(|limit| region.volume() > limit) {
                return Err(CommandError::dynamic(
                    "commands.fill.toobig",
                    [text(limit.unwrap_or_default()), text(region.volume())],
                ));
            }
            let dimension = ctx.source.dimension;
            if !in_world(from) || !in_world(to) {
                return Err(CommandError::simple("argument.pos.unloaded"));
            }
            ensure_loaded(ctx, dimension, &[from, to]).await?;
            let literals = ctx.literals();
            let mode = literals
                .iter()
                .skip(4)
                .find(|l| ["destroy", "hollow", "keep", "outline", "replace", "strict"].contains(l))
                .copied()
                .unwrap_or("replace");
            let (destroy, keep, strict) = match mode {
                "destroy" => (true, false, false),
                "keep" => (false, true, false),
                "strict" => (false, false, true),
                _ => (false, false, false),
            };
            let plan = FillPlan {
                region,
                block,
                filter,
                mode,
                flags: [destroy, keep, strict],
                bar: region.volume() > BAR_ABOVE,
            };
            let count = fill_parts(ctx, dimension, &plan).await;
            if plan.bar {
                fill_bar(ctx, None, false);
            }
            let count = count?;
            if count == 0 {
                return Err(CommandError::simple("commands.fill.failed"));
            }
            ctx.success(
                TextComponent::translate("commands.fill.success", [text(count)]),
                true,
            )
            .await;
            Ok(count as i32)
        })
    }
}

/// Copies `positions` a slab at a time, advancing the progress bar.
async fn clone_blocks(
    ctx: &mut Context<'_>,
    dimension: DimensionId,
    positions: &[[i32; 3]],
    bar: bool,
    volume: i64,
) -> Result<Vec<Option<(BlockState, Option<Compound>)>>, CommandError> {
    let mut copied = Vec::with_capacity(positions.len());
    for slab in positions.chunks(FILL_SLAB as usize) {
        copied.extend(copy_blocks(ctx, dimension, slab).await?);
        if bar {
            let done = copied.len() as i64;
            let fraction = (done as f64 / volume as f64) as f32;
            bar_titled(ctx, "Cloning blocks", Some((fraction, done, volume)), false);
        }
    }
    Ok(copied)
}

/// Pastes `changes` in order a slab at a time.
async fn paste_slabs(
    ctx: &mut Context<'_>,
    dimension: DimensionId,
    changes: &[([i32; 3], BlockState, Option<Compound>)],
    strict: bool,
) -> Result<usize, CommandError> {
    let mut count = 0;
    for slab in changes.chunks(FILL_SLAB as usize) {
        count += paste_blocks(ctx, dimension, slab, strict).await?;
    }
    Ok(count)
}

pub struct Clone;

impl Executor for Clone {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            let begin = block_pos_arg(ctx, "begin")?;
            let end = block_pos_arg(ctx, "end")?;
            let destination = block_pos_arg(ctx, "destination")?;
            let dimension_of =
                |ctx: &Context<'_>, name: &str| -> Result<DimensionId, CommandError> {
                    match ctx.arg(name) {
                        Some(Value::Identifier(key)) => ctx
                            .shared()
                            .dimensions
                            .by_key(key)
                            .map(|d| d.id)
                            .ok_or_else(|| {
                                CommandError::dynamic("argument.dimension.invalid", [key.clone()])
                            }),
                        _ => Ok(ctx.source.dimension),
                    }
                };
            let source_dimension = dimension_of(ctx, "sourceDimension")?;
            let target_dimension = dimension_of(ctx, "targetDimension")?;
            let literals = ctx.literals();
            let filter = match ctx.arg("filter") {
                Some(Value::BlockPredicate(predicate)) => Some(
                    predicate
                        .clone()
                        .resolved(&ctx.shared().datapacks.contents().tags)?,
                ),
                _ => None,
            };
            let masked = literals.contains(&"masked");
            let force = literals.contains(&"force");
            let mode_move = literals.contains(&"move");
            let strict = literals.contains(&"strict");
            let from = BlockBox::from_corners(begin, end);
            let size = [
                from.max[0] - from.min[0],
                from.max[1] - from.min[1],
                from.max[2] - from.min[2],
            ];
            let to = BlockBox::from_corners(
                destination,
                [
                    destination[0] + size[0],
                    destination[1] + size[1],
                    destination[2] + size[2],
                ],
            );
            if !force && !mode_move && source_dimension == target_dimension && from.intersects(&to)
            {
                return Err(CommandError::simple("commands.clone.overlap"));
            }
            let limit = fill_limit(ctx);
            if let Some(limit) = limit
                && from.volume() > limit
            {
                return Err(CommandError::dynamic(
                    "commands.clone.toobig",
                    [text(limit), text(from.volume())],
                ));
            }
            let bar = from.volume() > BAR_ABOVE;
            let corners = [from.min, from.max];
            let destination_corners = [to.min, to.max];
            if corners.iter().any(|c| !in_world(*c))
                || destination_corners.iter().any(|c| !in_world(*c))
            {
                return Err(CommandError::simple("argument.pos.unloaded"));
            }
            ensure_loaded(ctx, source_dimension, &corners).await?;
            ensure_loaded(ctx, target_dimension, &destination_corners).await?;
            let positions: Vec<[i32; 3]> = from.positions().collect();
            let volume = from.volume();
            if bar {
                bar_titled(ctx, "Cloning blocks", Some((0.0, 0, volume)), true);
            }
            let outcome = clone_blocks(ctx, source_dimension, &positions, bar, volume).await;
            let copied = match outcome {
                Ok(copied) => copied,
                Err(err) => {
                    if bar {
                        bar_titled(ctx, "", None, false);
                    }
                    return Err(err);
                }
            };
            let offset = [
                to.min[0] - from.min[0],
                to.min[1] - from.min[1],
                to.min[2] - from.min[2],
            ];
            let air = Block::Air.default_state();
            let mut solid = Vec::new();
            let mut with_entity = Vec::new();
            let mut other = Vec::new();
            let mut cleared = std::collections::VecDeque::new();
            for (pos, entry) in positions.iter().zip(copied) {
                let Some((state, data)) = entry else {
                    continue;
                };
                let kept = match &filter {
                    Some(filter) => filter.test(state),
                    None => !masked || state.block() != Block::Air,
                };
                if !kept {
                    continue;
                }
                let target = [pos[0] + offset[0], pos[1] + offset[1], pos[2] + offset[2]];
                if data.is_some() {
                    with_entity.push((target, state, data));
                    cleared.push_back((*pos, air, None));
                } else if state.is_solid_render() || state.is_collision_full_block() {
                    solid.push((target, state, None));
                    cleared.push_back((*pos, air, None));
                } else {
                    other.push((target, state, None));
                    cleared.push_front((*pos, air, None));
                }
            }
            if mode_move {
                let cleared: Vec<_> = cleared.into_iter().collect();
                paste_blocks(ctx, source_dimension, &cleared, strict).await?;
            }
            let mut changes = solid;
            changes.append(&mut with_entity);
            changes.append(&mut other);
            let count = paste_slabs(ctx, target_dimension, &changes, strict).await;
            if bar {
                bar_titled(ctx, "", None, false);
            }
            let count = count?;
            let ticks = copy_ticks(ctx, source_dimension, &positions, from).await?;
            let moved: Vec<CopiedTick> = ticks
                .into_iter()
                .map(|tick| CopiedTick {
                    pos: BlockPos::new(
                        tick.pos.x + offset[0],
                        tick.pos.y + offset[1],
                        tick.pos.z + offset[2],
                    ),
                    ..tick
                })
                .collect();
            paste_ticks(ctx, target_dimension, &moved).await?;
            if count == 0 {
                return Err(CommandError::simple("commands.clone.failed"));
            }
            ctx.success(
                TextComponent::translate("commands.clone.success", [text(count)]),
                true,
            )
            .await;
            Ok(count as i32)
        })
    }
}

pub struct Summon;

impl Executor for Summon {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            let Some(Value::Resource(name)) = ctx.arg("entity").cloned() else {
                return Err(CommandError::simple("command.unknown.argument"));
            };
            if EntityType::from_name(&name).is_none() && !ctx.shared().entity_types.knows(&name) {
                return Err(CommandError::simple("command.unknown.argument"));
            }
            if let Some(entity_type) = EntityType::from_name(&name)
                && !entity_type.mob().allowed_in_peaceful
                && let Some(outcome) = ctx
                    .ask(|reply| CoordinatorMessage::Difficulty {
                        difficulty: None,
                        reply,
                    })
                    .await
                && matches!(outcome, DifficultyOutcome::Query(Difficulty::Peaceful))
            {
                return Err(CommandError::simple("commands.summon.failed.peaceful"));
            }
            let position = match ctx.arg("pos") {
                Some(Value::Coordinates(coordinates)) => coordinates.position(&ctx.source.pose()),
                _ => ctx.source.position,
            };
            let (nbt, fresh) = match ctx.arg("nbt") {
                Some(Value::Nbt(Tag::Compound(compound))) => (compound.clone(), false),
                _ => (Default::default(), true),
            };
            let dimension = ctx.source.dimension;
            let block = position.map(|c| c.floor() as i32);
            if !in_world(block) {
                return Err(CommandError::simple("commands.summon.invalidPosition"));
            }
            let region_pos = ChunkPos::of_position(position).region();
            let Some(region) = ctx.actor.region(dimension, region_pos).await else {
                return Err(CommandError::simple("commands.summon.failed"));
            };
            if let Some(uuid) = nbt.get("UUID").and_then(Tag::as_uuid) {
                let entities = tuff_core::world::commands::targets::entities(ctx).await;
                let players = tuff_core::world::commands::targets::players(ctx).await;
                if entities.iter().any(|e| e.uuid == uuid) || players.iter().any(|p| p.uuid == uuid)
                {
                    return Err(CommandError::simple("commands.summon.failed.uuid"));
                }
            }
            let mut nbt = nbt;
            nbt.insert("id", name.as_str());
            let display_name = nbt.get("CustomName").and_then(TextComponent::from_nbt);
            let (reply, summoned) = tokio::sync::oneshot::channel();
            let request = RegionRequest::Summon {
                nbt,
                position,
                fresh,
                reply,
            };
            if !region.request(request).await || summoned.await != Ok(true) {
                return Err(CommandError::simple("commands.summon.failed"));
            }
            let (namespace, path) = name.split_once(':').unwrap_or(("minecraft", &name));
            ctx.success(
                TextComponent::translate(
                    "commands.summon.success",
                    [display_name.unwrap_or_else(|| {
                        TextComponent::translate(format!("entity.{namespace}.{path}"), [])
                    })],
                ),
                true,
            )
            .await;
            Ok(1)
        })
    }
}

/// Whether a predicate holds at a position, for `execute if block`.
pub async fn test_block(
    ctx: &mut Context<'_>,
    dimension: DimensionId,
    pos: [i32; 3],
    predicate: &BlockPredicate,
) -> Result<bool, CommandError> {
    let states = read_blocks(ctx, dimension, &[pos], true).await?;
    Ok(states[0].is_some_and(|state| predicate.test(state)))
}

const MAX_FORCELOAD_CHUNKS: i64 = 256;

fn chunk_text(chunk: ChunkPos) -> TextComponent {
    text(format!("[{}, {}]", chunk.x, chunk.z))
}

/// A `column_pos` argument as a chunk position.
fn column_chunk(ctx: &Context<'_>, name: &str) -> Option<ChunkPos> {
    match ctx.arg(name) {
        Some(Value::Coordinates(coordinates)) => {
            let [x, _, z] = coordinates.block(&ctx.source.pose());
            Some(ChunkPos::new(x >> 4, z >> 4))
        }
        _ => None,
    }
}

pub struct ForceLoad;

impl Executor for ForceLoad {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            let literals = ctx.literals();
            let dimension = ctx.source.dimension;
            let key = ctx.dimension_key().to_owned();
            let manager = ctx.shared().manager.clone();
            match literals.get(1).copied() {
                Some("add" | "remove") if !literals.contains(&"all") => {
                    let on = literals[1] == "add";
                    let from = column_chunk(ctx, "from")
                        .ok_or_else(|| CommandError::simple("command.unknown.argument"))?;
                    let to = column_chunk(ctx, "to").unwrap_or(from);
                    let (min_x, max_x) = (from.x.min(to.x), from.x.max(to.x));
                    let (min_z, max_z) = (from.z.min(to.z), from.z.max(to.z));
                    let count = i64::from(max_x - min_x + 1) * i64::from(max_z - min_z + 1);
                    if count > MAX_FORCELOAD_CHUNKS {
                        return Err(CommandError::dynamic(
                            "commands.forceload.toobig",
                            [text(MAX_FORCELOAD_CHUNKS), text(count)],
                        ));
                    }
                    let mut chunks = Vec::new();
                    for z in min_z..=max_z {
                        for x in min_x..=max_x {
                            chunks.push(ChunkPos::new(x, z));
                        }
                    }
                    let (reply, answer) = oneshot::channel();
                    let _ = manager
                        .send(ManagerMessage::ForceLoad {
                            dimension,
                            chunks: chunks.clone(),
                            ticket: ChunkTicket::Forced,
                            on,
                            reply,
                        })
                        .await;
                    let changed = answer.await.unwrap_or_default();
                    let affected: Vec<ChunkPos> = chunks
                        .iter()
                        .zip(changed)
                        .filter(|(_, changed)| *changed)
                        .map(|(chunk, _)| *chunk)
                        .collect();
                    if on {
                        await_forced_chunks(ctx, dimension, &affected).await;
                    }
                    let verb = if on { "added" } else { "removed" };
                    let message = match affected.as_slice() {
                        [] => {
                            return Err(CommandError::simple(
                                format!("commands.forceload.{verb}.failure").as_str(),
                            ));
                        }
                        [only] => TextComponent::translate(
                            format!("commands.forceload.{verb}.single"),
                            [chunk_text(*only), text(&key)],
                        ),
                        _ => TextComponent::translate(
                            format!("commands.forceload.{verb}.multiple"),
                            [
                                text(affected.len()),
                                text(&key),
                                chunk_text(ChunkPos::new(min_x, min_z)),
                                chunk_text(ChunkPos::new(max_x, max_z)),
                            ],
                        ),
                    };
                    ctx.success(message, true).await;
                    Ok(affected.len() as i32)
                }
                Some("remove") => {
                    let (reply, answer) = oneshot::channel();
                    let _ = manager
                        .send(ManagerMessage::ForcedChunks { dimension, reply })
                        .await;
                    let chunks = answer.await.unwrap_or_default();
                    if chunks.is_empty() {
                        ctx.fail(TextComponent::translate(
                            "commands.forceload.added.none",
                            [text(&key)],
                        ));
                        return Ok(0);
                    }
                    let (reply, answer) = oneshot::channel();
                    let _ = manager
                        .send(ManagerMessage::ForceLoad {
                            dimension,
                            chunks,
                            ticket: ChunkTicket::Forced,
                            on: false,
                            reply,
                        })
                        .await;
                    let _ = answer.await;
                    ctx.success(
                        TextComponent::translate("commands.forceload.removed.all", [text(&key)]),
                        true,
                    )
                    .await;
                    Ok(1)
                }
                Some("query") => {
                    let (reply, answer) = oneshot::channel();
                    let _ = manager
                        .send(ManagerMessage::ForcedChunks { dimension, reply })
                        .await;
                    let chunks = answer.await.unwrap_or_default();
                    if let Some(chunk) = column_chunk(ctx, "pos") {
                        if chunks.contains(&chunk) {
                            ctx.success(
                                TextComponent::translate(
                                    "commands.forceload.query.success",
                                    [chunk_text(chunk), text(&key)],
                                ),
                                false,
                            )
                            .await;
                            return Ok(1);
                        }
                        return Err(CommandError::dynamic(
                            "commands.forceload.query.failure",
                            [chunk_text(chunk), text(&key)],
                        ));
                    }
                    if chunks.is_empty() {
                        ctx.fail(TextComponent::translate(
                            "commands.forceload.added.none",
                            [text(&key)],
                        ));
                        return Ok(0);
                    }
                    let list = chunks
                        .iter()
                        .map(|c| format!("[{}, {}]", c.x, c.z))
                        .collect::<Vec<_>>()
                        .join(", ");
                    let message = if chunks.len() == 1 {
                        TextComponent::translate(
                            "commands.forceload.list.single",
                            [text(&key), text(list)],
                        )
                    } else {
                        TextComponent::translate(
                            "commands.forceload.list.multiple",
                            [text(chunks.len()), text(&key), text(list)],
                        )
                    };
                    ctx.success(message, false).await;
                    Ok(chunks.len() as i32)
                }
                _ => Err(CommandError::simple("command.unknown.command")),
            }
        })
    }
}

fn quantize(pos: [i32; 3]) -> [i32; 3] {
    pos.map(|c| (c >> 2) << 2)
}

pub struct FillBiome;

impl Executor for FillBiome {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            let from = block_pos_arg(ctx, "from")?;
            let to = block_pos_arg(ctx, "to")?;
            let Some(Value::Resource(name)) = ctx.arg("biome").cloned() else {
                return Err(CommandError::simple("command.unknown.argument"));
            };
            let biomes = ctx.shared().dimensions.biomes().clone();
            let biome = biomes
                .id(&name)
                .ok_or_else(|| CommandError::simple("command.unknown.argument"))?;
            let filter = match ctx.arg("filter").cloned() {
                Some(Value::ResourceOrTag { name, tag }) => {
                    let tags = ctx.shared().datapacks.contents().tags.clone();
                    let members = super::locate::biome_members(&biomes, &name, tag, &tags)
                        .ok_or_else(|| {
                            super::locate::unknown_tag(&name, "minecraft:worldgen/biome")
                        })?;
                    Some(
                        members
                            .into_iter()
                            .filter_map(|name| biomes.id(name))
                            .collect::<Vec<_>>(),
                    )
                }
                _ => None,
            };
            let dimension = ctx.source.dimension;
            if !in_world(from) || !in_world(to) {
                return Err(CommandError::simple("argument.pos.unloaded"));
            }
            ensure_loaded(ctx, dimension, &[from, to]).await?;
            let region = BlockBox::from_corners(quantize(from), quantize(to));
            let volume = region.volume();
            let limit = fill_limit(ctx);
            if limit.is_some_and(|limit| volume > limit) {
                return Err(CommandError::dynamic(
                    "commands.fillbiome.toobig",
                    [text(limit.unwrap_or_default()), text(volume)],
                ));
            }
            let bar = volume > BAR_ABOVE;
            let result = fill_biome_parts(ctx, dimension, &region, biome, filter, bar).await;
            if bar {
                fill_bar(ctx, None, false);
            }
            let count = result?;
            if count == 0 {
                return Err(CommandError::simple("commands.fillbiome.no_changes"));
            }
            ctx.success(
                TextComponent::translate(
                    "commands.fillbiome.success.count",
                    [
                        text(count),
                        text(region.min[0]),
                        text(region.min[1]),
                        text(region.min[2]),
                        text(region.max[0]),
                        text(region.max[1]),
                        text(region.max[2]),
                    ],
                ),
                true,
            )
            .await;
            Ok(count as i32)
        })
    }
}

/// Checks that every chunk of the box is loaded, then fills it region by
/// region, a window of regions at a time.
async fn fill_biome_parts(
    ctx: &mut Context<'_>,
    dimension: DimensionId,
    region: &BlockBox,
    biome: u16,
    filter: Option<Vec<u16>>,
    bar: bool,
) -> Result<usize, CommandError> {
    let parts = split_by_region(region);
    let volume = region.volume();
    let mut windows = Vec::new();
    for window in parts.chunks(FILL_REGIONS_IN_FLIGHT) {
        let mut handles = Vec::new();
        for (pos, _) in window {
            let Some(handle) = ctx.actor.region(dimension, *pos).await else {
                return Err(CommandError::simple("argument.pos.unloaded"));
            };
            handles.push(handle);
        }
        let ops = |apply: bool| -> Vec<(usize, BlockOp)> {
            window
                .iter()
                .enumerate()
                .map(|(i, (_, part))| {
                    (
                        i,
                        BlockOp::FillBiome {
                            min: to_block_pos(part.min),
                            max: to_block_pos(part.max),
                            biome,
                            filter: filter.clone(),
                            apply,
                        },
                    )
                })
                .collect()
        };
        let missing: usize = dispatch(&handles, ops(false))
            .await?
            .into_iter()
            .map(|result| match result {
                BlockOpResult::Set(n) => n,
                _ => 0,
            })
            .sum();
        if missing > 0 {
            return Err(CommandError::simple("argument.pos.unloaded"));
        }
        windows.push((
            handles,
            ops(true),
            window.iter().map(|(_, p)| p.volume()).sum::<i64>(),
        ));
    }
    let mut total = 0;
    let mut visited = 0;
    if bar {
        fill_bar(ctx, Some((0.0, 0, volume)), true);
    }
    for (handles, ops, size) in windows {
        for result in dispatch(&handles, ops).await? {
            if let BlockOpResult::Set(n) = result {
                total += n;
            }
        }
        visited += size;
        if bar {
            let fraction = (visited as f64 / volume as f64) as f32;
            fill_bar(ctx, Some((fraction, visited, volume)), false);
        }
    }
    Ok(total)
}
