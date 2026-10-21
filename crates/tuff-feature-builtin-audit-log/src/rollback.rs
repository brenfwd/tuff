//! Rolling logged block changes back, or restoring rolled-back ones. Rows
//! reduce to one target state per position, which go to the region actor
//! that owns each position (so a job may span any number of regions), many
//! regions at once. Rows are marked, never deleted.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use tokio::sync::oneshot;
use tokio::task::JoinSet;
use tuff_core::protocol::play::BlockUpdate;
use tuff_core::world::dimension::DimensionId;
use tuff_core::world::inventory_edit::InventoryEdit;
use tuff_core::world::messages::Packets;
use tuff_core::world::messages::RegionHandle;
use tuff_core::world::messages::{BlockOp, CoordinatorMessage, ManagerMessage, RegionRequest};
use tuff_core::world::region_api::{BlockCause, ContainerHolder, RegionTask, RegionWorld};
use tuff_core::world::{Shared, framed};
use tuff_item::item::ItemStack;
use tuff_nbt::Compound;
use tuff_vanilla_data::BlockState;
use tuff_vanilla_data::Item;
use tuff_world_model::BlockPos;
use tuff_world_model::pos::{ChunkPos, RegionPos};

use crate::container;
use crate::entity;
use crate::filter::Filter;
use crate::record::Kind;
use crate::service::{AuditLog, Job, Undo};
use crate::store::{MAX_ROWS, Order, Row};

/// Regions changed at once.
pub const PARALLEL_REGIONS: usize = 8;

type Change = (BlockPos, BlockState, Option<Compound>);

/// What one region gets: its block changes and the rows behind them,
/// then its container rows to undo in job order.
#[derive(Default)]
pub struct Batch {
    pub changes: Vec<Change>,
    pub ids: Vec<i64>,
    pub containers: Vec<(BlockPos, container::Undo)>,
    pub entities: Vec<entity::Undo>,
}

/// A job's work, grouped by owning region.
#[derive(Default)]
pub struct Plan {
    pub regions: BTreeMap<(DimensionId, RegionPos), Batch>,
    /// Pickups to take back from (or give again to) players, in job order.
    pub inventories: Vec<PlayerUndo>,
    pub rows: usize,
    /// Rows whose stored state or world could not be used.
    pub skipped: usize,
    /// The row limit was reached; running again picks up the rest.
    pub truncated: bool,
}

/// Undoing one pickup row on the inventory of the player who made it.
#[derive(Clone, Debug, PartialEq)]
pub struct PlayerUndo {
    pub id: i64,
    pub uuid: u128,
    pub edit: InventoryEdit,
}

/// The rows a rollback (or restore) of `filter` uses: block and container
/// rows not yet rolled back (or rolled back), newest first for a rollback
/// so the oldest state wins, oldest first for a restore so the newest wins.
pub fn job_filter(mut filter: Filter, restore: bool) -> (Filter, Order) {
    filter.kinds.retain(|kind| {
        kind.is_block()
            || kind.is_container()
            || kind.is_kill()
            || matches!(kind, Kind::Sign | Kind::ItemAdd)
    });
    if filter.kinds.is_empty() {
        filter.kinds = vec![
            Kind::BlockBreak,
            Kind::BlockPlace,
            Kind::ContainerRemove,
            Kind::ContainerAdd,
            Kind::Kill,
            Kind::Sign,
            Kind::ItemAdd,
        ];
    }
    filter.rolled_back = Some(restore);
    let order = if restore {
        Order::OldestFirst
    } else {
        Order::NewestFirst
    };
    (filter, order)
}

