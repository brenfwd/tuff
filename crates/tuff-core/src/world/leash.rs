//! Leads: what holds a leashed entity, in the saved `leash` form (the
//! holder's UUID, or the fence a knot is tied to) and as clients are told
//! through Set Entity Link, and the pull a lead has on the entity it holds,
//! which every leashable kind (mobs, boats) shares. Tying, knots and the
//! region's search for holders are a feature's.

use std::collections::HashMap;

use bytes::Bytes;
use rand::Rng;
use tuff_vanilla_data::Item;
use tuff_vanilla_data::registry::{EntityType, SoundEvent};

use crate::protocol::play;
use crate::world::entity::{
    Effect, EntityBase, EntitySummary, NearbyPlayer, RegionEntity, TickContext,
};
use crate::world::{framed, packets};
use tuff_item::item::{ItemStack, MintReason};
use tuff_nbt::{Compound, Tag};
use tuff_world_model::BlockPos;

pub const LEASH_KEY: &str = "leash";

/// What a lead is tied to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Anchor {
    /// An entity or player, by UUID.
    Entity(u128),
    /// A leash knot on the fence at this block.
    Knot(BlockPos),
}

/// The lead on an entity.
#[derive(Debug, Clone, PartialEq)]
pub struct Leash {
    pub anchor: Anchor,
    /// The holder's network id once it has been found; clients see the
    /// lead only then.
    pub holder: Option<i32>,
    /// The holder has been found since the lead was tied or loaded.
    pub found: bool,
    /// The holder is a player.
    pub player: bool,
    /// Ticks the holder has not been seen for.
    pub missing: u32,
    /// Spin the lead's pull gave it, taken off its yaw each tick.
    pub angular_momentum: f64,
}

impl Leash {
    /// A lead just tied to `anchor`, held by the entity `holder`.
    pub fn tied(anchor: Anchor, holder: i32, player: bool) -> Self {
        Self {
            anchor,
            holder: Some(holder),
            found: true,
            player,
            missing: 0,
            angular_momentum: 0.0,
        }
    }

    /// A lead to `anchor` whose holder is still to be found (loaded, or
    /// given to an entity created before its holder).
    pub fn pending(anchor: Anchor) -> Self {
        Self {
            anchor,
            holder: None,
            found: false,
            player: false,
            missing: 0,
            angular_momentum: 0.0,
        }
    }

    /// Takes the saved `leash` out of `nbt`: a compound with the holder's
    /// `UUID`, or the knot's fence as an int array.
    pub fn load(nbt: &mut Compound) -> Option<Self> {
        let anchor = match nbt.remove(LEASH_KEY)? {
            Tag::Compound(holder) => Anchor::Entity(holder.get("UUID")?.as_uuid()?),
            Tag::IntArray(pos) => match pos[..] {
                [x, y, z] => Anchor::Knot(BlockPos::new(x, y, z)),
                _ => return None,
            },
            _ => return None,
        };
        Some(Self::pending(anchor))
    }

    pub fn save(&self, out: &mut Compound) {
        let tag = match self.anchor {
            Anchor::Entity(uuid) => Tag::Compound(Compound::new().with("UUID", Tag::uuid(uuid))),
            Anchor::Knot(pos) => Tag::IntArray(vec![pos.x, pos.y, pos.z]),
        };
        out.insert(LEASH_KEY, tag);
    }
}

/// The lead's dimensions and pull, for an entity that can wear one now.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LeashProfile {
    /// Beyond this (less both widths) the lead starts to pull.
    pub elastic: f64,
    /// Beyond this the lead snaps.
    pub snap: f64,
    /// It is held at four corners (a horse, a boat).
    pub quad: bool,
}

impl Default for LeashProfile {
    fn default() -> Self {
        Self {
            elastic: 6.0,
            snap: 12.0,
            quad: false,
        }
    }
}

/// Set Entity Link for `attached`, held by `holder` or untied.
pub fn link_packet(attached: i32, holder: Option<i32>) -> Bytes {
    framed(&play::SetEntityLink {
        attached,
        holder: holder.unwrap_or(0),
    })
}

