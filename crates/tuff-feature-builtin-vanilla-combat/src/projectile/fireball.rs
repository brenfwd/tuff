//! Small fireballs, as blazes shoot them: no gravity, speeding up along
//! their heading, setting what they hit alight (5 damage) and lighting
//! fire where they land.

use tuff_vanilla_data::damage_type;
use tuff_vanilla_data::registry::EntityType;
use tuff_vanilla_data::{Block, Item};

use tuff_core::protocol::play::MetadataValue;
use tuff_core::world::entity::{
    AttackOutcome, Effect, EntityBase, EntityHit, RegionEntity, Tick, TickContext, take_number,
};
use tuff_core::world::projectile::{
    Owner, block_hit, hit_source, lerp_rotation, margin, rotation_of, targets_along,
};
use tuff_item::item::ItemStack;
use tuff_nbt::Compound;
use tuff_world_model::BlockPos;

const ACCELERATION: f64 = 0.1;
const INERTIA: f64 = 0.95;
const LIQUID_INERTIA: f64 = 0.8;
const DAMAGE: f32 = 5.0;
const FIRE_SECONDS: f32 = 5.0;
const DATA_ITEM: u8 = 8;

#[derive(Debug)]
pub struct SmallFireball {
    pub base: EntityBase,
    pub owner: Owner,
    acceleration: f64,
    ticks: u32,
    deflected: bool,
}

/// A hit by another projectile sends the fireball on along that
/// projectile's heading, and makes it its shooter's.
fn deflect(
    base: &mut EntityBase,
    owner: &mut Owner,
    acceleration: &mut f64,
    hit: &EntityHit,
) -> Option<AttackOutcome> {
    let (attacker, from) = (hit.attacker.as_ref()?, hit.position?);
    hit.projectile?;
    let heading = match momentum(hit) {
        Some(velocity) => normalize(velocity),
        None => normalize([
            base.position[0] - from[0],
            base.position[1] - from[1],
            base.position[2] - from[2],
        ]),
    };
    if heading == [0.0; 3] {
        return None;
    }
    base.velocity = heading;
    *owner = Owner {
        uuid: Some(attacker.uuid),
        has_been_shot: owner.has_been_shot,
        ..Owner::default()
    };
    *acceleration = ACCELERATION;
    Some(AttackOutcome {
        hurt: false,
        health_lost: 0.0,
    })
}

/// A shulker bullet sends what it strikes on along its own heading rather
/// than along the line from the point of impact.
pub fn momentum(hit: &EntityHit) -> Option<[f64; 3]> {
    if hit.projectile != Some(EntityType::ShulkerBullet) {
        return None;
    }
    hit.projectile_velocity
}

fn normalize(v: [f64; 3]) -> [f64; 3] {
    let length = v.iter().map(|c| c * c).sum::<f64>().sqrt();
    if length < 1.0e-5 {
        [0.0; 3]
    } else {
        v.map(|c| c / length)
    }
}

impl SmallFireball {
    /// A fireball at `position` heading along `direction`.
    pub fn new(id: i32, position: [f64; 3], direction: [f64; 3], owner: Owner) -> Self {
        let velocity = normalize(direction).map(|c| c * ACCELERATION);
        let mut base = EntityBase::new(
            EntityType::SmallFireball,
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
            acceleration: ACCELERATION,
            ticks: 0,
            deflected: false,
        }
    }

    fn land(
        &mut self,
        hit: Option<tuff_core::world::projectile::Target>,
        block: Option<tuff_core::world::raycast::Hit>,
        cx: &mut TickContext,
    ) {
        if let Some(target) = hit {
            let mut hit = EntityHit::new(DAMAGE, &damage_type::FIREBALL);
            hit.attacker = self.owner.culprit(cx);
            tuff_core::world::projectile::landed(
                &self.base,
                hit.attacker.clone(),
                None,
                Some(target.id),
                cx,
            );
            hit.projectile = Some(EntityType::SmallFireball);
            hit.position = Some(hit_source(target.position, self.base.velocity));
            hit.ignite = FIRE_SECONDS;
            cx.effects.push(Effect::Hurt {
                target: target.id,
                hit: Box::new(hit),
            });
            return;
        }
        let Some(block) = block else {
            return;
        };
        tuff_core::world::projectile::struck_block(block, &self.base, &self.owner, cx);
        let by_mob = self.owner.culprit.as_ref().is_some_and(|c| !c.is_player());
        if by_mob && !cx.shared.game_rules.borrow().bool("mob_griefing") {
            return;
        }
        let pos = tuff_vanilla_common::placement::step(block.pos, block.face);
        if !(cx.block)(pos).is_some_and(tuff_world_model::chunk::is_air) {
            return;
        }
        let below = BlockPos::new(pos.x, pos.y - 1, pos.z);
        let fire = if (cx.block)(below).is_some_and(|s| {
            tuff_datapack::tags::is(
                tuff_vanilla_data::tag::block::key::SOUL_FIRE_BASE_BLOCKS,
                s.block(),
            )
        }) {
            Block::SoulFire
        } else {
            Block::Fire
        };
        cx.effects.push(Effect::SetBlock {
            pos,
            state: fire.default_state(),
        });
    }
}

