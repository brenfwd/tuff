//! Where actions are seen: region hooks for blocks, a packet interceptor
//! for chat and commands, session hooks for joins and leaves. Each asks
//! the service whether the action is logged before building a record.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tuff_core::protocol::play::Serverbound;
use tuff_core::world::Shared;
use tuff_core::world::dimension::DimensionId;
use tuff_core::world::entity::RegionEntity;
use tuff_core::world::items::ItemEntity;
use tuff_core::world::messages::PlayerId;
use tuff_core::world::region_api::{
    BlockAction, BlockCause, BlockChange, ContainerEdit, EntityDeath, ItemPickup, ItemTaker,
    PlayerBlockAction, PlayerBlockUse, RegionHooks, RegionWorld, SignEdit, TickPhase,
};
use tuff_core::world::session::PlayerSession;
use tuff_core::world::session::hooks::{
    HookFuture, ItemAction, PacketInterceptor, SessionHooks, Verdict,
};
use tuff_nbt::{Compound, Tag};
use tuff_vanilla_data::{Block, BlockState};
use tuff_world_model::BlockPos;

use crate::command::line_component;
use crate::container::{MAX_PENDING, Pending};
use crate::filter::{Area, Filter};
use crate::record::{Kind, Record, Who, now_ms};
use crate::service::AuditLog;
use crate::store::Order;
use tuff_core::protocol::play::{BlockChangedAck, PlayerActionStatus};
use tuff_core::world::commands::text;

fn service(shared: &Shared) -> Option<&AuditLog> {
    shared.services.get::<AuditLog>()
}

fn block_pos(pos: BlockPos) -> [i32; 3] {
    [pos.x, pos.y, pos.z]
}

/// The player `id` acting in this region, from the published player index.
fn player(world: &dyn RegionWorld, id: PlayerId) -> Who {
    let index = world.shared().players.borrow().clone();
    index
        .near(world.dimension_id(), world.region_pos())
        .iter()
        .find(|player| player.handle.id == id)
        .map_or_else(
            || Who::cause("#player"),
            |player| Who::Player {
                uuid: player.uuid,
                name: player.name.clone(),
            },
        )
}

/// The player `uuid` near this region, for an explosion they lit.
fn player_by_uuid(world: &dyn RegionWorld, uuid: u128) -> Option<Who> {
    let index = world.shared().players.borrow().clone();
    index
        .near(world.dimension_id(), world.region_pos())
        .iter()
        .find(|player| player.uuid == uuid)
        .map(|player| Who::Player {
            uuid: player.uuid,
            name: player.name.clone(),
        })
}

/// How a block's own doing is written: `#fire`, `#decay`, `#grow`, or
/// `None` for the block's own name.
pub fn block_cause_name(block: Block) -> Option<&'static str> {
    let name = block.name();
    let name = name.strip_prefix("minecraft:").unwrap_or(name);
    Some(match block {
        Block::Fire | Block::SoulFire => "#fire",
        Block::Water | Block::BubbleColumn => "#water",
        Block::Lava => "#lava",
        Block::Piston | Block::StickyPiston | Block::PistonHead | Block::MovingPiston => "#piston",
        Block::Dispenser | Block::Dropper => "#dispenser",
        _ if name.ends_with("_leaves") => "#decay",
        _ if block.is_falling() => "#falling_block",
        _ if grows(name) => "#grow",
        _ => return None,
    })
}

fn grows(name: &str) -> bool {
    name.ends_with("_sapling")
        || name.ends_with("_propagule")
        || name.ends_with("_mushroom")
        || name.ends_with("_fungus")
        || name.ends_with("azalea")
        || matches!(
            name,
            "cactus"
                | "sugar_cane"
                | "bamboo"
                | "vine"
                | "kelp"
                | "chorus_flower"
                | "cave_vines"
                | "weeping_vines"
                | "twisting_vines"
                | "pumpkin_stem"
                | "melon_stem"
                | "pitcher_crop"
                | "sweet_berry_bush"
        )
}

