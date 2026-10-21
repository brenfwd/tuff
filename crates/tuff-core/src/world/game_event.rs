//! Game events and the vibration machinery listeners share.
//!
//! Something happens at a position (a step, a block break, an explosion):
//! the region posts a [`GameEvent`] with an [`Emission`], every feature's
//! `RegionHooks::game_event` hears it in the region and in neighbour
//! regions the event reaches, and listeners such as sculk sensors or the
//! warden turn it into a delayed vibration with [`Listener`].
//!
//! Core knows nothing about who listens; this module is the vocabulary
//! (frequencies, radii, validity and occlusion rules) plus the pending
//! vibration state, so every listener behaves alike.

use std::collections::HashMap;

pub use tuff_vanilla_data::registry::GameEvent;
use tuff_vanilla_data::tag::Key;
use tuff_vanilla_data::{BlockState, tag};

use crate::world::messages::PlayerId;
use tuff_nbt::{Compound, Tag};
use tuff_world_model::BlockPos;

/// What caused an event, when an entity did.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Source {
    pub entity: i32,
    /// The player to blame: the entity itself, its rider, or a
    /// projectile's or item's owner.
    pub player: Option<PlayerId>,
    /// Sneaking: ignored for the events tagged as such.
    pub careful: bool,
    pub spectator: bool,
    /// Wearing something that muffles vibrations.
    pub dampens: bool,
    pub uuid: Option<u128>,
    pub projectile_owner: Option<u128>,
}

impl Source {
    pub fn entity(entity: i32) -> Self {
        Self {
            entity,
            player: None,
            careful: false,
            spectator: false,
            dampens: false,
            uuid: None,
            projectile_owner: None,
        }
    }

    pub fn with_uuids(mut self, uuid: Option<u128>, projectile_owner: Option<u128>) -> Self {
        self.uuid = uuid;
        self.projectile_owner = projectile_owner;
        self
    }

    pub fn player(player: PlayerId) -> Self {
        Self {
            player: Some(player),
            ..Self::entity(player)
        }
    }

    pub fn sneaking(mut self, careful: bool) -> Self {
        self.careful = careful;
        self
    }

    pub fn spectating(mut self, spectator: bool) -> Self {
        self.spectator = spectator;
        self
    }
}

/// Who and what an event involved.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Context {
    pub source: Option<Source>,
    /// The block the event changed or touched.
    pub affected: Option<BlockState>,
}

impl Context {
    pub fn from(source: Source) -> Self {
        Self {
            source: Some(source),
            affected: None,
        }
    }

    pub fn block(state: BlockState) -> Self {
        Self {
            source: None,
            affected: Some(state),
        }
    }

    pub fn with_block(mut self, state: BlockState) -> Self {
        self.affected = Some(state);
        self
    }
}

/// One event, as listeners see it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Emission {
    pub event: GameEvent,
    pub pos: [f64; 3],
    pub context: Context,
}

impl Emission {
    pub fn block_pos(&self) -> BlockPos {
        floor_pos(self.pos)
    }

    /// Whether a listener at `listener` with its own `listener_radius`
    /// hears this event: the source block lies within both the listener's
    /// radius and the event's notification radius.
    pub fn reaches(&self, listener: BlockPos, listener_radius: i32) -> bool {
        let reach = i64::from(listener_radius.min(radius(self.event)));
        let source = self.block_pos();
        let (dx, dy, dz) = (
            i64::from(source.x) - i64::from(listener.x),
            i64::from(source.y) - i64::from(listener.y),
            i64::from(source.z) - i64::from(listener.z),
        );
        dx * dx + dy * dy + dz * dz <= reach * reach
    }
}

pub fn floor_pos(pos: [f64; 3]) -> BlockPos {
    BlockPos::new(
        pos[0].floor() as i32,
        pos[1].floor() as i32,
        pos[2].floor() as i32,
    )
}

/// How far from its position an event is announced.
pub fn radius(event: GameEvent) -> i32 {
    match event {
        GameEvent::Shriek => 32,
        GameEvent::JukeboxPlay | GameEvent::JukeboxStopPlay => 10,
        _ => 16,
    }
}

