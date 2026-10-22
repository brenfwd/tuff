//! What a fake player's hands do, as the packets a client sends for each.

use tuff_world_model::{BlockPos, Face};

use super::driver::Driver;
use tuff_vanilla_data::registry::{Attribute, EntityType};

use crate::protocol::play::{GameMode, PlayerActionStatus, Serverbound};
use crate::world::distance_sq;
use crate::world::physics::Aabb;
use crate::world::player;
use crate::world::projectile::clip;
use crate::world::session::PlayerSession;

const RESPAWN: i32 = 0;
const CREATIVE_ENTITY_BONUS: f64 = 2.0;
/// How long an attack on a target the client does not see yet waits for
/// it to come into view before it is dropped.
pub const ATTACK_WAIT_TICKS: u8 = 20;

#[derive(Debug, Clone, PartialEq)]
pub enum FakeAction {
    /// Turns the held look; the next movement report carries it.
    Look {
        yaw: f32,
        pitch: f32,
    },
    Swing,
    Attack(i32),
    Interact {
        entity: i32,
        off_hand: bool,
    },
    /// Starts mining; the driver keeps swinging and finishes the break when
    /// a client's progress reaches one.
    Dig {
        pos: BlockPos,
        face: Face,
    },
    AbortDig,
    UseItemOn {
        off_hand: bool,
        pos: BlockPos,
        face: Face,
        cursor: [f32; 3],
    },
    UseItem {
        off_hand: bool,
    },
    ReleaseUse,
    SelectSlot(i16),
    SwapHands,
    Drop {
        stack: bool,
    },
    Respawn,
}

/// A break in progress, tracked the way a client does.
#[derive(Debug, Clone, Copy)]
pub struct Dig {
    pos: BlockPos,
    face: Face,
    progress: f32,
}

impl Driver {
    pub(super) fn dig_progress(&self) -> f32 {
        self.dig.map_or(0.0, |dig| dig.progress)
    }

    fn next_sequence(&mut self) -> i32 {
        self.sequence += 1;
        self.sequence
    }

    fn action(&mut self, status: PlayerActionStatus, pos: BlockPos, face: Face) -> Serverbound {
        Serverbound::PlayerAction {
            status,
            position: pos,
            face: face.id() as i8,
            sequence: self.next_sequence(),
        }
    }
}

impl PlayerSession {
    /// Turns queued actions and the running dig into packets, in order.
    pub(crate) fn fake_actions(&mut self, driver: &mut Driver) -> Vec<Serverbound> {
        let mut out = Vec::new();
        if let Some((entity, left)) = driver.pending_attack.take()
            && !self.attack(driver, entity, &mut out)
            && left > 1
        {
            driver.pending_attack = Some((entity, left - 1));
        }
        while let Ok(action) = driver.actions.try_recv() {
            self.fake_action(driver, action, &mut out);
        }
        self.continue_dig(driver, &mut out);
        out
    }

