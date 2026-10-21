//! Sculk catalysts: a mob dying nearby feeds its experience to the closest
//! catalyst as charge cursors, which wander over sculk and veins turning
//! stone to sculk and growing sensors and shriekers. Cursors are saved
//! with the block entity.

use std::collections::BTreeMap;

use crate::growth::DIRECTIONS;
use tuff_core::world::features::{SculkCursor as Cursor, SculkEvent as Event};
use tuff_core::world::redstone::UPDATE_ALL;
use tuff_core::world::region_api::RegionWorld;
use tuff_nbt::{Compound, Tag};
use tuff_vanilla_data::Block;
use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::registry::SoundEvent;
use tuff_world_model::BlockPos;
use tuff_world_model::pos::ChunkPos;

use crate::growth::{Features, GrowthLevelExt};

type Totals = BTreeMap<(i32, i32, i32), (i32, Option<Vec<Facing>>)>;

pub const ID: &str = "minecraft:sculk_catalyst";

const LISTEN_RADIUS: f64 = 8.0;
const MAX_CURSORS: usize = 32;
const MAX_CHARGE: i32 = 1000;
const PULSE_TICKS: u64 = 8;
const PARTICLES_SCULK_CHARGE: i32 = 3006;

pub fn is_catalyst(nbt: &Compound) -> bool {
    nbt.get_str("id") == Some(ID)
}

#[derive(Default)]
pub struct Catalysts {
    cursors: BTreeMap<(i32, i32, i32), Vec<Cursor>>,
    pulses: BTreeMap<(i32, i32, i32), u64>,
}

fn key(pos: BlockPos) -> (i32, i32, i32) {
    (pos.y, pos.z, pos.x)
}

fn unkey(key: (i32, i32, i32)) -> BlockPos {
    BlockPos::new(key.2, key.0, key.1)
}

fn read_cursors(nbt: &Compound) -> Vec<Cursor> {
    let mut found = Vec::new();
    for tag in nbt.get_list("cursors").into_iter().flatten() {
        let Some(entry) = tag.as_compound() else {
            continue;
        };
        let Some([x, y, z]) = entry
            .get_int_array("pos")
            .and_then(|a| <[i32; 3]>::try_from(a).ok())
        else {
            continue;
        };
        let mut cursor = Cursor::new(
            BlockPos::new(x, y, z),
            entry.get_int("charge").unwrap_or(0).clamp(0, MAX_CHARGE),
        );
        cursor.decay_delay = entry.get_int("decay_delay").unwrap_or(1).clamp(0, 1);
        cursor.update_delay = entry.get_int("update_delay").unwrap_or(0).max(0);
        cursor.facings = entry.get_list("facings").map(|list| {
            list.iter()
                .filter_map(Tag::as_str)
                .filter_map(|name| DIRECTIONS.into_iter().find(|d| d.name() == name))
                .collect()
        });
        if found.len() < MAX_CURSORS {
            found.push(cursor);
        }
    }
    found
}

fn write_cursors(nbt: &mut Compound, cursors: &[Cursor]) {
    if cursors.is_empty() {
        nbt.remove("cursors");
        return;
    }
    let list: Vec<Compound> = cursors
        .iter()
        .map(|c| {
            let mut entry = Compound::new()
                .with("pos", vec![c.pos.x, c.pos.y, c.pos.z])
                .with("charge", c.charge)
                .with("decay_delay", c.decay_delay)
                .with("update_delay", c.update_delay);
            if let Some(facings) = &c.facings {
                entry.insert(
                    "facings",
                    Tag::List(facings.iter().map(|f| Tag::from(f.name())).collect()),
                );
            }
            entry
        })
        .collect();
    nbt.insert("cursors", list);
}

fn add_cursors(cursors: &mut Vec<Cursor>, pos: BlockPos, mut charge: i32) {
    while charge > 0 {
        let now = charge.min(MAX_CHARGE);
        if cursors.len() < MAX_CURSORS {
            cursors.push(Cursor::new(pos, now));
        }
        charge -= now;
    }
}

