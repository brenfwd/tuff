//! Player chunk subscription and per-chunk viewer fan-out: the machinery
//! that turns a player's view-distance subscription into chunk and entity
//! packets. Called from region.rs's dispatch.

use std::collections::HashSet;

use tuff_vanilla_data::registry::EntityType;

use super::{Presence, RegionActor};
use crate::world::messages::{PlayerEvent, PlayerHandle, PlayerId, PlayerUpdate, Track};
use crate::world::{PLAYER_HALF_WIDTH, block_contact, fall, physics, player};
use tuff_world_model::BlockPos;
use tuff_world_model::pos::ChunkPos;

impl RegionActor {
    pub(super) fn subscribe(&mut self, pos: ChunkPos, player: PlayerHandle) {
        let id = player.id;
        if let Some(column) = self.columns.get_mut(&pos) {
            if column.unloading {
                column.unloading = false;
                self.block_entity_generation += 1;
            }
            column.last_used = self.sched.server_tick;
            column.held.remove(&id);
            column.viewers.insert(id, player.clone());
            match &column.packet {
                Some(packet) => {
                    column.packet_used = self.sched.server_tick;
                    player.send(PlayerEvent::Chunk {
                        dimension: self.dimension,
                        pos,
                        packet: packet.clone(),
                        snapshot: column.snapshot.clone(),
                    });
                    self.send_entities(pos, &player);
                }
                None => {
                    column.awaiting_packet.insert(id);
                    self.request_packet(pos);
                }
            }
        } else {
            let loading = self.load(pos);
            loading.held.remove(&id);
            loading.viewers.insert(id, player);
        }
    }

    pub(super) fn peek(&mut self, pos: ChunkPos, player: PlayerHandle) {
        if self.columns.contains_key(&pos) {
            self.subscribe(pos, player);
            return;
        }
        let id = player.id;
        let loading = self.load(pos);
        loading.held.remove(&id);
        if loading.held.is_empty() && loading.viewers.keys().all(|v| loading.peeking.contains(v)) {
            loading.peeking.insert(id);
        }
        loading.viewers.insert(id, player);
    }

    /// Loads `pos` for `player` and keeps it lit without sending it,
    /// replacing any subscription the player had to it.
    pub(super) fn hold_lit(&mut self, pos: ChunkPos, player: PlayerId) {
        if let Some(column) = self.columns.get_mut(&pos) {
            if column.unloading {
                column.unloading = false;
                self.block_entity_generation += 1;
            }
            column.last_used = self.sched.server_tick;
            column.viewers.remove(&player);
            column.awaiting_packet.remove(&player);
            column.resending.remove(&player);
            column.held.insert(player);
        } else {
            let loading = self.load(pos);
            loading.viewers.remove(&player);
            loading.held.insert(player);
        }
    }

    /// Tells a new viewer about every entity in the chunk.
    pub(super) fn send_entities(&self, pos: ChunkPos, viewer: &PlayerHandle) {
        self.send_owned_entities(pos, viewer);
        self.send_displays(pos, viewer);
        for (id, presence) in &self.ents.presence {
            if *id != viewer.id && presence.chunk == pos {
                viewer.send(PlayerEvent::Track(Track {
                    entity: *id,
                    kind: EntityType::Player,
                    dimension: self.dimension,
                    chunk: pos,
                    seq: presence.seq,
                    position: presence.position,
                    range: EntityType::Player.tracking().range_chunks as i32 * 16,
                    spawn: presence.spawn.clone(),
                    update: presence.update.clone(),
                    moves: None,
                    head: None,
                    player: Some(presence.handle.clone()),
                    spectator: presence.spectator,
                    shape: None,
                }));
            }
        }
    }

    pub(super) fn fanout_chunk(
        &self,
        pos: ChunkPos,
        except: Option<PlayerId>,
        event: impl Fn(&PlayerHandle) -> PlayerEvent,
    ) {
        if let Some(column) = self.columns.get(&pos) {
            for (id, viewer) in &column.viewers {
                if Some(*id) != except {
                    viewer.send(event(viewer));
                }
            }
        }
    }

