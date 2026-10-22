//! Display entities standing in for blocks pistons move with their block
//! entity. Clients draw a moving block only through its model, so a block
//! drawn by its block entity (a chest) would vanish while it moves. The
//! region running the block event shows its viewers a display entity that
//! slides from the old position to the new one with the client's own
//! animation, and removes it the tick after the block lands. A viewer
//! arriving mid-move sees no moving block at all, so it is shown a display
//! for every carried block, as far along as the move is.

use tuff_vanilla_data::BlockState;
use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::registry::EntityType;

use super::super::{ChunkPos, RegionActor};
use super::MOVE_TICKS;
use super::carried::CARRY_WAIT;
use crate::protocol::play::{self, MetadataValue};
use crate::world::block_entity::raw::CARRIED;
use crate::world::dimension::DimensionId;
use crate::world::messages::{PlayerEvent, PlayerHandle, Track};
use crate::world::placement::step;
use crate::world::redstone::{CarriedLook, MovingBlock};
use crate::world::{framed, packets};
use tuff_world_model::BlockPos;

/// Synched data indices of display entities.
const TELEPORT_DURATION: u8 = 10;
const TRANSLATION: u8 = 11;
const LEFT_ROTATION: u8 = 13;
/// The block state of a block display, the item of an item display.
const SHOWN: u8 = 23;

/// Client ticks a block takes to move: its progress goes up by halves.
const CLIENT_STEPS: f32 = 2.0;

pub(super) struct CarriedDisplay {
    id: i32,
    uuid: u128,
    /// The block event's chunk: its viewers are the ones animating it.
    chunk: ChunkPos,
    dest: BlockPos,
    movement: Facing,
    moved: BlockState,
    look: CarriedLook,
    started: u64,
}

impl CarriedDisplay {
    /// What a viewer is told about the display, `progress` into the move.
    fn track(&self, dimension: DimensionId, progress: f32) -> Track {
        let corner = |pos: BlockPos| [f64::from(pos.x), f64::from(pos.y), f64::from(pos.z)];
        let from = corner(step(self.dest, self.movement.opposite()));
        let (dx, dy, dz) = self.movement.step();
        let along = f64::from(progress);
        let start = [
            from[0] + f64::from(dx) * along,
            from[1] + f64::from(dy) * along,
            from[2] + f64::from(dz) * along,
        ];
        let steps = ((1.0 - progress).max(0.0) * CLIENT_STEPS).round() as i32;
        let (kind, mut values) = match &self.look {
            CarriedLook::ItemDisplay {
                item,
                translation,
                rotation,
            } => (
                EntityType::ItemDisplay,
                vec![
                    (TRANSLATION, MetadataValue::Vector3(*translation)),
                    (LEFT_ROTATION, MetadataValue::Quaternion(*rotation)),
                    (
                        SHOWN,
                        MetadataValue::Slot(Some(tuff_item::item::ItemSnapshot::of(item.as_ref()))),
                    ),
                ],
            ),
            CarriedLook::Model | CarriedLook::BlockDisplay => (
                EntityType::BlockDisplay,
                vec![(SHOWN, MetadataValue::BlockState(self.moved.id()))],
            ),
        };
        values.push((TELEPORT_DURATION, MetadataValue::VarInt(steps)));
        let spawn = packets([
            framed(&play::AddEntity {
                entity_id: self.id,
                uuid: self.uuid,
                kind: kind.id() as i32,
                position: start,
                velocity: [0.0; 3],
                pitch: 0.0,
                yaw: 0.0,
                head_yaw: 0.0,
                data: 0,
            }),
            framed(&play::SetEntityData {
                entity_id: self.id,
                values: &values,
            }),
        ]);
        let end = corner(self.dest);
        let update = packets([framed(&play::EntityPositionSync {
            entity_id: self.id,
            position: end,
            yaw: 0.0,
            pitch: 0.0,
            on_ground: false,
        })]);
        Track {
            entity: self.id,
            kind,
            dimension,
            chunk: self.chunk,
            seq: 0,
            position: end,
            range: kind.tracking().range_chunks as i32 * 16,
            spawn,
            update,
            moves: None,
            head: None,
            player: None,
            spectator: false,
            shape: None,
        }
    }
}

