//! Evoker Fangs: the evoker's melee-range spell, a spike that rises out of
//! the ground after a warmup delay and hurts whatever stands on it once it
//! strikes.

use tuff_vanilla_data::damage_type;
use tuff_vanilla_data::registry::EntityType;

use tuff_core::protocol::play;
use tuff_core::world::entity::{Effect, EntityBase, EntityHit, RegionEntity, Tick, TickContext};
use tuff_core::world::physics;
use tuff_core::world::projectile::Owner;
use tuff_core::world::{framed, packets};
use tuff_nbt::Compound;

/// Ticks it stays around once it strikes, counting down from there.
const LIFE_TICKS: i16 = 22;
/// The entity event that tells clients to play the strike animation and
/// sound.
const STRIKE_STATUS: i8 = 4;
const DAMAGE: f32 = 6.0;

#[derive(Debug)]
pub struct EvokerFangs {
    pub base: EntityBase,
    owner: Owner,
    warmup: i32,
    life: i16,
    struck: bool,
    sent_event: bool,
}

impl EvokerFangs {
    /// A fang at `position`, facing `yaw` degrees, striking after `warmup`
    /// ticks (negative counts as already due).
    pub fn new(id: i32, position: [f64; 3], yaw: f32, warmup: i32, owner: Owner) -> Self {
        let uuid = rand::random();
        let mut base = EntityBase::new(EntityType::EvokerFangs, id, uuid, position, [0.0; 3]);
        base.yaw = yaw;
        Self {
            base,
            owner,
            warmup,
            life: LIFE_TICKS,
            struck: false,
            sent_event: false,
        }
    }

    fn strike(&self, cx: &mut TickContext) {
        let attacker = self.owner.culprit(cx);
        let owner_id = self.owner.find(cx).map(|(id, _)| id);
        let area = physics::inflate(self.base.bounds(), [0.2, 0.0, 0.2]);
        let kind = if attacker.is_some() {
            &damage_type::INDIRECT_MAGIC
        } else {
            &damage_type::MAGIC
        };
        let mut hurt = |target: i32| {
            if Some(target) == owner_id {
                return;
            }
            let mut hit = EntityHit::new(DAMAGE, kind);
            hit.attacker = attacker.clone();
            hit.projectile = Some(EntityType::EvokerFangs);
            hit.position = Some(self.base.position);
            cx.effects.push(Effect::Hurt {
                target,
                hit: Box::new(hit),
            });
        };
        for player in cx.players {
            if physics::intersects(area, player.bounds()) {
                hurt(player.id);
            }
        }
        for entity in cx.entities {
            if entity.id != self.base.id
                && entity.living.is_some_and(|l| l.alive)
                && physics::intersects(area, entity.bounds)
            {
                hurt(entity.id);
            }
        }
    }
}

impl RegionEntity for EvokerFangs {
    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn base_mut(&mut self) -> &mut EntityBase {
        &mut self.base
    }

    fn save(&self, out: &mut Compound) {
        out.insert("Warmup", self.warmup);
        self.owner.save(out);
    }

    fn ticking_data(&self) -> bool {
        true
    }

    fn tick(&mut self, cx: &mut TickContext) -> Tick {
        self.warmup -= 1;
        if self.warmup >= 0 {
            return Tick::Alive;
        }
        if self.warmup == -8 && !self.struck {
            self.struck = true;
            self.strike(cx);
        }
        if !self.sent_event {
            self.sent_event = true;
            let packet = framed(&play::EntityEvent {
                entity_id: self.base.id,
                status: STRIKE_STATUS,
            });
            cx.effects.push(Effect::ChunkPackets(packets([packet])));
        }
        self.life -= 1;
        if self.life < 0 {
            return Tick::Discard;
        }
        Tick::Alive
    }
}

pub fn load(base: EntityBase, nbt: &mut Compound) -> Option<Box<dyn RegionEntity>> {
    let warmup = tuff_core::world::entity::take_number(nbt, "Warmup").map_or(0, |v| v as i32);
    let mut owner = Owner::default();
    owner.load(nbt);
    Some(Box::new(EvokerFangs {
        base,
        owner,
        warmup,
        life: LIFE_TICKS,
        struck: warmup < -8,
        sent_event: warmup < 0,
    }))
}
