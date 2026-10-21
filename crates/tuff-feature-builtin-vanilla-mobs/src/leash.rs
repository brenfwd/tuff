//! Leads (the state and the elastic pull and snap are core's
//! `world::leash`): tying mobs and boats to players, other entities and
//! knots on fences, and leads whose holder stands in a neighbouring
//! region, which that region answers for through region tasks.
//!
//! A leashed entity looks its holder up every tick among the region's
//! players and entities; one it cannot see here goes into [`Leashes`] as
//! wanted, and the region's late tick asks the neighbours (or ties the knot
//! a fence of its own needs). A holder that stays unseen breaks the lead.

use rand::Rng;
use tuff_vanilla_data::Item;
use tuff_vanilla_data::registry::{EntityType, SoundEvent};
use tuff_vanilla_data::tag;

use tuff_core::protocol::play;
use tuff_core::world::entity::{
    AttackOutcome, Effect, EntityAttack, EntityBase, EntityInteraction, InteractOutcome,
    RegionEntity, Tick, TickContext,
};
use tuff_core::world::game_event::{Context as GameContext, GameEvent, Source};
use tuff_core::world::leash::{
    Anchor, Leash, LeashProfile, REMOTE_KEEP, SEARCH, center, distance, knot_pos, link_packet,
    sound_packet,
};
pub use tuff_core::world::leash::{
    Body, Holder, Leashes, Pull, angular_friction, lead, tick, untie,
};
use tuff_core::world::messages::{ActionOutcome, HandEffect, Place, PlayerHandle, RegionHandle};
use tuff_core::world::packets;
use tuff_core::world::region_api::{RegionTask, RegionWorld};
use tuff_nbt::Compound;
use tuff_world_model::BlockPos;
use tuff_world_model::pos::{ChunkPos, RegionPos};

/// Ticks a knot no lead holds stays before it goes.
const KNOT_GRACE: u32 = 5;
/// Ticks between a knot's checks that its fence is still there.
const KNOT_CHECK_TICKS: u32 = 100;
const KNOT_OFFSET_Y: f64 = 0.375;
/// The usual lead of a mob of `kind`, or `None` if it never takes one.
pub fn default_profile(kind: EntityType) -> Option<LeashProfile> {
    use EntityType as E;
    use tuff_vanilla_data::entity::MobCategory as C;
    let leashable = match kind {
        E::Hoglin
        | E::Zoglin
        | E::Squid
        | E::GlowSquid
        | E::Dolphin
        | E::Axolotl
        | E::ZombieHorse
        | E::CamelHusk
        | E::ZombieNautilus
        | E::SulfurCube => true,
        E::Turtle | E::Panda | E::Villager | E::WanderingTrader | E::Mannequin | E::Tadpole => {
            false
        }
        _ => !matches!(
            kind.category(),
            C::Monster | C::Ambient | C::WaterAmbient | C::UndergroundWaterCreature
        ),
    };
    leashable.then(|| profile_of(kind))
}

/// The lead a mob takes now: its kind's, unless its state says no (an
/// angry wolf, a mount a mob rides, a sulfur cube with nothing inside).
pub fn mob_profile(core: &crate::MobCore) -> Option<LeashProfile> {
    use EntityType as E;
    let profile = default_profile(core.kind())?;
    let mob_ridden = !core.base.entity_riders.is_empty();
    let allowed = match core.kind() {
        E::Wolf => core.anger.is_none(),
        E::ZombieHorse => core.tame.as_ref().is_some_and(|t| t.is_tamed()) || !mob_ridden,
        E::CamelHusk => !mob_ridden,
        E::ZombieNautilus => !mob_ridden && core.anger.is_none(),
        E::Nautilus => core.anger.is_none(),
        E::SulfurCube => core
            .equipment
            .get(tuff_vanilla_data::item::EquipmentSlot::Body)
            .is_some(),
        _ => true,
    };
    allowed.then_some(profile)
}

/// The distances and corners of `kind`'s lead, whether or not it is
/// leashable now.
pub fn profile_of(kind: EntityType) -> LeashProfile {
    use EntityType as E;
    let (elastic, snap) = match kind {
        E::Ghast | E::HappyGhast => (10.0, 16.0),
        _ => (6.0, 12.0),
    };
    let quad = matches!(
        kind,
        E::Horse
            | E::Donkey
            | E::Mule
            | E::SkeletonHorse
            | E::ZombieHorse
            | E::Camel
            | E::CamelHusk
            | E::Sniffer
    );
    LeashProfile {
        elastic,
        snap,
        quad,
    }
}

