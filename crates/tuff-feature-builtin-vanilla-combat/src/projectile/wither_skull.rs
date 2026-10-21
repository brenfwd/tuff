//! Wither skulls: the wither's own projectile. Flies like a small fireball
//! (accelerating along its heading), hits for 8 damage plus the Wither
//! status effect (scaled by difficulty), and explodes on any hit (entity or
//! block, power 1.0, no fire). The "dangerous" (black) variant, confirmed
//! against the reference rather than assumed from lore, differs from the
//! ordinary (blue) one only in flying straighter (less inertia loss per
//! tick) and in capping the resistance of blocks the wither can break at
//! 0.8 during its own impact blast.

use tuff_vanilla_data::damage_type;
use tuff_vanilla_data::registry::EntityType;

use crate::world::explosion::{BlockInteraction, Explosion};
use tuff_core::protocol::play::MetadataValue;
use tuff_core::world::effects::ActiveEffect;
use tuff_core::world::entity::{
    Effect, EntityBase, EntityHit, RegionEntity, Tick, TickContext, take_bool, take_number,
};
use tuff_core::world::player::Difficulty;
use tuff_core::world::projectile::{
    Owner, block_hit, hit_source, lerp_rotation, margin, rotation_of, targets_along,
};
use tuff_nbt::Compound;

const ACCELERATION: f64 = 0.1;
const INERTIA: f64 = 0.95;
/// A dangerous (black) skull flies straighter, losing less speed per tick.
const DANGEROUS_INERTIA: f64 = 0.73;
const LIQUID_INERTIA: f64 = 0.8;
const DAMAGE: f32 = 8.0;
const OWNERLESS_DAMAGE: f32 = 5.0;
const EXPLOSION_POWER: f32 = 1.0;
/// Entity data index of the dangerous flag; the base projectile entity
/// defines nothing of its own, so this is the first free index (matches
/// arrow's `DATA_FLAGS`, wither_skull_data_index_matches_the_dumped_layout).
const DATA_DANGEROUS: u8 = 8;

fn normalize(v: [f64; 3]) -> [f64; 3] {
    let length = v.iter().map(|c| c * c).sum::<f64>().sqrt();
    if length < 1.0e-5 {
        [0.0; 3]
    } else {
        v.map(|c| c / length)
    }
}

/// Seconds the Wither status effect lasts a hit for, by difficulty; 0 means
/// none (easy and peaceful).
fn wither_seconds(difficulty: Difficulty) -> i64 {
    match difficulty {
        Difficulty::Normal => 10,
        Difficulty::Hard => 40,
        Difficulty::Easy | Difficulty::Peaceful => 0,
    }
}

#[derive(Debug)]
pub struct WitherSkull {
    pub base: EntityBase,
    pub owner: Owner,
    pub dangerous: bool,
    acceleration: f64,
    ticks: u32,
}

impl WitherSkull {
    pub fn new(
        id: i32,
        position: [f64; 3],
        direction: [f64; 3],
        owner: Owner,
        dangerous: bool,
    ) -> Self {
        let velocity = normalize(direction).map(|c| c * ACCELERATION);
        let mut base = EntityBase::new(
            EntityType::WitherSkull,
            id,
            rand::random(),
            position,
            velocity,
        );
        let (yaw, pitch) = rotation_of(velocity);
        base.yaw = yaw;
        base.pitch = pitch;
        Self {
            base,
            owner,
            dangerous,
            acceleration: ACCELERATION,
            ticks: 0,
        }
    }

    fn impact(
        &mut self,
        target: Option<tuff_core::world::projectile::Target>,
        cx: &mut TickContext,
    ) -> Tick {
        if let Some(target) = target {
            let living_owner = self.owner.culprit.is_some();
            let amount = if living_owner {
                DAMAGE
            } else {
                OWNERLESS_DAMAGE
            };
            let kind = if living_owner {
                &damage_type::WITHER_SKULL
            } else {
                &damage_type::MAGIC
            };
            let mut hit = EntityHit::new(amount, kind);
            hit.attacker = self.owner.culprit(cx);
            hit.projectile = Some(EntityType::WitherSkull);
            hit.position = Some(hit_source(target.position, self.base.velocity));
            let seconds = wither_seconds(*cx.shared.difficulty.borrow());
            if seconds > 0 {
                hit.effects.push(ActiveEffect::timed(
                    tuff_vanilla_data::registry::MobEffect::Wither,
                    seconds * 20,
                    0,
                ));
            }
            cx.effects.push(Effect::Hurt {
                target: target.id,
                hit: Box::new(hit),
            });
        }
        let rules = cx.shared.game_rules.borrow();
        let grief = rules.bool("mob_griefing");
        let blocks = if !grief {
            BlockInteraction::Keep
        } else if rules.bool("mob_explosion_drop_decay") {
            BlockInteraction::DestroyWithDecay
        } else {
            BlockInteraction::Destroy
        };
        drop(rules);
        Tick::Explode(Explosion {
            center: self.base.position,
            radius: EXPLOSION_POWER,
            fire: false,
            blocks,
            damage_type: &damage_type::EXPLOSION,
            igniter: None,
            culprit: self.owner.culprit(cx).map(Box::new),
            dangerous_wither_skull: self.dangerous,
            spares_nether_portals: false,
        })
    }
}

