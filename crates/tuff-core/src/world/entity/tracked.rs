use super::*;

fn pose(entity: &dyn RegionEntity) -> Pose {
    let base = entity.base();
    Pose {
        position: base.position,
        velocity: base.velocity,
        yaw: base.yaw,
        pitch: base.pitch,
        on_ground: base.on_ground,
        riding: base.vehicle.is_some(),
    }
}

fn position_sync(base: &EntityBase, position: [f64; 3]) -> Bytes {
    framed(&play::EntityPositionSync {
        entity_id: base.id,
        position,
        yaw: base.yaw,
        pitch: base.pitch,
        on_ground: base.on_ground,
    })
}

fn position_sync_path(base: &EntityBase, path: &[play::PositionStep]) -> Bytes {
    if path.is_empty() {
        return position_sync(base, base.position);
    }
    framed(&play::EntityPositionSyncStepped {
        entity_id: base.id,
        steps: path,
        yaw: base.yaw,
        pitch: base.pitch,
        on_ground: base.on_ground,
    })
}

/// A region's entity with what its viewers need.
#[derive(Debug)]
pub struct TrackedEntity {
    pub entity: Box<dyn RegionEntity>,
    /// Orders this entity's reports to viewers; see `world::tracking`.
    pub seq: u64,
    spawn: Packets,
    pub sync: MovementSync,
    /// The `seq` of the last report whose changes viewers could follow; a
    /// report sent since breaks the chain and the next one is full.
    moves_seq: u64,
    head: i8,
    /// Ticks since it was loaded into this region.
    pub ticks: i64,
    /// Path search nodes it has saved up; refilled each tick by its share
    /// of the region's budget.
    pub path_credit: u32,
    /// The data values viewers last got, so unchanged ones are not resent.
    sent: Vec<(u8, MetadataValue)>,
    sent_attributes: Vec<(tuff_vanilla_data::registry::Attribute, f64)>,
    /// Track steps due to viewers with the next report.
    steps: Vec<play::TrackStep>,
}

impl TrackedEntity {
    /// Starts tracking an entity whose last report had sequence `seq`.
    pub fn new(entity: Box<dyn RegionEntity>, seq: u64) -> Self {
        let sync = MovementSync::new(entity.base().kind, &pose(&*entity));
        let mut sent = entity.base().metadata();
        sent.extend(entity.metadata());
        if !sent.iter().any(|(i, _)| *i == metadata::ENTITY_FLAGS) {
            sent.push((
                metadata::ENTITY_FLAGS,
                MetadataValue::Byte(entity.base().flags()),
            ));
        }
        Self {
            sent,
            sent_attributes: entity.synced_attributes(),
            spawn: spawn_packets(&*entity),
            head: crate::world::movement_sync::pack_degrees(entity.head_yaw()),
            entity,
            seq: seq + 1,
            sync,
            moves_seq: 0,
            ticks: 0,
            path_credit: 0,
            steps: Vec::new(),
        }
    }

    /// What its movement is synced from.
    pub fn pose(&self) -> Pose {
        pose(&*self.entity)
    }

    /// Advances its movement sync a tick and says what viewers are due.
    pub fn tick_movement(&mut self) -> Option<Movement> {
        let pose = self.pose();
        let flagged = self.entity.take_position_sync();
        self.sync.track_position(pose.position, flagged);
        if !pose.riding
            && let Some(steps) = self.entity.track_steps()
        {
            return self.sync.tick_track(&pose, steps, &mut self.steps);
        }
        self.sync.tick(&pose)
    }

    /// A report for every viewer of `chunk` carrying its full state, after
    /// which deltas start from here.
    pub fn broadcast(&mut self, dimension: DimensionId, chunk: ChunkPos) -> Track {
        self.seq += 1;
        self.moves_seq = self.seq;
        let pose = self.pose();
        self.sync.resync(&pose);
        self.head = crate::world::movement_sync::pack_degrees(self.entity.head_yaw());
        self.track(dimension, chunk)
    }