/// A leash knot: the lead's anchor on a fence. Leads that name its fence
/// tie it again on load, so it is never saved; it goes when no lead holds
/// it or its fence is gone.
#[derive(Debug)]
pub struct LeashKnot {
    base: EntityBase,
    pos: BlockPos,
    age: u32,
    check: u32,
    broken: bool,
}

impl LeashKnot {
    pub fn new(id: i32, uuid: u128, pos: BlockPos) -> Self {
        let position = [
            f64::from(pos.x) + 0.5,
            f64::from(pos.y) + KNOT_OFFSET_Y,
            f64::from(pos.z) + 0.5,
        ];
        Self {
            base: EntityBase::new(EntityType::LeashKnot, id, uuid, position, [0.0; 3]),
            pos,
            age: 0,
            check: 0,
            broken: false,
        }
    }

    pub fn pos(&self) -> BlockPos {
        self.pos
    }
}

fn is_fence(state: Option<tuff_vanilla_data::BlockState>) -> bool {
    state.is_some_and(|s| tuff_datapack::tags::is(tag::block::key::FENCES, s.block()))
}

impl RegionEntity for LeashKnot {
    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn base_mut(&mut self) -> &mut EntityBase {
        &mut self.base
    }

    fn save(&self, out: &mut Compound) {
        out.insert(
            "block_pos",
            tuff_nbt::Tag::IntArray(vec![self.pos.x, self.pos.y, self.pos.z]),
        );
    }

    fn persists(&self) -> bool {
        false
    }

    fn pickable(&self) -> bool {
        true
    }

    fn tick(&mut self, cx: &mut TickContext) -> Tick {
        self.base.velocity = [0.0; 3];
        self.age += 1;
        let now = cx.tick;
        let held = cx
            .state
            .get::<Leashes>()
            .held
            .get(&self.base.id)
            .is_some_and(|&at| now.saturating_sub(at) <= 1);
        self.check += 1;
        let fence_gone = self.check >= KNOT_CHECK_TICKS && {
            self.check = 0;
            (cx.block)(self.pos).is_some_and(|s| !is_fence(Some(s)))
        };
        if fence_gone || self.broken {
            let seed = cx.rng.r#gen();
            let category = play::sound_category::NEUTRAL;
            let sound = sound_packet(
                SoundEvent::ItemLeadUntied,
                self.base.position,
                category,
                seed,
            );
            cx.effects.push(Effect::ChunkPackets(sound));
        }
        if fence_gone || self.broken || (!held && self.age > KNOT_GRACE) {
            cx.state.get::<Leashes>().gone.insert(self.base.id, now);
            return Tick::Discard;
        }
        Tick::Alive
    }

    fn hurt(
        &mut self,
        _amount: f32,
        _damage_type: &'static tuff_vanilla_data::damage_type::DamageType,
    ) -> bool {
        self.broken = true;
        true
    }

    fn hit(
        &mut self,
        hit: &tuff_core::world::entity::EntityHit,
        cx: &mut TickContext,
    ) -> Option<AttackOutcome> {
        let mob_attacker = hit.attacker.as_ref().is_some_and(|a| !a.is_player());
        if mob_attacker && !cx.shared.rule_bool("mob_griefing") {
            return None;
        }
        self.broken = true;
        Some(AttackOutcome {
            hurt: true,
            health_lost: 0.0,
        })
    }

    fn ignores_pistons(&self) -> bool {
        true
    }

    fn attacked(&mut self, _attack: &EntityAttack, _cx: &mut TickContext) -> Option<AttackOutcome> {
        self.broken = true;
        Some(AttackOutcome {
            hurt: true,
            health_lost: 0.0,
        })
    }
}

/// Loads a knot saved by another server (the game itself never saves one).
pub fn load_knot(base: EntityBase, nbt: &mut Compound) -> Option<Box<dyn RegionEntity>> {
    let pos = match nbt.remove("block_pos") {
        Some(tuff_nbt::Tag::IntArray(pos)) if pos.len() == 3 => {
            BlockPos::new(pos[0], pos[1], pos[2])
        }
        _ => knot_pos(base.position),
    };
    Some(Box::new(LeashKnot::new(base.id, base.uuid, pos)))
}

pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    registry.register(EntityType::LeashKnot, load_knot);
}

fn leashes(world: &mut dyn RegionWorld) -> &mut Leashes {
    world.state().get::<Leashes>()
}

fn run_on(handle: &RegionHandle, task: impl FnOnce(&mut dyn RegionWorld) + Send + 'static) {
    handle.internal(tuff_core::world::messages::RegionInternal::Run(
        RegionTask::new(task),
    ));
}

/// The knot on the fence at `pos`, if one is tied there.
fn knot_at(world: &dyn RegionWorld, pos: BlockPos) -> Option<i32> {
    world.entity_ids().into_iter().find(|&id| {
        world
            .entity(id)
            .and_then(|t| t.entity.downcast_ref::<LeashKnot>())
            .is_some_and(|knot| knot.pos == pos)
    })
}

/// The knot on the fence at `pos`, tied now if need be; `None` when there
/// is no fence there to tie one to.
pub fn knot_or_tie(world: &mut dyn RegionWorld, pos: BlockPos) -> Option<i32> {
    if let Some(id) = knot_at(world, pos) {
        return Some(id);
    }
    if !world.owns(pos) || !is_fence(world.block(pos)) {
        return None;
    }
    let id = world.shared().next_entity_id();
    let uuid = world.rng().r#gen();
    world.add_entity(Box::new(LeashKnot::new(id, uuid, pos)));
    Some(id)
}

fn holder_in(world: &mut dyn RegionWorld, anchor: Anchor) -> Option<Holder> {
    let id = match anchor {
        Anchor::Knot(pos) => {
            let id = knot_or_tie(world, pos)?;
            let now = world.tick();
            leashes(world).held.insert(id, now);
            id
        }
        Anchor::Entity(uuid) => world.entity_ids().into_iter().find(|&id| {
            world
                .entity(id)
                .is_some_and(|t| t.entity.base().uuid == uuid)
        })?,
    };
    world.entity(id).map(|t| Holder::of_entity(&*t.entity))
}

fn neighbours_near(world: &dyn RegionWorld, at: [f64; 3], margin: f64) -> Vec<RegionHandle> {
    let here = world.region_pos();
    let mut found: Vec<RegionPos> = Vec::new();
    for dx in [-margin, 0.0, margin] {
        for dz in [-margin, 0.0, margin] {
            let region = ChunkPos::of_position([at[0] + dx, at[1], at[2] + dz]).region();
            if region != here && !found.contains(&region) {
                found.push(region);
            }
        }
    }
    found
        .into_iter()
        .filter_map(|region| world.neighbor(region))
        .collect()
}

/// The region's late tick: ties knots its own leads need and asks the
/// neighbours for holders it could not see.
pub fn tick_region(world: &mut dyn RegionWorld) {
    let now = world.tick();
    let wanted: Vec<(Anchor, [f64; 3])> = leashes(world).wanted.drain().collect();
    {
        let state = leashes(world);
        state.gone.retain(|_, at| now.saturating_sub(*at) <= 1);
        state
            .remote
            .retain(|_, (_, at)| now.saturating_sub(*at) <= REMOTE_KEEP * 4);
        state.held.retain(|_, at| now.saturating_sub(*at) <= 2);
        state.quad_held.retain(|_, at| now.saturating_sub(*at) <= 2);
    }
    let home = world.handle();
    for (anchor, at) in wanted {
        let handles = match anchor {
            Anchor::Knot(pos) if world.owns(pos) => {
                knot_or_tie(world, pos);
                continue;
            }
            Anchor::Knot(pos) => world
                .neighbor(ChunkPos::of_block(pos).region())
                .into_iter()
                .collect(),
            Anchor::Entity(_) => neighbours_near(world, at, SEARCH),
        };
        for handle in handles {
            let home = home.clone();
            run_on(&handle, move |other| {
                let Some(holder) = holder_in(other, anchor) else {
                    return;
                };
                let tick = other.tick();
                run_on(&home, move |here| {
                    leashes(here).remote.insert(anchor, (holder, tick));
                });
            });
        }
    }
}

/// An entity a lead can be tied to, and where.
#[derive(Debug, Clone, Copy)]
struct Target {
    id: i32,
    anchor: Anchor,
    holder: Holder,
}

