//! Primed TNT: how it falls, counts down and explodes. The explosion
//! itself is core's ([`tuff_core::world::explosion`]).

use rand::Rng;
use tuff_vanilla_data::damage_type;
use tuff_vanilla_data::registry::EntityType;
use tuff_vanilla_data::{Block, BlockState};

use tuff_core::protocol::play::{MetadataValue, metadata};
use tuff_core::world::entity::{
    BlockLookup, EntityBase, RegionEntity, Tick, TickContext, take, take_number,
};
use tuff_core::world::messages::Igniter;
use tuff_core::world::{fall, physics};
use tuff_nbt::{Compound, Tag};
use tuff_world_model::BlockPos;

pub use tuff_core::world::explosion::*;

/// The fuse primed TNT gets when lit, in ticks.
pub const TNT_FUSE: i32 = 80;
pub const TNT_POWER: f32 = 4.0;
const MAX_TNT_POWER: f32 = 128.0;
/// Primed TNT explodes this far above its feet.
pub const TNT_EXPLOSION_HEIGHT: f64 = 0.0625;
const TNT_GRAVITY: f64 = 0.04;
const TNT_DRAG: f64 = 0.98;
/// How much of a block's bounce things that are not alive keep.
const NON_LIVING_BOUNCE: f64 = 0.8;

/// Lights vanilla primed TNT for core.
pub struct VanillaTnt;

impl TntPrimer for VanillaTnt {
    fn prime(
        &self,
        id: i32,
        uuid: u128,
        pos: BlockPos,
        fuse: Option<i32>,
        igniter: Option<Igniter>,
        rng: &mut rand::rngs::StdRng,
    ) -> Box<dyn RegionEntity> {
        Box::new(PrimedTnt::lit(id, uuid, pos, fuse.unwrap_or(TNT_FUSE), rng).with_igniter(igniter))
    }

    fn chain_fuse(&self, rng: &mut rand::rngs::StdRng) -> i32 {
        PrimedTnt::chain_fuse(rng)
    }
}

/// A lit TNT block, falling and counting down.
#[derive(Debug, Clone)]
pub struct PrimedTnt {
    pub base: EntityBase,
    pub fuse: i32,
    pub power: f32,
    pub block: BlockState,
    /// The UUID of the player who lit it, as saved.
    pub owner: Option<u128>,
    /// That player, while known: the damage type and kill credit.
    pub igniter: Option<Igniter>,
    /// Set once it went through a portal: its blast spares nether portals.
    pub used_portal: bool,
}

pub fn load_tnt(base: EntityBase, nbt: &mut Compound) -> Option<Box<dyn RegionEntity>> {
    let fuse = take_number(nbt, "fuse").map_or(TNT_FUSE, |v| v as i16 as i32);
    let block = take(nbt, "block_state", |tag| {
        let name = match tag {
            Tag::String(name) => Some(name.as_str()),
            Tag::Compound(state) => state.get_str("id").or_else(|| state.get_str("Name")),
            _ => None,
        };
        Block::from_name(name?)?;
        Some(tuff_core::storage::chunk_nbt::decode_block_state(tag))
    })
    .unwrap_or(Block::Tnt.default_state());
    let power = take_number(nbt, "explosion_power")
        .map_or(TNT_POWER, |v| (v as f32).clamp(0.0, MAX_TNT_POWER));
    let owner = take(nbt, "owner", Tag::as_uuid);
    Some(Box::new(PrimedTnt {
        base,
        fuse,
        power,
        block,
        owner,
        igniter: None,
        used_portal: false,
    }))
}

impl PrimedTnt {
    /// TNT lit at `pos`: centered, hopping up with a small random sideways
    /// nudge.
    pub fn lit(id: i32, uuid: u128, pos: BlockPos, fuse: i32, rng: &mut impl Rng) -> Self {
        let angle = rng.r#gen::<f64>() * std::f64::consts::TAU;
        Self {
            base: EntityBase::new(
                EntityType::Tnt,
                id,
                uuid,
                [pos.x as f64 + 0.5, pos.y as f64, pos.z as f64 + 0.5],
                [-angle.sin() * 0.02, 0.2, -angle.cos() * 0.02],
            ),
            fuse,
            power: TNT_POWER,
            block: Block::Tnt.default_state(),
            owner: None,
            igniter: None,
            used_portal: false,
        }
    }