/// Who a block change is logged under, or `None` when it is not logged.
fn cause_who(world: &dyn RegionWorld, cause: BlockCause) -> Option<Who> {
    match cause {
        BlockCause::World => None,
        BlockCause::Player(id) => Some(player(world, id)),
        BlockCause::Entity { kind, .. } => Some(Who::cause(kind.name())),
        BlockCause::Block(block) => Some(Who::cause(
            block_cause_name(block).unwrap_or_else(|| block.name()),
        )),
        BlockCause::Weather => Some(Who::cause("#weather")),
        BlockCause::Explosion { igniter, culprit } => Some(
            igniter
                .and_then(|uuid| player_by_uuid(world, uuid))
                .or_else(|| culprit.map(|kind| Who::cause(kind.name())))
                .unwrap_or_else(|| Who::cause("#explosion")),
        ),
    }
}

fn is_air(state: BlockState) -> bool {
    matches!(state.block(), Block::Air | Block::CaveAir | Block::VoidAir)
}

/// Whether `new` taking `old`'s place removes it rather than placing
/// something there: air, fire, a liquid or a piston taking it.
fn removes(old: BlockState, new: BlockState) -> bool {
    !is_air(old)
        && (is_air(new)
            || matches!(
                new.block(),
                Block::Fire | Block::SoulFire | Block::Water | Block::Lava | Block::MovingPiston
            ))
}

/// Natural changes nobody wants back: fire going out, a flow drying up.
fn natural_noise(change: &BlockChange) -> bool {
    let old = change.old.block();
    matches!(old, Block::Fire | Block::SoulFire)
        || matches!(old, Block::Water | Block::Lava | Block::BubbleColumn) && is_air(change.new)
}

/// A piston extending or retracting, which a rollback of what it moved
/// must undo too.
fn extends(change: &BlockChange) -> bool {
    matches!(change.old.block(), Block::Piston | Block::StickyPiston)
}

/// A player's changes that keep the block (a slab made double, a candle
/// added), held until the action behind them says whether it placed.
#[derive(Default)]
struct SameBlockChanges(Vec<BlockChange>);

const SAME_BLOCK_CHANGES: usize = 16;

fn block_record(world: &dyn RegionWorld, who: Who, change: &BlockChange) -> Record {
    let kind = if removes(change.old, change.new) {
        Kind::BlockBreak
    } else {
        Kind::BlockPlace
    };
    let mut record = Record::new(
        now_ms(),
        who,
        kind,
        world.dimension_id(),
        block_pos(change.pos),
    );
    record.old = Some(change.old);
    record.new = Some(change.new);
    if change.old.block().block_entity().is_some() {
        record.data = world.saved_block_entity(change.pos);
    }
    record
}

/// Logs every block change with a cause: players (with what a placement
/// replaced), entities, explosions, and blocks' own ticks and events.
/// Changes that keep the block (a door opening, water levels, crop ages)
/// are not block rows.
pub struct BlockHooks;

impl RegionHooks for BlockHooks {
    fn observes_block_changes(&self) -> bool {
        true
    }

    fn block_changed(&self, world: &mut dyn RegionWorld, change: &BlockChange) {
        if change.cause == BlockCause::World {
            return;
        }
        let shared = world.shared().clone();
        let Some(log) = service(&shared)
            .filter(|log| log.enabled(Kind::BlockBreak) || log.enabled(Kind::BlockPlace))
        else {
            return;
        };
        let by_player = matches!(change.cause, BlockCause::Player(_));
        if change.old.block() == change.new.block() && (by_player || !extends(change)) {
            if by_player {
                let held = &mut world.state().get::<SameBlockChanges>().0;
                if held.len() >= SAME_BLOCK_CHANGES {
                    held.remove(0);
                }
                held.push(*change);
            }
            return;
        }
        if !by_player && natural_noise(change) {
            return;
        }
        let Some(who) = cause_who(world, change.cause) else {
            return;
        };
        let record = block_record(world, who, change);
        if log.enabled(record.kind) {
            log.log(record);
        }
    }