/// Entities of this region whose lead `held` holds, within the 32-block
/// box around `around`.
fn leashed_to(world: &dyn RegionWorld, held: Anchor, around: [f64; 3]) -> Vec<i32> {
    world
        .entity_ids()
        .into_iter()
        .filter(|&id| {
            world.entity(id).is_some_and(|t| {
                let c = center(t.entity.bounds());
                t.entity.leash().is_some_and(|l| l.anchor == held)
                    && (0..3).all(|axis| (c[axis] - around[axis]).abs() <= SEARCH)
            })
        })
        .collect()
}

/// Moves the leads of `ids` over to `to` where they reach; how many moved.
fn retie(world: &mut dyn RegionWorld, ids: &[i32], to: Target) -> usize {
    let mut moved = 0;
    for &id in ids {
        if id == to.id {
            continue;
        }
        let Some(tracked) = world.entity_mut(id) else {
            continue;
        };
        let Some(profile) = tracked.entity.leash_profile() else {
            continue;
        };
        if distance(center(tracked.entity.bounds()), center(to.holder.bounds)) > profile.snap {
            continue;
        }
        let Some(slot) = tracked.entity.leash_mut() else {
            continue;
        };
        *slot = Some(Leash::tied(to.anchor, to.holder.id, to.holder.is_player()));
        tracked.refresh_spawn();
        let chunk = tracked.chunk();
        world.send_to_chunk(chunk, None, packets([link_packet(id, Some(to.holder.id))]));
        moved += 1;
    }
    moved
}

/// What a tie that only neighbour regions completed reports once they
/// answer: the same sound and game event a local tie plays.
#[derive(Clone, Copy)]
struct Report {
    sound_at: [f64; 3],
    category: i32,
    event: GameEvent,
    event_at: [f64; 3],
    source: GameContext,
}

/// Has the regions the box around `around` reaches move the leads `held`
/// holds over to `to` too; `report` plays when only they moved any.
fn retie_in_neighbours(
    world: &mut dyn RegionWorld,
    held: Anchor,
    around: [f64; 3],
    to: Target,
    report: Report,
) {
    tuff_core::world::area::query(
        world,
        tuff_core::world::area::around(around, SEARCH),
        move |other| {
            let ids = leashed_to(other, held, around);
            retie(other, &ids, to)
        },
        move |world, moved| {
            if moved.iter().sum::<usize>() > 0 {
                world.game_event(report.event, report.event_at, report.source);
                entity_sound(
                    world,
                    SoundEvent::ItemLeadTied,
                    report.sound_at,
                    report.category,
                );
            }
        },
    );
}

/// Takes the lead off the region's entity `id`, dropping it as an item
/// when `drop`.
pub fn untie_in(world: &mut dyn RegionWorld, id: i32, drop: bool) -> bool {
    let Some(tracked) = world.entity_mut(id) else {
        return false;
    };
    let position = tracked.entity.base().position;
    let Some(slot) = tracked.entity.leash_mut() else {
        return false;
    };
    let Some(leash) = slot.take() else {
        return false;
    };
    tracked.refresh_spawn();
    let chunk = tracked.chunk();
    if leash.holder.is_some() {
        world.send_to_chunk(chunk, None, packets([link_packet(id, None)]));
    }
    if drop {
        let rng = world.rng();
        let velocity = [
            rng.r#gen::<f64>() * 0.2 - 0.1,
            0.2,
            rng.r#gen::<f64>() * 0.2 - 0.1,
        ];
        world.spawn_item_at(
            lead(),
            position,
            velocity,
            tuff_core::world::items::BLOCK_DROP_PICKUP_DELAY,
        );
    }
    true
}

/// Shears (or a firework, or a dispenser's shears) cut every lead on or
/// held by the region's entity `id`, dropping them; whether any was cut.
/// The cut sound and game event are the caller's.
pub fn cut_all(world: &mut dyn RegionWorld, id: i32) -> bool {
    let Some(tracked) = world.entity(id) else {
        return false;
    };
    let uuid = tracked.entity.base().uuid;
    let around = center(tracked.entity.bounds());
    let held = match tracked.entity.downcast_ref::<LeashKnot>() {
        Some(knot) => Anchor::Knot(knot.pos),
        None => Anchor::Entity(uuid),
    };
    let mut cut = untie_in(world, id, true);
    for other in leashed_to(world, held, around) {
        cut |= untie_in(world, other, true);
    }
    for handle in neighbours_near(world, around, SEARCH) {
        run_on(&handle, move |other| {
            for id in leashed_to(other, held, around) {
                untie_in(other, id, true);
            }
        });
    }
    cut
}