fn catalyst_at(world: &dyn RegionWorld, pos: BlockPos) -> bool {
    world
        .block(pos)
        .is_some_and(|s| s.block() == Block::SculkCatalyst)
        && world.raw_block_entity(pos).is_some_and(is_catalyst)
}

pub fn mob_experience(world: &mut dyn RegionWorld, position: [f64; 3], amount: i32) -> bool {
    if amount <= 0 {
        return false;
    }
    let distance = |pos: BlockPos| {
        let dx = f64::from(pos.x) + 0.5 - position[0];
        let dy = f64::from(pos.y) + 0.5 - position[1];
        let dz = f64::from(pos.z) + 0.5 - position[2];
        dx * dx + dy * dy + dz * dz
    };
    let mut nearby: Vec<BlockPos> = world
        .raw_block_entities_where(&is_catalyst)
        .into_iter()
        .filter(|&pos| distance(pos) <= LISTEN_RADIUS * LISTEN_RADIUS && catalyst_at(world, pos))
        .collect();
    nearby.sort_by(|a, b| {
        distance(*a)
            .total_cmp(&distance(*b))
            .then(key(*a).cmp(&key(*b)))
    });
    let Some(&pos) = nearby.first() else {
        return false;
    };
    let start = BlockPos::new(
        position[0].floor() as i32,
        (position[1] + 0.5).floor() as i32,
        position[2].floor() as i32,
    );
    let catalysts = world.state().get::<Catalysts>();
    let mut cursors = catalysts.cursors.remove(&key(pos)).unwrap_or_default();
    if cursors.is_empty()
        && let Some(nbt) = world.raw_block_entity(pos)
    {
        cursors = read_cursors(nbt);
    }
    add_cursors(&mut cursors, start, amount);
    world
        .state()
        .get::<Catalysts>()
        .cursors
        .insert(key(pos), cursors);
    bloom(world, pos);
    true
}

fn bloom(world: &mut dyn RegionWorld, pos: BlockPos) {
    let Some(state) = world.block(pos) else {
        return;
    };
    if let Some(lit) = state.with("bloom", "true") {
        world.set_block_flags(pos, lit, UPDATE_ALL);
    }
    let now = world.tick();
    world
        .state()
        .get::<Catalysts>()
        .pulses
        .insert(key(pos), now + PULSE_TICKS);
    let pitch = 0.6 + world.random_float() * 0.4;
    world.play_sound(SoundEvent::BlockSculkCatalystBloom, pos, 2.0, pitch, None);
}

fn pack(faces: &[Facing]) -> i32 {
    faces
        .iter()
        .map(|f| 1 << DIRECTIONS.iter().position(|d| d == f).unwrap_or(0))
        .fold(0, |a, b| a | b)
}

pub fn tick(world: &mut dyn RegionWorld, positions: &[BlockPos]) {
    let now = world.tick();
    if world.tick().is_multiple_of(20) {
        for &pos in positions {
            let loaded = world
                .state()
                .get::<Catalysts>()
                .cursors
                .contains_key(&key(pos));
            let lit = world
                .block(pos)
                .is_some_and(|s| s.get("bloom") == Some("true"));
            let pending = world
                .state()
                .get::<Catalysts>()
                .pulses
                .contains_key(&key(pos));
            if lit && !pending {
                world
                    .state()
                    .get::<Catalysts>()
                    .pulses
                    .insert(key(pos), now + PULSE_TICKS);
            }
            if !loaded && let Some(nbt) = world.raw_block_entity(pos) {
                let cursors = read_cursors(nbt);
                if !cursors.is_empty() {
                    world
                        .state()
                        .get::<Catalysts>()
                        .cursors
                        .insert(key(pos), cursors);
                }
            }
        }
    }
    let due: Vec<_> = world
        .state()
        .get::<Catalysts>()
        .pulses
        .iter()
        .filter(|(_, until)| **until <= now)
        .map(|(k, _)| *k)
        .collect();
    for k in due {
        world.state().get::<Catalysts>().pulses.remove(&k);
        let pos = unkey(k);
        if let Some(state) = world.block(pos)
            && state.block() == Block::SculkCatalyst
            && let Some(off) = state.with("bloom", "false")
        {
            world.set_block_flags(pos, off, UPDATE_ALL);
        }
    }
    let active: Vec<_> = world
        .state()
        .get::<Catalysts>()
        .cursors
        .keys()
        .copied()
        .collect();
    for k in active {
        let pos = unkey(k);
        let mut cursors = world
            .state()
            .get::<Catalysts>()
            .cursors
            .remove(&k)
            .unwrap_or_default();
        if !catalyst_at(world, pos) {
            continue;
        }
        let before = cursors.clone();
        update_cursors(world, pos, &mut cursors);
        if cursors != before
            && let Some(nbt) = world.raw_block_entity_mut(pos)
        {
            write_cursors(nbt, &cursors);
            world.mark_blocks_dirty(pos);
        }
        if !cursors.is_empty() {
            world.state().get::<Catalysts>().cursors.insert(k, cursors);
        }
    }
}

