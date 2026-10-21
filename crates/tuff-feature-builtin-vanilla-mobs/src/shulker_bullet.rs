//! Shulker bullets: they home in on their target one axis at a time,
//! stepping along a block axis toward it and turning at walls or when level
//! with it, hit for 4 and give 10 seconds of Levitation. Any hit destroys
//! them; with no target they fall.

use rand::Rng;
use rand::rngs::StdRng;
use tuff_vanilla_data::block::{Axis, Facing};
use tuff_vanilla_data::damage_type;
use tuff_vanilla_data::registry::{EntityType, MobEffect, ParticleType, SoundEvent};

use tuff_core::protocol::play::{self, MetadataValue};
use tuff_core::world::effects::ActiveEffect;
use tuff_core::world::entity::{
    AttackOutcome, Effect, EntityAttack, EntityBase, EntityHit, RegionEntity, Tick, TickContext,
    take, take_number,
};
use tuff_core::world::game_event::GameEvent;
use tuff_core::world::physics::block_pos;
use tuff_core::world::player::Difficulty;
use tuff_core::world::projectile::{Owner, block_hit, hit_source, margin, targets_along};
use tuff_core::world::{framed, packets};
use tuff_nbt::{Compound, Tag};
use tuff_world_model::BlockPos;

const SPEED: f64 = 0.15;
const GRAVITY: f64 = 0.04;
const DAMAGE: f32 = 4.0;
const LEVITATION_TICKS: i64 = 200;
const HOMING_GROWTH: f64 = 1.025;
const STEERING: f64 = 0.2;

/// Where a bullet's target is, as it steers.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Aim {
    pub position: [f64; 3],
    pub height: f64,
}

/// A bullet's homing state, apart from the entity it moves.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Homing {
    pub direction: Option<Facing>,
    pub steps: i32,
    pub delta: [f64; 3],
}

fn is_empty(
    pos: BlockPos,
    block: &dyn Fn(BlockPos) -> Option<tuff_vanilla_data::BlockState>,
) -> bool {
    block(pos).is_some_and(tuff_world_model::chunk::is_air)
}

fn offset(pos: BlockPos, face: Facing) -> BlockPos {
    let (x, y, z) = face.step();
    BlockPos::new(pos.x + x, pos.y + y, pos.z + z)
}

fn random_facing(rng: &mut StdRng) -> Facing {
    const ORDER: [Facing; 6] = [
        Facing::Down,
        Facing::Up,
        Facing::North,
        Facing::South,
        Facing::West,
        Facing::East,
    ];
    ORDER[rng.gen_range(0..6)]
}

pub fn facing_from_id(id: i64) -> Option<Facing> {
    Facing::ALL.into_iter().find(|f| f.id() as i64 == id)
}

impl Homing {
    /// Picks the next block axis to fly along toward `aim` (straight at it
    /// once within two blocks), never along `avoid`.
    pub fn select(
        &mut self,
        position: [f64; 3],
        avoid: Option<Axis>,
        aim: Option<Aim>,
        block: &dyn Fn(BlockPos) -> Option<tuff_vanilla_data::BlockState>,
        rng: &mut StdRng,
    ) {
        let (target_pos, y_offset) = match aim {
            None => {
                let here = block_pos(position);
                (BlockPos::new(here.x, here.y - 1, here.z), 0.5)
            }
            Some(aim) => {
                let y_offset = aim.height * 0.5;
                let [x, y, z] = aim.position;
                (block_pos([x, y + y_offset, z]), y_offset)
            }
        };
        let mut target = [
            f64::from(target_pos.x) + 0.5,
            f64::from(target_pos.y) + y_offset,
            f64::from(target_pos.z) + 0.5,
        ];
        let center = [
            f64::from(target_pos.x) + 0.5,
            f64::from(target_pos.y) + 0.5,
            f64::from(target_pos.z) + 0.5,
        ];
        let mut selection = None;
        if tuff_core::world::distance_sq(center, position) >= 4.0 {
            let current = block_pos(position);
            let mut options = Vec::new();
            let mut consider = |axis: Axis, here: i32, there: i32, up: Facing, down: Facing| {
                if avoid == Some(axis) {
                    return;
                }
                if here < there && is_empty(offset(current, up), block) {
                    options.push(up);
                } else if here > there && is_empty(offset(current, down), block) {
                    options.push(down);
                }
            };
            consider(Axis::X, current.x, target_pos.x, Facing::East, Facing::West);
            consider(Axis::Y, current.y, target_pos.y, Facing::Up, Facing::Down);
            consider(
                Axis::Z,
                current.z,
                target_pos.z,
                Facing::South,
                Facing::North,
            );
            let mut chosen = random_facing(rng);
            if options.is_empty() {
                let mut attempts = 5;
                while !is_empty(offset(current, chosen), block) && attempts > 0 {
                    chosen = random_facing(rng);
                    attempts -= 1;
                }
            } else {
                chosen = options[rng.gen_range(0..options.len())];
            }
            let (x, y, z) = chosen.step();
            target = [
                position[0] + f64::from(x),
                position[1] + f64::from(y),
                position[2] + f64::from(z),
            ];
            selection = Some(chosen);
        }
        self.direction = selection;
        let delta = [0, 1, 2].map(|i| target[i] - position[i]);
        let distance = delta.iter().map(|c| c * c).sum::<f64>().sqrt();
        self.delta = if distance == 0.0 {
            [0.0; 3]
        } else {
            delta.map(|c| c / distance * SPEED)
        };
        self.steps = 10 + rng.gen_range(0..5) * 10;
    }