    fn fake_action(&mut self, driver: &mut Driver, action: FakeAction, out: &mut Vec<Serverbound>) {
        match action {
            FakeAction::Look { yaw, pitch } => {
                driver.held.yaw = yaw;
                driver.held.pitch = pitch;
            }
            FakeAction::Swing => out.push(Serverbound::Punch),
            FakeAction::Attack(entity) => {
                if !self.attack(driver, entity, out) {
                    driver.pending_attack = Some((entity, ATTACK_WAIT_TICKS));
                }
            }
            FakeAction::Interact { entity, off_hand } => {
                out.push(Serverbound::Interact {
                    entity,
                    off_hand,
                    location: [0.0; 3],
                    sneaking: driver.held.sneak,
                });
                out.push(Serverbound::Punch);
            }
            FakeAction::Dig { pos, face } => {
                if self.is_dead() {
                    return;
                }
                self.face(driver, face_point(pos, face), out);
                if let Some(old) = driver.dig.take() {
                    out.push(driver.action(
                        PlayerActionStatus::AbortDestroyBlock,
                        old.pos,
                        old.face,
                    ));
                }
                out.push(driver.action(PlayerActionStatus::StartDestroyBlock, pos, face));
                out.push(Serverbound::Punch);
                let mut dig = Dig {
                    pos,
                    face,
                    progress: 0.0,
                };
                if self.dig_delta(pos) < 1.0 && !self.mode.abilities.instabuild {
                    dig.progress = 0.0;
                    driver.dig = Some(dig);
                }
            }
            FakeAction::AbortDig => {
                if let Some(old) = driver.dig.take() {
                    out.push(driver.action(
                        PlayerActionStatus::AbortDestroyBlock,
                        old.pos,
                        old.face,
                    ));
                }
            }
            FakeAction::UseItemOn {
                off_hand,
                pos,
                face,
                cursor,
            } => {
                self.face(driver, face_point(pos, face), out);
                out.push(Serverbound::UseItemOn {
                    hand: i32::from(off_hand),
                    position: pos,
                    face,
                    cursor,
                    sequence: driver.next_sequence(),
                });
                out.push(Serverbound::Punch);
            }
            FakeAction::UseItem { off_hand } => out.push(Serverbound::UseItem {
                off_hand,
                sequence: driver.next_sequence(),
            }),
            FakeAction::ReleaseUse => {
                out.push(driver.action(
                    PlayerActionStatus::ReleaseUseItem,
                    BlockPos::new(0, 0, 0),
                    Face::Down,
                ));
            }
            FakeAction::SelectSlot(slot) => out.push(Serverbound::SetCarriedItem(slot)),
            FakeAction::SwapHands => out.push(driver.action(
                PlayerActionStatus::SwapItemWithOffhand,
                BlockPos::new(0, 0, 0),
                Face::Down,
            )),
            FakeAction::Drop { stack } => out.push(driver.action(
                if stack {
                    PlayerActionStatus::DropAllItems
                } else {
                    PlayerActionStatus::DropItem
                },
                BlockPos::new(0, 0, 0),
                Face::Down,
            )),
            FakeAction::Respawn => out.push(Serverbound::ClientCommand(RESPAWN)),
        }
    }

    /// Swings at `entity`, hitting it when within reach; false when the
    /// client does not see it.
    fn attack(&mut self, driver: &mut Driver, entity: i32, out: &mut Vec<Serverbound>) -> bool {
        let Some(bounds) = self.target_bounds(entity) else {
            return false;
        };
        let aim = centre(bounds);
        self.face(driver, aim, out);
        if hit_distance(self.eye(), bounds, aim) < self.attack_reach() {
            out.push(Serverbound::Attack(entity));
        }
        out.push(Serverbound::Punch);
        true
    }

    /// Turns toward `target` and reports it ahead of the action, as a
    /// client that aims first would.
    fn face(&mut self, driver: &mut Driver, target: [f64; 3], out: &mut Vec<Serverbound>) {
        let eye = self.eye();
        let (dx, dy, dz) = (target[0] - eye[0], target[1] - eye[1], target[2] - eye[2]);
        let yaw = (-dx).atan2(dz).to_degrees() as f32;
        let pitch = (-dy).atan2(dx.hypot(dz)).to_degrees() as f32;
        driver.held.yaw = yaw;
        driver.held.pitch = pitch;
        if (yaw, pitch) != driver.sent_rotation {
            driver.sent_rotation = (yaw, pitch);
            out.push(Serverbound::Move(crate::protocol::play::Movement {
                position: None,
                rotation: Some((yaw, pitch)),
                on_ground: driver.sent_ground,
                horizontal_collision: false,
            }));
        }
    }

