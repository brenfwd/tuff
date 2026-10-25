//! Bees leaving a beehive or bee nest: the occupants saved in the hive's
//! block entity come out in front of it as live bees.

use rand::Rng;
use tuff_vanilla_data::registry::SoundEvent;
use tuff_vanilla_data::{Block, BlockState, Item, tag};

use tuff_core::world::game_event::{Context, GameEvent, Source};
use tuff_core::world::messages::{ActionOutcome, HandEffect};
use tuff_core::world::region_api::{DispenseOutcome, RegionWorld};
use tuff_core::world::stored_entity::{EntityBurn, StoredEntity};
use tuff_item::item::{ItemStack, MintReason};
use tuff_nbt::{Compound, Tag};
use tuff_world_model::BlockPos;

const HONEYCOMBS: usize = 3;
const MAX_HONEY: &str = "5";

/// Keys an occupant's saved data never brings out of the hive.
const IGNORED_TAGS: &[&str] = &[
    "Air",
    "drop_chances",
    "equipment",
    "Brain",
    "CanPickUpLoot",
    "DeathTime",
    "fall_distance",
    "FallFlying",
    "Fire",
    "HurtTime",
    "LeftHanded",
    "Motion",
    "NoGravity",
    "OnGround",
    "PortalCooldown",
    "Pos",
    "Rotation",
    "sleeping_pos",
    "CannotEnterHiveTicks",
    "TicksSincePollination",
    "CropsGrownSincePollination",
    "hive_pos",
    "Passengers",
    "leash",
    "UUID",
];
/// How often a released bee takes up the hive's remembered flower.
const FLOWER_CHANCE: f32 = 0.9;
/// A released bee starts this far in front of the hive, plus half its
/// width.
const EXIT_DISTANCE: f64 = 0.55;

/// Ticks a bee that left stays out of hives.
const CANNOT_ENTER_TICKS: i32 = 400;
/// Bees a hive holds.
pub const CAPACITY: usize = 3;
const MIN_TICKS_NECTAR: i32 = 2400;
const MIN_TICKS_EMPTY: i32 = 600;
/// How far bees notice a hive being robbed.
const ANGER_BOX: [f64; 3] = [8.0, 6.0, 8.0];

/// Bees on their way into a hive, queued by the bees and settled by the
/// region's late tick.
#[derive(Default)]
pub struct Entries(Vec<(i32, BlockPos)>);

impl Entries {
    pub fn push(&mut self, bee: i32, hive: BlockPos) {
        self.0.push((bee, hive));
    }
}

/// Why bees leave.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Release {
    /// Let out (the hive was harvested): not at night or in rain, and not
    /// through a blocked front.
    Released,
    /// Left on their own once their time was up: a bee carrying nectar
    /// hands it over (the hive gains honey), and the same night, rain and
    /// blocked-front rules as `Released` apply.
    Exited,
    /// Driven out (the hive broke or caught fire): always.
    Emergency,
}

/// Lets the bees in the hive at `pos` (whose state is `state`) out in
/// front of it, removing them from its block entity. Returns the released
/// entities' ids.
pub fn release(
    world: &mut dyn RegionWorld,
    pos: BlockPos,
    state: BlockState,
    reason: Release,
) -> Vec<i32> {
    release_where(world, pos, state, reason, &|_| true)
}