    /// Steers `velocity` a fifth of the way to the wanted delta, which
    /// grows a little each tick.
    pub fn steer(&mut self, velocity: [f64; 3]) -> [f64; 3] {
        self.delta = self.delta.map(|c| (c * HOMING_GROWTH).clamp(-1.0, 1.0));
        [0, 1, 2].map(|i| velocity[i] + (self.delta[i] - velocity[i]) * STEERING)
    }

    /// After moving to `position`: turns when the steps run out, a solid
    /// block is ahead, or it is level with the target along its axis.
    /// Returns whether it picked a new direction.
    pub fn after_move(
        &mut self,
        position: [f64; 3],
        aim: Aim,
        block: &dyn Fn(BlockPos) -> Option<tuff_vanilla_data::BlockState>,
        rng: &mut StdRng,
    ) -> bool {
        let mut turned = false;
        if self.steps > 0 {
            self.steps -= 1;
            if self.steps == 0 {
                let axis = self.direction.map(Facing::axis);
                self.select(position, axis, Some(aim), block, rng);
                turned = true;
            }
        }
        let Some(direction) = self.direction else {
            return turned;
        };
        let current = block_pos(position);
        let axis = direction.axis();
        let ahead = offset(current, direction);
        if block(ahead).is_some_and(|state| state.is_collision_face_full(Facing::Up)) {
            self.select(position, Some(axis), Some(aim), block, rng);
            return true;
        }
        let target = block_pos(aim.position);
        let level = match axis {
            Axis::X => current.x == target.x,
            Axis::Y => current.y == target.y,
            Axis::Z => current.z == target.z,
        };
        if level {
            self.select(position, Some(axis), Some(aim), block, rng);
            return true;
        }
        turned
    }
}

#[derive(Debug)]
pub struct ShulkerBullet {
    pub base: EntityBase,
    pub owner: Owner,
    pub target: Option<u128>,
    pub homing: Homing,
    ticks: u32,
    /// Hurt by something: it breaks apart on its next tick.
    broken: bool,
    /// Already broken apart; it goes on its next tick.
    gone: bool,
    resync: bool,
}

/// The target as the bullet sees it: where it is, how tall, and whether it
/// still counts.
fn find_target(uuid: u128, cx: &TickContext) -> Option<(Aim, bool)> {
    if let Some(player) = cx.players.iter().find(|p| p.uuid == uuid) {
        let aim = Aim {
            position: player.position,
            height: player.height,
        };
        return Some((aim, true));
    }
    let entity = cx.entities.iter().find(|e| e.uuid == uuid)?;
    let aim = Aim {
        position: entity.position,
        height: entity.bounds[4] - entity.bounds[1],
    };
    Some((aim, entity.living.is_none_or(|l| l.alive)))
}