/// A sound at an entity for the viewers of its chunk.
fn entity_sound(world: &mut dyn RegionWorld, sound: SoundEvent, position: [f64; 3], category: i32) {
    let seed = world.rng().r#gen();
    let chunk = ChunkPos::of_position(position);
    world.send_to_chunk(chunk, None, sound_packet(sound, position, category, seed));
}

fn player_holder(interaction: &EntityInteraction) -> Holder {
    let [x, y, z] = interaction.position;
    let height = if interaction.sneaking { 1.5 } else { 1.8 };
    Holder {
        id: interaction.player,
        uuid: interaction.player_uuid,
        kind: None,
        position: interaction.position,
        bounds: [x - 0.3, y, z - 0.3, x + 0.3, y + height, z + 0.3],
        movement: [0.0; 3],
        yaw: 0.0,
        alive: true,
    }
}

fn sound_category_of(entity: &dyn RegionEntity) -> i32 {
    if entity.base().kind.category() == tuff_vanilla_data::entity::MobCategory::Monster {
        play::sound_category::HOSTILE
    } else {
        play::sound_category::NEUTRAL
    }
}

/// A player's click on the region's entity `target`, where leads come
/// first: tying the player's leashed mobs to it (sneaking), cutting its
/// leads with shears, untying it from the player, or leashing it.
pub fn interact(
    world: &mut dyn RegionWorld,
    player: &PlayerHandle,
    target: i32,
    interaction: &EntityInteraction,
) -> InteractOutcome {
    let _ = player;
    let tracked = world.entity(target)?;
    if tracked.entity.downcast_ref::<LeashKnot>().is_some() {
        return interact_knot(world, target, interaction);
    }
    let entity = &*tracked.entity;
    let base = entity.base();
    let living = entity.living();
    let alive = living.is_none_or(|l| l.alive);
    let baby = living.is_some_and(|l| l.age < 0);
    let profile = entity.leash_profile();
    let leashable = entity.leash().is_some() || profile.is_some();
    let holder = Holder::of_entity(entity);
    let (position, category) = (base.position, sound_category_of(entity));
    let own = entity.leash().cloned();
    let player_anchor = Anchor::Entity(interaction.player_uuid);
    let item = interaction.stack.as_ref().map(|s| s.item);
    if interaction.sneaking && profile.is_some() && alive && !baby {
        let around = center(holder.bounds);
        let ids = leashed_to(world, player_anchor, around);
        let to = Target {
            id: target,
            anchor: Anchor::Entity(holder.uuid),
            holder,
        };
        let report = Report {
            sound_at: position,
            category,
            event: GameEvent::EntityAction,
            event_at: position,
            source: GameContext::from(Source::player(interaction.player)),
        };
        retie_in_neighbours(world, player_anchor, around, to, report);
        if !ids.is_empty() && retie(world, &ids, to) > 0 {
            world.game_event(
                GameEvent::EntityAction,
                position,
                GameContext::from(Source::player(interaction.player)),
            );
            entity_sound(world, SoundEvent::ItemLeadTied, position, category);
            return Some(HandEffect::None);
        }
    }
    if item == Some(Item::Shears) && cut_all(world, target) {
        world.game_event(
            GameEvent::Shear,
            position,
            GameContext::from(Source::player(interaction.player)),
        );
        entity_sound(
            world,
            SoundEvent::ItemShearsSnip,
            position,
            play::sound_category::PLAYERS,
        );
        return Some(HandEffect::Damage(1));
    }
    if !alive || !leashable {
        return None;
    }
    if own.as_ref().is_some_and(|l| l.anchor == player_anchor) {
        untie_in(world, target, !interaction.creative);
        world.game_event(
            GameEvent::EntityInteract,
            position,
            GameContext::from(Source::player(interaction.player)),
        );
        entity_sound(world, SoundEvent::ItemLeadUntied, position, category);
        return Some(HandEffect::None);
    }
    if item != Some(Item::Lead) || own.as_ref().is_some_and(|l| l.player) {
        return None;
    }
    let profile = profile?;
    let by = player_holder(interaction);
    if distance(center(by.bounds), center(holder.bounds)) > profile.snap {
        return None;
    }
    if own.is_some() {
        untie_in(world, target, true);
    }
    let tracked = world.entity_mut(target)?;
    let slot = tracked.entity.leash_mut()?;
    *slot = Some(Leash::tied(player_anchor, by.id, true));
    tracked.refresh_spawn();
    let chunk = tracked.chunk();
    world.send_to_chunk(chunk, None, packets([link_packet(target, Some(by.id))]));
    entity_sound(world, SoundEvent::ItemLeadTied, position, category);
    Some(HandEffect::Consume(1))
}

