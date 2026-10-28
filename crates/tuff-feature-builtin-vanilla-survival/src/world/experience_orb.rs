//! The experience orb entity: movement, player attraction and pickup.

use rand::Rng;
use tuff_core::protocol::play::{MetadataValue, metadata};
use tuff_core::world::contact::{Immersion, immersion};
use tuff_core::world::entity::{EntityBase, RegionEntity, Tick, TickContext, take, take_number};
use tuff_core::world::experience::{MERGE_GROUPS, MERGE_SCAN_PERIOD, Orb, intersects};
use tuff_core::world::items::blocks_items;
use tuff_core::world::{PLAYER_HALF_WIDTH, PLAYER_HEIGHT, distance_sq};
use tuff_nbt::Compound;
use tuff_vanilla_data::damage_type::DamageType;
use tuff_vanilla_data::registry::EntityType;
use tuff_world_model::BlockPos;

const GRAVITY: f64 = 0.03;
const LAVA_KICK_UP: f64 = 0.20000000298023224;
const AIR_DRAG: f64 = 0.98;
const BOUNCE: f64 = 0.4;
const HALF_WIDTH: f64 = 0.25;
const HEIGHT: f64 = 0.5;
const DEFAULT_HEALTH: i16 = 5;
const FOLLOW_DISTANCE: f64 = 8.0;
const FOLLOW_STRENGTH: f64 = 0.1;
const EYE_HEIGHT: f64 = 1.62;

#[derive(Debug, Clone)]
pub struct ExperienceOrb {
    pub base: EntityBase,
    pub value: i32,
    pub count: i32,
    pub age: u32,
    pub health: i16,
    /// Ticks lived since spawning here, for the merge scan period.
    pub ticks: u32,
    pub following: Option<i32>,
}

pub fn load(base: EntityBase, nbt: &mut Compound) -> Option<Box<dyn RegionEntity>> {
    let short = |value: f64| (value as i16).max(0);
    Some(Box::new(ExperienceOrb {
        base,
        health: take_number(nbt, "Health").map_or(DEFAULT_HEALTH, |v| v as i16),
        age: take_number(nbt, "Age").map_or(0, |v| short(v) as u32),
        value: take_number(nbt, "Value").map_or(0, |v| short(v).into()),
        count: take(nbt, "Count", |tag| tag.as_int().filter(|count| *count > 0)).unwrap_or(1),
        ticks: 0,
        following: None,
    }))
}

impl Orb for ExperienceOrb {
    fn value(&self) -> i32 {
        self.value
    }

    fn count(&self) -> i32 {
        self.count
    }

    fn age(&self) -> u32 {
        self.age
    }

    fn merges_with(&self, id: i32, value: i32) -> bool {
        ExperienceOrb::merges_with(self, id, value)
    }

    fn merge_due(&self) -> bool {
        self.ticks % MERGE_SCAN_PERIOD == 1
    }

    fn absorb(&mut self, count: i32, age: u32) {
        self.count += count;
        self.age = self.age.min(age);
    }

    fn touches(&self, feet: [f64; 3]) -> bool {
        ExperienceOrb::touches(self, feet)
    }

    fn take_one(&mut self) -> bool {
        self.count -= 1;
        self.count > 0
    }
}