    fn player_used_block(&self, world: &mut dyn RegionWorld, used: &PlayerBlockUse) {
        if !clickable(used.old.block()) {
            return;
        }
        let shared = world.shared().clone();
        let Some(log) = service(&shared).filter(|log| log.enabled(Kind::Click)) else {
            return;
        };
        let mut record = Record::new(
            now_ms(),
            player(world, used.player),
            Kind::Click,
            world.dimension_id(),
            block_pos(used.pos),
        );
        record.old = Some(used.old);
        record.new = used.new;
        log.log(record);
    }

    fn sign_edited(&self, world: &mut dyn RegionWorld, edit: &SignEdit) {
        let shared = world.shared().clone();
        let Some(log) = service(&shared).filter(|log| log.enabled(Kind::Sign)) else {
            return;
        };
        let text = sign_text(&edit.old, edit.front);
        let now = sign_text(&edit.new, edit.front);
        if text == now {
            return;
        }
        let mut record = Record::new(
            now_ms(),
            player(world, edit.player),
            Kind::Sign,
            world.dimension_id(),
            block_pos(edit.pos),
        );
        record.old = Some(edit.state);
        record.new = Some(edit.state);
        record.text = Some(format!(
            "{} {text} -> {now}",
            if edit.front { "front" } else { "back" }
        ));
        record.data = Some(sign_data(&edit.old, &edit.new));
        log.log(record);
    }

    fn player_block_action(&self, world: &mut dyn RegionWorld, action: &PlayerBlockAction) {
        let held = std::mem::take(&mut world.state().get::<SameBlockChanges>().0);
        if action.action != BlockAction::Placed {
            return;
        }
        let Some(change) = held
            .iter()
            .rev()
            .find(|change| change.pos == action.pos && change.new == action.state)
        else {
            return;
        };
        let shared = world.shared().clone();
        let Some(log) = service(&shared).filter(|log| log.enabled(Kind::BlockPlace)) else {
            return;
        };
        let mut record = block_record(world, player(world, action.player), change);
        record.kind = Kind::BlockPlace;
        record.data = None;
        log.log(record);
    }
}

/// Blocks a right click is worth a row for.
fn clickable(block: Block) -> bool {
    let name = block.name();
    let name = name.strip_prefix("minecraft:").unwrap_or(name);
    matches!(
        name,
        "lever"
            | "repeater"
            | "comparator"
            | "note_block"
            | "cake"
            | "daylight_detector"
            | "bell"
            | "jukebox"
            | "decorated_pot"
    ) || [
        "_door",
        "_trapdoor",
        "_fence_gate",
        "_button",
        "_candle_cake",
    ]
    .iter()
    .any(|suffix| name.ends_with(suffix))
}

/// One side's four lines, joined for display.
pub fn sign_text(nbt: &Compound, front: bool) -> String {
    let side = if front { "front_text" } else { "back_text" };
    let Some(Tag::Compound(side)) = nbt.get(side) else {
        return String::new();
    };
    let Some(Tag::List(lines)) = side.get("messages") else {
        return String::new();
    };
    lines
        .iter()
        .map(|line| match line {
            Tag::String(text) => text.trim_matches('"').to_owned(),
            _ => String::new(),
        })
        .collect::<Vec<_>>()
        .join(" | ")
}

/// Both block entities, so a rollback restores `old` and a restore `new`.
pub fn sign_data(old: &Compound, new: &Compound) -> Compound {
    Compound::new()
        .with("old", Tag::Compound(old.clone()))
        .with("new", Tag::Compound(new.clone()))
}