/// Half the side of the box searched for an entity's leads (32 blocks).
pub const SEARCH: f64 = 16.0;
/// Ticks a found holder may go unseen before its lead breaks.
pub const MISSING_GRACE: u32 = 10;
/// Ticks a loaded lead waits for its holder before it drops.
pub const UNRESOLVED_TICKS: u32 = 100;
/// Ticks a neighbour's answer about a holder stays good.
pub const REMOTE_KEEP: u64 = 3;
const STIFFNESS: f64 = 0.11;
const TORSIONAL_ELASTICITY: f64 = 10.0;
const AXIS_ELASTICITY: [f64; 3] = [0.8, 0.2, 0.8];
const SAME_DIRECTION_DAMPING: f64 = 0.300_000_011_920_928_96;
const ENTITY_POINT: [[f64; 3]; 1] = [[0.0, 0.5, 0.5]];
const HOLDER_POINT: [[f64; 3]; 1] = [[0.0, 0.5, 0.0]];
const QUAD_POINTS: [[f64; 3]; 4] = [
    [-0.5, 0.5, 0.5],
    [-0.5, 0.5, -0.5],
    [0.5, 0.5, -0.5],
    [0.5, 0.5, 0.5],
];

/// A lead's holder as a leashed entity sees it this tick.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Holder {
    pub id: i32,
    pub uuid: u128,
    /// `None` for a player.
    pub kind: Option<EntityType>,
    pub position: [f64; 3],
    pub bounds: [f64; 6],
    /// How far it moved last tick.
    pub movement: [f64; 3],
    pub yaw: f32,
    pub alive: bool,
}

impl Holder {
    pub fn of_player(player: &NearbyPlayer) -> Self {
        Self {
            id: player.id,
            uuid: player.uuid,
            kind: None,
            position: player.position,
            bounds: player.bounds(),
            movement: player.movement,
            yaw: player.yaw,
            alive: true,
        }
    }

    pub fn of_summary(entity: &EntitySummary) -> Self {
        Self {
            id: entity.id,
            uuid: entity.uuid,
            kind: Some(entity.kind),
            position: entity.position,
            bounds: entity.bounds,
            movement: entity.velocity,
            yaw: entity.yaw,
            alive: entity.living.is_none_or(|l| l.alive),
        }
    }

    pub fn of_entity(entity: &dyn RegionEntity) -> Self {
        let base = entity.base();
        Self {
            id: base.id,
            uuid: base.uuid,
            kind: Some(base.kind),
            position: base.position,
            bounds: entity.bounds(),
            movement: base.velocity,
            yaw: base.yaw,
            alive: entity.living().is_none_or(|l| l.alive),
        }
    }

    pub fn is_player(&self) -> bool {
        self.kind.is_none()
    }

    pub fn is_knot(&self) -> bool {
        self.kind == Some(EntityType::LeashKnot)
    }

    fn width(&self) -> f64 {
        self.bounds[3] - self.bounds[0]
    }

    fn height(&self) -> f64 {
        self.bounds[4] - self.bounds[1]
    }

    /// Holds a four-cornered lead at four corners (a ghast's harness).
    fn holds_quad(&self) -> bool {
        matches!(self.kind, Some(EntityType::Ghast | EntityType::HappyGhast))
    }

    /// The block it stands in.
    pub fn block_pos(&self) -> BlockPos {
        BlockPos::new(
            self.position[0].floor() as i32,
            self.position[1].floor() as i32,
            self.position[2].floor() as i32,
        )
    }
}

/// What the region knows of leads beyond what one entity's tick sees.
#[derive(Debug, Default)]
pub struct Leashes {
    /// Holders found in neighbouring regions, with the tick they answered.
    pub remote: HashMap<Anchor, (Holder, u64)>,
    /// Anchors this region's leashed entities could not find, with where
    /// such an entity stands.
    pub wanted: HashMap<Anchor, [f64; 3]>,
    /// The last tick some lead held each knot, by id.
    pub held: HashMap<i32, u64>,
    /// The last tick a four-cornered lead pulled on each holder, by id.
    pub quad_held: HashMap<i32, u64>,
    /// Holders gone this tick (knots untied or broken): their leads drop
    /// at once rather than after the grace.
    pub gone: HashMap<i32, u64>,
}

impl Leashes {
    /// Whether a lead held at four corners pulled on `holder` lately (a
    /// happy ghast shows it holds one).
    pub fn holds_quad(&self, holder: i32, now: u64) -> bool {
        self.quad_held
            .get(&holder)
            .is_some_and(|&at| now.saturating_sub(at) <= 1)
    }
}

/// Notes that a leashed entity at `position` looks for `anchor`. Of several
/// such entities the least position wins, so the note does not depend on
/// the order the entities ticked in.
fn want(cx: &mut TickContext, anchor: Anchor, position: [f64; 3]) {
    let least = |a: [f64; 3], b: [f64; 3]| {
        let order = (0..3)
            .map(|i| a[i].total_cmp(&b[i]))
            .find(|o| o.is_ne())
            .unwrap_or(std::cmp::Ordering::Equal);
        if order.is_le() { a } else { b }
    };
    cx.state
        .get::<Leashes>()
        .wanted
        .entry(anchor)
        .and_modify(|at| *at = least(*at, position))
        .or_insert(position);
}