/// The vibration frequency of an event (1-15), 0 for none.
pub fn frequency(event: GameEvent) -> u8 {
    use GameEvent::*;
    match event {
        Step | Swim | Flap => 1,
        ProjectileLand | HitGround | Splash | Bounce => 2,
        ItemInteractFinish | ProjectileShoot | InstrumentPlay => 3,
        EntityAction | ElytraGlide | Unequip => 4,
        EntityDismount | Equip => 5,
        EntityInteract | Shear | EntityMount => 6,
        EntityDamage => 7,
        Drink | Eat => 8,
        ContainerClose | BlockClose | BlockDeactivate | BlockDetach => 9,
        ContainerOpen | BlockOpen | BlockActivate | BlockAttach | PrimeFuse | NoteBlockPlay => 10,
        BlockChange => 11,
        BlockDestroy | FluidPickup => 12,
        BlockPlace | FluidPlace => 13,
        EntityPlace | LightningStrike | Teleport => 14,
        EntityDie | Explode => 15,
        Resonate1 => 1,
        Resonate2 => 2,
        Resonate3 => 3,
        Resonate4 => 4,
        Resonate5 => 5,
        Resonate6 => 6,
        Resonate7 => 7,
        Resonate8 => 8,
        Resonate9 => 9,
        Resonate10 => 10,
        Resonate11 => 11,
        Resonate12 => 12,
        Resonate13 => 13,
        Resonate14 => 14,
        Resonate15 => 15,
        _ => 0,
    }
}

/// The resonance event a sensor repeats a vibration of `frequency` as.
pub fn resonance(frequency: u8) -> Option<GameEvent> {
    const ALL: [GameEvent; 15] = [
        GameEvent::Resonate1,
        GameEvent::Resonate2,
        GameEvent::Resonate3,
        GameEvent::Resonate4,
        GameEvent::Resonate5,
        GameEvent::Resonate6,
        GameEvent::Resonate7,
        GameEvent::Resonate8,
        GameEvent::Resonate9,
        GameEvent::Resonate10,
        GameEvent::Resonate11,
        GameEvent::Resonate12,
        GameEvent::Resonate13,
        GameEvent::Resonate14,
        GameEvent::Resonate15,
    ];
    ALL.get(usize::from(frequency).checked_sub(1)?).copied()
}

/// The redstone power a vibration gives at `distance` from a listener
/// that hears `radius` blocks far.
pub fn redstone_strength(distance: f32, radius: i32) -> u8 {
    let scale = 15.0 / f64::from(radius);
    let lost = (scale * f64::from(distance)).floor() as i32;
    (15 - lost).clamp(1, 15) as u8
}

/// Whether `emission` may be heard at all by a listener of `listens`
/// (an event tag): spectators, careful sneakers on the quiet events,
/// muffled sources and muffling blocks are not.
pub fn audible(listens: Key<GameEvent>, emission: &Emission) -> bool {
    audible_checking(listens, emission, |_| {})
}

/// [`audible`], calling `avoided` with the source when only sneaking
/// kept the event quiet (sculk sensors award an advancement for that).
pub fn audible_checking(
    listens: Key<GameEvent>,
    emission: &Emission,
    avoided: impl FnOnce(&Source),
) -> bool {
    if !tuff_datapack::tags::is(listens, emission.event) {
        return false;
    }
    if let Some(source) = &emission.context.source {
        if source.spectator {
            return false;
        }
        if source.careful
            && tuff_datapack::tags::is(
                tag::game_event::key::IGNORE_VIBRATIONS_SNEAKING,
                emission.event,
            )
        {
            avoided(source);
            return false;
        }
        if source.dampens {
            return false;
        }
    }
    !emission.context.affected.is_some_and(|state| {
        tuff_datapack::tags::is(tag::block::key::DAMPENS_VIBRATIONS, state.block())
    })
}

/// Tells a player whose sneaking kept a sculk sensor or warden from
/// hearing it, for the `avoid_vibration` advancement trigger.
pub fn report_avoided(world: &mut dyn crate::world::region_api::RegionWorld, source: &Source) {
    if source.player != Some(source.entity) {
        return;
    }
    if let Some((handle, _)) = world
        .seen_players()
        .into_iter()
        .find(|(handle, _)| handle.id == source.entity)
    {
        handle.send(crate::world::messages::PlayerEvent::Advancement(Box::new(
            crate::world::messages::AdvancementEvent::AvoidVibration,
        )));
    }
}