/// Like [`release`], for the occupants `ready` accepts.
pub fn release_where(
    world: &mut dyn RegionWorld,
    pos: BlockPos,
    state: BlockState,
    reason: Release,
    ready: &dyn Fn(&Compound) -> bool,
) -> Vec<i32> {
    let shared = world.shared().clone();
    if reason != Release::Emergency
        && tuff_core::world::bees_stay_in_hive(&shared, world.dimension_info())
    {
        return Vec::new();
    }
    let Some(facing) = state.facing() else {
        return Vec::new();
    };
    let (sx, _, sz) = facing.step();
    let front = BlockPos::new(pos.x + sx, pos.y, pos.z + sz);
    let blocked = world
        .block(front)
        .is_some_and(|s| !s.collision_boxes().is_empty());
    if blocked && reason != Release::Emergency {
        return Vec::new();
    }
    let Some(hive) = world.raw_block_entity(pos) else {
        return Vec::new();
    };
    let flower = hive.get("flower_pos").cloned();
    let occupants: Vec<Tag> = hive
        .get_list("bees")
        .map(<[Tag]>::to_vec)
        .unwrap_or_default();
    if occupants.is_empty() {
        return Vec::new();
    }
    let mut kept = Vec::new();
    let mut released = Vec::new();
    let mut honey = 0;
    for occupant in occupants {
        if !occupant.as_compound().is_some_and(ready) {
            kept.push(occupant);
            continue;
        }
        let spawned = occupant.as_compound().and_then(|occupant| {
            bee_of(
                world,
                pos,
                occupant,
                flower.as_ref(),
                reason == Release::Exited,
                &mut honey,
            )
        });
        let Some(mut entity) = spawned else {
            kept.push(occupant);
            continue;
        };
        let size = entity.base().kind.size();
        let delta = if blocked {
            0.0
        } else {
            EXIT_DISTANCE + f64::from(size.width) / 2.0
        };
        entity.base_mut().position = [
            f64::from(pos.x) + 0.5 + delta * f64::from(sx),
            f64::from(pos.y) + 0.5 - f64::from(size.height) / 2.0,
            f64::from(pos.z) + 0.5 + delta * f64::from(sz),
        ];
        let id = entity.base().id;
        world.play_sound(SoundEvent::BlockBeehiveExit, pos, 1.0, 1.0, None);
        let context = Context {
            source: Some(Source::entity(id)),
            affected: world.block(pos),
        };
        world.game_event(GameEvent::BlockChange, pos.center(), context);
        world.add_entity(entity);
        released.push(id);
    }
    if !released.is_empty()
        && let Some(hive) = world.raw_block_entity_mut(pos)
    {
        hive.insert("bees", Tag::List(kept));
        world.block_entity_changed(pos);
    }
    if honey > 0 {
        add_honey(world, pos, honey);
    }
    released
}

/// The live bee an occupant entry describes, aged by its time in the hive.
fn bee_of(
    world: &mut dyn RegionWorld,
    hive: BlockPos,
    occupant: &Compound,
    flower: Option<&Tag>,
    deliver: bool,
    honey: &mut i32,
) -> Option<Box<dyn tuff_core::world::entity::RegionEntity>> {
    let mut stored = StoredEntity::from_storage(occupant.get_compound("entity_data")?.clone());
    let data = stored.nbt_mut();
    for key in IGNORED_TAGS {
        data.remove(key);
    }
    let inhabitor =
        tuff_vanilla_data::registry::EntityType::from_name(data.get_str("id").unwrap_or(""))
            .is_some_and(|kind| {
                tuff_datapack::tags::is(tag::entity_type::key::BEEHIVE_INHABITORS, kind)
            });
    if !inhabitor {
        stored.burn(EntityBurn::Unloadable);
        return None;
    }
    let ticks = occupant.get_int("ticks_in_hive").unwrap_or(0);
    age(data, ticks);
    data.insert("NoGravity", true);
    data.insert("CannotEnterHiveTicks", CANNOT_ENTER_TICKS);
    if deliver && data.get_bool("HasNectar").unwrap_or(false) {
        data.insert("HasNectar", false);
        *honey += if world.rng().gen_range(0..100) == 0 {
            2
        } else {
            1
        };
    }
    let center = hive.center();
    data.insert("Pos", Tag::List(center.map(Tag::Double).to_vec()));
    data.insert("hive_pos", Tag::IntArray(vec![hive.x, hive.y, hive.z]));
    if let Some(flower) = flower
        && !data.contains_key("flower_pos")
        && world.rng().r#gen::<f32>() < FLOWER_CHANCE
    {
        data.insert("flower_pos", flower.clone());
    }
    let id = world.shared().next_entity_id();
    let data = stored.into_spawn_data();
    world.shared().entity_types.load(&data, id)
}

/// Brings a bee's age and love time forward by the ticks it spent inside.
fn age(data: &mut Compound, ticks: i32) {
    if !data.get_bool("AgeLocked").unwrap_or(false) {
        let age = data.get_int("Age").unwrap_or(0);
        let aged = if age < 0 {
            (age.saturating_add(ticks)).min(0)
        } else {
            (age.saturating_sub(ticks)).max(0)
        };
        data.insert("Age", aged);
    }
    let love = data.get_int("InLove").unwrap_or(0);
    data.insert("InLove", love.saturating_sub(ticks).max(0));
}

fn is_hive(state: BlockState) -> bool {
    matches!(
        state.block(),
        tuff_vanilla_data::Block::Beehive | tuff_vanilla_data::Block::BeeNest
    )
}

