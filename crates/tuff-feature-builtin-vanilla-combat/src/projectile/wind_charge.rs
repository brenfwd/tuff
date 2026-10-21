//! Wind charges: a player's (thrown or dispensed) and a breeze's. Both fly
//! straight at a constant speed, hit for one point of damage and burst in
//! a wind blast that pushes without hurting or breaking blocks; a player's
//! burst is smaller but pushes harder.

use rand::rngs::StdRng;
use tuff_vanilla_data::damage_type;
use tuff_vanilla_data::registry::{EntityType, SoundEvent};

use crate::world::explosion::{BlockInteraction, Explosion, WindBurst};
use tuff_core::protocol::play::MetadataValue;
use tuff_core::world::entity::{
    AttackOutcome, Effect, EntityBase, EntityHit, RegionEntity, Tick, TickContext,
};
use tuff_core::world::messages::Igniter;
use tuff_core::world::physics;
use tuff_core::world::projectile::{Owner, block_hit, hit_source, margin, targets_along};
use tuff_nbt::Compound;
use tuff_world_model::pos::REGION_CHUNKS;

const DAMAGE: f32 = 1.0;
const PLAYER_RADIUS: f32 = 1.2;
const PLAYER_KNOCKBACK: f32 = 1.22;
const BREEZE_RADIUS: f32 = 3.0;
/// How far from a dispenser's centre a dispensed charge starts.
pub const DISPENSE_OFFSET: f64 = 1.0;
/// The spread of a dispensed charge's direction on each axis.
pub const DISPENSE_SPREAD: f64 = 0.114_85;
/// A burst at a block it hit sits this far out of the struck face.
const FACE_OFFSET: f64 = 0.25;
/// A charge this far above the world's top bursts on its own.
const CEILING_MARGIN: i32 = 30;
/// A player's charge cannot be knocked back for its first ticks.
const NO_DEFLECT_TICKS: u32 = 5;
/// A charge's box hangs this far below its position.
const BOX_DROP: f64 = 0.150_000_005_960_464_48;

#[derive(Debug)]
pub struct WindCharge {
    pub base: EntityBase,
    pub owner: Owner,
    ticks: u32,
    deflected: bool,
    owner_seen: Option<[f64; 3]>,
}

impl WindCharge {
    /// A breeze's charge at `position` heading along `velocity` (already
    /// scaled to its shot speed).
    pub fn breeze(id: i32, position: [f64; 3], velocity: [f64; 3], owner: Owner) -> Self {
        Self::of(EntityType::BreezeWindCharge, id, position, velocity, owner)
    }

    /// A player's charge, thrown along `velocity`.
    pub fn thrown(id: i32, position: [f64; 3], velocity: [f64; 3], owner: Owner) -> Self {
        Self::of(EntityType::WindCharge, id, position, velocity, owner)
    }

    /// A charge a dispenser facing `step` fires from `position`: its
    /// direction is spread around the facing and kept as its velocity.
    pub fn dispensed(id: i32, position: [f64; 3], step: [f64; 3], rng: &mut StdRng) -> Self {
        let velocity = step.map(|c| physics::triangle(rng, c, DISPENSE_SPREAD));
        Self::of(
            EntityType::WindCharge,
            id,
            position,
            velocity,
            Owner::default(),
        )
    }

    fn of(kind: EntityType, id: i32, position: [f64; 3], velocity: [f64; 3], owner: Owner) -> Self {
        let base = EntityBase::new(kind, id, rand::random(), position, velocity);
        Self {
            base,
            owner,
            ticks: 0,
            deflected: false,
            owner_seen: None,
        }
    }

    /// Its mob owner was here and is no longer: it died, unless it was last
    /// seen near the region border, where it may have walked into the
    /// neighbouring region with the charge still in flight.
    fn owner_gone(&mut self, cx: &TickContext) -> bool {
        let Some(uuid) = self.owner.uuid else {
            return false;
        };
        if cx.players.iter().any(|p| p.uuid == uuid) {
            return false;
        }
        match cx.entities.iter().find(|e| e.uuid == uuid) {
            Some(owner) => {
                self.owner_seen = Some(owner.position);
                false
            }
            None => self.owner_seen.is_some_and(owner_vanished_inside),
        }
    }