impl ShulkerBullet {
    /// A bullet leaving the middle of `owner_bounds` for `target`, first
    /// flying along any axis but `avoid` (its shooter's attachment axis).
    pub fn new(
        id: i32,
        owner_bounds: [f64; 6],
        owner: Owner,
        target: (u128, Aim),
        avoid: Option<Axis>,
        cx: &mut TickContext,
    ) -> Self {
        let position = [0, 1, 2].map(|i| (owner_bounds[i] + owner_bounds[i + 3]) / 2.0);
        let base = EntityBase::new(
            EntityType::ShulkerBullet,
            id,
            rand::random(),
            position,
            [0.0; 3],
        );
        let mut homing = Homing {
            direction: Some(Facing::Up),
            ..Homing::default()
        };
        homing.select(position, avoid, Some(target.1), cx.block, cx.rng);
        Self {
            base,
            owner,
            target: Some(target.0),
            homing,
            ticks: 0,
            broken: false,
            gone: false,
            resync: false,
        }
    }

    fn sound(&self, sound: SoundEvent, cx: &mut TickContext) {
        let packet = framed(&play::Sound {
            sound_event: sound.id(),
            category: play::sound_category::HOSTILE,
            position: self.base.position,
            volume: 1.0,
            pitch: 1.0,
            seed: cx.rng.r#gen(),
        });
        cx.effects.push(Effect::ChunkPackets(packets([packet])));
    }

    fn particles(&self, particle: ParticleType, count: i32, cx: &mut TickContext) {
        let packet = framed(&play::Particles {
            particle: particle.id(),
            position: self.base.position,
            spread: [0.2, 0.2, 0.2],
            speed: 0.0,
            count,
            alternative: false,
        });
        cx.effects.push(Effect::ChunkPackets(packets([packet])));
    }

    fn destroy(&mut self, cx: &mut TickContext) {
        self.gone = true;
        cx.effects.push(Effect::GameEvent {
            event: GameEvent::EntityDamage,
            position: self.base.position,
            source: Some(self.base.id),
        });
    }

    fn break_apart(&mut self, cx: &mut TickContext) -> Option<AttackOutcome> {
        if self.gone {
            return None;
        }
        self.sound(SoundEvent::EntityShulkerBulletHurt, cx);
        self.particles(ParticleType::Crit, 15, cx);
        self.destroy(cx);
        Some(AttackOutcome {
            hurt: true,
            health_lost: 0.0,
        })
    }
}

impl RegionEntity for ShulkerBullet {
    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn base_mut(&mut self) -> &mut EntityBase {
        &mut self.base
    }

    fn save(&self, out: &mut Compound) {
        self.owner.save(out);
        if let Some(target) = self.target {
            out.insert("Target", Tag::uuid(target));
        }
        if let Some(direction) = self.homing.direction {
            out.insert("Dir", direction.id() as i8);
        }
        out.insert("Steps", self.homing.steps);
        out.insert("TXD", self.homing.delta[0]);
        out.insert("TYD", self.homing.delta[1]);
        out.insert("TZD", self.homing.delta[2]);
    }

    fn metadata(&self) -> Vec<(u8, MetadataValue)> {
        Vec::new()
    }

    fn spawn_data(&self) -> i32 {
        self.owner.spawn_data()
    }

    fn pickable(&self) -> bool {
        true
    }

    fn hurt(
        &mut self,
        _amount: f32,
        _damage_type: &'static tuff_vanilla_data::damage_type::DamageType,
    ) -> bool {
        self.broken = true;
        true
    }

    fn attacked(&mut self, _attack: &EntityAttack, cx: &mut TickContext) -> Option<AttackOutcome> {
        self.break_apart(cx)
    }

    fn hit(&mut self, _hit: &EntityHit, cx: &mut TickContext) -> Option<AttackOutcome> {
        self.break_apart(cx)
    }

    fn take_impulse(&mut self) -> bool {
        std::mem::take(&mut self.resync)
    }