fn interact_knot(
    world: &mut dyn RegionWorld,
    knot: i32,
    interaction: &EntityInteraction,
) -> InteractOutcome {
    let tracked = world.entity(knot)?;
    let pos = tracked.entity.downcast_ref::<LeashKnot>()?.pos;
    let holder = Holder::of_entity(&*tracked.entity);
    let position = holder.position;
    let source = GameContext::from(Source::player(interaction.player));
    if interaction
        .stack
        .as_ref()
        .is_some_and(|s| s.item == Item::Shears)
        && cut_all(world, knot)
    {
        world.game_event(GameEvent::Shear, position, source);
        entity_sound(
            world,
            SoundEvent::ItemShearsSnip,
            position,
            play::sound_category::PLAYERS,
        );
        return Some(HandEffect::Damage(1));
    }
    let player_anchor = Anchor::Entity(interaction.player_uuid);
    let by = player_holder(interaction);
    let to_knot = Target {
        id: knot,
        anchor: Anchor::Knot(pos),
        holder,
    };
    let around_player = center(by.bounds);
    let mine = leashed_to(world, player_anchor, around_player);
    let report = Report {
        sound_at: position,
        category: play::sound_category::NEUTRAL,
        event: GameEvent::BlockAttach,
        event_at: position,
        source,
    };
    retie_in_neighbours(world, player_anchor, around_player, to_knot, report);
    let mut tied = retie(world, &mine, to_knot) > 0;
    if !tied && !interaction.sneaking {
        let knot_anchor = Anchor::Knot(pos);
        let around = center(holder.bounds);
        let on_knot = leashed_to(world, knot_anchor, around);
        let to_player = Target {
            id: by.id,
            anchor: player_anchor,
            holder: by,
        };
        retie_in_neighbours(world, knot_anchor, around, to_player, report);
        tied = retie(world, &on_knot, to_player) > 0;
    }
    if !tied {
        return None;
    }
    world.game_event(GameEvent::BlockAttach, position, source);
    entity_sound(
        world,
        SoundEvent::ItemLeadTied,
        position,
        play::sound_category::NEUTRAL,
    );
    Some(HandEffect::None)
}