/// Reduces `rows` (in job order) to each position's final state.
pub fn plan(shared: &Shared, rows: &[Row], restore: bool, strict: bool) -> Plan {
    let mut plan = Plan {
        rows: rows.len(),
        truncated: rows.len() >= MAX_ROWS,
        ..Plan::default()
    };
    let mut targets: HashMap<(DimensionId, BlockPos), (BlockState, Option<Compound>)> =
        HashMap::new();
    let mut ids: BTreeMap<(DimensionId, RegionPos), Vec<i64>> = BTreeMap::new();
    for row in rows {
        let Some(dimension) = shared.dimensions.by_key(&row.world).map(|d| d.id) else {
            plan.skipped += 1;
            continue;
        };
        if row.kind == Some(Kind::ItemAdd) {
            match pickup_undo(row, restore) {
                Some(undo) => plan.inventories.push(undo),
                None => plan.skipped += 1,
            }
            continue;
        }
        if row.kind.is_some_and(Kind::is_kill) {
            let pos = BlockPos::new(row.pos[0], row.pos[1], row.pos[2]);
            match entity_undo(row, restore, pos) {
                Some(undo) => plan
                    .regions
                    .entry((dimension, ChunkPos::of_block(pos).region()))
                    .or_default()
                    .entities
                    .push(undo),
                None => plan.skipped += 1,
            }
            continue;
        }
        if row.kind.is_some_and(Kind::is_container) {
            match container_undo(row, restore, strict) {
                Some(undo) => {
                    let pos = BlockPos::new(row.pos[0], row.pos[1], row.pos[2]);
                    plan.regions
                        .entry((dimension, ChunkPos::of_block(pos).region()))
                        .or_default()
                        .containers
                        .push((pos, undo));
                }
                None => plan.skipped += 1,
            }
            continue;
        }
        let stored = if restore { &row.new } else { &row.old };
        let Some(state) = stored
            .as_deref()
            .and_then(|state| state.parse::<BlockState>().ok())
        else {
            plan.skipped += 1;
            continue;
        };
        let data = if row.kind == Some(Kind::Sign) {
            sign_side(row, restore)
        } else if restore {
            None
        } else {
            row.block_entity()
        };
        let pos = BlockPos::new(row.pos[0], row.pos[1], row.pos[2]);
        targets.insert((dimension, pos), (state, data));
        ids.entry((dimension, ChunkPos::of_block(pos).region()))
            .or_default()
            .push(row.id);
    }
    for ((dimension, pos), (state, data)) in targets {
        plan.regions
            .entry((dimension, ChunkPos::of_block(pos).region()))
            .or_default()
            .changes
            .push((pos, state, data));
    }
    for (key, ids) in ids {
        plan.regions.entry(key).or_default().ids = ids;
    }
    for batch in plan.regions.values_mut() {
        batch.changes.sort_by_key(|(pos, ..)| (pos.y, pos.x, pos.z));
    }
    plan
}

/// What undoing `row` (a `+item` row) does to the inventory of the player
/// who picked the items up: take them back, or on a restore give them
/// again. `None` for a craft, a pickup by a mob, or an unreadable item.
pub fn pickup_undo(row: &Row, restore: bool) -> Option<PlayerUndo> {
    if row.text.is_some() || row.amount <= 0 {
        return None;
    }
    let stack = container::stack_of(row.block_entity().as_ref())?;
    let count = u32::try_from(row.amount).ok()?;
    let edit = if restore {
        InventoryEdit::Give { stack, count }
    } else {
        InventoryEdit::Take { stack, count }
    };
    Some(PlayerUndo {
        id: row.id,
        uuid: row.uuid?,
        edit,
    })
}

impl PlayerUndo {
    fn edit_count(&self) -> u32 {
        match &self.edit {
            InventoryEdit::Take { count, .. } | InventoryEdit::Give { count, .. } => *count,
        }
    }
}

/// Applies `undo` through the coordinator, which owns online and offline
/// players' inventories alike; true if it applied.
async fn apply_inventory(shared: &Shared, undo: PlayerUndo) -> bool {
    let (reply, answer) = oneshot::channel();
    let message = CoordinatorMessage::EditInventory {
        uuid: undo.uuid,
        edit: undo.edit,
        reply,
    };
    shared.coordinator.send(message).await.is_ok() && answer.await.unwrap_or(false)
}

/// The sign block entity a rollback (`old`) or restore (`new`) puts back.
fn sign_side(row: &Row, restore: bool) -> Option<Compound> {
    match row
        .block_entity()?
        .get(if restore { "new" } else { "old" })?
    {
        tuff_nbt::Tag::Compound(nbt) => Some(nbt.clone()),
        _ => None,
    }
}

/// What undoing `row` (a kill row) does: summon the victim again, or on a
/// restore remove it. `None` for a player, or a victim whose data is
/// missing or unreadable.
pub fn entity_undo(row: &Row, restore: bool, pos: BlockPos) -> Option<entity::Undo> {
    let uuid = entity::parse_text(row.text.as_deref()?)?;
    let summon = if restore {
        None
    } else {
        let nbt = row.block_entity()?;
        nbt.get_str("id")?;
        let at = entity::position_of(&nbt, pos);
        Some((nbt, at))
    };
    Some(entity::Undo {
        id: row.id,
        uuid,
        pos,
        summon,
    })
}