/// What a leashed entity's lead did this tick.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Pull {
    /// No lead, or a holder still being looked for.
    Idle,
    /// The lead came off (holder gone, or the entity died).
    Dropped,
    /// Pulled too far: it snapped.
    Snapped(Holder),
    /// Past its slack, the lead pulled the entity.
    Elastic(Holder),
    /// Within its slack.
    Close(Holder),
}

/// A leashed entity's body, as its lead pulls it.
pub struct Body<'a> {
    pub base: &'a mut EntityBase,
    pub bounds: [f64; 6],
    pub profile: LeashProfile,
    /// Alive and able to act on the world.
    pub alive: bool,
    /// How much its spin slows each tick.
    pub angular_friction: f32,
    /// Its own motion as the lead sees it (zero for a mob without AI).
    pub motion: [f64; 3],
}

/// The lead stack an entity drops when its lead comes off.
#[expect(
    clippy::disallowed_methods,
    reason = "entity drops or fires an item outside loot tables (lead)"
)]
pub fn lead() -> ItemStack {
    ItemStack::mint(Item::Lead, 1, MintReason::EntityDrop)
}

pub fn center(bounds: [f64; 6]) -> [f64; 3] {
    [
        (bounds[0] + bounds[3]) / 2.0,
        (bounds[1] + bounds[4]) / 2.0,
        (bounds[2] + bounds[5]) / 2.0,
    ]
}

pub fn distance(a: [f64; 3], b: [f64; 3]) -> f64 {
    crate::world::distance_sq(a, b).sqrt()
}

pub fn sound_packet(
    sound: SoundEvent,
    position: [f64; 3],
    category: i32,
    seed: i64,
) -> crate::world::messages::Packets {
    packets([framed(&play::Sound {
        sound_event: sound.id(),
        category,
        position,
        volume: 1.0,
        pitch: 1.0,
        seed,
    })])
}

/// The holder of a lead to `anchor`, and whether it was seen only through
/// a neighbour's answer.
fn find(anchor: Anchor, cx: &mut TickContext) -> Option<(Holder, bool)> {
    let local = match anchor {
        Anchor::Entity(uuid) => cx
            .players
            .iter()
            .find(|p| p.uuid == uuid)
            .map(Holder::of_player)
            .or_else(|| {
                cx.entities
                    .iter()
                    .find(|e| e.uuid == uuid)
                    .map(Holder::of_summary)
            }),
        Anchor::Knot(pos) => cx
            .entities
            .iter()
            .find(|e| e.kind == EntityType::LeashKnot && knot_pos(e.position) == pos)
            .map(Holder::of_summary),
    };
    if let Some(holder) = local {
        return Some((holder, false));
    }
    let now = cx.tick;
    cx.state
        .get::<Leashes>()
        .remote
        .get(&anchor)
        .filter(|(_, at)| now.saturating_sub(*at) <= REMOTE_KEEP)
        .map(|(holder, _)| (*holder, true))
}

pub fn knot_pos(position: [f64; 3]) -> BlockPos {
    BlockPos::new(
        position[0].floor() as i32,
        position[1].floor() as i32,
        position[2].floor() as i32,
    )
}

/// Takes the lead off `base`'s entity: the lead drops as an item when
/// `drop`, and viewers see it untied.
pub fn untie(
    slot: &mut Option<Leash>,
    base: &EntityBase,
    drop: bool,
    cx: &mut TickContext,
) -> Option<Leash> {
    let leash = slot.take()?;
    if drop {
        drop_lead(base.position, cx);
    }
    if leash.holder.is_some() {
        cx.effects
            .push(Effect::EntityPackets(packets([link_packet(base.id, None)])));
    }
    Some(leash)
}

fn drop_lead(position: [f64; 3], cx: &mut TickContext) {
    let velocity = [
        cx.rng.r#gen::<f64>() * 0.2 - 0.1,
        0.2,
        cx.rng.r#gen::<f64>() * 0.2 - 0.1,
    ];
    cx.effects.push(Effect::DropItem {
        stack: lead(),
        position,
        velocity,
        pickup_delay: crate::world::items::BLOCK_DROP_PICKUP_DELAY,
    });
}

fn entity_drops(cx: &TickContext) -> bool {
    cx.shared.game_rules.borrow().bool("entity_drops")
}

