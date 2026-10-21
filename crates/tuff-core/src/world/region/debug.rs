//! World debug feeds: values kept in sync for subscribed chunk viewers,
//! and one-off events. Idle (one `Arc` clone a tick) while nobody
//! subscribes.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use bytes::Bytes;
use tuff_vanilla_data::registry::DebugSubscription;
use tuff_world_model::BlockPos;
use tuff_world_model::pos::ChunkPos;

use super::RegionActor;
use tuff_nbt::{Compound, Tag};
use tuff_vanilla_data::structure::TerrainAdaptation;
use tuff_vanilla_data::{Block, BlockState};

use crate::protocol::play::{self, DebugBox, DebugStructure, DebugUpdate, DebugValue};
use crate::world::debug_feeds::{ENTITY_FEEDS, SharedSubscribers, bit};
use crate::world::framed;
use crate::world::messages::{PlayerEvent, PlayerHandle, PlayerId};
use crate::world::poi::NO_VILLAGE;

/// Feeds whose values are kept per block (or per chunk, for
/// structures), chunk by chunk.
const BLOCK_FEEDS: [DebugSubscription; 5] = [
    DebugSubscription::Pois,
    DebugSubscription::VillageSections,
    DebugSubscription::BeeHives,
    DebugSubscription::GameEventListeners,
    DebugSubscription::Structures,
];

#[derive(Default)]
pub(super) struct DebugFeeds {
    subscribers: SharedSubscribers,
    synced: [HashMap<ChunkPos, Synced>; BLOCK_FEEDS.len()],
    /// What subscribed viewers were last sent of each entity feed, by
    /// entity and the feed's index in `ENTITY_FEEDS`.
    entities: HashMap<(i32, u8), EntitySynced>,
}

#[derive(Default)]
struct EntitySynced {
    value: Option<DebugValue>,
    players: HashSet<PlayerId>,
}

/// What a chunk's subscribed viewers were last sent of one feed.
#[derive(Default)]
struct Synced {
    values: HashMap<BlockPos, DebugValue>,
    players: HashSet<PlayerId>,
}

fn block_value(pos: BlockPos, feed: DebugSubscription, value: Option<DebugValue>) -> Bytes {
    let update = DebugUpdate {
        subscription: feed as i32,
        value,
    };
    if feed == DebugSubscription::Structures {
        return framed(&play::DebugChunkValue {
            x: pos.x,
            z: pos.z,
            update,
        });
    }
    framed(&play::DebugBlockValue { pos, update })
}

/// Listener radius of the block entities that listen for game events.
fn listener_radius(block: Block) -> Option<i32> {
    match block {
        Block::SculkSensor | Block::SculkShrieker | Block::SculkCatalyst => Some(8),
        Block::CalibratedSculkSensor => Some(16),
        _ => None,
    }
}

fn read_box(bb: &[i32]) -> Option<DebugBox> {
    let [x0, y0, z0, x1, y1, z1] = <[i32; 6]>::try_from(bb).ok()?;
    Some(DebugBox {
        min: BlockPos::new(x0, y0, z0),
        max: BlockPos::new(x1, y1, z1),
    })
}

/// The structures starting in a chunk, from its saved `structures` data:
/// each start's box (all pieces, grown by 12 for structures that adapt
/// the terrain) and its pieces' boxes.
pub(super) fn structure_starts(extra: &Compound) -> Vec<DebugStructure> {
    let Some(starts) = extra
        .get_compound("structures")
        .and_then(|s| s.get_compound("starts"))
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (id, start) in starts.iter() {
        let Some(start) = start.as_compound() else {
            continue;
        };
        let pieces: Vec<DebugBox> = start
            .get_list("Children")
            .unwrap_or(&[])
            .iter()
            .filter_map(Tag::as_compound)
            .filter_map(|piece| read_box(piece.get_int_array("BB")?))
            .collect();
        let Some(first) = pieces.first() else {
            continue;
        };
        let mut bounds = pieces.iter().fold(*first, |b, p| DebugBox {
            min: BlockPos::new(
                b.min.x.min(p.min.x),
                b.min.y.min(p.min.y),
                b.min.z.min(p.min.z),
            ),
            max: BlockPos::new(
                b.max.x.max(p.max.x),
                b.max.y.max(p.max.y),
                b.max.z.max(p.max.z),
            ),
        });
        let adapts = tuff_vanilla_data::structure::Structure::get(id)
            .is_some_and(|s| s.terrain_adaptation != TerrainAdaptation::None);
        if adapts {
            bounds.min = BlockPos::new(bounds.min.x - 12, bounds.min.y - 12, bounds.min.z - 12);
            bounds.max = BlockPos::new(bounds.max.x + 12, bounds.max.y + 12, bounds.max.z + 12);
        }
        out.push(DebugStructure { bounds, pieces });
    }
    out
}