    /// The burst this charge sets off at `position`.
    pub fn explosion(&self, position: [f64; 3], cx: &TickContext) -> Explosion {
        let (radius, wind) = if self.base.kind == EntityType::WindCharge {
            (
                PLAYER_RADIUS,
                WindBurst {
                    knockback: PLAYER_KNOCKBACK,
                    sound: SoundEvent::EntityWindChargeWindBurst,
                    source: self.base.kind,
                    spares_fall: true,
                },
            )
        } else {
            (
                BREEZE_RADIUS,
                WindBurst {
                    knockback: 1.0,
                    sound: SoundEvent::EntityBreezeWindBurst,
                    source: self.base.kind,
                    spares_fall: false,
                },
            )
        };
        let culprit = self.owner.culprit(cx);
        let igniter = culprit
            .as_ref()
            .and_then(|c| c.player_name().map(|name| (c, name)))
            .map(|(c, name)| Igniter {
                id: c.id,
                uuid: c.uuid,
                name: name.to_owned(),
            });
        Explosion {
            center: position,
            radius,
            fire: false,
            blocks: BlockInteraction::Trigger(wind),
            damage_type: &damage_type::WIND_CHARGE,
            igniter,
            culprit: culprit.filter(|c| !c.is_player()).map(Box::new),
            dangerous_wither_skull: false,
            spares_nether_portals: false,
        }
    }
}

impl RegionEntity for WindCharge {
    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn base_mut(&mut self) -> &mut EntityBase {
        &mut self.base
    }

    fn bounds(&self) -> [f64; 6] {
        let mut bounds = self.base.bounds();
        bounds[1] -= BOX_DROP;
        bounds[4] -= BOX_DROP;
        bounds
    }

    fn save(&self, out: &mut Compound) {
        self.owner.save(out);
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

    /// A player's hit sends it off along their look, as theirs; a fresh
    /// player charge does not turn yet.
    fn attacked(
        &mut self,
        attack: &tuff_core::world::entity::EntityAttack,
        _cx: &mut TickContext,
    ) -> Option<tuff_core::world::entity::AttackOutcome> {
        if self.base.kind == EntityType::WindCharge && self.ticks < NO_DEFLECT_TICKS {
            return None;
        }
        let (yaw, pitch) = (
            attack.attacker_yaw.to_radians(),
            attack.attacker_pitch.to_radians(),
        );
        self.base.velocity = [
            f64::from(-yaw.sin() * pitch.cos()),
            f64::from(-pitch.sin()),
            f64::from(yaw.cos() * pitch.cos()),
        ];
        self.owner = Owner {
            uuid: Some(attack.attacker_uuid),
            has_been_shot: self.owner.has_been_shot,
            ..Owner::default()
        };
        self.deflected = true;
        Some(tuff_core::world::entity::AttackOutcome {
            hurt: false,
            health_lost: 0.0,
        })
    }

    fn redirect_owner(&mut self, owner: u128) {
        self.owner = Owner {
            uuid: Some(owner),
            has_been_shot: self.owner.has_been_shot,
            ..Owner::default()
        };
        self.owner_seen = None;
    }

    /// A projectile that strikes it sends it on along its heading, as the
    /// shooter's.
    fn hit(&mut self, hit: &EntityHit, _cx: &mut TickContext) -> Option<AttackOutcome> {
        let (attacker, from) = (hit.attacker.as_ref()?, hit.position?);
        hit.projectile?;
        if let Some(velocity) = super::fireball::momentum(hit) {
            let length = velocity.iter().map(|c| c * c).sum::<f64>().sqrt();
            if length < 1.0e-5 {
                return None;
            }
            self.base.velocity = velocity.map(|c| c / length);
        } else {
            let (dx, dz) = (
                self.base.position[0] - from[0],
                self.base.position[2] - from[2],
            );
            let length = dx.hypot(dz);
            if length < 1.0e-5 {
                return None;
            }
            self.base.velocity = [dx / length, 0.0, dz / length];
        }
        self.owner = Owner {
            uuid: Some(attacker.uuid),
            has_been_shot: self.owner.has_been_shot,
            ..Owner::default()
        };
        self.owner_seen = None;
        self.deflected = true;
        Some(AttackOutcome {
            hurt: false,
            health_lost: 0.0,
        })
    }

    fn take_impulse(&mut self) -> bool {
        std::mem::take(&mut self.deflected)
    }

    fn tick(&mut self, cx: &mut TickContext) -> Tick {
        if self
            .owner
            .handle
            .as_ref()
            .is_some_and(|handle| !handle.is_connected())
        {
            return Tick::Discard;
        }
        if self.owner_gone(cx) {
            return Tick::Discard;
        }
        self.ticks += 1;
        self.owner.has_been_shot = true;
        let from = self.base.position;
        if from[1].floor() as i32 > cx.max_y + CEILING_MARGIN {
            return Tick::Explode(self.explosion(from, cx));
        }
        let velocity = self.base.velocity;
        if (cx.block)(physics::block_pos(from)).is_none() {
            return Tick::Discard;
        }
        let end = [
            from[0] + velocity[0],
            from[1] + velocity[1],
            from[2] + velocity[2],
        ];
        let block = block_hit(from, end, cx);
        let to = block.map_or(end, |hit| hit.location);
        let skip_owner = !self.owner.left;
        let owner_id = self.owner.find(cx).map(|(id, _)| id);
        let target = targets_along(
            &self.base,
            self.bounds(),
            from,
            to,
            margin(self.ticks),
            |id| skip_owner && Some(id) == owner_id,
            cx,
        )
        .into_iter()
        .find(|target| {
            !matches!(
                target.kind,
                EntityType::WindCharge | EntityType::BreezeWindCharge | EntityType::EndCrystal
            )
        });
        self.owner.check_left(self.bounds(), velocity, cx);
        if let Some(target) = &target {
            self.base.position = target.location;
            let mut hit = EntityHit::new(DAMAGE, &damage_type::WIND_CHARGE);
            hit.attacker = self.owner.culprit(cx);
            hit.projectile = Some(self.base.kind);
            hit.position = Some(hit_source(target.position, velocity));
            cx.effects.push(Effect::Hurt {
                target: target.id,
                hit: Box::new(hit),
            });
            return Tick::Explode(self.explosion(self.base.position, cx));
        }
        if let Some(block) = block {
            tuff_core::world::projectile::struck_block(block, &self.base, &self.owner, cx);
            let (nx, ny, nz) = block.face.step();
            let at = [
                block.location[0] + f64::from(nx) * FACE_OFFSET,
                block.location[1] + f64::from(ny) * FACE_OFFSET,
                block.location[2] + f64::from(nz) * FACE_OFFSET,
            ];
            return Tick::Explode(self.explosion(at, cx));
        }
        self.base.position = to;
        tuff_core::world::block_contact::projectile_bubble_column_push(
            self.base.bounds(),
            &mut self.base.velocity,
            cx.block,
        );
        Tick::Alive
    }
}

pub fn load(base: EntityBase, nbt: &mut Compound) -> Option<Box<dyn RegionEntity>> {
    let mut owner = Owner::default();
    owner.load(nbt);
    Some(Box::new(WindCharge {
        base,
        owner,
        ticks: 0,
        deflected: false,
        owner_seen: None,
    }))
}

const BORDER_MARGIN: f64 = 8.0;

fn owner_vanished_inside(seen: [f64; 3]) -> bool {
    let side = f64::from(REGION_CHUNKS * 16);
    [seen[0], seen[2]].iter().all(|c| {
        let within = c.rem_euclid(side);
        within >= BORDER_MARGIN && side - within >= BORDER_MARGIN
    })
}

pub const BURST_SOUND: SoundEvent = SoundEvent::EntityBreezeWindBurst;
pub const SHOOT_SOUND: SoundEvent = SoundEvent::EntityBreezeShoot;
pub const INHALE_SOUND: SoundEvent = SoundEvent::EntityBreezeInhale;

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;