fn add_honey(world: &mut dyn RegionWorld, pos: BlockPos, amount: i32) {
    let Some(state) = world.block(pos).filter(|s| is_hive(*s)) else {
        return;
    };
    let level: i32 = state
        .get("honey_level")
        .and_then(|l| l.parse().ok())
        .unwrap_or(0);
    let next = (level + amount).min(5);
    if next != level
        && let Some(raised) = state.with("honey_level", &next.to_string())
    {
        world.set_block_flags(pos, raised, 3);
    }
}

/// Whether a lit campfire smokes the hive at `pos` (directly below it, or
/// below the first solid block under it, within five blocks).
pub fn smoky(world: &dyn RegionWorld, pos: BlockPos) -> bool {
    let lit = |at: BlockPos| {
        world.block(at).is_some_and(|s| {
            matches!(
                s.block(),
                tuff_vanilla_data::Block::Campfire | tuff_vanilla_data::Block::SoulCampfire
            ) && s.get("lit") == Some("true")
        })
    };
    for i in 1..=5 {
        let below = BlockPos::new(pos.x, pos.y - i, pos.z);
        if lit(below) {
            return true;
        }
        if world
            .block(below)
            .is_some_and(|s| !s.collision_boxes().is_empty())
        {
            return lit(BlockPos::new(below.x, below.y - 1, below.z));
        }
    }
    false
}

/// Whether the hive at `pos` holds any bees.
pub fn has_bees(world: &dyn RegionWorld, pos: BlockPos) -> bool {
    world
        .raw_block_entity(pos)
        .and_then(|hive| hive.get_list("bees"))
        .is_some_and(|bees| !bees.is_empty())
}

/// Whether a fire burns within a block of the hive at `pos`.
pub fn fire_near(block: &dyn Fn(BlockPos) -> Option<BlockState>, pos: BlockPos) -> bool {
    (-1..=1).any(|dx| {
        (-1..=1).any(|dy| {
            (-1..=1).any(|dz| {
                block(BlockPos::new(pos.x + dx, pos.y + dy, pos.z + dz)).is_some_and(|s| {
                    matches!(
                        s.block(),
                        tuff_vanilla_data::Block::Fire | tuff_vanilla_data::Block::SoulFire
                    )
                })
            })
        })
    })
}

/// An explosion destroys the hive at `pos`: a blast set off by TNT, a
/// creeper, a wither or a TNT minecart lets the bees out; any blast angers
/// the bees around.
pub fn exploded(
    world: &mut dyn RegionWorld,
    pos: BlockPos,
    state: BlockState,
    cause: &tuff_core::world::explosion::ExplosionCause,
) {
    use tuff_vanilla_data::registry::EntityType as E;
    if !is_hive(state) {
        return;
    }
    let releasing = cause.igniter.is_some()
        || cause.culprit.as_ref().is_some_and(|c| {
            matches!(
                c.kind,
                E::Tnt | E::Creeper | E::WitherSkull | E::Wither | E::TntMinecart
            )
        });
    if releasing && has_bees(world, pos) {
        release(world, pos, state, Release::Emergency);
    }
    anger_nearby(world, pos);
}

/// Bees near `pos` that have no target turn on a player nearby.
pub fn anger_nearby(world: &mut dyn RegionWorld, pos: BlockPos) {
    let center = pos.center();
    let inside = |at: [f64; 3]| (0..3).all(|i| (at[i] - center[i]).abs() <= ANGER_BOX[i]);
    let players: Vec<i32> = world
        .seen_player_details()
        .into_iter()
        .filter(|p| inside(p.position))
        .map(|p| p.handle.id)
        .collect();
    if players.is_empty() {
        return;
    }
    for id in world.entity_ids() {
        let pick = players[world.rng().gen_range(0..players.len())];
        let Some(tracked) = world.entity_mut(id) else {
            continue;
        };
        let base = tracked.entity.base();
        if base.kind == tuff_vanilla_data::registry::EntityType::Bee && inside(base.position) {
            tracked
                .entity
                .notice(tuff_core::world::entity::Notice::Alert { attacker: pick });
        }
    }
}

/// A player took honey from the full hive at `pos`: unless a campfire
/// smokes it, the bees come out angry.
pub fn harvested(world: &mut dyn RegionWorld, pos: BlockPos, state: BlockState) {
    if !smoky(world, pos) && has_bees(world, pos) {
        release(world, pos, state, Release::Emergency);
        anger_nearby(world, pos);
    }
    if let Some(emptied) = state.with("honey_level", "0") {
        world.put_block(pos, emptied);
    }
}