/// Whether sound-deadening blocks stand on every line from `from` to
/// `to`, tested from the source nudged along each axis as the game does.
/// Blocks the region cannot read do not block.
pub fn occluded(
    block: &dyn Fn(BlockPos) -> Option<BlockState>,
    from: [f64; 3],
    to: [f64; 3],
) -> bool {
    let centre = |p: [f64; 3]| [p[0].floor() + 0.5, p[1].floor() + 0.5, p[2].floor() + 0.5];
    let (from, to) = (centre(from), centre(to));
    const NUDGE: f64 = 9.999_999_747_378_752e-6;
    for axis in 0..3 {
        for sign in [-1.0, 1.0] {
            let mut start = from;
            start[axis] += sign * NUDGE;
            if !line_blocked(block, start, to) {
                return false;
            }
        }
    }
    true
}

fn line_blocked(
    block: &dyn Fn(BlockPos) -> Option<BlockState>,
    from: [f64; 3],
    to: [f64; 3],
) -> bool {
    let hit = |pos: BlockPos| {
        block(pos).is_some_and(|s| {
            tuff_datapack::tags::is(tag::block::key::OCCLUDES_VIBRATION_SIGNALS, s.block())
        })
    };
    let mut cell = floor_pos(from);
    if hit(cell) {
        return true;
    }
    let end = floor_pos(to);
    let delta = [to[0] - from[0], to[1] - from[1], to[2] - from[2]];
    let mut step = [0i32; 3];
    let mut max = [f64::INFINITY; 3];
    let mut span = [f64::INFINITY; 3];
    let origin = [from[0], from[1], from[2]];
    let cells = [cell.x, cell.y, cell.z];
    for axis in 0..3 {
        if delta[axis] > 0.0 {
            step[axis] = 1;
            span[axis] = 1.0 / delta[axis];
            max[axis] = (f64::from(cells[axis]) + 1.0 - origin[axis]) * span[axis];
        } else if delta[axis] < 0.0 {
            step[axis] = -1;
            span[axis] = -1.0 / delta[axis];
            max[axis] = (origin[axis] - f64::from(cells[axis])) * span[axis];
        }
    }
    let mut guard = 0;
    while cell != end && guard < 512 {
        guard += 1;
        let axis = if max[0] <= max[1] && max[0] <= max[2] {
            0
        } else if max[1] <= max[2] {
            1
        } else {
            2
        };
        if max[axis] > 1.0 {
            break;
        }
        max[axis] += span[axis];
        match axis {
            0 => cell.x += step[0],
            1 => cell.y += step[1],
            _ => cell.z += step[2],
        }
        if hit(cell) {
            return true;
        }
    }
    false
}

/// A vibration on its way to a listener.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Candidate {
    pub event: GameEvent,
    pub distance: f32,
    pub origin: [f64; 3],
    pub source: Option<Source>,
}

/// The vibration state of one listener: the best event heard this tick,
/// then the one travelling toward it.
#[derive(Debug, Default, Clone)]
pub struct Listener {
    selected: Option<(Candidate, u64)>,
    current: Option<Candidate>,
    travel: i32,
}

impl Listener {
    /// Whether it is busy with a vibration and deaf to others.
    pub fn is_busy(&self) -> bool {
        self.current.is_some()
    }

    pub fn is_idle(&self) -> bool {
        self.current.is_none() && self.selected.is_none()
    }

    pub fn current(&self) -> Option<&Candidate> {
        self.current.as_ref()
    }

    /// Offers a vibration heard on `tick`. Within one tick the nearest
    /// wins, the higher frequency on a tie.
    pub fn offer(&mut self, candidate: Candidate, tick: u64) {
        let replace = match &self.selected {
            None => true,
            Some((old, at)) => {
                *at == tick
                    && (candidate.distance < old.distance
                        || (candidate.distance <= old.distance
                            && frequency(candidate.event) > frequency(old.event)))
            }
        };
        if replace {
            self.selected = Some((candidate, tick));
        }
    }