/// A click on a fence (or a lead used on one): ties the clicking player's
/// leashed mobs to a knot there.
pub fn bind_to_fence(
    world: &mut dyn RegionWorld,
    place: &Place,
    state: tuff_vanilla_data::BlockState,
) -> Option<ActionOutcome> {
    if !is_fence(Some(state)) {
        return None;
    }
    let pos = place.target;
    let player_anchor = Anchor::Entity(place.uuid);
    let around = pos.center();
    let mine = leashed_to(world, player_anchor, around);
    let existing = knot_at(world, pos);
    let knot = existing.or_else(|| knot_or_tie(world, pos))?;
    let tracked = world.entity(knot)?;
    let to = Target {
        id: knot,
        anchor: Anchor::Knot(pos),
        holder: Holder::of_entity(&*tracked.entity),
    };
    let report = Report {
        sound_at: to.holder.position,
        category: play::sound_category::NEUTRAL,
        event: GameEvent::BlockAttach,
        event_at: around,
        source: GameContext::from(Source::player(place.player.id)),
    };
    retie_in_neighbours(world, player_anchor, around, to, report);
    let now = world.tick();
    leashes(world).held.insert(knot, now);
    if retie(world, &mine, to) == 0 {
        if existing.is_none() && mine.is_empty() {
            world.remove_entity(knot);
        }
        return None;
    }
    entity_sound(
        world,
        SoundEvent::ItemLeadTied,
        to.holder.position,
        play::sound_category::NEUTRAL,
    );
    world.game_event(
        GameEvent::BlockAttach,
        around,
        GameContext::from(Source::player(place.player.id)),
    );
    Some(ActionOutcome::Interacted {
        off_hand: place.off_hand,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::with_entities;
    use tuff_core::world::entity::EntitySummary;
    use tuff_core::world::leash::{MISSING_GRACE, UNRESOLVED_TICKS};

    fn holder(x: f64, alive: bool) -> EntitySummary {
        EntitySummary {
            id: 9,
            uuid: 99,
            kind: EntityType::Cow,
            position: [x, 64.0, 0.5],
            velocity: [0.0; 3],
            yaw: 0.0,
            bounds: [x - 0.45, 64.0, 0.05, x + 0.45, 65.4, 0.95],
            living: Some(tuff_core::world::entity::LivingSummary {
                unpushable: false,
                alive,
                target: None,
                age: 0,
                in_love: false,
                panicking: false,
                eye_height: 1.3,
                in_water: false,
                seen_as_enemy: true,
                enemy: false,
                health: 10.0,
                tamed: false,
                owner: None,
                chasing: None,
            }),
            pickable: true,
            item: None,
            vehicle: None,
            carries: false,
            ominous_banner: false,
            blue_sheep: false,
        }
    }

    fn pull(core: &mut crate::MobCore, cx: &mut TickContext) -> Pull {
        let body = Body {
            bounds: core.bounds(),
            profile: LeashProfile::default(),
            alive: true,
            angular_friction: 0.91,
            motion: core.base.velocity,
            base: &mut core.base,
        };
        tick(&mut core.leash, body, cx)
    }

    fn dropped_leads(cx: &TickContext) -> usize {
        cx.effects
            .iter()
            .filter(|e| matches!(e, Effect::DropItem { stack, .. } if stack.item == Item::Lead))
            .count()
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_taut_lead_pulls_toward_its_holder_and_links_it() {
        with_entities(&[holder(9.5, true)], |cow, cx| {
            cow.leash = Some(Leash::pending(Anchor::Entity(99)));
            assert!(matches!(pull(cow, cx), Pull::Elastic(_)));
            let leash = cow.leash.as_ref().expect("still on");
            assert_eq!(leash.holder, Some(9));
            assert!(leash.found);
            assert!(cow.base.velocity[0] > 0.1, "{:?}", cow.base.velocity);
            assert!(
                cx.effects
                    .iter()
                    .any(|e| matches!(e, Effect::EntityPackets(_))),
                "viewers see the lead"
            );
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_slack_lead_does_not_pull() {
        with_entities(&[holder(3.5, true)], |cow, cx| {
            cow.leash = Some(Leash::pending(Anchor::Entity(99)));
            assert!(matches!(pull(cow, cx), Pull::Close(_)));
            assert_eq!(cow.base.velocity, [0.0; 3]);
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_lead_pulled_past_twelve_blocks_snaps_and_drops() {
        with_entities(&[holder(14.0, true)], |cow, cx| {
            cow.leash = Some(Leash::pending(Anchor::Entity(99)));
            assert!(matches!(pull(cow, cx), Pull::Snapped(_)));
            assert!(cow.leash.is_none());
            assert_eq!(dropped_leads(cx), 1);
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_dead_holder_lets_go() {
        with_entities(&[holder(4.0, false)], |cow, cx| {
            cow.leash = Some(Leash::pending(Anchor::Entity(99)));
            assert_eq!(pull(cow, cx), Pull::Dropped);
            assert!(cow.leash.is_none());
            assert_eq!(dropped_leads(cx), 1);
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_loaded_lead_waits_a_hundred_ticks_for_its_holder() {
        with_entities(&[], |cow, cx| {
            cow.leash = Some(Leash::pending(Anchor::Entity(99)));
            for _ in 0..UNRESOLVED_TICKS {
                assert_eq!(pull(cow, cx), Pull::Idle);
            }
            assert!(cow.leash.is_some());
            assert_eq!(pull(cow, cx), Pull::Dropped);
            assert!(cow.leash.is_none());
            assert_eq!(dropped_leads(cx), 1);
            assert!(
                cx.state
                    .get::<Leashes>()
                    .wanted
                    .contains_key(&Anchor::Entity(99)),
                "the neighbours were asked"
            );
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_holder_that_vanished_breaks_the_lead_after_a_grace() {
        with_entities(&[], |cow, cx| {
            let mut leash = Leash::tied(Anchor::Entity(99), 9, false);
            leash.found = true;
            cow.leash = Some(leash);
            for _ in 0..MISSING_GRACE {
                assert_eq!(pull(cow, cx), Pull::Idle);
            }
            assert_eq!(pull(cow, cx), Pull::Dropped);
            assert_eq!(dropped_leads(cx), 1);
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_neighbours_answer_stands_in_for_a_holder_out_of_sight() {
        with_entities(&[], |cow, cx| {
            let seen = Holder::of_summary(&holder(9.5, true));
            let now = cx.tick;
            cx.state
                .get::<Leashes>()
                .remote
                .insert(Anchor::Entity(99), (seen, now));
            cow.leash = Some(Leash::pending(Anchor::Entity(99)));
            assert!(matches!(pull(cow, cx), Pull::Elastic(_)));
            assert_eq!(cow.leash.as_ref().and_then(|l| l.holder), Some(9));
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn hostile_and_water_ambient_mobs_take_no_lead() {
        assert!(default_profile(EntityType::Cow).is_some());
        assert!(default_profile(EntityType::Hoglin).is_some());
        assert!(default_profile(EntityType::Dolphin).is_some());
        assert!(default_profile(EntityType::IronGolem).is_some());
        assert!(default_profile(EntityType::Zombie).is_none());
        assert!(default_profile(EntityType::Ghast).is_none());
        assert!(default_profile(EntityType::Bat).is_none());
        assert!(default_profile(EntityType::Cod).is_none());
        assert!(default_profile(EntityType::Villager).is_none());
        assert!(default_profile(EntityType::Turtle).is_none());
        for kind in [
            EntityType::Allay,
            EntityType::SnowGolem,
            EntityType::CopperGolem,
            EntityType::Strider,
            EntityType::SkeletonHorse,
            EntityType::Camel,
            EntityType::Frog,
            EntityType::Nautilus,
            EntityType::Parrot,
        ] {
            assert!(default_profile(kind).is_some(), "{kind:?}");
        }
        for kind in [
            EntityType::Creeper,
            EntityType::Phantom,
            EntityType::Slime,
            EntityType::Tadpole,
            EntityType::Salmon,
            EntityType::Piglin,
            EntityType::Witch,
        ] {
            assert!(default_profile(kind).is_none(), "{kind:?}");
        }
        let ghast = default_profile(EntityType::HappyGhast).unwrap();
        assert_eq!((ghast.elastic, ghast.snap), (10.0, 16.0));
        assert!(default_profile(EntityType::Horse).unwrap().quad);
        assert!(!default_profile(EntityType::Llama).unwrap().quad);
    }

    fn knot_hit(
        attacker: tuff_core::world::entity::Culprit,
    ) -> tuff_core::world::entity::EntityHit {
        let mut hit = tuff_core::world::entity::EntityHit::new(
            1.0,
            &tuff_vanilla_data::damage_type::MOB_ATTACK,
        );
        hit.attacker = Some(attacker);
        hit
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_mob_cannot_break_a_knot_without_mob_griefing() {
        crate::tests::with_rule("mob_griefing", false, |_, cx| {
            let mob_base = EntityBase::new(EntityType::Zombie, 5, 5, [0.0; 3], [0.0; 3]);
            let mob = tuff_core::world::entity::Culprit::entity(&mob_base);
            let mut knot = LeashKnot::new(2, 2, BlockPos::new(0, 64, 0));
            assert!(knot.hit(&knot_hit(mob.clone()), cx).is_none());
            assert!(!knot.broken);
            let player = tuff_core::world::entity::Culprit::player(1, 1, "P");
            assert!(knot.hit(&knot_hit(player), cx).is_some());
            assert!(knot.broken);
        });
        crate::tests::with_context(|_, cx| {
            let mob_base = EntityBase::new(EntityType::Zombie, 5, 5, [0.0; 3], [0.0; 3]);
            let mob = tuff_core::world::entity::Culprit::entity(&mob_base);
            let mut other = LeashKnot::new(3, 3, BlockPos::new(1, 64, 0));
            assert!(other.hit(&knot_hit(mob), cx).is_some());
            assert!(other.broken);
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_knot_breaks_when_hurt_by_an_explosion() {
        let mut knot = LeashKnot::new(2, 2, BlockPos::new(0, 64, 0));
        assert!(knot.hurt(4.0, &tuff_vanilla_data::damage_type::EXPLOSION));
        assert!(knot.broken);
    }
}