/// Summons or removes the victims of `undos` in this region, returning
/// how many changed and the rows that did.
async fn apply_entities(handle: &RegionHandle, undos: Vec<entity::Undo>) -> (usize, Vec<i64>) {
    let mut changed = 0;
    let mut ids = Vec::new();
    for undo in undos {
        let (reply, answer) = oneshot::channel();
        let done = match undo.summon {
            Some((nbt, position)) => {
                let sent = handle
                    .request(RegionRequest::Summon {
                        nbt,
                        position,
                        fresh: false,
                        reply,
                    })
                    .await;
                sent && answer.await.unwrap_or(false)
            }
            None => {
                let uuid = undo.uuid;
                let run = RegionTask::new(move |world| {
                    let found = world.entity_ids().into_iter().find(|id| {
                        world
                            .entity(*id)
                            .is_some_and(|t| t.entity.base().uuid == uuid)
                    });
                    let _ = reply.send(found.is_some_and(|id| world.remove_entity(id).is_some()));
                });
                handle
                    .request(RegionRequest::WhenLoaded {
                        min: undo.pos,
                        max: undo.pos,
                        run,
                    })
                    .await
                    && answer.await.unwrap_or(false)
            }
        };
        if done {
            changed += 1;
            ids.push(undo.id);
        }
    }
    (changed, ids)
}

/// What undoing `row` (a container row) does, or `None` if its item is
/// unknown or cannot be put back whole.
pub fn container_undo(row: &Row, restore: bool, strict: bool) -> Option<container::Undo> {
    let place = row
        .text
        .as_deref()
        .map(container::parse_text)
        .unwrap_or(container::Place {
            slot: None,
            entity: None,
        });
    let add = (row.kind? == Kind::ContainerRemove) != restore;
    let (stack, any_components) = match container::stack_of(row.block_entity().as_ref()) {
        Some(stack) => (stack, false),
        None if !add => (
            ItemStack::new(Item::from_name(row.material.as_deref()?)?, 1),
            true,
        ),
        None => return None,
    };
    let taker = (strict && add && !restore).then_some(row.uuid).flatten();
    Some(container::Undo {
        id: row.id,
        entity: place.entity.map(|(_, uuid)| uuid),
        stack,
        any_components,
        amount: row.amount.max(0),
        add,
        slot: place.slot,
        taker,
    })
}

fn is_pot(world: &dyn RegionWorld, pos: BlockPos) -> bool {
    world
        .block(pos)
        .is_some_and(|state| state.block() == tuff_vanilla_data::Block::DecoratedPot)
}

fn pot_item(world: &dyn RegionWorld, pos: BlockPos) -> Option<ItemStack> {
    world
        .raw_block_entity(pos)?
        .get("item")
        .and_then(|tag| match tag {
            tuff_nbt::Tag::Compound(nbt) => ItemStack::from_nbt(nbt),
            _ => None,
        })
}

/// Undoes `undos` on the containers this region holds, in order, under no
/// cause (so it is not logged again); returns each row that moved any
/// items with how many.
fn undo_containers(
    world: &mut dyn RegionWorld,
    undos: Vec<(BlockPos, container::Undo)>,
) -> Vec<(i64, i64)> {
    let mut done = Vec::new();
    world.caused(BlockCause::World, |world| {
        for (pos, undo) in undos {
            let held = undo
                .entity
                .filter(|uuid| world.entity_container_items(*uuid).is_none())
                .and_then(|uuid| world.entity_held_items(uuid))
                .is_some();
            let pot =
                undo.entity.is_none() && world.container_items(pos).is_none() && is_pot(world, pos);
            let items = match undo.entity {
                Some(uuid) if held => world.entity_held_items(uuid),
                Some(uuid) => world.entity_container_items(uuid).map(<[_]>::to_vec),
                None if pot => Some(vec![pot_item(world, pos)]),
                None => world.container_items(pos).map(<[_]>::to_vec),
            };
            let Some(before) = items else {
                continue;
            };
            let mut after = before.clone();
            let count = container::apply(&mut after, &undo);
            if count == 0 {
                continue;
            }
            if pot {
                let (old, new) = (before[0].clone(), after[0].clone());
                if let Some(nbt) = world.raw_block_entity_mut(pos) {
                    match &new {
                        Some(stack) => {
                            nbt.insert("item", stack.to_nbt());
                        }
                        None => {
                            nbt.remove("item");
                        }
                    }
                }
                world.block_entity_changed(pos);
                world.report_container_edit(
                    ContainerHolder::Block(pos),
                    &[(0, old.as_ref(), new.as_ref())],
                );
                done.push((undo.id, count));
                continue;
            }
            let slots: Vec<_> = before
                .iter()
                .zip(after)
                .enumerate()
                .filter(|(_, (old, new))| *old != new)
                .map(|(slot, (_, new))| (slot, new))
                .collect();
            let set = match undo.entity {
                Some(uuid) if held => world.set_entity_held_items(uuid, slots),
                Some(uuid) => world.set_entity_container_items(uuid, slots),
                None => world.set_container_items(pos, slots),
            };
            if set {
                done.push((undo.id, count));
            }
        }
    });
    done
}