fn section_center(section: [i32; 3]) -> BlockPos {
    BlockPos::new(
        section[0] * 16 + 8,
        section[1] * 16 + 8,
        section[2] * 16 + 8,
    )
}

impl RegionActor {
    /// Reads who subscribes to which feed, for this tick's events.
    pub(super) fn refresh_debug_subscribers(&mut self) {
        let current = self.shared.debug_subscribers.borrow();
        if !Arc::ptr_eq(&current, &self.debug.subscribers) {
            self.debug.subscribers = current.clone();
        }
    }

    /// Sends subscribed viewers what changed in the per-block feeds since
    /// last tick, and everything to viewers new to a chunk or feed.
    pub(super) fn sync_debug_feeds(&mut self) {
        self.sync_debug_entities();
        self.debug_block_intersections();
        let subscribers = self.debug.subscribers.clone();
        for (slot, feed) in BLOCK_FEEDS.into_iter().enumerate() {
            let mut synced = std::mem::take(&mut self.debug.synced[slot]);
            if !subscribers.any(feed) {
                continue;
            }
            synced.retain(|chunk, _| self.columns.contains_key(chunk));
            let chunks: Vec<ChunkPos> = self
                .columns
                .keys()
                .copied()
                .filter(|c| self.pos.contains(*c))
                .collect();
            for chunk in chunks {
                let viewers: Vec<PlayerHandle> = self.columns[&chunk]
                    .viewers
                    .iter()
                    .filter(|(id, _)| subscribers.wants(**id, feed))
                    .map(|(_, handle)| handle.clone())
                    .collect();
                if viewers.is_empty() {
                    synced.remove(&chunk);
                    continue;
                }
                let current = match feed {
                    DebugSubscription::Pois => self.debug_pois(chunk),
                    DebugSubscription::VillageSections => self.debug_village_sections(chunk),
                    DebugSubscription::Structures => self.debug_structures(chunk),
                    _ => self.debug_block_entities(chunk, feed),
                };
                let entry = synced.entry(chunk).or_default();
                if current.is_empty() && entry.values.is_empty() {
                    entry.players = viewers.iter().map(|v| v.id).collect();
                    continue;
                }
                let mut changes = Vec::new();
                for (pos, value) in &current {
                    if entry.values.get(pos) != Some(value) {
                        changes.push(block_value(*pos, feed, Some(value.clone())));
                    }
                }
                for pos in entry.values.keys() {
                    if !current.contains_key(pos) {
                        changes.push(block_value(*pos, feed, None));
                    }
                }
                let changes: crate::world::messages::Packets = changes.into();
                let mut full = None;
                for viewer in &viewers {
                    let packets = if entry.players.contains(&viewer.id) {
                        if changes.is_empty() {
                            continue;
                        }
                        changes.clone()
                    } else {
                        full.get_or_insert_with(|| {
                            current
                                .iter()
                                .map(|(pos, value)| block_value(*pos, feed, Some(value.clone())))
                                .collect::<crate::world::messages::Packets>()
                        })
                        .clone()
                    };
                    if !packets.is_empty() {
                        viewer.send(PlayerEvent::ChunkPackets {
                            dimension: self.dimension,
                            pos: chunk,
                            packets,
                        });
                    }
                }
                entry.players = viewers.iter().map(|v| v.id).collect();
                entry.values = current;
            }
            self.debug.synced[slot] = synced;
        }
    }

    fn debug_structures(&self, chunk: ChunkPos) -> HashMap<BlockPos, DebugValue> {
        let starts = structure_starts(&self.columns[&chunk].meta.extra);
        if starts.is_empty() {
            return HashMap::new();
        }
        HashMap::from([(
            BlockPos::new(chunk.x, 0, chunk.z),
            DebugValue::Structures(starts),
        )])
    }