impl RegionEntity for SmallFireball {
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

    fn metadata(&self) -> Vec<(u8, MetadataValue)> {
        vec![(
            DATA_ITEM,
            MetadataValue::Slot(Some(tuff_item::item::ItemSnapshot::of(&ItemStack::new(
                Item::FireCharge,
                1,
            )))),
        )]
    }

    fn spawn_data(&self) -> i32 {
        self.owner.spawn_data()
    }

    fn pickable(&self) -> bool {
        true
    }

    fn hit(&mut self, hit: &EntityHit, _cx: &mut TickContext) -> Option<AttackOutcome> {
        deflect(&mut self.base, &mut self.owner, &mut self.acceleration, hit)
    }

    fn take_impulse(&mut self) -> bool {
        std::mem::take(&mut self.deflected)
    }

    fn tick(&mut self, cx: &mut TickContext) -> Tick {
        let skip_owner = !self.owner.left;
        let Some((target, block)) = fly(
            &mut self.base,
            &mut self.owner,
            self.acceleration,
            &mut self.ticks,
            skip_owner,
            cx,
        ) else {
            return Tick::Discard;
        };
        if target.is_some() || block.is_some() {
            self.land(target, if target.is_some() { None } else { block }, cx);
            return Tick::Discard;
        }
        Tick::Alive
    }
}

/// One tick of a fireball's flight: it speeds up along its heading (less
/// in water), moves, and reports what it hit, if anything. `None` when it
/// left the loaded world and should go.
#[allow(clippy::type_complexity)]
fn fly(
    base: &mut EntityBase,
    owner: &mut Owner,
    acceleration: f64,
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
        INERTIA
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
    base.fire = base.fire.max(20);
    Some((target, block))
}

pub fn load(base: EntityBase, nbt: &mut Compound) -> Option<Box<dyn RegionEntity>> {
    let mut owner = Owner::default();
    owner.load(nbt);
    let acceleration = take_number(nbt, "acceleration_power").unwrap_or(ACCELERATION);
    Some(Box::new(SmallFireball {
        base,
        owner,
        acceleration,
        ticks: 0,
        deflected: false,
    }))
}

const LARGE_DAMAGE: f32 = 6.0;
const DEFAULT_EXPLOSION_POWER: i32 = 1;

/// Large fireballs, as ghasts shoot them: they fly like small ones, hurt
/// what they hit and explode, setting fires if mobs may grief. A player's
/// hit sends one back along the player's view, and makes it theirs.
#[derive(Debug)]
pub struct LargeFireball {
    pub base: EntityBase,
    pub owner: Owner,
    pub explosion_power: i32,
    acceleration: f64,
    ticks: u32,
    deflected: bool,
}

impl LargeFireball {
    /// A fireball at `position` heading along `direction`.
    pub fn new(
        id: i32,
        position: [f64; 3],
        direction: [f64; 3],
        owner: Owner,
        explosion_power: i32,
    ) -> Self {
        let velocity = normalize(direction).map(|c| c * ACCELERATION);
        let base = EntityBase::new(EntityType::Fireball, id, rand::random(), position, velocity);
        Self {
            base,
            owner,
            explosion_power,
            acceleration: ACCELERATION,
            ticks: 0,
            deflected: false,
        }
    }

    fn explosion(&self, cx: &TickContext) -> crate::world::explosion::Explosion {
        use crate::world::explosion::BlockInteraction;
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
        let culprit = self.owner.culprit(cx);
        let igniter = culprit.as_ref().filter(|c| c.is_player()).map(|c| {
            tuff_core::world::messages::Igniter {
                id: c.id,
                uuid: c.uuid,
                name: c.name.plain(),
            }
        });
        let damage_type = if culprit.is_some() {
            &damage_type::PLAYER_EXPLOSION
        } else {
            &damage_type::EXPLOSION
        };
        crate::world::explosion::Explosion {
            center: self.base.position,
            radius: self.explosion_power as f32,
            fire: grief,
            blocks,
            damage_type,
            igniter,
            culprit: culprit.filter(|c| !c.is_player()).map(Box::new),
            dangerous_wither_skull: false,
            spares_nether_portals: false,
        }
    }
}

impl RegionEntity for LargeFireball {
    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn base_mut(&mut self) -> &mut EntityBase {
        &mut self.base
    }

    fn redirect_owner(&mut self, owner: u128) {
        self.owner = Owner {
            uuid: Some(owner),
            has_been_shot: self.owner.has_been_shot,
            ..Owner::default()
        };
    }