/// Runs the container undos of one region, chunk by chunk with each
/// chunk loaded.
async fn apply_containers(
    handle: &RegionHandle,
    containers: Vec<(BlockPos, container::Undo)>,
    done: &mut Vec<(i64, i64)>,
) -> bool {
    let mut by_chunk: BTreeMap<(i32, i32), Vec<(BlockPos, container::Undo)>> = BTreeMap::new();
    for (pos, undo) in containers {
        let chunk = ChunkPos::of_block(pos);
        by_chunk
            .entry((chunk.x, chunk.z))
            .or_default()
            .push((pos, undo));
    }
    for undos in by_chunk.into_values() {
        let pos = undos[0].0;
        let (reply, answer) = oneshot::channel();
        let run = RegionTask::new(move |world| {
            let _ = reply.send(undo_containers(world, undos));
        });
        if !handle
            .request(RegionRequest::WhenLoaded {
                min: pos,
                max: pos,
                run,
            })
            .await
        {
            return false;
        }
        let Ok(moved) = answer.await else {
            return false;
        };
        done.extend(moved);
    }
    true
}

/// Sends `changes` to the region owning them, then undoes its container
/// rows; `None` if it is gone.
async fn apply_region(
    shared: Arc<Shared>,
    dimension: DimensionId,
    region: RegionPos,
    changes: Vec<Change>,
    containers: Vec<(BlockPos, container::Undo)>,
    entities: Vec<entity::Undo>,
    container_done: &mut Vec<(i64, i64)>,
) -> Option<(usize, Vec<i64>)> {
    let (reply, handle) = oneshot::channel();
    let acquire = ManagerMessage::Acquire {
        dimension,
        region,
        reply,
    };
    if shared.manager.send(acquire).await.is_err() {
        return None;
    }
    let Ok(handle) = handle.await else {
        return None;
    };
    let mut applied = Some((0, Vec::new()));
    if !changes.is_empty() {
        let (reply, answer) = oneshot::channel();
        let op = BlockOp::Paste {
            changes,
            strict: false,
        };
        if !(handle.request(RegionRequest::Blocks { op, reply }).await && answer.await.is_ok()) {
            applied = None;
        }
    }
    if applied.is_some()
        && !containers.is_empty()
        && !apply_containers(&handle, containers, container_done).await
    {
        applied = None;
    }
    if let Some((moved, ids)) = applied.as_mut() {
        *moved += container_done
            .iter()
            .map(|(_, n)| *n as usize)
            .sum::<usize>();
        ids.extend(container_done.iter().map(|(id, _)| *id));
    }
    if let Some((moved, ids)) = applied.as_mut()
        && !entities.is_empty()
    {
        let (changed, done) = apply_entities(&handle, entities).await;
        *moved += changed;
        ids.extend(done);
    }
    let _ = shared
        .manager
        .send(ManagerMessage::Release { dimension, region })
        .await;
    applied
}

/// What a job did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    pub rows: usize,
    pub blocks: usize,
    pub items: usize,
    pub regions: usize,
    pub failed_regions: usize,
    pub skipped: usize,
    /// Strict rows whose player no longer held the items taken back.
    pub strict_refused: usize,
    /// Pickup rows whose player no longer held the items (or had no room
    /// for them, or was never seen).
    pub refused: usize,
    pub truncated: bool,
    pub cancelled: bool,
    pub ids: Vec<i64>,
}

/// One region's part of a job, once sent.
struct Applied {
    ok: bool,
    blocks: usize,
    items: usize,
    ids: Vec<i64>,
    strict_refused: usize,
}

/// Items a strict rollback took from players ahead of putting them back.
#[derive(Default)]
struct Takes {
    taken: Vec<(u128, container::Undo)>,
    refused: usize,
}