    pub fn with_igniter(mut self, igniter: Option<Igniter>) -> Self {
        self.owner = igniter.as_ref().map(|i| i.uuid);
        self.igniter = igniter;
        self
    }

    /// The fuse the game gives TNT caught in another explosion.
    pub fn chain_fuse(rng: &mut impl Rng) -> i32 {
        rng.gen_range(0..TNT_FUSE / 4) + TNT_FUSE / 8
    }

    pub fn bounds(&self) -> [f64; 6] {
        self.base.bounds()
    }

    /// Moves one tick and burns the fuse; true once it runs out.
    pub fn step_tick(&mut self, block: BlockLookup) -> bool {
        let gravity = if self.base.no_gravity {
            0.0
        } else {
            TNT_GRAVITY
        };
        self.base.velocity[1] -= gravity;
        self.step(gravity, block);
        let factor = self.block_speed_factor(block);
        self.base.velocity[0] *= factor;
        self.base.velocity[2] *= factor;
        if tuff_core::world::block_contact::bubble_column_push(
            self.base.bounds(),
            &mut self.base.velocity,
            block,
        ) {
            self.base.fall_distance = 0.0;
        }
        let velocity = &mut self.base.velocity;
        for v in velocity.iter_mut() {
            *v *= TNT_DRAG;
        }
        if self.base.on_ground {
            velocity[0] *= 0.7;
            velocity[1] *= -0.5;
            velocity[2] *= 0.7;
        }
        self.fuse -= 1;
        self.fuse <= 0
    }

    /// Lets the currents of the fluids it sits in push it, after a tick it
    /// did not explode in.
    pub fn push_by_fluids(&mut self, immersion: &tuff_core::world::contact::Immersion) {
        immersion.push(&mut self.base.velocity);
    }

    fn block_speed_factor(&self, block: BlockLookup) -> f64 {
        let factor_at = |pos: BlockPos| {
            block(pos)
                .unwrap_or(Block::Air.default_state())
                .block()
                .movement()
                .speed_factor
        };
        let [x, y, z] = self.base.position;
        let here = BlockPos::new(x.floor() as i32, y.floor() as i32, z.floor() as i32);
        let in_fluid =
            block(here).is_some_and(|s| matches!(s.block(), Block::Water | Block::BubbleColumn));
        let mut factor = factor_at(here);
        if !in_fluid && factor == 1.0 {
            let below =
                tuff_core::world::block_contact::on_pos(self.base.position, None, 0.500_001, block);
            factor = factor_at(below);
        }
        f64::from(factor)
    }

    /// Moves by the velocity through the blocks' collision boxes, stopping
    /// on the axes it hits, or bouncing off a bouncy block below.
    fn step(&mut self, gravity: f64, block: BlockLookup) {
        let movement = self.base.velocity;
        let moved = physics::collide(
            self.base.bounds(),
            movement,
            0.0,
            self.base.on_ground,
            block,
        );
        for axis in 0..3 {
            self.base.position[axis] += moved[axis];
        }
        let differs = |a: f64, b: f64| (a - b).abs() >= 1.0e-5;
        let vertical = moved[1] != movement[1];
        self.base.on_ground = vertical && movement[1] < 0.0;
        for axis in [0, 2] {
            if differs(moved[axis], movement[axis]) {
                self.base.velocity[axis] = 0.0;
            }
        }
        if !vertical {
            return;
        }
        let restitution = if self.base.on_ground {
            let [x, y, z] = self.base.position;
            let below = BlockPos::new(x.floor() as i32, (y - 0.2).floor() as i32, z.floor() as i32);
            fall::bounce_restitution(block(below), -movement[1], gravity, false, 0.0)
                * NON_LIVING_BOUNCE
        } else {
            0.0
        };
        self.base.velocity[1] = if restitution > 0.0 {
            let portion = moved[1] / movement[1];
            let drag = 1.0 + portion * (TNT_DRAG - 1.0);
            (portion * gravity - movement[1]) * drag * restitution
        } else {
            0.0
        };
    }
}