    /// A report for viewers after `movement`: those that saw the previous
    /// report get only what changed, the rest its full state.
    pub fn moved(&mut self, dimension: DimensionId, chunk: ChunkPos, movement: Movement) -> Track {
        if self.seq != self.moves_seq {
            return self.broadcast(dimension, chunk);
        }
        self.seq += 1;
        self.moves_seq = self.seq;
        let base = self.entity.base();
        let id = base.id;
        let mut moves = Vec::new();
        if let Some(rotation) = movement.rotation
            && movement.placement != Placement::Absolute
        {
            let [yaw, pitch] = rotation;
            moves.push(match movement.placement {
                Placement::Delta(delta) => framed(&play::MoveEntityPosRot {
                    entity_id: id,
                    delta,
                    yaw,
                    pitch,
                    on_ground: base.on_ground,
                }),
                _ => framed(&play::MoveEntityRot {
                    entity_id: id,
                    yaw,
                    pitch,
                    on_ground: base.on_ground,
                }),
            });
        } else {
            match movement.placement {
                Placement::Delta(delta) => moves.push(framed(&play::MoveEntityPos {
                    entity_id: id,
                    delta,
                    on_ground: base.on_ground,
                })),
                Placement::Absolute => {
                    let path = self.sync.take_path();
                    moves.push(position_sync_path(base, &path))
                }
                Placement::Track => moves.push(framed(&play::MoveMinecartAlongTrack {
                    entity_id: id,
                    steps: &std::mem::take(&mut self.steps),
                })),
                Placement::Unchanged => {}
            }
        }
        if movement.velocity {
            moves.push(framed(&play::SetEntityMotion {
                entity_id: id,
                velocity: base.velocity,
            }));
        }
        if self.entity.rotates()
            && let Some(head) = self.head_packets()
        {
            moves.extend(head.iter().cloned());
        }
        let mut track = self.track(dimension, chunk);
        track.moves = Some(moves.into());
        track
    }

    /// A head rotation packet if the head turned since the last one.
    pub fn head_packets(&mut self) -> Option<Packets> {
        let head = crate::world::movement_sync::pack_degrees(self.entity.head_yaw());
        if head == self.head {
            return None;
        }
        self.head = head;
        Some(packets([framed(&play::RotateHead {
            entity_id: self.id(),
            head_yaw: self.entity.head_yaw(),
        })]))
    }

    pub fn id(&self) -> i32 {
        self.entity.base().id
    }

    pub fn chunk(&self) -> ChunkPos {
        self.entity.base().chunk()
    }

    /// Rebuilds the spawn packets after the entity data changed.
    /// Update Attributes for the values that differ from what viewers last
    /// got (the changed ones only); `None` when nothing does.
    pub fn attribute_packet(&mut self) -> Option<bytes::Bytes> {
        let now = self.entity.synced_attributes();
        if now == self.sent_attributes {
            return None;
        }
        let properties: Vec<play::AttributeProperty> = now
            .iter()
            .filter(|entry| !self.sent_attributes.contains(entry))
            .map(|&(attribute, base)| play::AttributeProperty {
                attribute,
                base,
                modifiers: &[],
            })
            .collect();
        let packet = framed(&play::UpdateAttributes {
            entity_id: self.entity.base().id,
            properties: &properties,
        });
        self.sent_attributes = now;
        self.refresh_spawn();
        Some(packet)
    }

    pub fn refresh_spawn(&mut self) {
        self.spawn = spawn_packets(&*self.entity);
    }

    /// The report viewers get: spawn packets for those new to it and a full
    /// movement sync, which doubles as its current state. The position is
    /// the one deltas continue from, so new viewers follow them exactly.
    pub fn track(&self, dimension: DimensionId, chunk: ChunkPos) -> Track {
        let base = self.entity.base();
        let mut update = vec![position_sync(base, self.sync.base())];
        if base.kind.tracking().track_deltas {
            update.push(framed(&play::SetEntityMotion {
                entity_id: base.id,
                velocity: base.velocity,
            }));
        }
        let head = self.entity.rotates().then(|| {
            framed(&play::RotateHead {
                entity_id: base.id,
                head_yaw: self.entity.head_yaw(),
            })
        });
        Track {
            entity: base.id,
            kind: base.kind,
            dimension,
            chunk,
            seq: self.seq,
            position: base.position,
            range: tracking_range(base),
            spawn: if self.entity.ticking_data() {
                spawn_packets(&*self.entity)
            } else {
                self.spawn.clone()
            },
            update: update.into(),
            moves: None,
            head,
            player: None,
            spectator: false,
            shape: self.entity.track_shape(),
        }
    }

    /// Entity data for current viewers after a change, which also becomes
    /// part of the spawn packets.
    pub fn data_packets(&mut self, values: &[(u8, MetadataValue)]) -> Packets {
        self.refresh_spawn();
        packets([framed(&play::SetEntityData {
            entity_id: self.id(),
            values,
        })])
    }

    /// Like [`Self::data_packets`] but only for values that differ from what
    /// viewers last got; `None` when nothing does.
    pub fn delta_packets(&mut self, values: &[(u8, MetadataValue)]) -> Option<Packets> {
        let mut fresh: Vec<(u8, MetadataValue)> = Vec::with_capacity(values.len());
        for (index, value) in values {
            if fresh.iter().any(|(i, _)| i == index) {
                continue;
            }
            match self.sent.iter_mut().find(|(i, _)| i == index) {
                Some((_, old)) if old == value => {}
                Some((_, old)) => {
                    *old = value.clone();
                    fresh.push((*index, value.clone()));
                }
                None => {
                    self.sent.push((*index, value.clone()));
                    fresh.push((*index, value.clone()));
                }
            }
        }
        if fresh.is_empty() {
            return None;
        }
        Some(self.data_packets(&fresh))
    }