    /// Advances one tick: picks what was heard on an earlier tick, counts
    /// down its travel (`travel_time` ticks for a distance) and returns it
    /// on arrival when `ready` agrees (a listener needs its surroundings
    /// loaded; if not, it keeps waiting).
    pub fn advance(
        &mut self,
        tick: u64,
        travel_time: impl Fn(f32) -> i32,
        ready: impl FnOnce() -> bool,
    ) -> Option<Candidate> {
        if self.current.is_none()
            && let Some((candidate, at)) = self.selected
            && at < tick
        {
            self.current = Some(candidate);
            self.travel = travel_time(candidate.distance);
            self.selected = None;
        }
        let current = self.current?;
        self.travel = (self.travel - 1).max(0);
        if self.travel > 0 || !ready() {
            return None;
        }
        self.current = None;
        Some(current)
    }
}

fn uuid_tag(uuid: u128) -> Tag {
    Tag::IntArray(
        (0..4)
            .map(|i| (uuid >> (96 - 32 * i)) as u32 as i32)
            .collect(),
    )
}

fn uuid_from_tag(tag: &Tag) -> Option<u128> {
    match tag {
        Tag::IntArray(v) if v.len() == 4 => {
            Some(v.iter().fold(0u128, |a, x| (a << 32) | *x as u32 as u128))
        }
        _ => None,
    }
}

const LISTENER_KEY: &str = "listener";

impl Listener {
    /// The in-flight vibration as vanilla stores it in a listener's block
    /// entity, including the source and projectile owner UUIDs.
    pub fn save(&self) -> Option<Compound> {
        let current = self.current.as_ref()?;
        let mut event = Compound::new()
            .with("game_event", current.event.name())
            .with("distance", current.distance)
            .with(
                "pos",
                Tag::List(current.origin.iter().map(|v| Tag::Double(*v)).collect()),
            );
        if let Some(source) = &current.source {
            if let Some(uuid) = source.uuid {
                event = event.with("source", uuid_tag(uuid));
            }
            if let Some(owner) = source.projectile_owner {
                event = event.with("projectile_owner", uuid_tag(owner));
            }
        }
        Some(
            Compound::new()
                .with("event", event)
                .with("event_delay", self.travel),
        )
    }

    pub fn load(nbt: &Compound) -> Option<Self> {
        let event = nbt.get_compound("event")?;
        let name = event.get_str("game_event")?;
        let game_event = GameEvent::from_name(name)?;
        let uuid = event.get("source").and_then(uuid_from_tag);
        let projectile_owner = event.get("projectile_owner").and_then(uuid_from_tag);
        let pos = event.get_list("pos")?;
        let [x, y, z] = [0, 1, 2].map(|i| pos.get(i).and_then(Tag::as_double));
        Some(Self {
            selected: None,
            current: Some(Candidate {
                event: game_event,
                distance: event.get_float("distance")?,
                origin: [x?, y?, z?],
                source: (uuid.is_some() || projectile_owner.is_some())
                    .then(|| Source::entity(-1).with_uuids(uuid, projectile_owner)),
            }),
            travel: nbt.get_int("event_delay").unwrap_or(0).max(0),
        })
    }
}

/// Writes (or clears) a listener's in-flight vibration in the block
/// entity at `pos`, marking it changed only when the stored form differs.
pub fn store(
    world: &mut dyn crate::world::region_api::RegionWorld,
    pos: BlockPos,
    l: Option<&Listener>,
) {
    let saved = l.and_then(Listener::save);
    let Some(nbt) = world.raw_block_entity_mut(pos) else {
        return;
    };
    let same = match (&saved, nbt.get_compound(LISTENER_KEY)) {
        (Some(a), Some(b)) => a == b,
        (None, None) => true,
        _ => false,
    };
    if same {
        return;
    }
    match saved {
        Some(tag) => {
            nbt.insert(LISTENER_KEY, tag);
        }
        None => {
            nbt.remove(LISTENER_KEY);
        }
    }
    world.block_entity_changed(pos);
}