/// Container changes netted per holder and cause until the region's next
/// late tick phase, so one click or one tick's hopper moves make one row
/// per item.
#[derive(Default)]
struct PendingContainers(Vec<Pending>);

fn flush_containers(world: &mut dyn RegionWorld) {
    let pending = std::mem::take(&mut world.state().get::<PendingContainers>().0);
    if pending.is_empty() {
        return;
    }
    let shared = world.shared().clone();
    let Some(log) = service(&shared) else {
        return;
    };
    let time = now_ms();
    let dimension = world.dimension_id();
    for record in pending
        .into_iter()
        .flat_map(|pending| pending.records(time, dimension))
    {
        if log.enabled(record.kind) {
            log.log(record);
        }
    }
}

/// Logs what players, hoppers, droppers and entities put into and took
/// out of containers, item frames and armor stands. A dispenser using up
/// its own items is not logged: what it does to the world is a block row.
pub struct ContainerHooks;

impl RegionHooks for ContainerHooks {
    fn observes_container_changes(&self) -> bool {
        true
    }

    fn container_changed(&self, world: &mut dyn RegionWorld, edit: &ContainerEdit) {
        if edit.cause == BlockCause::Block(Block::Dispenser) {
            return;
        }
        let shared = world.shared().clone();
        if !service(&shared).is_some_and(|log| {
            log.enabled(Kind::ContainerAdd) || log.enabled(Kind::ContainerRemove)
        }) {
            return;
        }
        let Some(who) = cause_who(world, edit.cause) else {
            return;
        };
        let pending = &mut world.state().get::<PendingContainers>().0;
        let index = match pending
            .iter()
            .position(|p| p.holder == edit.holder && p.cause == edit.cause)
        {
            Some(index) => index,
            None => {
                pending.push(Pending::new(edit.holder, edit.cause, who));
                pending.len() - 1
            }
        };
        pending[index].apply(&edit.slots);
        if pending.len() >= MAX_PENDING {
            flush_containers(world);
        }
    }

    fn tick(&self, world: &mut dyn RegionWorld, phase: TickPhase) {
        if phase == TickPhase::Late {
            flush_containers(world);
        }
    }
}

fn world_pos(position: [f64; 3]) -> [i32; 3] {
    position.map(|axis| axis.floor() as i32)
}

fn item_record(
    who: Who,
    kind: Kind,
    world: &dyn RegionWorld,
    at: [f64; 3],
    stack: tuff_item::item::ItemView<'_>,
) -> Record {
    let mut record = Record::new(now_ms(), who, kind, world.dimension_id(), world_pos(at));
    record.material = Some(stack.item.name().to_owned());
    record.amount = i32::from(stack.count());
    record.data = Some(stack.to_nbt());
    record
}

/// Logs items players throw or drop (`-item`) and what players and mobs
/// pick up (`+item`), and deaths of the region's entities (`kill`) with
/// the victim's saved form so a rollback can bring it back.
pub struct ItemHooks;

impl RegionHooks for ItemHooks {
    fn observes_items_and_deaths(&self) -> bool {
        true
    }

    fn entity_spawned(&self, world: &mut dyn RegionWorld, entity: &dyn RegionEntity) {
        let Some(item) = entity.downcast_ref::<ItemEntity>() else {
            return;
        };
        let Some(thrower) = item.thrower() else {
            return;
        };
        let shared = world.shared().clone();
        let Some(log) = service(&shared).filter(|log| log.enabled(Kind::ItemRemove)) else {
            return;
        };
        let who = player_by_uuid(world, thrower).unwrap_or_else(|| Who::cause("#item"));
        log.log(item_record(
            who,
            Kind::ItemRemove,
            world,
            item.base.position,
            item.stack.view(),
        ));
    }