    #[tuff_core::watchdog]
    #[test]
    fn a_dispensed_charge_flies_along_the_facing_at_about_unit_speed() {
        let mut rng = StdRng::seed_from_u64(7);
        for _ in 0..64 {
            let charge = WindCharge::dispensed(1, [0.0; 3], [1.0, 0.0, 0.0], &mut rng);
            let [x, y, z] = charge.base.velocity;
            assert!((1.0 - DISPENSE_SPREAD..=1.0 + DISPENSE_SPREAD).contains(&x));
            assert!(y.abs() <= DISPENSE_SPREAD && z.abs() <= DISPENSE_SPREAD);
            assert_eq!(charge.base.kind, EntityType::WindCharge);
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn an_owner_last_seen_near_a_region_border_is_not_taken_for_dead() {
        assert!(owner_vanished_inside([256.0, 70.0, 256.0]));
        assert!(!owner_vanished_inside([2.0, 70.0, 256.0]));
        assert!(!owner_vanished_inside([256.0, 70.0, 509.0]));
        assert!(!owner_vanished_inside([-1.0, 70.0, 256.0]));
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_charge_box_hangs_below_its_position() {
        let charge = WindCharge::thrown(1, [0.0, 10.0, 0.0], [0.0; 3], Owner::default());
        let bounds = charge.bounds();
        assert!((bounds[1] - (10.0 - BOX_DROP)).abs() < 1e-9);
    }
}