    /// Bee hives or game event listeners among the chunk's block entities.
    fn debug_block_entities(
        &self,
        chunk: ChunkPos,
        feed: DebugSubscription,
    ) -> HashMap<BlockPos, DebugValue> {
        let column = &self.columns[&chunk];
        let positions = column
            .meta
            .block_entities
            .keys()
            .chain(column.meta.raw_block_entities.keys());
        let mut out = HashMap::new();
        for pos in positions {
            let state = column.chunk.get(pos.x, pos.y, pos.z);
            let block = state.block();
            let value = if feed == DebugSubscription::GameEventListeners {
                listener_radius(block).map(|radius| DebugValue::GameEventListener { radius })
            } else if matches!(block, Block::Beehive | Block::BeeNest) {
                let occupants = column
                    .meta
                    .raw_block_entities
                    .get(pos)
                    .and_then(|nbt| nbt.get_list("bees"))
                    .map_or(0, |bees| bees.len() as i32);
                Some(DebugValue::Hive {
                    block: block.id() as i32,
                    occupants,
                    honey_level: state
                        .get("honey_level")
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(0),
                    sedated: self.smoky(*pos),
                })
            } else {
                None
            };
            if let Some(value) = value {
                out.insert(*pos, value);
            }
        }
        out
    }

    /// Whether a lit campfire smokes up through `pos`.
    fn smoky(&self, pos: BlockPos) -> bool {
        let lit = |state: BlockState| {
            matches!(state.block(), Block::Campfire | Block::SoulCampfire)
                && state.get("lit") == Some("true")
        };
        for depth in 1..=5 {
            let Some(state) = self.block(BlockPos::new(pos.x, pos.y - depth, pos.z)) else {
                return false;
            };
            if lit(state) {
                return true;
            }
            if !state.collision_boxes().is_empty() {
                return self
                    .block(BlockPos::new(pos.x, pos.y - depth - 1, pos.z))
                    .is_some_and(lit);
            }
        }
        false
    }

    fn debug_pois(&self, chunk: ChunkPos) -> HashMap<BlockPos, DebugValue> {
        self.chunk_io
            .pois
            .records(chunk)
            .iter()
            .map(|r| {
                let value = DebugValue::Poi {
                    pos: r.pos,
                    kind: r.kind.id() as i32,
                    free_tickets: r.free as i32,
                };
                (r.pos, value)
            })
            .collect()
    }

    /// The village sections of `chunk` within one section of a point.
    fn debug_village_sections(&self, chunk: ChunkPos) -> HashMap<BlockPos, DebugValue> {
        let mut sections = HashSet::new();
        for dx in -1..=1 {
            for dz in -1..=1 {
                let near = ChunkPos::new(chunk.x + dx, chunk.z + dz);
                for record in self.chunk_io.pois.records(near) {
                    let [x, y, z] = [record.pos.x >> 4, record.pos.y >> 4, record.pos.z >> 4];
                    for ox in -1..=1 {
                        for oz in -1..=1 {
                            if x + ox != chunk.x || z + oz != chunk.z {
                                continue;
                            }
                            for oy in -1..=1 {
                                sections.insert([x + ox, y + oy, z + oz]);
                            }
                        }
                    }
                }
            }
        }
        sections
            .into_iter()
            .map(section_center)
            .filter(|center| self.chunk_io.pois.sections_to_village(*center) < NO_VILLAGE)
            .map(|center| (center, DebugValue::Unit))
            .collect()
    }

    /// Sends a one-off `feed` event at `pos` to the subscribed viewers of
    /// its chunk.
    pub(super) fn debug_event(&self, feed: DebugSubscription, pos: BlockPos, value: DebugValue) {
        if !self.debug.subscribers.any(feed) {
            return;
        }
        let chunk = ChunkPos::of_block(pos);
        let Some(column) = self.columns.get(&chunk) else {
            return;
        };
        let mut packets = None;
        for (id, viewer) in &column.viewers {
            if !self.debug.subscribers.wants(*id, feed) {
                continue;
            }
            let packets = packets
                .get_or_insert_with(|| {
                    crate::world::packets([framed(&play::DebugEvent {
                        subscription: feed as i32,
                        value: value.clone(),
                    })])
                })
                .clone();
            viewer.send(PlayerEvent::ChunkPackets {
                dimension: self.dimension,
                pos: chunk,
                packets,
            });
        }
    }