    /// The box of a target this client sees, at its last known position.
    fn target_bounds(&self, entity: i32) -> Option<Aabb> {
        let (kind, position) = match self.view.entities.visible_player(entity) {
            Some((position, _)) => (EntityType::Player, position),
            None => {
                let (kind, _, position) = self.view.entities.visible_entity(entity)?;
                (kind, position)
            }
        };
        let size = kind.size();
        let half = f64::from(size.width) / 2.0;
        Some([
            position[0] - half,
            position[1],
            position[2] - half,
            position[0] + half,
            position[1] + f64::from(size.height),
            position[2] + half,
        ])
    }

    /// How far a client's crosshair picks an entity to hit: the entity
    /// interaction range (creative adds its bonus), or the held weapon's own
    /// attack range when that reaches farther.
    fn attack_reach(&self) -> f64 {
        let creative = self.mode.game_mode == GameMode::Creative;
        let range = self.attribute(Attribute::EntityInteractionRange)
            + if creative { CREATIVE_ENTITY_BONUS } else { 0.0 };
        let weapon = self
            .inventory
            .hand(false)
            .and_then(|stack| stack.item().attack_range())
            .map_or(0.0, |reach| {
                f64::from(reach.reach(creative).1) + f64::from(reach.hitbox_margin)
            });
        range.max(weapon)
    }

    /// What one tick of mining `pos` removes, from the client's own world.
    fn dig_delta(&self, pos: BlockPos) -> f32 {
        let Some(state) = self.client_world().block_at(pos) else {
            return 0.0;
        };
        player::destroy_progress(state, self.inventory.hand(false), &self.miner(), false)
    }

    fn continue_dig(&mut self, driver: &mut Driver, out: &mut Vec<Serverbound>) {
        let Some(mut dig) = driver.dig else {
            return;
        };
        let gone = self
            .client_world()
            .block_at(dig.pos)
            .is_none_or(tuff_world_model::chunk::is_air);
        if gone || self.is_dead() {
            driver.dig = None;
            return;
        }
        dig.progress += self.dig_delta(dig.pos);
        out.push(Serverbound::Punch);
        if dig.progress >= 1.0 {
            driver.dig = None;
            out.push(driver.action(PlayerActionStatus::StopDestroyBlock, dig.pos, dig.face));
        } else {
            driver.dig = Some(dig);
        }
    }
}

fn centre(b: Aabb) -> [f64; 3] {
    [
        (b[0] + b[3]) / 2.0,
        (b[1] + b[4]) / 2.0,
        (b[2] + b[5]) / 2.0,
    ]
}

/// How far along the look from `eye` toward `aim` the crosshair meets
/// `bounds`; zero from inside the box.
fn hit_distance(eye: [f64; 3], bounds: Aabb, aim: [f64; 3]) -> f64 {
    clip(bounds, eye, aim).map_or(0.0, |hit| distance_sq(eye, hit).sqrt())
}

fn face_point(pos: BlockPos, face: Face) -> [f64; 3] {
    let (nx, ny, nz) = face.normal();
    let center = pos.center();
    [
        center[0] + f64::from(nx) * 0.5,
        center[1] + f64::from(ny) * 0.5,
        center[2] + f64::from(nz) * 0.5,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAYER_AT_FOUR: Aabb = [-3.8, 150.0, 0.2, -3.2, 151.8, 0.8];

    #[tuff_core::watchdog]
    #[test]
    fn the_crosshair_meets_a_box_at_its_near_face() {
        let eye = [0.5, 151.62, 0.5];
        let distance = hit_distance(eye, PLAYER_AT_FOUR, centre(PLAYER_AT_FOUR));
        assert!((distance - 3.76).abs() < 0.01, "{distance}");
    }

    #[tuff_core::watchdog]
    #[test]
    fn an_eye_inside_the_box_is_at_distance_zero() {
        let eye = centre(PLAYER_AT_FOUR);
        assert_eq!(hit_distance(eye, PLAYER_AT_FOUR, eye), 0.0);
    }
}