/// Block entities matching `is_listener` that carry a saved vibration
/// not yet in `listeners` (a chunk was just loaded) are loaded into it.
pub fn adopt_saved(
    world: &mut dyn crate::world::region_api::RegionWorld,
    listeners: fn(&mut dyn crate::world::region_api::RegionWorld) -> &mut Listeners,
    is_listener: fn(&Compound) -> bool,
) {
    let saved = world.raw_block_entities_where(&|n| is_listener(n) && n.contains_key(LISTENER_KEY));
    for pos in saved {
        if listeners(world).contains_key(&pos) {
            continue;
        }
        let loaded = world
            .raw_block_entity(pos)
            .and_then(|n| n.get_compound(LISTENER_KEY))
            .and_then(Listener::load);
        match loaded {
            Some(l) => {
                listeners(world).insert(pos, l);
            }
            None => store(world, pos, None),
        }
    }
}

/// How far each player has walked toward its next step event.
#[derive(Default)]
pub struct Steps {
    players: HashMap<PlayerId, ([f64; 3], f32)>,
}

impl Steps {
    pub fn forget(&mut self, player: PlayerId) {
        self.players.remove(&player);
    }

    /// Records the player at `position`; true when the walk since the
    /// last step is long enough for another.
    pub fn walk(&mut self, player: PlayerId, position: [f64; 3], grounded: bool) -> bool {
        let entry = self.players.entry(player).or_insert((position, 0.0));
        let dx = position[0] - entry.0[0];
        let dz = position[2] - entry.0[2];
        entry.0 = position;
        let moved = (dx * dx + dz * dz).sqrt();
        if !grounded || moved > 8.0 {
            entry.1 = 0.0;
            return false;
        }
        entry.1 += (moved * 0.6) as f32;
        if entry.1 > 1.0 {
            entry.1 = 0.0;
            return true;
        }
        false
    }
}