    /// Polls each entity's value of each subscribed entity feed and sends
    /// changes, or the whole value to viewers new to it, to the subscribed
    /// viewers of the entity's chunk. Sessions show it once they show the
    /// entity.
    fn sync_debug_entities(&mut self) {
        let subscribers = self.debug.subscribers.clone();
        let mask = ENTITY_FEEDS.iter().fold(0, |m, f| m | bit(*f));
        if !subscribers.any_of(mask) {
            self.debug.entities.clear();
            return;
        }
        let tick = self.sched.tick;
        let mut synced = std::mem::take(&mut self.debug.entities);
        let mut live = HashSet::new();
        for (id, tracked) in &self.ents.entities {
            let entity = &tracked.entity;
            let chunk = ChunkPos::of_position(entity.base().position);
            let Some(column) = self.columns.get(&chunk) else {
                continue;
            };
            for (slot, feed) in ENTITY_FEEDS.into_iter().enumerate() {
                if !subscribers.any(feed) {
                    continue;
                }
                let key = (*id, slot as u8);
                let viewers: Vec<&PlayerHandle> = column
                    .viewers
                    .iter()
                    .filter(|(p, _)| subscribers.wants(**p, feed))
                    .map(|(_, h)| h)
                    .collect();
                let value = entity.debug_value(feed, tick);
                let entry = synced.entry(key).or_default();
                live.insert(key);
                let changed = entry.value != value;
                let mut packet = None;
                for viewer in &viewers {
                    let new = !entry.players.contains(&viewer.id);
                    if !changed && !(new && value.is_some()) {
                        continue;
                    }
                    let packet = packet
                        .get_or_insert_with(|| {
                            framed(&play::DebugEntityValue {
                                entity: *id,
                                update: DebugUpdate {
                                    subscription: feed as i32,
                                    value: value.clone(),
                                },
                            })
                        })
                        .clone();
                    viewer.send(PlayerEvent::DebugEntityValue {
                        entity: *id,
                        feed: feed as u8,
                        packet,
                    });
                }
                entry.players = viewers.iter().map(|v| v.id).collect();
                entry.value = value;
            }
        }
        synced.retain(|key, _| live.contains(key));
        self.debug.entities = synced;
    }

    /// The blocks each entity's box is in, sent every tick as the client
    /// fades them out.
    fn debug_block_intersections(&self) {
        let feed = DebugSubscription::EntityBlockIntersections;
        if !self.debug.subscribers.any(feed) {
            return;
        }
        let boxes = self
            .ents
            .entities
            .values()
            .map(|tracked| tracked.entity.bounds())
            .chain(
                self.ents
                    .presence
                    .values()
                    .filter(|presence| presence.is_physical())
                    .map(|presence| super::redstone::player_bounds(presence.position)),
            );
        let mut seen = HashSet::new();
        for b in boxes {
            let lo = |v: f64| (v + 1e-5).floor() as i32;
            let hi = |v: f64| (v - 1e-5).floor() as i32;
            for x in lo(b[0])..=hi(b[3]) {
                for y in lo(b[1])..=hi(b[4]) {
                    for z in lo(b[2])..=hi(b[5]) {
                        let pos = BlockPos::new(x, y, z);
                        if !self.owns(pos) || !seen.insert(pos) {
                            continue;
                        }
                        let Some(state) = self.block(pos) else {
                            continue;
                        };
                        let kind = if tuff_world_model::chunk::is_air(state) {
                            2
                        } else if !state.fluid_state().is_empty() {
                            1
                        } else {
                            0
                        };
                        self.debug_block_value(feed, pos, DebugValue::BlockIntersection(kind));
                    }
                }
            }
        }
    }

    /// Shows subscribed viewers the orientation an experimental wire
    /// update used at `pos`.
    pub(super) fn debug_wire_orientation(
        &self,
        pos: BlockPos,
        orientation: crate::world::redstone::Orientation,
    ) {
        let feed = DebugSubscription::RedstoneWireOrientations;
        if self.debug.subscribers.any(feed) {
            self.debug_block_value(feed, pos, DebugValue::Orientation(orientation.index()));
        }
    }

    /// Sends a block value outside any kept state (it expires on the
    /// client) to the subscribed viewers of its chunk.
    fn debug_block_value(&self, feed: DebugSubscription, pos: BlockPos, value: DebugValue) {
        let chunk = ChunkPos::of_block(pos);
        let Some(column) = self.columns.get(&chunk) else {
            return;
        };
        let mut packets = None;
        for (id, viewer) in &column.viewers {
            if !self.debug.subscribers.wants(*id, feed) {
                continue;
            }
            let packets = packets
                .get_or_insert_with(|| {
                    crate::world::packets([block_value(pos, feed, Some(value.clone()))])
                })
                .clone();
            viewer.send(PlayerEvent::ChunkPackets {
                dimension: self.dimension,
                pos: chunk,
                packets,
            });
        }
    }
}
