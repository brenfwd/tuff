//! The dragon's breath fireballs it spits while strafing, which burst
//! into a lingering cloud of harming.

use rand::Rng;
use tuff_vanilla_data::registry::EntityType;

use tuff_core::protocol::play;
use tuff_core::world::entity::{
    Culprit, Effect, EntityBase, RegionEntity, Tick, TickContext, take_number,
};
use tuff_core::world::projectile::{
    Owner, block_hit, in_water, lerp_rotation, margin, rotation_of, targets_along,
};
use tuff_core::world::{framed, packets};
use tuff_nbt::Compound;
use tuff_vanilla_common::effect_cloud::AreaEffectCloud;

const ACCELERATION: f64 = 0.1;
const INERTIA: f64 = 0.95;
const LIQUID_INERTIA: f64 = 0.8;
/// Living things this close to a bursting fireball move its cloud onto
/// themselves.
const SPLASH_RANGE: f64 = 4.0;

fn normalize(v: [f64; 3]) -> [f64; 3] {
    let length = v.iter().map(|c| c * c).sum::<f64>().sqrt();
    if length < 1.0e-5 {
        [0.0; 3]
    } else {
        v.map(|c| c / length)
    }
}

/// A breath fireball flying at a player.
#[derive(Debug)]
pub struct DragonFireball {
    pub base: EntityBase,
    pub owner: Owner,
    acceleration: f64,
    ticks: u32,
}

impl DragonFireball {
    pub fn new(
        id: i32,
        position: [f64; 3],
        direction: [f64; 3],
        owner: Culprit,
        rng: &mut impl Rng,
    ) -> Self {
        let velocity = normalize(direction).map(|c| c * ACCELERATION);
        let mut base = EntityBase::new(
            EntityType::DragonFireball,
            id,
            rng.r#gen(),
            position,
            velocity,
        );
        let (yaw, pitch) = rotation_of(velocity);
        base.yaw = yaw;
        base.pitch = pitch;
        Self {
            base,
            owner: Owner::of(owner, None),
            acceleration: ACCELERATION,
            ticks: 0,
        }
    }

    /// Bursts into a cloud of harming where it hit, or on a living thing
    /// close by.
    fn burst(&self, cx: &mut TickContext) {
        let mut at = self.base.position;
        let bounds = self.base.bounds();
        let area = [
            bounds[0] - SPLASH_RANGE,
            bounds[1] - 2.0,
            bounds[2] - SPLASH_RANGE,
            bounds[3] + SPLASH_RANGE,
            bounds[4] + 2.0,
            bounds[5] + SPLASH_RANGE,
        ];
        let inside = |b: [f64; 6]| {
            area[0] < b[3]
                && area[3] > b[0]
                && area[1] < b[4]
                && area[4] > b[1]
                && area[2] < b[5]
                && area[5] > b[2]
        };
        let near = cx
            .players
            .iter()
            .filter(|p| inside(p.bounds()))
            .map(|p| p.position)
            .chain(
                cx.entities
                    .iter()
                    .filter(|e| e.living.is_some_and(|l| l.alive) && inside(e.bounds))
                    .map(|e| e.position),
            )
            .find(|p| tuff_core::world::distance_sq(*p, self.base.position) < SPLASH_RANGE.powi(2));
        if let Some(position) = near {
            at = position;
        }
        let id = cx.shared.next_entity_id();
        let owner = self.owner.culprit(cx);
        let cloud = AreaEffectCloud::lingering(id, at, owner, cx.rng);
        let [x, y, z] = self.base.position;
        let event = framed(&play::LevelEvent {
            event: play::level_event::PARTICLES_DRAGON_FIREBALL_SPLASH,
            position: tuff_world_model::BlockPos::new(
                x.floor() as i32,
                y.floor() as i32,
                z.floor() as i32,
            ),
            data: if self.base.silent { -1 } else { 1 },
            global: false,
        });
        cx.effects.push(Effect::ChunkPackets(packets([event])));
        cx.effects.push(Effect::Spawn(Box::new(cloud)));
    }
}

impl RegionEntity for DragonFireball {
    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn base_mut(&mut self) -> &mut EntityBase {
        &mut self.base
    }

    fn save(&self, out: &mut Compound) {
        self.owner.save(out);
        out.insert("acceleration_power", self.acceleration);
    }

    fn spawn_data(&self) -> i32 {
        self.owner.spawn_data()
    }

    fn tick(&mut self, cx: &mut TickContext) -> Tick {
        self.ticks += 1;
        self.owner.has_been_shot = true;
        let v = self.base.velocity;
        let push = normalize(v).map(|c| c * self.acceleration);
        let inertia = if in_water(self.base.bounds(), cx) {
            LIQUID_INERTIA
        } else {
            INERTIA
        };
        self.base.velocity = [0, 1, 2].map(|i| (v[i] + push[i]) * inertia);
        let from = self.base.position;
        if (cx.block)(tuff_core::world::physics::block_pos(from)).is_none() {
            return Tick::Discard;
        }
        let velocity = self.base.velocity;
        let end = [
            from[0] + velocity[0],
            from[1] + velocity[1],
            from[2] + velocity[2],
        ];
        let block = block_hit(from, end, cx);
        let to = block.map_or(end, |hit| hit.location);
        let owner = self.owner.find(cx).map(|(id, _)| id);
        let target = targets_along(
            &self.base,
            self.base.bounds(),
            from,
            to,
            margin(self.ticks),
            |id| Some(id) == owner,
            cx,
        )
        .into_iter()
        .next();
        self.base.position = target.as_ref().map_or(to, |t| t.location);
        let (yaw, pitch) = rotation_of(velocity);
        self.base.pitch = lerp_rotation(self.base.pitch, pitch);
        self.base.yaw = lerp_rotation(self.base.yaw, yaw);
        tuff_core::world::block_contact::projectile_bubble_column_push(
            self.base.bounds(),
            &mut self.base.velocity,
            cx.block,
        );
        if target.is_some() || block.is_some() {
            self.burst(cx);
            return Tick::Discard;
        }
        Tick::Alive
    }
}

pub fn load_fireball(base: EntityBase, nbt: &mut Compound) -> Option<Box<dyn RegionEntity>> {
    let mut owner = Owner::default();
    owner.load(nbt);
    let acceleration = take_number(nbt, "acceleration_power").unwrap_or(ACCELERATION);
    Some(Box::new(DragonFireball {
        base,
        owner,
        acceleration,
        ticks: 0,
    }))
}