impl RegionActor {
    /// Starts a display for each block the block event at `chunk` started
    /// moving with its block entity, shown to the event's viewers when the
    /// moving piston would not draw it.
    pub(super) fn show_displays(&mut self, chunk: ChunkPos, shown: &[(BlockPos, MovingBlock)]) {
        for (dest, moving) in shown {
            self.drop_display(*dest);
            let nbt = self
                .raw_block_entity(*dest)
                .and_then(|nbt| nbt.get_compound(CARRIED));
            let look = self
                .shared
                .block_entity_carrier
                .as_ref()
                .map_or(CarriedLook::Model, |carrier| {
                    carrier.look(moving.moved, nbt)
                });
            let display = CarriedDisplay {
                id: self.shared.next_entity_id(),
                uuid: rand::random(),
                chunk,
                dest: *dest,
                movement: moving.movement(),
                moved: moving.moved,
                look,
                started: self.now(),
            };
            if display.look != CarriedLook::Model {
                let track = display.track(self.dimension, 0.0);
                self.send_to_viewers(&[chunk], || PlayerEvent::Track(track.clone()));
            }
            self.sim.redstone.carried.displays.push(display);
        }
    }

    /// Removes the displays of blocks that landed (or broke) by the last
    /// tick, so the landed block is in place before its display goes.
    pub(super) fn expire_displays(&mut self) {
        if self.sim.redstone.carried.displays.is_empty() {
            return;
        }
        let displays = std::mem::take(&mut self.sim.redstone.carried.displays);
        let (gone, kept): (Vec<_>, Vec<_>) = displays
            .into_iter()
            .partition(|display| self.display_done(display));
        self.sim.redstone.carried.displays = kept;
        for display in gone {
            self.untrack_display(&display);
        }
    }

    /// Tells a viewer new to `chunk` about the moves its block events
    /// started there that are still under way.
    pub(in super::super) fn send_displays(&self, chunk: ChunkPos, viewer: &PlayerHandle) {
        for display in &self.sim.redstone.carried.displays {
            if display.chunk == chunk {
                let progress = self.display_progress(display);
                viewer.send(PlayerEvent::Track(display.track(self.dimension, progress)));
            }
        }
    }

    fn drop_display(&mut self, dest: BlockPos) {
        let displays = &mut self.sim.redstone.carried.displays;
        if let Some(index) = displays.iter().position(|display| display.dest == dest) {
            let display = displays.swap_remove(index);
            self.untrack_display(&display);
        }
    }

    fn untrack_display(&self, display: &CarriedDisplay) {
        let entity = display.id;
        self.send_to_viewers(&[display.chunk], || PlayerEvent::Untrack { entity, seq: 1 });
    }

    fn display_done(&self, display: &CarriedDisplay) -> bool {
        let since = self.sched.tick.saturating_sub(display.started);
        if !self.pos.contains(ChunkPos::of_block(display.dest)) {
            return since >= MOVE_TICKS;
        }
        since > MOVE_TICKS + CARRY_WAIT
            || self
                .moving_at(display.dest)
                .is_none_or(|moving| moving.source || moving.moved.block() != display.moved.block())
    }

    fn display_progress(&self, display: &CarriedDisplay) -> f32 {
        let moving = if self.pos.contains(ChunkPos::of_block(display.dest)) {
            self.moving_at(display.dest)
        } else {
            self.remote_moving_at(display.dest)
        };
        moving.map_or(1.0, |moving| moving.progress)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_vanilla_data::entity::data_serializer;

    #[tuff_core::watchdog]
    #[test]
    fn display_data_indices_match_the_layouts() {
        for kind in [EntityType::BlockDisplay, EntityType::ItemDisplay] {
            assert_eq!(
                kind.data_serializer(TELEPORT_DURATION),
                Some(data_serializer::INT)
            );
            assert_eq!(
                kind.data_serializer(TRANSLATION),
                Some(data_serializer::VECTOR3)
            );
            assert_eq!(
                kind.data_serializer(LEFT_ROTATION),
                Some(data_serializer::QUATERNION)
            );
        }
        assert_eq!(
            EntityType::BlockDisplay.data_serializer(SHOWN),
            Some(data_serializer::BLOCK_STATE)
        );
        assert_eq!(
            EntityType::ItemDisplay.data_serializer(SHOWN),
            Some(data_serializer::ITEM_STACK)
        );
    }
}