/// Shears or a glass bottle on a hive full of honey: honeycomb or a honey
/// bottle, and bees that come out angry unless a campfire smokes the hive.
pub fn harvest(
    world: &mut dyn RegionWorld,
    place: &tuff_core::world::messages::Place,
    state: BlockState,
) -> Option<ActionOutcome> {
    let item = place.held?;
    if !full(state) || !matches!(item, Item::Shears | Item::GlassBottle) {
        return None;
    }
    let pos = place.target;
    let effect = if item == Item::Shears {
        world.play_sound(SoundEvent::BlockBeehiveShear, pos, 1.0, 1.0, None);
        let at = pos.center();
        for _ in 0..HONEYCOMBS {
            let velocity = [
                world.rng().r#gen::<f64>() * 0.2 - 0.1,
                0.2,
                world.rng().r#gen::<f64>() * 0.2 - 0.1,
            ];
            world.spawn_item_at(
                ItemStack::mint(Item::Honeycomb, 1, MintReason::Spawn),
                at,
                velocity,
                10,
            );
        }
        HandEffect::Damage(1)
    } else {
        world.play_sound(SoundEvent::ItemBottleFill, pos, 1.0, 1.0, None);
        HandEffect::Exchange(Item::HoneyBottle)
    };
    harvested(world, pos, state);
    world.game_event(
        GameEvent::Shear,
        pos.center(),
        Context::from(Source::player(place.player.id)),
    );
    Some(ActionOutcome::Used {
        off_hand: place.off_hand,
        item,
        effect,
    })
}

/// A dispenser fires shears or a glass bottle at the hive full of honey
/// at `target`: its honey comes off and its bees come out, not angry (no
/// player to blame).
pub fn dispensed_at(
    world: &mut dyn RegionWorld,
    target: BlockPos,
    item: Item,
) -> Option<DispenseOutcome> {
    if !matches!(item, Item::Shears | Item::GlassBottle) {
        return None;
    }
    let state = world.block(target).filter(|s| full(*s))?;
    let outcome = if item == Item::Shears {
        world.play_sound(SoundEvent::BlockBeehiveShear, target, 1.0, 1.0, None);
        let at = target.center();
        for _ in 0..HONEYCOMBS {
            let velocity = [
                world.rng().r#gen::<f64>() * 0.2 - 0.1,
                0.2,
                world.rng().r#gen::<f64>() * 0.2 - 0.1,
            ];
            world.spawn_item_at(
                ItemStack::mint(Item::Honeycomb, 1, MintReason::Spawn),
                at,
                velocity,
                10,
            );
        }
        DispenseOutcome::Worn(1)
    } else {
        DispenseOutcome::Exchanged(ItemStack::mint(Item::HoneyBottle, 1, MintReason::Spawn))
    };
    if let Some(emptied) = state.with("honey_level", "0") {
        world.put_block(target, emptied);
    }
    release(world, target, state, Release::Released);
    Some(outcome)
}

/// A hive or nest full of honey.
pub fn full(state: BlockState) -> bool {
    matches!(state.block(), Block::Beehive | Block::BeeNest)
        && state.get("honey_level") == Some(MAX_HONEY)
}

/// A player broke the hive at `pos` with `tool`: without Silk Touch its
/// bees come out and turn on the players around.
pub fn broken(
    world: &mut dyn RegionWorld,
    action: &tuff_core::world::region_api::PlayerBlockAction,
    tool: Option<tuff_item::item::AnyItem<'_>>,
) {
    if !is_hive(action.state) {
        return;
    }
    let creative = world
        .seen_player_details()
        .iter()
        .any(|p| p.handle.id == action.player && p.looks.creative);
    let silk = tool.is_some_and(|tool| {
        tool.enchantments().is_some_and(|e| {
            tuff_datapack::tags::with(|tags| {
                e.entries().iter().any(|(id, _)| {
                    tags.contains(
                        "minecraft:enchantment",
                        "minecraft:prevents_bee_spawns_when_mining",
                        id,
                    )
                })
            })
        })
    });
    if creative || silk || !has_bees(world, action.pos) {
        return;
    }
    release(world, action.pos, action.state, Release::Emergency);
    anger_nearby(world, action.pos);
}