    fn item_picked_up(&self, world: &mut dyn RegionWorld, pickup: &ItemPickup) {
        let shared = world.shared().clone();
        let Some(log) = service(&shared).filter(|log| log.enabled(Kind::ItemAdd)) else {
            return;
        };
        let who = match pickup.taker {
            ItemTaker::Player(id) => player(world, id),
            ItemTaker::Entity { kind, .. } => Who::cause(kind.name()),
        };
        log.log(item_record(
            who,
            Kind::ItemAdd,
            world,
            pickup.position,
            pickup.stack.view(),
        ));
    }

    fn item_violation(
        &self,
        world: &mut dyn RegionWorld,
        violation: &tuff_core::world::conservation::ItemViolation,
    ) {
        let shared = world.shared().clone();
        let Some(log) = service(&shared) else {
            return;
        };
        let who = violation.player.map_or_else(
            || Who::cause("#server"),
            |player_id| player(world, player_id),
        );
        let mut record = Record::new(
            now_ms(),
            who,
            Kind::Custom,
            world.dimension_id(),
            block_pos(violation.pos),
        );
        record.material = Some("tuff:item_conservation".to_owned());
        record.text = Some(format!(
            "{} refused: {}",
            violation.operation, violation.detail
        ));
        if let Some(item) = &violation.item {
            record.amount = i32::from(item.count());
            record.data = Some(item.to_nbt());
        }
        log.log(record);
    }

    fn entity_died(&self, world: &mut dyn RegionWorld, death: &EntityDeath) {
        let shared = world.shared().clone();
        let Some(log) = service(&shared).filter(|log| log.enabled(Kind::Kill)) else {
            return;
        };
        let who = death
            .player
            .and_then(|uuid| player_by_uuid(world, uuid))
            .or_else(|| death.attacker.map(|kind| Who::cause(kind.name())))
            .unwrap_or_else(|| Who::cause(death.damage));
        log.log(kill_record(who, world.dimension_id(), death));
    }
}

/// The row for a death: the victim's type, where, and its saved form
/// without the health that killed it.
pub fn kill_record(who: Who, world: DimensionId, death: &EntityDeath) -> Record {
    let mut record = Record::new(now_ms(), who, Kind::Kill, world, world_pos(death.position));
    record.material = Some(death.kind.name().to_owned());
    record.text = Some(crate::entity::encode_text(death.uuid));
    let mut nbt = death.nbt.clone();
    for key in ["Health", "DeathTime", "HurtTime"] {
        nbt.remove(key);
    }
    record.data = Some(nbt);
    record
}

fn session_record(session: &PlayerSession, kind: Kind) -> Record {
    let [x, y, z] = session.pose.position;
    Record::new(
        now_ms(),
        Who::Player {
            uuid: session.profile.id,
            name: Arc::from(session.profile.name.as_str()),
        },
        kind,
        session.dimension,
        [x.floor() as i32, y.floor() as i32, z.floor() as i32],
    )
}

/// `record` filled in as the row for `action` on `stack`.
fn action_record(
    mut record: Record,
    action: ItemAction,
    stack: tuff_item::item::ItemView<'_>,
) -> Record {
    record.material = Some(stack.item.name().to_owned());
    record.amount = i32::from(stack.count());
    record.data = Some(stack.to_nbt());
    record.text = Some(action.name().to_owned());
    record
}

fn log_session(session: &PlayerSession, kind: Kind) {
    let shared = session.shared.clone();
    if let Some(log) = service(&shared).filter(|log| log.enabled(kind)) {
        log.log(session_record(session, kind));
    }
}

pub struct Sessions;

impl SessionHooks for Sessions {
    fn item_action(
        &self,
        session: &PlayerSession,
        action: ItemAction,
        stack: tuff_item::item::ItemView<'_>,
    ) {
        let kind = match action {
            ItemAction::Crafted => Kind::ItemAdd,
            ItemAction::Shot | ItemAction::Broken => Kind::ItemRemove,
        };
        let shared = session.shared.clone();
        let Some(log) = service(&shared).filter(|log| log.enabled(kind)) else {
            return;
        };
        log.log(action_record(session_record(session, kind), action, stack));
    }

