use super::*;
use crate::world::physics::{self, Aabb};

/// Squared distance one move packet may cover beyond the velocity the
/// server gave the player.
const MOVED_TOO_QUICKLY: f64 = 100.0;
const MOVED_TOO_QUICKLY_GLIDING: f64 = 300.0;
/// Squared horizontal distance the claimed end of a move may sit from where
/// the same move replayed against the world ends.
const MOVED_WRONGLY: f64 = 0.0625;
const MAX_PACKETS_PER_TICK: i64 = 5;
const HORIZONTAL_LIMIT: f64 = 3.0e7;
const VERTICAL_LIMIT: f64 = 2.0e7;
const SHRINK: f64 = 1.0e-5;
/// Ticks a block change near the player excuses the replay: the client may
/// have moved through what it predicted before the server's copy changed.
const BLOCK_CHANGE_GRACE: u64 = 3;
/// Ticks a velocity given to the player stays part of what it may cover.
const VELOCITY_MEMORY: u64 = 20;
const CRAWL_HEIGHT: f64 = 0.6;

pub(super) fn clamp_position(position: [f64; 3]) -> [f64; 3] {
    [
        position[0].clamp(-HORIZONTAL_LIMIT, HORIZONTAL_LIMIT),
        position[1].clamp(-VERTICAL_LIMIT, VERTICAL_LIMIT),
        position[2].clamp(-HORIZONTAL_LIMIT, HORIZONTAL_LIMIT),
    ]
}

impl PlayerSession {
    fn moves_checked(&self) -> bool {
        self.shared.rule_bool("player_movement_check")
            && (!self.is_gliding() || self.shared.rule_bool("elytra_movement_check"))
    }

    fn velocity_given_sq(&self) -> f64 {
        self.motion
            .velocity_sent
            .recent
            .iter()
            .filter(|given| self.tick.saturating_sub(given.tick) <= VELOCITY_MEMORY)
            .map(|given| given.velocity.iter().map(|v| v * v).sum::<f64>())
            .fold(0.0, f64::max)
    }

    fn player_box(&self, at: [f64; 3]) -> Aabb {
        let size = self.current_pose().player_size();
        let half = f64::from(size.width) / 2.0;
        let height = if self.pose.swimming || self.surroundings.contact.cramped > 0 {
            f64::from(size.height).min(CRAWL_HEIGHT)
        } else {
            f64::from(size.height)
        };
        [
            at[0] - half,
            at[1],
            at[2] - half,
            at[0] + half,
            at[1] + height,
            at[2] + half,
        ]
    }

    /// Counts a move packet against the tick's allowance, which is
    /// refreshed when the tick runs.
    fn move_packet_allowance(&mut self) -> f64 {
        self.sync.move_packets += 1;
        let pending = i64::from(self.sync.move_packets) - i64::from(self.sync.known_move_packets);
        if pending > MAX_PACKETS_PER_TICK {
            1.0
        } else {
            pending as f64
        }
    }

    pub(super) fn tick_move_packets(&mut self) {
        self.sync.known_move_packets = self.sync.move_packets;
    }

    /// The checks the game always makes on a position the client reports:
    /// too far for the packets it may send, a replay of the move against
    /// the world that ends somewhere else, or a body pushed into blocks it
    /// was not already in. True when the move was refused and the player
    /// put back at `old`.
    pub(super) fn refuses_move(&mut self, old: [f64; 3], new: [f64; 3]) -> bool {
        let allowance = self.move_packet_allowance();
        let delta = [new[0] - old[0], new[1] - old[1], new[2] - old[2]];
        let moved_sq = delta.iter().map(|v| v * v).sum::<f64>();
        if self.moves_checked() {
            let limit = if self.is_gliding() {
                MOVED_TOO_QUICKLY_GLIDING
            } else {
                MOVED_TOO_QUICKLY
            };
            if moved_sq - self.velocity_given_sq() > limit * allowance {
                self.teleport(old);
                return true;
            }
        }
        if self.mode.game_mode == GameMode::Spectator || self.sleep.sleeping.is_some() {
            return false;
        }
        let old_box = self.player_box(old);
        let new_box = physics::offset(old_box, delta);
        let reach = physics::expand_towards(old_box, delta);
        let mirror = &self.view.mirror;
        if mirror.touches_unknown(reach) || mirror.changed_within(reach, BLOCK_CHANGE_GRACE) {
            return false;
        }
        let shapes = |pos| match mirror.block_at(pos) {
            Some(state) => physics::Shape::collision(Some(state), pos),
            None => physics::Shape::from(&[][..]),
        };
        let wrong =
            self.mode.game_mode != GameMode::Creative && !self.body.fall.in_impulse_grace() && {
                let step = self.attribute(Attribute::StepHeight) as f32;
                let moved =
                    physics::collide_shaped(old_box, delta, step, self.pose.on_ground, &shapes);
                let off = [delta[0] - moved[0], delta[2] - moved[2]];
                off[0] * off[0] + off[1] * off[1] > MOVED_WRONGLY
            };
        let blocked = (wrong
            && mirror
                .colliders(physics::inflate(old_box, [-SHRINK; 3]))
                .is_empty())
            || mirror
                .colliders(physics::inflate(new_box, [-SHRINK; 3]))
                .into_iter()
                .any(|shape| !physics::intersects(shape, physics::inflate(old_box, [-SHRINK; 3])));
        if blocked {
            self.teleport(old);
        }
        blocked
    }

    /// How far from the eyes an entity may be used, before the tolerance.
    pub fn entity_interaction_range(&self) -> f64 {
        let creative = self.mode.game_mode == GameMode::Creative;
        self.attribute(Attribute::EntityInteractionRange)
            + if creative { CREATIVE_ENTITY_BONUS } else { 0.0 }
    }

    /// How far from the eyes an attack with the held item reaches, before
    /// the tolerance.
    pub fn attack_range(&self) -> f64 {
        let creative = self.mode.game_mode == GameMode::Creative;
        match self
            .inventory
            .hand(false)
            .and_then(|stack| stack.item().attack_range())
        {
            Some(range) => f64::from(range.reach(creative).1) + f64::from(range.hitbox_margin),
            None => self.entity_interaction_range(),
        }
    }

    /// Whether the eyes are close enough to `kind`'s hitbox at `position`
    /// to reach it with a reach of `range` plus the tolerance the game
    /// adds for the client's older view of where it stood.
    pub fn within_entity_reach(
        &self,
        kind: tuff_vanilla_data::registry::EntityType,
        position: [f64; 3],
        range: f64,
    ) -> bool {
        let size = kind.size();
        let half = f64::from(size.width) / 2.0;
        let bounds = [
            position[0] - half,
            position[1],
            position[2] - half,
            position[0] + half,
            position[1] + f64::from(size.height),
            position[2] + half,
        ];
        let eye = self.eye();
        let distance_sq: f64 = (0..3)
            .map(|axis| {
                let gap = (bounds[axis] - eye[axis])
                    .max(eye[axis] - bounds[axis + 3])
                    .max(0.0);
                gap * gap
            })
            .sum();
        distance_sq <= (range + ENTITY_REACH_TOLERANCE).powi(2)
    }
}

pub const ENTITY_REACH_TOLERANCE: f64 = 3.0;
const CREATIVE_ENTITY_BONUS: f64 = 2.0;