    pub fn flags_packets(&mut self) -> Option<Packets> {
        let flags = self.entity.base().flags();
        self.delta_packets(&[(metadata::ENTITY_FLAGS, MetadataValue::Byte(flags))])
    }
}

pub fn spawn_packets(entity: &dyn RegionEntity) -> Packets {
    let base = entity.base();
    let mut out = vec![framed(&play::AddEntity {
        entity_id: base.id,
        uuid: base.uuid,
        kind: base.kind.id() as i32,
        position: entity.spawn_position(),
        velocity: base.velocity,
        pitch: base.pitch,
        yaw: base.yaw,
        head_yaw: entity.head_yaw(),
        data: entity.spawn_data(),
    })];
    let mut values = base.metadata();
    values.extend(entity.metadata());
    if !values.is_empty() {
        out.push(framed(&play::SetEntityData {
            entity_id: base.id,
            values: &values,
        }));
    }
    let attributes = entity.synced_attributes();
    if !attributes.is_empty() {
        let properties: Vec<play::AttributeProperty> = attributes
            .iter()
            .map(|&(attribute, base)| play::AttributeProperty {
                attribute,
                base,
                modifiers: &[],
            })
            .collect();
        out.push(framed(&play::UpdateAttributes {
            entity_id: base.id,
            properties: &properties,
        }));
    }
    if let Some(packet) = equipment_packet(entity) {
        out.push(packet);
    }
    if !base.riders.is_empty() || !base.entity_riders.is_empty() {
        out.push(passengers_packet(base));
    }
    if let Some(mount) = &base.vehicle {
        out.push(framed(&play::SetPassengers {
            vehicle: mount.id,
            passengers: &mount.passengers,
        }));
    }
    if let Some(holder) = entity.leash().and_then(|leash| leash.holder) {
        out.push(crate::world::leash::link_packet(base.id, Some(holder)));
    }
    out.into()
}

/// Set Passengers with the entity's current riders.
pub fn passengers_packet(base: &EntityBase) -> bytes::Bytes {
    let riders = all_passengers(base);
    framed(&play::SetPassengers {
        vehicle: base.id,
        passengers: &riders,
    })
}

/// Everyone riding it in the order they boarded (players and entities
/// alike); passengers with no recorded boarding come last, players first.
pub fn all_passengers(base: &EntityBase) -> Vec<i32> {
    let mut all: Vec<i32> = base
        .riders
        .iter()
        .map(|rider| rider.id)
        .chain(base.entity_riders.iter().copied())
        .collect();
    let rank = |id: &i32| {
        base.mount_order
            .iter()
            .position(|other| other == id)
            .unwrap_or(usize::MAX)
    };
    all.sort_by_key(rank);
    all
}

impl EntityBase {
    /// Records that `id` boarded, after everyone already aboard.
    pub fn note_boarded(&mut self, id: i32) {
        let aboard = |base: &EntityBase, other: i32| {
            base.riders.iter().any(|rider| rider.id == other) || base.entity_riders.contains(&other)
        };
        let mut order = std::mem::take(&mut self.mount_order);
        order.retain(|other| *other != id && aboard(self, *other));
        order.push(id);
        self.mount_order = order;
    }

    /// The seat of `id` among everyone aboard, and how many are aboard.
    pub fn seat_index(&self, id: i32) -> (usize, usize) {
        let all = all_passengers(self);
        (
            all.iter().position(|other| *other == id).unwrap_or(0),
            all.len(),
        )
    }
}

/// Set Equipment with everything the entity wears and holds, if anything.
pub fn equipment_packet(entity: &dyn RegionEntity) -> Option<bytes::Bytes> {
    let equipment = entity.equipment();
    if equipment.iter().all(|(_, stack)| stack.is_none()) {
        return None;
    }
    Some(framed(&play::SetEquipment {
        entity_id: entity.base().id,
        items: &equipment,
    }))
}

/// The entity's tracking range in blocks, widened to that of any player
/// riding it.
pub fn tracking_range(base: &crate::world::entity::EntityBase) -> i32 {
    let own = base.kind.tracking().range_chunks as i32 * 16;
    match base.riders.is_empty() {
        true => own,
        false => own.max(EntityType::Player.tracking().range_chunks as i32 * 16),
    }
}