    fn died(
        &self,
        session: &mut PlayerSession,
        kind: &tuff_vanilla_data::damage_type::DamageType,
        killer: Option<&tuff_core::world::entity::Culprit>,
    ) {
        let shared = session.shared.clone();
        let Some(log) = service(&shared).filter(|log| log.enabled(Kind::Kill)) else {
            return;
        };
        let who = match killer {
            Some(killer) => match killer.player_name() {
                Some(name) => Who::Player {
                    uuid: killer.uuid,
                    name: Arc::from(name),
                },
                None => Who::cause(killer.kind.name()),
            },
            None => Who::cause(kind.name),
        };
        let mut record = session_record(session, Kind::Kill);
        record.who = who;
        record.material = Some("minecraft:player".to_owned());
        record.text = Some(crate::entity::encode_player(&session.profile.name));
        log.log(record);
    }

    fn joined<'a>(&'a self, session: &'a mut PlayerSession) -> HookFuture<'a> {
        log_session(session, Kind::Username);
        log_session(session, Kind::Login);
        Box::pin(async {})
    }

    fn leaving<'a>(&'a self, session: &'a mut PlayerSession) -> HookFuture<'a> {
        log_session(session, Kind::Logout);
        Box::pin(async {})
    }
}

/// Logs chat lines and commands as the player sends them. It runs after
/// every other interceptor, so what it sees is what the server acts on.
pub struct ChatLog;

/// After the anti-cheat's chat checks.
pub const CHAT_ORDER: i32 = 10_000;

impl PacketInterceptor for ChatLog {
    fn intercept<'a>(
        &'a self,
        session: &'a mut PlayerSession,
        packet: &'a Serverbound,
    ) -> Pin<Box<dyn Future<Output = Verdict> + Send + 'a>> {
        let (kind, text) = match packet {
            Serverbound::Chat(message) => (Kind::Chat, message.clone()),
            Serverbound::ChatCommand(command) => (Kind::Command, format!("/{command}")),
            _ => return Box::pin(async { Verdict::Allow }),
        };
        let shared = session.shared.clone();
        if let Some(log) = service(&shared).filter(|log| log.enabled(kind)) {
            let mut record = session_record(session, kind);
            record.text = Some(text);
            log.log(record);
        }
        Box::pin(async { Verdict::Allow })
    }
}

/// Before everything that acts on a break or use, so an inspector never
/// changes the world.
pub const INSPECT_ORDER: i32 = -10_000;

const INSPECT_ROWS: usize = 50;
const INSPECT_LINES: usize = 10;

/// Turns breaking and using blocks into history lookups for players who
/// toggled `/tuff audit inspect`. A use shows the clicked block, or the
/// block beside it on the clicked face when the clicked one has no history.
pub struct Inspector;

impl Inspector {
    async fn show(session: &mut PlayerSession, pos: BlockPos, adjacent: Option<BlockPos>) {
        let shared = session.shared.clone();
        let Some(log) = service(&shared) else {
            return;
        };
        let world = shared.dimensions.get(session.dimension).key.clone();
        let _ = log.sync().await;
        for pos in std::iter::once(pos).chain(adjacent) {
            let filter = Filter {
                area: Area::Radius {
                    world: world.clone(),
                    center: [pos.x, pos.y, pos.z],
                    radius: 0,
                },
                kinds: vec![
                    Kind::BlockBreak,
                    Kind::BlockPlace,
                    Kind::ContainerAdd,
                    Kind::ContainerRemove,
                ],
                ..Filter::default()
            };
            let Ok(rows) = log
                .lookup(filter, Order::NewestFirst, INSPECT_ROWS, 0)
                .await
            else {
                return;
            };
            let rows: Vec<_> = rows
                .into_iter()
                .filter(|row| row.pos == [pos.x, pos.y, pos.z])
                .take(INSPECT_LINES)
                .collect();
            if rows.is_empty() {
                continue;
            }
            let now = now_ms();
            session.system_message(&text(format!(
                "----- History of {} {} {} -----",
                pos.x, pos.y, pos.z
            )));
            for row in &rows {
                session.system_message(&line_component(row, now, false));
            }
            return;
        }
        session.system_message(&text(format!(
            "No history for {} {} {}",
            pos.x, pos.y, pos.z
        )));
    }
}