    fn save(&self, out: &mut Compound) {
        self.owner.save(out);
        out.insert("acceleration_power", self.acceleration);
        out.insert("ExplosionPower", self.explosion_power as i8);
    }

    fn metadata(&self) -> Vec<(u8, MetadataValue)> {
        vec![(
            DATA_ITEM,
            MetadataValue::Slot(Some(tuff_item::item::ItemSnapshot::of(&ItemStack::new(
                Item::FireCharge,
                1,
            )))),
        )]
    }

    fn spawn_data(&self) -> i32 {
        self.owner.spawn_data()
    }

    fn pickable(&self) -> bool {
        true
    }

    fn hit(&mut self, hit: &EntityHit, _cx: &mut TickContext) -> Option<AttackOutcome> {
        let outcome = deflect(&mut self.base, &mut self.owner, &mut self.acceleration, hit);
        self.deflected |= outcome.is_some();
        outcome
    }

    fn take_impulse(&mut self) -> bool {
        std::mem::take(&mut self.deflected)
    }

    fn attacked(
        &mut self,
        attack: &tuff_core::world::entity::EntityAttack,
        _cx: &mut TickContext,
    ) -> Option<tuff_core::world::entity::AttackOutcome> {
        let (yaw, pitch) = (
            attack.attacker_yaw.to_radians(),
            attack.attacker_pitch.to_radians(),
        );
        let look = [
            f64::from(-yaw.sin() * pitch.cos()),
            f64::from(-pitch.sin()),
            f64::from(yaw.cos() * pitch.cos()),
        ];
        self.base.velocity = look;
        self.owner = Owner {
            uuid: Some(attack.attacker_uuid),
            has_been_shot: self.owner.has_been_shot,
            ..Owner::default()
        };
        self.acceleration = ACCELERATION;
        self.deflected = true;
        Some(tuff_core::world::entity::AttackOutcome {
            hurt: false,
            health_lost: 0.0,
        })
    }

    fn tick(&mut self, cx: &mut TickContext) -> Tick {
        let Some((target, block)) = fly(
            &mut self.base,
            &mut self.owner,
            self.acceleration,
            &mut self.ticks,
            false,
            cx,
        ) else {
            return Tick::Discard;
        };
        if target.is_none() && block.is_none() {
            return Tick::Alive;
        }
        if let Some(target) = target {
            let mut hit = EntityHit::new(LARGE_DAMAGE, &damage_type::FIREBALL);
            hit.attacker = self.owner.culprit(cx);
            tuff_core::world::projectile::landed(
                &self.base,
                hit.attacker.clone(),
                None,
                Some(target.id),
                cx,
            );
            hit.projectile = Some(EntityType::Fireball);
            hit.position = Some(hit_source(target.position, self.base.velocity));
            cx.effects.push(Effect::Hurt {
                target: target.id,
                hit: Box::new(hit),
            });
        } else if let Some(block) = block {
            tuff_core::world::projectile::struck_block(block, &self.base, &self.owner, cx);
        }
        Tick::Explode(self.explosion(cx))
    }
}

pub fn load_large(base: EntityBase, nbt: &mut Compound) -> Option<Box<dyn RegionEntity>> {
    let mut owner = Owner::default();
    owner.load(nbt);
    let acceleration = take_number(nbt, "acceleration_power").unwrap_or(ACCELERATION);
    let explosion_power =
        take_number(nbt, "ExplosionPower").map_or(DEFAULT_EXPLOSION_POWER, |v| v as i8 as i32);
    Some(Box::new(LargeFireball {
        base,
        owner,
        explosion_power,
        acceleration,
        ticks: 0,
        deflected: false,
    }))
}

#[cfg(test)]
mod momentum_tests {
    use super::*;
    use tuff_core::world::entity::Culprit;

    fn shulker_hit(velocity: [f64; 3], from: [f64; 3]) -> EntityHit {
        let mut hit = EntityHit::new(4.0, &damage_type::MOB_PROJECTILE);
        hit.attacker = Some(Culprit {
            id: 9,
            uuid: 9,
            kind: EntityType::Shulker,
            name: Default::default(),
            charged: None,
            owner_name: None,
            held_name: None,
        });
        hit.projectile = Some(EntityType::ShulkerBullet);
        hit.projectile_velocity = Some(velocity);
        hit.position = Some(from);
        hit
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_shulker_bullet_sends_a_fireball_along_its_own_heading() {
        let mut base = EntityBase::new(EntityType::SmallFireball, 1, 1, [0.0, 0.0, 0.0], [0.0; 3]);
        let mut owner = Owner::default();
        let mut acceleration = 0.0;
        let hit = shulker_hit([0.0, 0.0, 2.0], [5.0, 0.0, 0.0]);
        assert!(deflect(&mut base, &mut owner, &mut acceleration, &hit).is_some());
        assert_eq!(base.velocity, [0.0, 0.0, 1.0]);
        assert_eq!(owner.uuid, Some(9));
    }
}