impl RegionEntity for WitherSkull {
    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn base_mut(&mut self) -> &mut EntityBase {
        &mut self.base
    }

    fn save(&self, out: &mut Compound) {
        self.owner.save(out);
        out.insert("acceleration_power", self.acceleration);
        out.insert("dangerous", self.dangerous);
    }

    fn metadata(&self) -> Vec<(u8, MetadataValue)> {
        vec![(DATA_DANGEROUS, MetadataValue::Boolean(self.dangerous))]
    }

    fn spawn_data(&self) -> i32 {
        self.owner.spawn_data()
    }

    fn tick(&mut self, cx: &mut TickContext) -> Tick {
        let skip_owner = !self.owner.left;
        let inertia = if self.dangerous {
            DANGEROUS_INERTIA
        } else {
            INERTIA
        };
        let Some((target, block)) = fly(
            &mut self.base,
            &mut self.owner,
            self.acceleration,
            inertia,
            &mut self.ticks,
            skip_owner,
            cx,
        ) else {
            return Tick::Discard;
        };
        if target.is_none() && block.is_none() {
            return Tick::Alive;
        }
        if let Some(block) = block.filter(|_| target.is_none()) {
            tuff_core::world::projectile::struck_block(block, &self.base, &self.owner, cx);
        }
        self.impact(target, cx)
    }
}

/// One tick of a wither skull's flight, like [`super::fireball::fly`] but
/// with a caller-chosen inertia (dangerous skulls drift less).
#[allow(clippy::type_complexity)]
fn fly(
    base: &mut EntityBase,
    owner: &mut Owner,
    acceleration: f64,
    inertia: f64,
    ticks: &mut u32,
    skip_owner: bool,
    cx: &mut TickContext,
) -> Option<(
    Option<tuff_core::world::projectile::Target>,
    Option<tuff_core::world::raycast::Hit>,
)> {
    *ticks += 1;
    owner.has_been_shot = true;
    let v = base.velocity;
    let push = normalize(v).map(|c| c * acceleration);
    let inertia = if tuff_core::world::projectile::in_water(base.bounds(), cx) {
        LIQUID_INERTIA
    } else {
        inertia
    };
    base.velocity = [0, 1, 2].map(|i| (v[i] + push[i]) * inertia);
    let from = base.position;
    (cx.block)(tuff_core::world::physics::block_pos(from))?;
    let velocity = base.velocity;
    let end = [
        from[0] + velocity[0],
        from[1] + velocity[1],
        from[2] + velocity[2],
    ];
    let block = block_hit(from, end, cx);
    let to = block.map_or(end, |hit| hit.location);
    let owner_id = owner.find(cx).map(|(id, _)| id);
    let target = targets_along(
        base,
        base.bounds(),
        from,
        to,
        margin(*ticks),
        |id| skip_owner && Some(id) == owner_id,
        cx,
    )
    .into_iter()
    .next()
    .filter(|t| !tuff_core::world::projectile::deflect_off(base, t, cx));
    base.position = target.as_ref().map_or(to, |t| t.location);
    let (yaw, pitch) = rotation_of(velocity);
    base.pitch = lerp_rotation(base.pitch, pitch);
    base.yaw = lerp_rotation(base.yaw, yaw);
    owner.check_left(base.bounds(), velocity, cx);
    tuff_core::world::block_contact::projectile_bubble_column_push(
        base.bounds(),
        &mut base.velocity,
        cx.block,
    );
    Some((target, block))
}

pub fn load(base: EntityBase, nbt: &mut Compound) -> Option<Box<dyn RegionEntity>> {
    let mut owner = Owner::default();
    owner.load(nbt);
    let acceleration = take_number(nbt, "acceleration_power").unwrap_or(ACCELERATION);
    let dangerous = take_bool(nbt, "dangerous").unwrap_or(false);
    Some(Box::new(WitherSkull {
        base,
        owner,
        dangerous,
        acceleration,
        ticks: 0,
    }))
}
