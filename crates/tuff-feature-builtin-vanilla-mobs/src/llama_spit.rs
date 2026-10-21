//! Llama spit: a small projectile that does 1 damage to what it passes
//! through and vanishes in water or against a block.

use rand::rngs::StdRng;
use tuff_vanilla_data::damage_type;
use tuff_vanilla_data::registry::EntityType;

use super::MobCore;
use super::target::TargetView;
use tuff_core::world::entity::{
    Culprit, Effect, EntityBase, EntityHit, RegionEntity, Tick, TickContext,
};
use tuff_core::world::projectile::{
    Owner, block_hit, hit_source, in_water, lerp_rotation, margin, rotation_of, shot_velocity,
    targets_along,
};
use tuff_nbt::Compound;
use tuff_world_model::BlockPos;

const GRAVITY: f64 = 0.06;
const AIR_DRAG: f64 = 0.99;
const DAMAGE: f32 = 1.0;
const SPEED: f32 = 1.5;
const UNCERTAINTY: f32 = 10.0;

#[derive(Debug)]
pub struct LlamaSpit {
    pub base: EntityBase,
    pub owner: Owner,
    ticks: u32,
    hit: Vec<i32>,
}

impl LlamaSpit {
    /// Spit from `llama`'s mouth at `target`.
    pub fn at(llama: &MobCore, target: &TargetView, id: i32, rng: &mut StdRng) -> Self {
        let reach = f64::from(llama.size.width + 1.0) * 0.5;
        let yaw = f64::from(llama.body_yaw).to_radians();
        let [x, _, z] = llama.base.position;
        let start = [
            x - reach * yaw.sin(),
            llama.eye_position()[1] - 0.100_000_001_490_116_12,
            z + reach * yaw.cos(),
        ];
        let dx = target.position[0] - x;
        let height = target.bounds[4] - target.bounds[1];
        let dy = target.position[1] + height / 3.0 - start[1];
        let dz = target.position[2] - z;
        let lift = (dx * dx + dz * dz).sqrt() * 0.200_000_002_980_232_24;
        let velocity = shot_velocity([dx, dy + lift, dz], SPEED, UNCERTAINTY, rng);
        let uuid = rand::Rng::r#gen(rng);
        let mut base = EntityBase::new(EntityType::LlamaSpit, id, uuid, start, velocity);
        let (yaw, pitch) = rotation_of(velocity);
        base.yaw = yaw;
        base.pitch = pitch;
        Self {
            base,
            owner: Owner::of(Culprit::entity(&llama.base), None),
            ticks: 0,
            hit: Vec::new(),
        }
    }

    fn touches_only_solids(&self, cx: &TickContext) -> bool {
        let [x0, y0, z0, x1, y1, z1] = self.base.bounds();
        let span = |a: f64, b: f64| a.floor() as i32..=b.floor() as i32;
        for x in span(x0, x1) {
            for y in span(y0, y1) {
                for z in span(z0, z1) {
                    if (cx.block)(BlockPos::new(x, y, z))
                        .is_none_or(tuff_world_model::chunk::is_air)
                    {
                        return false;
                    }
                }
            }
        }
        true
    }
}

impl RegionEntity for LlamaSpit {
    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn base_mut(&mut self) -> &mut EntityBase {
        &mut self.base
    }

    fn save(&self, out: &mut Compound) {
        self.owner.save(out);
    }

    fn spawn_data(&self) -> i32 {
        self.owner.spawn_data()
    }

    fn tick(&mut self, cx: &mut TickContext) -> Tick {
        self.ticks += 1;
        self.owner.has_been_shot = true;
        let from = self.base.position;
        let velocity = self.base.velocity;
        let end = [
            from[0] + velocity[0],
            from[1] + velocity[1],
            from[2] + velocity[2],
        ];
        let block = block_hit(from, end, cx);
        let to = block.map_or(end, |hit| hit.location);
        let owner = self.owner.find(cx).map(|(id, _)| id);
        let left = self.owner.left;
        let target = targets_along(
            &self.base,
            self.base.bounds(),
            from,
            to,
            margin(self.ticks),
            |id| (!left && Some(id) == owner) || self.hit.contains(&id),
            cx,
        )
        .into_iter()
        .next()
        .filter(|t| !tuff_core::world::projectile::deflect_off(&mut self.base, t, cx));
        if let Some(target) = target {
            self.hit.push(target.id);
            if let Some(attacker) = self.owner.culprit(cx) {
                let mut hit = EntityHit::new(DAMAGE, &damage_type::SPIT);
                hit.attacker = Some(attacker);
                hit.projectile = Some(EntityType::LlamaSpit);
                hit.position = Some(hit_source(target.position, velocity));
                cx.effects.push(Effect::Hurt {
                    target: target.id,
                    hit: Box::new(hit),
                });
            }
            return Tick::Discard;
        } else if block.is_some() {
            return Tick::Discard;
        }
        let (yaw, pitch) = rotation_of(velocity);
        self.base.pitch = lerp_rotation(self.base.pitch, pitch);
        self.base.yaw = lerp_rotation(self.base.yaw, yaw);
        self.owner.check_left(self.base.bounds(), velocity, cx);
        if self.touches_only_solids(cx) || in_water(self.base.bounds(), cx) {
            return Tick::Discard;
        }
        self.base.velocity = velocity.map(|c| c * AIR_DRAG);
        if !self.base.no_gravity {
            self.base.velocity[1] -= GRAVITY;
        }
        self.base.position = end;
        Tick::Alive
    }
}

/// Loads saved spit.
pub fn load(base: EntityBase, nbt: &mut Compound) -> Option<Box<dyn RegionEntity>> {
    let mut owner = Owner::default();
    owner.load(nbt);
    Some(Box::new(LlamaSpit {
        base,
        owner,
        ticks: 0,
        hit: Vec::new(),
    }))
}