    fn tick(&mut self, cx: &mut TickContext) -> Tick {
        if self.gone {
            return Tick::Discard;
        }
        if std::mem::take(&mut self.broken) {
            self.break_apart(cx);
            return Tick::Discard;
        }
        if *cx.shared.difficulty.borrow() == Difficulty::Peaceful {
            return Tick::Discard;
        }
        self.ticks += 1;
        self.owner.has_been_shot = true;
        let skip_owner = !self.owner.left;
        let found = self.target.and_then(|uuid| find_target(uuid, cx));
        if found.is_none() {
            self.target = None;
        }
        let homing_on = found.filter(|(_, alive)| *alive).map(|(aim, _)| aim);
        self.base.velocity = match homing_on {
            Some(_) => self.homing.steer(self.base.velocity),
            None => {
                let [x, y, z] = self.base.velocity;
                [x, y - GRAVITY, z]
            }
        };
        let from = self.base.position;
        if (cx.block)(block_pos(from)).is_none() {
            return Tick::Discard;
        }
        let velocity = self.base.velocity;
        let end = [0, 1, 2].map(|i| from[i] + velocity[i]);
        let block = block_hit(from, end, cx);
        let to = block.map_or(end, |hit| hit.location);
        let owner_id = self.owner.find(cx).map(|(id, _)| id);
        let target = targets_along(
            &self.base,
            self.base.bounds(),
            from,
            to,
            margin(self.ticks),
            |id| skip_owner && Some(id) == owner_id,
            cx,
        )
        .into_iter()
        .next()
        .filter(|t| !tuff_core::world::projectile::deflect_off(&mut self.base, t, cx));
        self.base.position = end;
        self.owner.check_left(self.base.bounds(), velocity, cx);
        if let Some(target) = target {
            let mut hit = EntityHit::new(DAMAGE, &damage_type::MOB_PROJECTILE);
            hit.attacker = self.owner.culprit(cx);
            hit.projectile = Some(EntityType::ShulkerBullet);
            hit.position = Some(hit_source(target.position, velocity));
            hit.projectile_velocity = Some(velocity);
            hit.effects.push(ActiveEffect::timed(
                MobEffect::Levitation,
                LEVITATION_TICKS,
                0,
            ));
            cx.effects.push(Effect::Hurt {
                target: target.id,
                hit: Box::new(hit),
            });
            self.destroy(cx);
            return Tick::Discard;
        }
        if let Some(block) = block {
            tuff_core::world::projectile::struck_block(block, &self.base, &self.owner, cx);
            self.particles(ParticleType::Explosion, 2, cx);
            self.sound(SoundEvent::EntityShulkerBulletHit, cx);
            self.destroy(cx);
            return Tick::Discard;
        }
        let (yaw, pitch) = tuff_core::world::projectile::rotation_of(velocity);
        self.base.yaw = rotate_half(self.base.yaw, yaw);
        self.base.pitch = rotate_half(self.base.pitch, pitch);
        if let Some(aim) = found.map(|(aim, _)| aim) {
            self.resync |= self
                .homing
                .after_move(self.base.position, aim, cx.block, cx.rng);
        }
        Tick::Alive
    }
}

/// Turns `current` half way to `target`, the short way round.
fn rotate_half(current: f32, target: f32) -> f32 {
    let mut current = current;
    while target - current < -180.0 {
        current -= 360.0;
    }
    while target - current >= 180.0 {
        current += 360.0;
    }
    current + 0.5 * (target - current)
}