impl RegionEntity for PrimedTnt {
    fn used_portal(&mut self) {
        self.used_portal = true;
    }

    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn base_mut(&mut self) -> &mut EntityBase {
        &mut self.base
    }

    fn save(&self, out: &mut Compound) {
        out.insert(
            "fuse",
            self.fuse.clamp(i16::MIN.into(), i16::MAX.into()) as i16,
        );
        out.insert(
            "block_state",
            tuff_core::storage::chunk_nbt::encode_block_state(self.block),
        );
        if self.power != TNT_POWER {
            out.insert("explosion_power", self.power);
        }
        if let Some(owner) = self.owner {
            out.insert("owner", Tag::uuid(owner));
        }
    }

    fn metadata(&self) -> Vec<(u8, MetadataValue)> {
        vec![
            (metadata::TNT_FUSE, MetadataValue::VarInt(self.fuse)),
            (
                metadata::TNT_BLOCK_STATE,
                MetadataValue::BlockState(self.block.id()),
            ),
        ]
    }

    fn display_name(&self) -> String {
        "Primed TNT".to_owned()
    }

    fn ticking_data(&self) -> bool {
        true
    }

    fn tick(&mut self, context: &mut TickContext) -> Tick {
        let done = self.step_tick(context.block);
        if !done {
            self.push_by_fluids(&tuff_core::world::contact::immersion(
                context,
                self.base.bounds(),
                self.base.eye_position(),
            ));
            return Tick::Alive;
        }
        if !context.shared.rule_bool("tnt_explodes") {
            return Tick::Discard;
        }
        if self.igniter.is_none()
            && let Some(owner) = self.owner
        {
            self.igniter = context
                .players
                .iter()
                .find(|player| player.uuid == owner)
                .map(|player| Igniter {
                    id: player.id,
                    uuid: player.uuid,
                    name: player.name.to_owned(),
                });
        }
        let [x, y, z] = self.base.position;
        let blocks = if context.shared.rule_bool("tnt_explosion_drop_decay") {
            BlockInteraction::DestroyWithDecay
        } else {
            BlockInteraction::Destroy
        };
        let damage_type = if self.igniter.is_some() {
            &damage_type::PLAYER_EXPLOSION
        } else {
            &damage_type::EXPLOSION
        };
        Tick::Explode(Explosion {
            center: [x, y + TNT_EXPLOSION_HEIGHT, z],
            radius: self.power,
            fire: false,
            blocks,
            damage_type,
            igniter: self.igniter.take(),
            culprit: None,
            dangerous_wither_skull: false,
            spares_nether_portals: self.used_portal,
        })
    }
}