fn update_cursors(world: &mut dyn RegionWorld, origin: BlockPos, cursors: &mut Vec<Cursor>) {
    if cursors.is_empty() {
        return;
    }
    let mut groups: Vec<(ChunkPos, Vec<Cursor>)> = Vec::new();
    for cursor in cursors.drain(..) {
        if cursor.is_unreasonable(origin) {
            continue;
        }
        let chunk = ChunkPos::of_block(cursor.pos);
        match groups.iter_mut().find(|(c, _)| *c == chunk) {
            Some((_, group)) => group.push(cursor),
            None => groups.push((chunk, vec![cursor])),
        }
    }
    let mut events = Vec::new();
    let mut updated = Vec::new();
    for (_, mut group) in groups {
        let center = group[0].pos;
        let ran = world.with_features(center, &mut |features: &mut Features| {
            features
                .region
                .sculk_update(&mut group, origin, features.random, &mut events);
            true
        });
        if !ran {
            events.clear();
        }
        updated.extend(group);
    }
    for event in events {
        match event {
            Event::Spread(pos) => {
                world.play_sound(SoundEvent::BlockSculkSpread, pos, 1.0, 1.0, None);
            }
            Event::Grew(pos, block) => {
                let sound = if block == Block::SculkShrieker {
                    SoundEvent::BlockSculkShriekerPlace
                } else {
                    SoundEvent::BlockSculkSensorPlace
                };
                world.play_sound(sound, pos, 1.0, 1.0, None);
            }
        }
    }
    merge(world, updated, cursors);
}

fn merge(world: &mut dyn RegionWorld, updated: Vec<Cursor>, out: &mut Vec<Cursor>) {
    let mut totals: Totals = BTreeMap::new();
    let mut at: BTreeMap<(i32, i32, i32), usize> = BTreeMap::new();
    for cursor in updated {
        if cursor.charge <= 0 {
            world.level_event(PARTICLES_SCULK_CHARGE, cursor.pos, 0);
            continue;
        }
        let k = key(cursor.pos);
        totals.entry(k).or_insert((0, None)).0 += cursor.charge;
        match at.get(&k).copied() {
            None => {
                at.insert(k, out.len());
                totals.get_mut(&k).expect("just inserted").1 = cursor.facings.clone();
                out.push(cursor);
            }
            Some(index) if out[index].charge + cursor.charge <= MAX_CHARGE => {
                let existing = &mut out[index];
                existing.charge += cursor.charge;
                existing.update_delay = existing.update_delay.min(cursor.update_delay);
            }
            Some(index) => {
                if cursor.charge < out[index].charge {
                    at.insert(k, out.len());
                    totals.get_mut(&k).expect("present").1 = cursor.facings.clone();
                }
                out.push(cursor);
            }
        }
    }
    for (k, (charge, faces)) in totals {
        if let Some(faces) = faces.filter(|_| charge > 0) {
            let particles = ((f64::from(charge)).ln_1p() / 2.3) as i32 + 1;
            world.level_event(
                PARTICLES_SCULK_CHARGE,
                unkey(k),
                (particles << 6) + pack(&faces),
            );
        }
    }
}