/// Pending vibrations of one kind of listener in a region, by position.
pub type Listeners = HashMap<BlockPos, Listener>;

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_vanilla_data::Block;

    #[tuff_core::watchdog]
    #[test]
    fn listeners_hear_events_inside_the_smaller_of_both_radii() {
        let shriek = Emission {
            event: GameEvent::Shriek,
            pos: [0.5, 0.5, 0.5],
            context: Context::default(),
        };
        assert!(shriek.reaches(BlockPos::new(8, 0, 0), 8));
        assert!(!shriek.reaches(BlockPos::new(9, 0, 0), 8));
        assert!(!shriek.reaches(BlockPos::new(33, 0, 0), 64));
        let step = Emission {
            event: GameEvent::BlockChange,
            ..shriek
        };
        assert!(step.reaches(BlockPos::new(16, 0, 0), 64));
        assert!(!step.reaches(BlockPos::new(17, 0, 0), 64));
    }

    fn emission(event: GameEvent, source: Option<Source>) -> Emission {
        Emission {
            event,
            pos: [0.5, 0.5, 0.5],
            context: Context {
                source,
                affected: None,
            },
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn frequencies_follow_the_table_and_resonance_round_trips() {
        assert_eq!(frequency(GameEvent::Step), 1);
        assert_eq!(frequency(GameEvent::BlockPlace), 13);
        assert_eq!(frequency(GameEvent::Explode), 15);
        assert_eq!(frequency(GameEvent::Shriek), 0);
        assert_eq!(frequency(GameEvent::ItemInteractStart), 0);
        for f in 1..=15u8 {
            assert_eq!(frequency(resonance(f).unwrap()), f);
        }
        assert_eq!(resonance(0), None);
    }

    #[tuff_core::watchdog]
    #[test]
    fn strength_fades_with_distance() {
        assert_eq!(redstone_strength(0.0, 8), 15);
        assert_eq!(redstone_strength(4.0, 8), 8);
        assert_eq!(redstone_strength(8.0, 8), 1);
        assert_eq!(redstone_strength(5.0, 16), 11);
    }

    #[tuff_core::watchdog]
    #[test]
    fn sneaking_mutes_quiet_events_only() {
        let listens = tuff_vanilla_data::tag::game_event::key::VIBRATIONS;
        let sneaker = Source::player(1).sneaking(true);
        assert!(!audible(listens, &emission(GameEvent::Step, Some(sneaker))));
        assert!(audible(
            listens,
            &emission(GameEvent::BlockDestroy, Some(sneaker))
        ));
        assert!(audible(listens, &emission(GameEvent::Step, None)));
        let ghost = Source::player(1).spectating(true);
        assert!(!audible(
            listens,
            &emission(GameEvent::BlockDestroy, Some(ghost))
        ));
        let mut muffled = Source::entity(2);
        muffled.dampens = true;
        assert!(!audible(
            listens,
            &emission(GameEvent::BlockDestroy, Some(muffled))
        ));
        assert!(!audible(
            tuff_vanilla_data::tag::game_event::key::SHRIEKER_CAN_LISTEN,
            &emission(GameEvent::Step, None)
        ));
    }

    #[tuff_core::watchdog]
    #[test]
    fn muffling_blocks_make_their_events_silent() {
        let listens = tuff_vanilla_data::tag::game_event::key::VIBRATIONS;
        let mut quiet = emission(GameEvent::BlockPlace, None);
        quiet.context.affected = Some(Block::WhiteWool.default_state());
        assert!(!audible(listens, &quiet));
        quiet.context.affected = Some(Block::Stone.default_state());
        assert!(audible(listens, &quiet));
    }

    #[tuff_core::watchdog]
    #[test]
    fn wool_between_source_and_listener_occludes() {
        let wool = Block::WhiteWool.default_state();
        let air = Block::Air.default_state();
        let wall = |pos: BlockPos| Some(if pos.x == 3 { wool } else { air });
        assert!(occluded(&wall, [0.5, 0.5, 0.5], [6.5, 0.5, 0.5]));
        assert!(!occluded(&wall, [0.5, 0.5, 0.5], [2.5, 0.5, 0.5]));
        let gap = |pos: BlockPos| Some(if pos.x == 3 && pos.y != 1 { wool } else { air });
        assert!(!occluded(&gap, [0.5, 0.5, 0.5], [6.5, 2.5, 0.5]));
        assert!(!occluded(&|_| None, [0.5, 0.5, 0.5], [6.5, 0.5, 0.5]));
    }

    fn heard(event: GameEvent, distance: f32) -> Candidate {
        Candidate {
            event,
            distance,
            origin: [0.0; 3],
            source: None,
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_nearest_vibration_of_a_tick_is_chosen_and_travels() {
        let mut listener = Listener::default();
        listener.offer(heard(GameEvent::Step, 5.0), 10);
        listener.offer(heard(GameEvent::Explode, 3.0), 10);
        listener.offer(heard(GameEvent::BlockPlace, 3.0), 10);
        listener.offer(heard(GameEvent::Splash, 4.0), 11);
        let travel = |d: f32| d.floor() as i32;
        assert_eq!(listener.advance(10, travel, || true), None);
        assert!(!listener.is_busy());
        assert_eq!(listener.advance(11, travel, || true), None);
        assert!(listener.is_busy());
        assert_eq!(listener.advance(12, travel, || true), None);
        let arrived = listener.advance(13, travel, || true).expect("arrives");
        assert_eq!(arrived.event, GameEvent::Explode);
        assert!(listener.is_idle());
    }

    #[tuff_core::watchdog]
    #[test]
    fn an_unready_listener_keeps_the_vibration() {
        let mut listener = Listener::default();
        listener.offer(heard(GameEvent::Step, 0.0), 1);
        assert_eq!(listener.advance(2, |_| 0, || false), None);
        assert!(listener.is_busy());
        assert!(listener.advance(3, |_| 0, || true).is_some());
    }

    #[tuff_core::watchdog]
    #[test]
    fn source_uuids_survive_save_and_load() {
        let mut candidate = heard(GameEvent::Step, 3.0);
        candidate.source = Some(
            Source::entity(5).with_uuids(Some(0x1234_5678_9abc_def0_1122_3344_5566_7788), Some(42)),
        );
        let mut listener = Listener::default();
        listener.offer(candidate, 1);
        let _ = listener.advance(2, |_| 10, || true);
        let loaded = Listener::load(&listener.save().unwrap()).unwrap();
        let source = loaded.current().unwrap().source.unwrap();
        assert_eq!(source.uuid, Some(0x1234_5678_9abc_def0_1122_3344_5566_7788));
        assert_eq!(source.projectile_owner, Some(42));
    }
}