/// Takes the items of every strict undo from the player who removed them
/// (all or nothing per row) and drops the undos whose player could not
/// give them, so nothing is put back that was not taken.
async fn take_from_players(
    shared: &Shared,
    containers: Vec<(BlockPos, container::Undo)>,
) -> (Vec<(BlockPos, container::Undo)>, Takes) {
    let mut takes = Takes::default();
    let mut kept = Vec::with_capacity(containers.len());
    for (pos, undo) in containers {
        let Some(uuid) = undo.taker else {
            kept.push((pos, undo));
            continue;
        };
        let Ok(count) = u32::try_from(undo.amount) else {
            takes.refused += 1;
            continue;
        };
        let edit = InventoryEdit::Take {
            stack: undo.stack.clone(),
            count,
        };
        if apply_inventory(
            shared,
            PlayerUndo {
                id: undo.id,
                uuid,
                edit,
            },
        )
        .await
        {
            takes.taken.push((uuid, undo.clone()));
            kept.push((pos, undo));
        } else {
            takes.refused += 1;
        }
    }
    (kept, takes)
}

/// Gives back to the players what their containers did not take after
/// all (full, gone, or the region failed).
async fn refund_players(shared: &Shared, taken: Vec<(u128, container::Undo)>, done: &[(i64, i64)]) {
    for (uuid, undo) in taken {
        let moved = done
            .iter()
            .filter(|(id, _)| *id == undo.id)
            .map(|(_, n)| *n)
            .sum::<i64>();
        let Ok(left) = u32::try_from(undo.amount - moved) else {
            continue;
        };
        if left == 0 {
            continue;
        }
        let edit = InventoryEdit::Give {
            stack: undo.stack,
            count: left,
        };
        if !apply_inventory(
            shared,
            PlayerUndo {
                id: undo.id,
                uuid,
                edit,
            },
        )
        .await
        {
            log::warn!("audit log: could not give {left} items back to {uuid:032x}");
        }
    }
}

/// Rolls back (or restores) the rows `filter` matches on behalf of `by`:
/// the whole job a command runs, for features and the panel to ask for.
/// Fails when another job is running or the log is not.
pub async fn request(
    shared: &Arc<Shared>,
    log: &AuditLog,
    mut filter: Filter,
    restore: bool,
    strict: bool,
    by: String,
) -> Result<Outcome, String> {
    if !log.running() {
        return Err("the audit log is not running".to_owned());
    }
    let _ = log.sync().await;
    if let Some(ids) = Some(&mut filter.row_ids).filter(|ids| ids.len() > MAX_ROWS) {
        ids.truncate(MAX_ROWS);
    }
    let (filter, order) = job_filter(filter, restore);
    let rows = log.lookup(filter, order, MAX_ROWS, 0).await?;
    let plan = plan(shared, &rows, restore, strict);
    let job = Arc::new(Job {
        restore,
        by,
        regions: plan.regions.len(),
        done: std::sync::atomic::AtomicUsize::new(0),
        cancelled: std::sync::atomic::AtomicBool::new(false),
    });
    if !log.begin(job.clone()) {
        return Err("A rollback or restore is already running".to_owned());
    }
    let outcome = run(shared, log, plan, &job).await;
    log.end();
    if !outcome.ids.is_empty() {
        log.remember(
            &job.by,
            Undo {
                restore,
                ids: outcome.ids.clone(),
            },
        );
    }
    Ok(outcome)
}