/// Settles the bees entering hives, then ages every hive's occupants and
/// lets those whose time is up out.
pub fn tick(world: &mut dyn RegionWorld) {
    let tick = world.tick();
    let entering = std::mem::take(&mut world.state().get::<Entries>().0);
    for (bee, hive) in entering {
        enter(world, bee, hive);
    }
    let hives: Vec<BlockPos> = {
        let pois = world.pois();
        pois.own_chunks()
            .flat_map(|chunk| pois.records(chunk).iter())
            .filter(|r| r.kind == tuff_vanilla_data::registry::PointOfInterestType::Beehive)
            .map(|r| r.pos)
            .collect()
    };
    for pos in hives {
        let Some(state) = world.block(pos).filter(|s| is_hive(*s)) else {
            continue;
        };
        let Some(hive) = world.raw_block_entity_mut(pos) else {
            continue;
        };
        let Some(Tag::List(bees)) = hive.get_mut("bees") else {
            continue;
        };
        if bees.is_empty() {
            continue;
        }
        let mut due = false;
        for occupant in bees.iter_mut() {
            if let Tag::Compound(occupant) = occupant {
                let ticks = occupant
                    .get_int("ticks_in_hive")
                    .unwrap_or(0)
                    .saturating_add(1);
                occupant.insert("ticks_in_hive", ticks);
                due |= ticks > occupant.get_int("min_ticks_in_hive").unwrap_or(0);
            }
        }
        if tick.is_multiple_of(20) {
            if fire_near(&|p| world.block(p), pos) {
                release(world, pos, state, Release::Emergency);
                continue;
            }
            world.block_entity_changed(pos);
        }
        if due {
            release_where(world, pos, state, Release::Exited, &|occupant| {
                occupant.get_int("ticks_in_hive").unwrap_or(0)
                    > occupant.get_int("min_ticks_in_hive").unwrap_or(0)
            });
        }
    }
}

/// The bee `id` goes into the hive at `pos` if it has room.
fn enter(world: &mut dyn RegionWorld, id: i32, pos: BlockPos) {
    if world.block(pos).is_none_or(|s| !is_hive(s)) {
        return;
    }
    let Some(hive) = world.raw_block_entity(pos) else {
        return;
    };
    if hive.get_list("bees").map_or(0, <[Tag]>::len) >= CAPACITY {
        return;
    }
    let Some(tracked) = world.entity(id) else {
        return;
    };
    if tracked.entity.base().kind != tuff_vanilla_data::registry::EntityType::Bee {
        return;
    }
    let Some(mut stored) = world.take_entity(id) else {
        return;
    };
    let data = stored.nbt_mut();
    for key in IGNORED_TAGS {
        data.remove(key);
    }
    data.insert("TicksSincePollination", 0);
    let nectar = data.get_bool("HasNectar").unwrap_or(false);
    let flower = data.get("flower_pos").cloned();
    let occupant = Compound::new()
        .with("entity_data", stored.into_storage())
        .with("ticks_in_hive", 0)
        .with(
            "min_ticks_in_hive",
            if nectar {
                MIN_TICKS_NECTAR
            } else {
                MIN_TICKS_EMPTY
            },
        );
    if let Some(hive) = world.raw_block_entity_mut(pos) {
        if let Some(Tag::List(bees)) = hive.get_mut("bees") {
            bees.push(Tag::Compound(occupant));
        } else {
            hive.insert("bees", Tag::List(vec![Tag::Compound(occupant)]));
        }
        if let Some(flower) = flower
            && !hive.contains_key("flower_pos")
        {
            hive.insert("flower_pos", flower);
        }
    }
    world.block_entity_changed(pos);
    world.play_sound(SoundEvent::BlockBeehiveEnter, pos, 1.0, 1.0, None);
    let context = Context {
        source: Some(Source::entity(id)),
        affected: world.block(pos),
    };
    world.game_event(GameEvent::BlockChange, pos.center(), context);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn time_in_the_hive_grows_babies_and_cools_adults() {
        let mut baby = Compound::new().with("Age", -1000).with("InLove", 300);
        age(&mut baby, 600);
        assert_eq!(baby.get_int("Age"), Some(-400));
        assert_eq!(baby.get_int("InLove"), Some(0));
        let mut adult = Compound::new().with("Age", 400);
        age(&mut adult, 600);
        assert_eq!(adult.get_int("Age"), Some(0));
        let mut locked = Compound::new().with("Age", -1000).with("AgeLocked", true);
        age(&mut locked, 600);
        assert_eq!(locked.get_int("Age"), Some(-1000));
    }
}