pub fn load(base: EntityBase, nbt: &mut Compound) -> Option<Box<dyn RegionEntity>> {
    let mut owner = Owner::default();
    owner.load(nbt);
    let target = take(nbt, "Target", Tag::as_uuid);
    let direction = take_number(nbt, "Dir").and_then(|id: f64| facing_from_id(id as i64));
    let homing = Homing {
        direction,
        steps: take_number(nbt, "Steps").map_or(0, |s: f64| s as i32),
        delta: ["TXD", "TYD", "TZD"].map(|key| take_number(nbt, key).unwrap_or(0.0)),
    };
    Some(Box::new(ShulkerBullet {
        base,
        owner,
        target,
        homing,
        ticks: 0,
        broken: false,
        gone: false,
        resync: false,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use tuff_vanilla_data::Block;

    fn open_air(_: BlockPos) -> Option<tuff_vanilla_data::BlockState> {
        Some(Block::Air.default_state())
    }

    fn aim_at(position: [f64; 3]) -> Aim {
        Aim {
            position,
            height: 1.8,
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_selected_step_moves_along_one_axis_toward_the_target() {
        let mut rng = StdRng::seed_from_u64(7);
        for _ in 0..50 {
            let mut homing = Homing::default();
            homing.select(
                [0.5, 0.5, 0.5],
                Some(Axis::Y),
                Some(aim_at([10.5, 0.0, -10.5])),
                &open_air,
                &mut rng,
            );
            let direction = homing.direction.expect("far from the target");
            assert!(
                matches!(direction, Facing::East | Facing::North),
                "{direction:?}"
            );
            let speed = homing.delta.iter().map(|c| c * c).sum::<f64>().sqrt();
            assert!((speed - SPEED).abs() < 1e-9);
            let (x, y, z) = direction.step();
            assert!((homing.delta[0] - f64::from(x) * SPEED).abs() < 1e-9);
            assert!((homing.delta[1] - f64::from(y) * SPEED).abs() < 1e-9);
            assert!((homing.delta[2] - f64::from(z) * SPEED).abs() < 1e-9);
            assert!((10..=50).contains(&homing.steps) && homing.steps % 10 == 0);
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn close_to_the_target_it_flies_straight_at_it() {
        let mut rng = StdRng::seed_from_u64(1);
        let mut homing = Homing::default();
        homing.select(
            [0.5, 0.5, 0.5],
            None,
            Some(aim_at([1.5, 0.0, 0.5])),
            &open_air,
            &mut rng,
        );
        assert_eq!(homing.direction, None);
        assert!(homing.delta[0] > 0.0);
        assert!((homing.delta[1] - 0.4 / 1.0770329614269007 * SPEED).abs() < 1e-9);
    }

    #[tuff_core::watchdog]
    #[test]
    fn walls_turn_it_aside() {
        let wall = |pos: BlockPos| {
            Some(if pos.x == 1 {
                Block::Stone.default_state()
            } else {
                Block::Air.default_state()
            })
        };
        let mut rng = StdRng::seed_from_u64(3);
        let mut homing = Homing::default();
        homing.select(
            [0.5, 0.5, 0.5],
            Some(Axis::Y),
            Some(aim_at([10.5, 0.0, 0.5])),
            &wall,
            &mut rng,
        );
        assert_ne!(homing.direction, Some(Facing::East), "the way east is shut");
        homing.direction = Some(Facing::East);
        homing.steps = 30;
        assert!(homing.after_move([0.5, 0.5, 0.5], aim_at([10.5, 0.0, 0.5]), &wall, &mut rng));
        assert_ne!(homing.direction, Some(Facing::East));
        assert_ne!(homing.direction.map(Facing::axis), Some(Axis::X));
    }

    #[tuff_core::watchdog]
    #[test]
    fn it_turns_once_level_with_the_target_on_its_axis() {
        let mut rng = StdRng::seed_from_u64(5);
        let mut homing = Homing {
            direction: Some(Facing::East),
            steps: 30,
            delta: [SPEED, 0.0, 0.0],
        };
        let aim = aim_at([4.5, 0.0, 8.5]);
        assert!(!homing.after_move([2.5, 0.5, 0.5], aim, &open_air, &mut rng));
        assert_eq!(homing.steps, 29);
        assert!(homing.after_move([4.5, 0.5, 0.5], aim, &open_air, &mut rng));
        assert_ne!(homing.direction.map(Facing::axis), Some(Axis::X));
    }

    #[tuff_core::watchdog]
    #[test]
    fn steering_eases_toward_a_growing_delta() {
        let mut homing = Homing {
            direction: Some(Facing::Up),
            steps: 10,
            delta: [0.0, SPEED, 0.0],
        };
        let velocity = homing.steer([0.0; 3]);
        assert!((homing.delta[1] - SPEED * HOMING_GROWTH).abs() < 1e-12);
        assert!((velocity[1] - SPEED * HOMING_GROWTH * STEERING).abs() < 1e-12);
        for _ in 0..1000 {
            homing.steer([0.0; 3]);
        }
        assert_eq!(homing.delta[1], 1.0, "clamped");
    }
}