impl PacketInterceptor for Inspector {
    fn intercept<'a>(
        &'a self,
        session: &'a mut PlayerSession,
        packet: &'a Serverbound,
    ) -> Pin<Box<dyn Future<Output = Verdict> + Send + 'a>> {
        Box::pin(async move {
            let shared = session.shared.clone();
            if !service(&shared).is_some_and(|log| log.inspecting(session.id)) {
                return Verdict::Allow;
            }
            match packet {
                Serverbound::PlayerAction {
                    status: PlayerActionStatus::StartDestroyBlock,
                    position,
                    sequence,
                    ..
                } => {
                    session.send(&BlockChangedAck(*sequence));
                    Self::show(session, *position, None).await;
                    Verdict::Cancel
                }
                Serverbound::UseItemOn {
                    position,
                    face,
                    sequence,
                    ..
                } => {
                    session.send(&BlockChangedAck(*sequence));
                    Self::show(session, *position, Some(position.offset(*face))).await;
                    Verdict::Cancel
                }
                _ => Verdict::Allow,
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn natural_causes_get_their_group_names() {
        assert_eq!(block_cause_name(Block::SoulFire), Some("#fire"));
        assert_eq!(block_cause_name(Block::Lava), Some("#lava"));
        assert_eq!(block_cause_name(Block::MovingPiston), Some("#piston"));
        assert_eq!(block_cause_name(Block::DarkOakLeaves), Some("#decay"));
        assert_eq!(
            block_cause_name(Block::RedConcretePowder),
            Some("#falling_block")
        );
        assert_eq!(block_cause_name(Block::MangrovePropagule), Some("#grow"));
        assert_eq!(block_cause_name(Block::CrimsonFungus), Some("#grow"));
        assert_eq!(block_cause_name(Block::Dropper), Some("#dispenser"));
        assert_eq!(block_cause_name(Block::GrassBlock), None);
    }

    #[tuff_core::watchdog]
    #[test]
    fn removal_and_noise_follow_what_replaces_the_block() {
        let stone = Block::Stone.default_state();
        let air = Block::Air.default_state();
        let water = Block::Water.default_state();
        assert!(removes(stone, air));
        assert!(removes(stone, Block::Fire.default_state()));
        assert!(!removes(air, water));
        assert!(!removes(water, stone));
        let change = |old, new| BlockChange {
            pos: BlockPos::new(0, 0, 0),
            old,
            new,
            cause: BlockCause::Block(Block::Water),
        };
        assert!(natural_noise(&change(water, air)));
        assert!(natural_noise(&change(Block::Fire.default_state(), air)));
        assert!(!natural_noise(&change(air, water)));
        assert!(!natural_noise(&change(water, stone)));
    }
}

#[cfg(test)]
mod action_tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn item_action_rows_carry_the_action_and_the_stack() {
        let base = Record::new(
            1,
            Who::cause("#test"),
            Kind::ItemRemove,
            DimensionId::default(),
            [0, 0, 0],
        );
        let row = action_record(
            base,
            ItemAction::Broken,
            tuff_item::item::ItemStack::mint(
                tuff_vanilla_data::Item::IronPickaxe,
                1,
                tuff_item::item::MintReason::Test,
            )
            .view(),
        );
        assert_eq!(row.text.as_deref(), Some("break"));
        assert_eq!(row.material.as_deref(), Some("minecraft:iron_pickaxe"));
        assert_eq!(row.amount, 1);
        assert!(row.data.is_some());
    }
}