/// Runs `plan` under `job`, marking the rows of every region that took
/// its changes.
pub async fn run(shared: &Arc<Shared>, log: &AuditLog, plan: Plan, job: &Job) -> Outcome {
    let mut outcome = Outcome {
        rows: plan.rows,
        skipped: plan.skipped,
        truncated: plan.truncated,
        ..Outcome::default()
    };
    let mut running: JoinSet<Applied> = JoinSet::new();
    let mut applied_ids: Vec<i64> = Vec::new();
    let mut finish = |result: Result<Applied, tokio::task::JoinError>, outcome: &mut Outcome| {
        match result {
            Ok(applied) if applied.ok => {
                outcome.regions += 1;
                outcome.blocks += applied.blocks;
                outcome.items += applied.items;
                outcome.strict_refused += applied.strict_refused;
                applied_ids.extend(applied.ids);
            }
            Ok(applied) => {
                outcome.strict_refused += applied.strict_refused;
                outcome.failed_regions += 1;
            }
            Err(_) => outcome.failed_regions += 1,
        }
        job.done.fetch_add(1, Ordering::Relaxed);
    };
    for ((dimension, region), batch) in plan.regions {
        if job.cancelled.load(Ordering::Relaxed) {
            outcome.cancelled = true;
            break;
        }
        if running.len() >= PARALLEL_REGIONS
            && let Some(result) = running.join_next().await
        {
            finish(result, &mut outcome);
        }
        let shared = shared.clone();
        running.spawn(async move {
            let blocks = batch.changes.len();
            let (containers, takes) = take_from_players(&shared, batch.containers).await;
            let mut container_done = Vec::new();
            let applied = apply_region(
                shared.clone(),
                dimension,
                region,
                batch.changes,
                containers,
                batch.entities,
                &mut container_done,
            )
            .await;
            let strict_refused = takes.refused;
            refund_players(&shared, takes.taken, &container_done).await;
            match applied {
                Some((items, container_ids)) => {
                    let mut ids = batch.ids;
                    ids.extend(container_ids);
                    Applied {
                        ok: true,
                        blocks,
                        items,
                        ids,
                        strict_refused,
                    }
                }
                None => Applied {
                    ok: false,
                    blocks: 0,
                    items: 0,
                    ids: Vec::new(),
                    strict_refused,
                },
            }
        });
    }
    while let Some(result) = running.join_next().await {
        finish(result, &mut outcome);
    }
    for undo in plan.inventories {
        if job.cancelled.load(Ordering::Relaxed) {
            outcome.cancelled = true;
            break;
        }
        let (id, count) = (undo.id, undo.edit_count());
        if apply_inventory(shared, undo).await {
            outcome.items += count as usize;
            applied_ids.push(id);
        } else {
            outcome.refused += 1;
        }
    }
    if !applied_ids.is_empty() {
        let restore = job.restore;
        outcome.ids = applied_ids.clone();
        if !matches!(log.mark(applied_ids, !restore).await, Ok(true)) {
            log::error!("audit log: a finished job's rows could not be marked");
        }
    }
    outcome
}

/// What a preview sends one player: the target states, and the current
/// states to put back if it is cancelled. Only blocks in `world`.
pub fn preview_packets(rows: &[Row], restore: bool, world: &str) -> (Packets, Packets) {
    let mut targets: HashMap<[i32; 3], (BlockState, BlockState)> = HashMap::new();
    let mut seen: HashMap<[i32; 3], BlockState> = HashMap::new();
    for row in rows.iter().filter(|row| row.world == world) {
        let (target, current) = if restore {
            (row.new.clone(), row.old.clone())
        } else {
            (row.old.clone(), row.new.clone())
        };
        let parse = |state: Option<String>| state?.parse::<BlockState>().ok();
        let (Some(target), Some(current)) = (parse(target), parse(current)) else {
            continue;
        };
        let current = *seen.entry(row.pos).or_insert(current);
        targets.insert(row.pos, (target, current));
    }
    let frame = |pick: fn(&(BlockState, BlockState)) -> BlockState| -> Packets {
        targets
            .iter()
            .map(|(pos, states)| {
                framed(&BlockUpdate {
                    position: BlockPos::new(pos[0], pos[1], pos[2]),
                    state: pick(states).id(),
                })
            })
            .collect()
    };
    (frame(|s| s.0), frame(|s| s.1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_vanilla_data::Block;

    fn row(id: i64, world: &str, old: &str, new: &str) -> Row {
        Row {
            id,
            time: id,
            user: "a".into(),
            kind: Some(Kind::BlockBreak),
            world: world.into(),
            pos: [1, 2, 3],
            material: None,
            old: Some(old.into()),
            new: Some(new.into()),
            amount: 0,
            text: None,
            data: None,
            rolled_back: false,
            uuid: None,
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_preview_shows_the_oldest_old_state_and_reverts_to_the_newest_new() {
        let overworld = "minecraft:overworld";
        let rows = [
            row(2, overworld, "minecraft:dirt", "minecraft:air"),
            row(1, overworld, "minecraft:stone", "minecraft:dirt"),
            row(
                3,
                "minecraft:the_nether",
                "minecraft:stone",
                "minecraft:air",
            ),
        ];
        let (shown, revert) = preview_packets(&rows, false, overworld);
        let stone = framed(&BlockUpdate {
            position: BlockPos::new(1, 2, 3),
            state: Block::Stone.default_state().id(),
        });
        let air = framed(&BlockUpdate {
            position: BlockPos::new(1, 2, 3),
            state: Block::Air.default_state().id(),
        });
        assert_eq!(&*shown, [stone]);
        assert_eq!(&*revert, [air]);
    }
}