/// One tick of the lead on `body`'s entity: finds its holder, breaks it
/// when the holder is gone or too far, and pulls the entity once it is
/// past its slack.
pub fn tick(slot: &mut Option<Leash>, mut body: Body, cx: &mut TickContext) -> Pull {
    let Some(leash) = slot.as_mut() else {
        return Pull::Idle;
    };
    let anchor = leash.anchor;
    let found = find(anchor, cx);
    let now = cx.tick;
    let Some((holder, remote)) = found else {
        leash.missing += 1;
        if !(leash.found && leash.player) {
            want(cx, anchor, body.base.position);
        }
        if !leash.found {
            if leash.missing > UNRESOLVED_TICKS {
                *slot = None;
                drop_lead(body.base.position, cx);
                return Pull::Dropped;
            }
            return Pull::Idle;
        }
        let gone = leash
            .holder
            .is_some_and(|id| cx.state.get::<Leashes>().gone.contains_key(&id));
        if leash.player || gone || leash.missing > MISSING_GRACE {
            let drop = entity_drops(cx);
            untie(slot, body.base, drop, cx);
            return Pull::Dropped;
        }
        return Pull::Idle;
    };
    if let Anchor::Knot(_) = anchor {
        cx.state.get::<Leashes>().held.insert(holder.id, now);
    }
    if remote {
        want(cx, anchor, body.base.position);
    }
    leash.missing = 0;
    leash.found = true;
    leash.player = holder.is_player();
    if leash.holder != Some(holder.id) {
        leash.holder = Some(holder.id);
        cx.effects.push(Effect::EntityPackets(packets([link_packet(
            body.base.id,
            Some(holder.id),
        )])));
    }
    let gone = cx.state.get::<Leashes>().gone.contains_key(&holder.id);
    if !body.alive || !holder.alive || gone {
        let drop = entity_drops(cx);
        untie(slot, body.base, drop, cx);
        return Pull::Dropped;
    }
    if body.profile.quad && holder.holds_quad() {
        cx.state.get::<Leashes>().quad_held.insert(holder.id, now);
    }
    let width = body.bounds[3] - body.bounds[0];
    let reach = distance(center(holder.bounds), center(body.bounds));
    let pull = if reach > body.profile.snap {
        let category = play::sound_category::NEUTRAL;
        let seed = cx.rng.r#gen();
        let sound = sound_packet(SoundEvent::ItemLeadBreak, holder.position, category, seed);
        cx.effects.push(Effect::ChunkPackets(sound));
        untie(slot, body.base, true, cx);
        Pull::Snapped(holder)
    } else if reach > body.profile.elastic - holder.width() - width
        && elastic_pull(leash, &mut body, &holder)
    {
        Pull::Elastic(holder)
    } else {
        Pull::Close(holder)
    };
    let momentum = match slot.as_mut() {
        Some(leash) => {
            let momentum = leash.angular_momentum;
            leash.angular_momentum *= f64::from(body.angular_friction);
            momentum
        }
        None => 0.0,
    };
    body.base.yaw = (f64::from(body.base.yaw) - momentum) as f32;
    pull
}

fn y_rot(v: [f64; 3], angle: f32) -> [f64; 3] {
    let (sin, cos) = (f64::from(angle.sin()), f64::from(angle.cos()));
    [v[0] * cos + v[2] * sin, v[1], v[2] * cos - v[0] * sin]
}

fn attach(position: [f64; 3], point: [f64; 3], size: [f64; 3], yaw: f32) -> ([f64; 3], [f64; 3]) {
    let scaled = [point[0] * size[0], point[1] * size[1], point[2] * size[2]];
    let lever = y_rot(scaled, -yaw * (std::f32::consts::PI / 180.0));
    (
        lever,
        [
            position[0] + lever[0],
            position[1] + lever[1],
            position[2] + lever[2],
        ],
    )
}

/// The spring between a holder's attachment point and the entity's:
/// nothing within `slack`, else a pull back toward the pivot (weaker when
/// it already moves that way) and the twist it gives about the lever.
fn spring(
    pivot: [f64; 3],
    object: [f64; 3],
    slack: f64,
    motion: [f64; 3],
    lever: [f64; 3],
) -> Option<([f64; 3], f64)> {
    let length = distance(object, pivot);
    if length < slack {
        return None;
    }
    let toward = [
        pivot[0] - object[0],
        pivot[1] - object[1],
        pivot[2] - object[2],
    ];
    let norm = (toward[0] * toward[0] + toward[1] * toward[1] + toward[2] * toward[2]).sqrt();
    let unit = if norm < 1.0e-5 {
        [0.0; 3]
    } else {
        toward.map(|c| c / norm)
    };
    let mut force = unit.map(|c| c * (length - slack));
    let torque = lever[2] * force[0] - lever[0] * force[2];
    let along = motion[0] * force[0] + motion[1] * force[1] + motion[2] * force[2];
    if along >= 0.0 {
        force = force.map(|c| c * SAME_DIRECTION_DAMPING);
    }
    Some((force, torque))
}