impl ExperienceOrb {
    /// A freshly awarded orb with the game's random initial velocity.
    pub fn new(id: i32, position: [f64; 3], value: i32, rng: &mut impl Rng) -> Self {
        let velocity = [
            (rng.r#gen::<f64>() * 0.2 - 0.1) * 2.0,
            rng.r#gen::<f64>() * 0.2 * 2.0,
            (rng.r#gen::<f64>() * 0.2 - 0.1) * 2.0,
        ];
        Self {
            base: EntityBase::new(
                EntityType::ExperienceOrb,
                id,
                rng.r#gen(),
                position,
                velocity,
            ),
            value,
            count: 1,
            age: 0,
            health: DEFAULT_HEALTH,
            ticks: 0,
            following: None,
        }
    }

    pub fn bounds(&self) -> [f64; 6] {
        self.base.bounds()
    }

    /// Whether this orb can merge into one with `id` and `value`: same
    /// value and the same id group.
    pub fn merges_with(&self, id: i32, value: i32) -> bool {
        self.value == value && (self.base.id - id) % MERGE_GROUPS == 0
    }

    /// Whether a player standing at `feet` touches this orb: the player's
    /// box grown by one block sideways and half a block vertically.
    pub fn touches(&self, feet: [f64; 3]) -> bool {
        let orb = self.bounds();
        let player = [
            feet[0] - PLAYER_HALF_WIDTH - 1.0,
            feet[1] - 0.5,
            feet[2] - PLAYER_HALF_WIDTH - 1.0,
            feet[0] + PLAYER_HALF_WIDTH + 1.0,
            feet[1] + PLAYER_HEIGHT + 0.5,
            feet[2] + PLAYER_HALF_WIDTH + 1.0,
        ];
        intersects(orb, player)
    }

    /// Picks the player to follow: the current one while within 8 blocks,
    /// else the nearest within 8 blocks. `players` are `(id, feet)`.
    pub fn choose_target(&mut self, players: &[(i32, [f64; 3])]) -> Option<[f64; 3]> {
        let limit = FOLLOW_DISTANCE * FOLLOW_DISTANCE;
        let current = self
            .following
            .and_then(|id| players.iter().find(|(player, _)| *player == id))
            .filter(|(_, feet)| distance_sq(*feet, self.base.position) <= limit);
        let target = current.or_else(|| {
            players
                .iter()
                .map(|player| (distance_sq(player.1, self.base.position), player))
                .filter(|(distance, _)| *distance <= limit)
                .min_by(|a, b| a.0.total_cmp(&b.0))
                .map(|(_, player)| player)
        });
        self.following = target.map(|(id, _)| *id);
        target.map(|(_, feet)| *feet)
    }

    /// Advances one tick. `solid(x, y, z)` reports whether the block at
    /// those block coordinates has a collision box; `immersion` is what the
    /// orb feels of fluids, and `lava_kick` the random bounce it takes when
    /// its block is lava. `target` is the followed player's feet.
    pub fn step_tick(
        &mut self,
        solid: impl Fn(i32, i32, i32) -> bool,
        slip_below: impl Fn([f64; 3]) -> f32,
        immersion: &Immersion,
        lava_kick: Option<[f64; 3]>,
        target: Option<[f64; 3]>,
    ) {
        self.ticks = self.ticks.wrapping_add(1);
        let block = |p: [f64; 3], f: &dyn Fn(i32, i32, i32) -> bool| {
            f(
                p[0].floor() as i32,
                p[1].floor() as i32,
                p[2].floor() as i32,
            )
        };
        immersion.push(&mut self.base.velocity);
        let colliding = block(self.base.position, &solid);
        if immersion.eyes_in_water {
            self.base.velocity[0] *= 0.99;
            self.base.velocity[1] = (self.base.velocity[1] + 5.0e-4).min(0.06);
            self.base.velocity[2] *= 0.99;
        } else if !colliding && !self.base.no_gravity {
            self.base.velocity[1] -= GRAVITY;
        }
        if let Some(kick) = lava_kick {
            self.base.velocity = kick;
        }
        if let Some(feet) = target {
            let delta = [
                feet[0] - self.base.position[0],
                feet[1] + EYE_HEIGHT / 2.0 - self.base.position[1],
                feet[2] - self.base.position[2],
            ];
            let length = delta.iter().map(|d| d * d).sum::<f64>().sqrt();
            if length > 1.0e-4 {
                let power = 1.0 - length / FOLLOW_DISTANCE;
                let scale = power * power * FOLLOW_STRENGTH / length;
                for axis in 0..3 {
                    self.base.velocity[axis] += delta[axis] * scale;
                }
            }
        }
        let fall_speed = self.base.velocity[1];
        if colliding {
            self.base.position[1] = self.base.position[1].floor() + 1.0;
            self.base.velocity[1] = self.base.velocity[1].max(0.0);
        }
        for axis in [0, 2] {
            let mut next = self.base.position;
            let edge = self.base.velocity[axis].signum() * HALF_WIDTH;
            next[axis] += self.base.velocity[axis] + edge;
            if block(next, &solid) {
                self.base.velocity[axis] = 0.0;
            } else {
                self.base.position[axis] += self.base.velocity[axis];
            }
        }
        let mut next = self.base.position;
        next[1] += self.base.velocity[1];
        let mut landed = false;
        self.base.on_ground = false;
        if self.base.velocity[1] < 0.0 && block(next, &solid) {
            self.base.position[1] = next[1].floor() + 1.0;
            self.base.velocity[1] = 0.0;
            self.base.on_ground = true;
            landed = true;
        } else if self.base.velocity[1] > 0.0 && block([next[0], next[1] + HEIGHT, next[2]], &solid)
        {
            self.base.velocity[1] = 0.0;
        } else {
            self.base.position[1] = next[1];
        }
        let mut friction = AIR_DRAG;
        if self.base.on_ground {
            friction = f64::from(0.98_f32 * slip_below(self.base.position));
        }
        for component in &mut self.base.velocity {
            *component *= friction;
        }
        if landed && fall_speed < -GRAVITY {
            self.base.velocity[1] = -fall_speed * BOUNCE;
        }
        self.age += 1;
    }
}

impl RegionEntity for ExperienceOrb {
    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn base_mut(&mut self) -> &mut EntityBase {
        &mut self.base
    }

    fn save(&self, out: &mut Compound) {
        out.insert("Health", self.health);
        out.insert("Age", self.age.min(i16::MAX as u32) as i16);
        out.insert("Value", self.value.clamp(0, i16::MAX as i32) as i16);
        out.insert("Count", self.count);
    }

    fn metadata(&self) -> Vec<(u8, MetadataValue)> {
        vec![(
            metadata::EXPERIENCE_VALUE,
            MetadataValue::VarInt(self.value),
        )]
    }

    fn display_name(&self) -> String {
        "Experience Orb".to_owned()
    }

    fn orb(&self) -> Option<&dyn Orb> {
        Some(self)
    }

    fn orb_mut(&mut self) -> Option<&mut dyn Orb> {
        Some(self)
    }

    fn experience(&self) -> i32 {
        self.value * self.count
    }

    fn hurt(&mut self, amount: f32, _damage_type: &'static DamageType) -> bool {
        self.health = (f32::from(self.health) - amount) as i16;
        self.health <= 0
    }

    fn tick(&mut self, context: &mut TickContext) -> Tick {
        let players: Vec<(i32, [f64; 3])> = context
            .players
            .iter()
            .map(|player| (player.id, player.position))
            .collect();
        let target = self.choose_target(&players);
        let block = context.block;
        let immersion = immersion(context, self.base.bounds(), self.base.eye_position());
        let [x, y, z] = self.base.position.map(|c| c.floor() as i32);
        let lava_kick = block(BlockPos::new(x, y, z))
            .is_some_and(|state| {
                tuff_datapack::tags::is(
                    tuff_vanilla_data::tag::fluid::key::LAVA,
                    state.fluid_state().fluid(),
                )
            })
            .then(|| {
                let rng = &mut *context.rng;
                let mut spread = || f64::from((rng.r#gen::<f32>() - rng.r#gen::<f32>()) * 0.2);
                let x = spread();
                let z = spread();
                [x, LAVA_KICK_UP, z]
            });
        let start = self.base.position;
        let webbed = tuff_core::world::items::in_cobweb(self.base.bounds(), block);
        self.step_tick(
            |x, y, z| blocks_items(block(BlockPos::new(x, y, z))),
            |position| {
                let below =
                    tuff_core::world::block_contact::on_pos(position, None, 0.500_001, block);
                block(below).map_or(0.6, |state| state.block().movement().friction)
            },
            &immersion,
            lava_kick,
            target,
        );
        if webbed {
            tuff_core::world::items::hold_in_cobweb(&mut self.base, start);
        }
        tuff_core::world::physics::stop_at_moving_blocks(&mut self.base, start, context.pistons);
        let bounds = self.base.bounds();
        if tuff_core::world::block_contact::bubble_column_push(
            bounds,
            &mut self.base.velocity,
            block,
        ) {
            self.base.fall_distance = 0.0;
        }
        if tuff_core::world::block_contact::pricked_by_cactus(self, block) {
            return Tick::Discard;
        }
        if self.age
            >= tuff_core::world::despawn_settings::DespawnSettings::from_config(
                &context.shared.config,
            )
            .orb_age
        {
            Tick::Discard
        } else {
            Tick::Alive
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand::rngs::StdRng;
    use tuff_vanilla_data::Block;

    #[tuff_core::watchdog]
    #[test]
    fn orbs_rise_while_their_eyes_are_in_water() {
        let water = Immersion {
            eyes_in_water: true,
            ..Default::default()
        };
        let mut orb = ExperienceOrb::new(1, [0.5, 60.0, 0.5], 1, &mut StdRng::seed_from_u64(1));
        orb.base.velocity = [0.0, 0.1, 0.0];
        orb.step_tick(|_, _, _| false, |_| 0.6, &water, None, None);
        assert!((orb.base.velocity[1] - 0.06 * AIR_DRAG).abs() < 1e-9);
        let start = orb.base.position[1];
        for _ in 0..40 {
            orb.step_tick(|_, _, _| false, |_| 0.6, &water, None, None);
        }
        assert!(orb.base.position[1] > start);
    }

    #[tuff_core::watchdog]
    #[test]
    fn lava_bounces_orbs_up() {
        let mut orb = ExperienceOrb::new(1, [0.5, 60.0, 0.5], 1, &mut StdRng::seed_from_u64(1));
        orb.step_tick(
            |_, _, _| false,
            |_| 0.6,
            &Immersion::default(),
            Some([0.05, LAVA_KICK_UP, -0.05]),
            None,
        );
        assert!((orb.base.velocity[1] - LAVA_KICK_UP * AIR_DRAG).abs() < 1e-9);
        assert!(orb.base.velocity[0] > 0.0 && orb.base.velocity[2] < 0.0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn orbs_fall_bounce_and_settle() {
        let floor = |_x, y, _z| y <= 71;
        let mut rng = StdRng::seed_from_u64(1);
        let mut orb = ExperienceOrb::new(1, [0.5, 75.0, 0.5], 3, &mut rng);
        orb.base.velocity = [0.0; 3];
        let mut bounced = false;
        for _ in 0..200 {
            orb.step_tick(floor, |_| 0.6, &Immersion::default(), None, None);
            if orb.base.on_ground && orb.base.velocity[1] > 0.0 {
                bounced = true;
            }
        }
        assert!(bounced);
        assert!(orb.base.on_ground);
        assert!((orb.base.position[1] - 72.0).abs() < 1e-9);
        assert!(orb.base.velocity[1].abs() < 1e-3);
    }

    #[tuff_core::watchdog]
    #[test]
    fn orbs_slide_further_on_slippery_ground() {
        let floor = |_x, y, _z| y <= 71;
        let mut rng = StdRng::seed_from_u64(1);
        let mut travelled = Vec::new();
        for slip in [0.6_f32, 0.98] {
            let mut orb = ExperienceOrb::new(1, [0.5, 72.0, 0.5], 3, &mut rng);
            orb.base.velocity = [0.3, 0.0, 0.0];
            orb.step_tick(floor, move |_| slip, &Immersion::default(), None, None);
            travelled.push(orb.base.velocity[0]);
        }
        assert!(travelled[1] > travelled[0] * 1.5, "{travelled:?}");
    }

    #[tuff_core::watchdog]
    #[test]
    fn orbs_drift_towards_a_nearby_player_only() {
        let floor = |_x, y, _z| y <= 71;
        let mut rng = StdRng::seed_from_u64(2);
        let mut orb = ExperienceOrb::new(1, [0.5, 72.0, 0.5], 3, &mut rng);
        orb.base.velocity = [0.0; 3];
        assert_eq!(orb.choose_target(&[(9, [20.0, 72.0, 0.5])]), None);
        let target = orb.choose_target(&[(9, [5.0, 72.0, 0.5]), (8, [6.0, 72.0, 0.5])]);
        assert_eq!(target, Some([5.0, 72.0, 0.5]));
        assert_eq!(orb.following, Some(9));
        for _ in 0..40 {
            orb.step_tick(floor, |_| 0.6, &Immersion::default(), None, target);
        }
        assert!(orb.base.position[0] > 1.5);
        assert!(orb.touches([orb.base.position[0] + 1.0, 72.0, 0.5]));
        assert!(!orb.touches([orb.base.position[0] + 3.0, 72.0, 0.5]));
    }

    #[tuff_core::watchdog]
    #[test]
    fn merging_needs_the_same_value_and_id_group() {
        let mut rng = StdRng::seed_from_u64(0);
        let orb = ExperienceOrb::new(45, [0.0; 3], 7, &mut rng);
        assert!(orb.merges_with(5, 7));
        assert!(orb.merges_with(85, 7));
        assert!(!orb.merges_with(6, 7));
        assert!(!orb.merges_with(5, 3));
    }

    #[tuff_core::watchdog]
    #[test]
    fn cactus_destroys_a_resting_orb() {
        let cactus =
            |pos: BlockPos| (pos == BlockPos::new(0, 64, 0)).then(|| Block::Cactus.default_state());
        let mut rng = StdRng::seed_from_u64(1);
        let mut orb = ExperienceOrb::new(2, [0.5, 65.0, 0.5], 3, &mut rng);
        let destroyed = (1..=10)
            .find(|_| tuff_core::world::block_contact::pricked_by_cactus(&mut orb, &cactus));
        assert_eq!(destroyed, Some(5));
    }
}