#[cfg(test)]
mod tests {
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn a_water_current_pushes_primed_tnt() {
        use tuff_core::world::contact::{Current, Immersion, Submersion};
        let mut rng = StdRng::seed_from_u64(3);
        let mut tnt = PrimedTnt::lit(1, 1, BlockPos::new(0, 64, 0), TNT_FUSE, &mut rng);
        tnt.base.velocity = [0.0; 3];
        let water = Immersion {
            submersion: Submersion {
                water: 1.0,
                water_current: Current {
                    sum: [1.0, 0.0, 0.0],
                    count: 1,
                },
                ..Submersion::default()
            },
            ..Immersion::default()
        };
        tnt.push_by_fluids(&Immersion::default());
        assert_eq!(tnt.base.velocity, [0.0; 3]);
        tnt.push_by_fluids(&water);
        assert!(tnt.base.velocity[0] > 0.0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn primed_tnt_hops_lands_and_runs_down_its_fuse() {
        let mut rng = StdRng::seed_from_u64(3);
        let mut tnt = PrimedTnt::lit(1, 1, BlockPos::new(0, 64, 0), TNT_FUSE, &mut rng);
        let floor = |pos: BlockPos| {
            Some(if pos.y < 64 {
                Block::Stone.default_state()
            } else {
                Block::Air.default_state()
            })
        };
        let mut peak: f64 = 0.0;
        for tick in 1..=TNT_FUSE {
            let done = tnt.step_tick(&floor);
            peak = peak.max(tnt.base.position[1]);
            assert_eq!(done, tick == TNT_FUSE);
        }
        assert!(peak > 64.3, "lit TNT hops");
        assert!(tnt.base.on_ground || (tnt.base.position[1] - 64.0).abs() < 1e-6);
        assert!((tnt.base.position[1] - 64.0).abs() < 1e-6);
        let fuse = PrimedTnt::chain_fuse(&mut rng);
        assert!((10..30).contains(&fuse));
    }

    #[tuff_core::watchdog]
    #[test]
    fn primed_tnt_slows_on_soul_sand() {
        let factor = |floor_block: Block| {
            let mut rng = StdRng::seed_from_u64(3);
            let tnt = PrimedTnt::lit(1, 1, BlockPos::new(0, 64, 0), TNT_FUSE, &mut rng);
            let level = |pos: BlockPos| {
                Some(if pos.y < 64 {
                    floor_block.default_state()
                } else {
                    Block::Air.default_state()
                })
            };
            tnt.block_speed_factor(&level)
        };
        assert_eq!(factor(Block::Stone), 1.0);
        assert!((factor(Block::SoulSand) - 0.4).abs() < 1e-6);
    }

    #[tuff_core::watchdog]
    #[test]
    fn primed_tnt_rests_on_the_real_shape_of_slabs_and_fences() {
        let mut rng = StdRng::seed_from_u64(3);
        let mut tnt = PrimedTnt::lit(1, 1, BlockPos::new(0, 66, 0), TNT_FUSE, &mut rng);
        tnt.base.velocity = [0.0; 3];
        let level = |pos: BlockPos| {
            Some(match (pos.x, pos.y, pos.z) {
                (0, 64, 0) => Block::StoneSlab.default_state(),
                (_, y, _) if y < 64 => Block::Stone.default_state(),
                _ => Block::Air.default_state(),
            })
        };
        for _ in 0..40 {
            tnt.step_tick(&level);
        }
        assert!(
            (tnt.base.position[1] - 64.5).abs() < 1e-6,
            "{:?}",
            tnt.base.position
        );
        assert!(tnt.base.on_ground);

        let mut tnt = PrimedTnt::lit(2, 2, BlockPos::new(0, 66, 0), TNT_FUSE, &mut rng);
        tnt.base.velocity = [0.0; 3];
        let posts = |pos: BlockPos| {
            Some(match (pos.x, pos.y, pos.z) {
                (0, 64, 0) => Block::OakFence.default_state(),
                (_, y, _) if y < 64 => Block::Stone.default_state(),
                _ => Block::Air.default_state(),
            })
        };
        for _ in 0..40 {
            tnt.step_tick(&posts);
        }
        assert!(
            (tnt.base.position[1] - 65.5).abs() < 1e-6,
            "{:?}",
            tnt.base.position
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn primed_tnt_rises_in_an_upward_bubble_column() {
        let mut rng = StdRng::seed_from_u64(3);
        let mut tnt = PrimedTnt::lit(1, 1, BlockPos::new(0, 60, 0), TNT_FUSE, &mut rng);
        tnt.base.velocity = [0.0; 3];
        let column = Block::BubbleColumn
            .default_state()
            .with("drag", "false")
            .unwrap();
        let level = move |pos: BlockPos| {
            Some(if pos.y < 70 {
                column
            } else {
                Block::Water.default_state()
            })
        };
        for _ in 0..20 {
            tnt.step_tick(&level);
        }
        assert!(
            tnt.base.position[1] > 61.0,
            "rose to {}",
            tnt.base.position[1]
        );
    }
}