/// Pulls the entity toward its holder past the lead's slack; false when no
/// attachment point is past it.
fn elastic_pull(leash: &mut Leash, body: &mut Body, holder: &Holder) -> bool {
    let quad = body.profile.quad && holder.holds_quad();
    let (entity_points, holder_points): (&[[f64; 3]], &[[f64; 3]]) = if quad {
        (&QUAD_POINTS, &QUAD_POINTS)
    } else {
        (&ENTITY_POINT, &HOLDER_POINT)
    };
    let width = body.bounds[3] - body.bounds[0];
    let height = body.bounds[4] - body.bounds[1];
    let size = [width, height, width];
    let holder_size = [holder.width(), holder.height(), holder.width()];
    let mut force = [0.0; 3];
    let mut torque = 0.0;
    let mut any = false;
    for (entity_point, holder_point) in entity_points.iter().zip(holder_points) {
        let (lever, at) = attach(body.base.position, *entity_point, size, body.base.yaw);
        let (_, pivot) = attach(holder.position, *holder_point, holder_size, holder.yaw);
        if let Some((f, t)) = spring(pivot, at, body.profile.elastic, body.motion, lever) {
            any = true;
            for axis in 0..3 {
                force[axis] += f[axis];
            }
            torque += t;
        }
    }
    if !any {
        return false;
    }
    let scale = if quad { 0.25 } else { 1.0 };
    let force = force.map(|c| c * scale);
    leash.angular_momentum += TORSIONAL_ELASTICITY * torque * scale;
    for axis in 0..3 {
        let relative = holder.movement[axis] - body.base.velocity[axis];
        body.base.velocity[axis] += force[axis] * AXIS_ELASTICITY[axis] + relative * STIFFNESS;
    }
    true
}

/// What turns the holder's spin with the lead: the friction of the block
/// under it, slower in liquid.
pub fn angular_friction(on_ground: bool, in_liquid: bool, block_friction: f32) -> f32 {
    if on_ground {
        block_friction * 0.91
    } else if in_liquid {
        0.8
    } else {
        0.91
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn springs_pull_only_past_their_slack() {
        assert!(spring([0.0; 3], [5.0, 0.0, 0.0], 6.0, [0.0; 3], [0.0; 3]).is_none());
        let (force, torque) =
            spring([0.0; 3], [8.0, 0.0, 0.0], 6.0, [1.0, 0.0, 0.0], [0.0; 3]).expect("pulls");
        assert!((force[0] + 2.0).abs() < 1e-9, "{force:?}");
        assert_eq!(torque, 0.0);
        let (damped, _) =
            spring([0.0; 3], [8.0, 0.0, 0.0], 6.0, [-1.0, 0.0, 0.0], [0.0; 3]).expect("pulls");
        assert!((damped[0] + 2.0 * SAME_DIRECTION_DAMPING).abs() < 1e-9);
    }

    #[tuff_core::watchdog]
    #[test]
    fn leashes_round_trip_both_saved_forms() {
        for anchor in [
            Anchor::Entity(0x0123_4567_89ab_cdef_0011_2233_4455_6677),
            Anchor::Knot(BlockPos::new(-3, 70, 12)),
        ] {
            let mut out = Compound::new();
            Leash::pending(anchor).save(&mut out);
            let loaded = Leash::load(&mut out).expect("loads");
            assert_eq!(loaded.anchor, anchor);
            assert!(out.is_empty(), "the key is taken");
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn malformed_leashes_are_ignored() {
        let mut nbt = Compound::new().with(LEASH_KEY, Tag::IntArray(vec![1, 2]));
        assert!(Leash::load(&mut nbt).is_none());
        let mut nbt = Compound::new().with(LEASH_KEY, Compound::new());
        assert!(Leash::load(&mut nbt).is_none());
    }

    #[tuff_core::watchdog]
    #[test]
    fn link_packets_write_two_ints() {
        let packet = link_packet(5, None);
        assert!(packet.ends_with(&[0, 0, 0, 5, 0, 0, 0, 0]), "{packet:?}");
    }
}