    pub(super) fn player_update(&mut self, update: PlayerUpdate) {
        let id = update.handle.id;
        if self.pos.contains(update.chunk) {
            let portal = update.alive.then(|| self.portal_contact(&update)).flatten();
            let block = |pos| self.block(pos);
            let landed_pos = block_contact::on_pos(
                update.position,
                physics::supporting_block(player_bounds(&update), &block),
                block_contact::STEP_OFFSET,
                &block,
            );
            let climb = (update.alive && !update.spectator)
                .then(|| player::climbable_block(update.position, block))
                .flatten();
            update.handle.send(PlayerEvent::Surroundings {
                dimension: self.dimension,
                seq: update.seq,
                in_water: player::touches_water(update.position, block),
                submersion: player::submersion(update.position, update.looks.eye_height, block),
                in_lava: update.alive
                    && !update.spectator
                    && player::touches_lava(update.position, block),
                in_fire: (update.alive && !update.spectator)
                    .then(|| player::fire_contact(update.position, block))
                    .flatten(),
                climbable: climb.is_some(),
                climb_location: climb.map(crate::world::death::FallLocation::of_block),
                resets_fall: update.alive
                    && !update.spectator
                    && (fall::resets_fall(player_bounds(&update), &block)
                        || self.player_move_resets_fall(&update)),
                landed_on: self.block(landed_pos),
                portal,
                biome: self.biome_at(BlockPos::containing(update.position)),
                stepping_on: self.block(BlockPos::containing([
                    update.position[0],
                    update.position[1] - STEPPING_ON_DEPTH,
                    update.position[2],
                ])),
                contact: if update.alive && !update.spectator {
                    let bounds = player_bounds(&update);
                    let from = update.swept_from;
                    let jump = [0, 1, 2].map(|axis| update.position[axis] - from[axis]);
                    let previous =
                        if jump.iter().map(|d| d * d).sum::<f64>() <= MAX_SWEEP * MAX_SWEEP {
                            physics::offset(bounds, jump.map(|d| -d))
                        } else {
                            bounds
                        };
                    block_contact::Contact {
                        in_rain: self.rained_on(update.position, bounds[4]),
                        ..block_contact::player_contact_swept(
                            previous,
                            bounds,
                            update.looks.eye_height,
                            &block,
                        )
                    }
                } else {
                    block_contact::Contact::default()
                },
            });
            if update.alive && !update.spectator {
                if let Some(fall_distance) = update.landed
                    && self
                        .block(landed_pos)
                        .is_some_and(|s| !tuff_world_model::chunk::is_air(s))
                {
                    self.entity_landed(&crate::world::region_api::Landing {
                        entity: id,
                        position: update.position,
                        pos: landed_pos,
                        fall_distance,
                        kind: EntityType::Player,
                        size: [0.6, update.looks.height],
                        living: true,
                        gamemaster: update.gamemaster,
                    });
                }
                self.player_moved(&update);
            }
        }
        let previous_home = self
            .ents
            .presence
            .get(&id)
            .map(|p| p.chunk)
            .filter(|chunk| self.pos.contains(*chunk));
        let track = Track {
            entity: id,
            kind: EntityType::Player,
            dimension: self.dimension,
            chunk: update.chunk,
            seq: update.seq,
            position: update.position,
            range: EntityType::Player.tracking().range_chunks as i32 * 16,
            spawn: update.spawn.clone(),
            update: update.update.clone(),
            moves: update.moves.clone(),
            head: update.head.clone(),
            player: Some(update.handle.clone()),
            spectator: update.spectator,
            shape: None,
        };
        let mut sent = HashSet::new();
        for chunk in [Some(update.chunk), previous_home].into_iter().flatten() {
            if let Some(column) = self.columns.get(&chunk) {
                for (viewer, handle) in &column.viewers {
                    if *viewer != id && sent.insert(*viewer) {
                        handle.send(PlayerEvent::Track(track.clone()));
                    }
                }
            }
        }
        self.ents.presence.insert(
            id,
            Presence {
                handle: update.handle,
                uuid: update.uuid,
                name: update.name,
                position: update.position,
                chunk: update.chunk,
                alive: update.alive,
                spectator: update.spectator,
                explosion_knockback_resistance: update.explosion_knockback_resistance,
                creative_flying: update.creative_flying,
                grounded: update.grounded,
                looks: update.looks,
                spawn: update.spawn,
                update: update.update,
                seq: update.seq,
            },
        );
    }
}

/// How far below the feet the block a player stands on is looked for.
fn player_bounds(update: &PlayerUpdate) -> physics::Aabb {
    let [x, y, z] = update.position;
    [
        x - PLAYER_HALF_WIDTH,
        y,
        z - PLAYER_HALF_WIDTH,
        x + PLAYER_HALF_WIDTH,
        y + update.looks.height,
        z + PLAYER_HALF_WIDTH,
    ]
}

/// A longer stretch than this between updates was a teleport, not a walk.
const MAX_SWEEP: f64 = 10.0;
const STEPPING_ON_DEPTH: f64 = 1.0e-5;

impl RegionActor {
    /// Whether the player's last move passed through something that
    /// resets a fall (traced) or slid down a honey block side.
    fn player_move_resets_fall(&self, update: &PlayerUpdate) -> bool {
        let block = |pos| self.block(pos);
        let movement = update.looks.movement;
        let from = [0, 1, 2].map(|i| update.position[i] - movement[i]);
        let instant_portal = self.shared.rule_int("players_nether_portal_default_delay") == 0;
        fall::move_resets_fall(from, movement, true, instant_portal, &block)
            || fall::honey_slide(
                player_bounds(update),
                update.position,
                -movement[1],
                update.grounded,
                &block,
            )
    }
}
