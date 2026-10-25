//! Eyes of ender: thrown toward the nearest stronghold, they fly a dozen
//! blocks that way while climbing, hover, and after 80 ticks either drop
//! back as an item or shatter (one in five).

use rand::Rng;
use tuff_item::item::BurnReason;
use tuff_vanilla_data::Item;
use tuff_vanilla_data::registry::{EntityType, SoundEvent};

use tuff_core::protocol::play::{self, MetadataValue};
use tuff_core::world::entity::{Effect, EntityBase, RegionEntity, Tick, TickContext, take};
use tuff_core::world::{framed, packets};
use tuff_item::item::{ItemStack, MintReason};
use tuff_nbt::Compound;
use tuff_world_model::BlockPos;

const DATA_ITEM: u8 = 8;
/// A target farther than this is replaced by one this far along the way.
const TOO_FAR_DISTANCE: f64 = 12.0;
/// ... and this much higher.
const TOO_FAR_SIGNAL_HEIGHT: f64 = 8.0;
const LIFETIME: u32 = 80;
/// The level event of a shattering eye.
pub const PARTICLES_EYE_OF_ENDER_DEATH: i32 = 2003;

#[derive(Debug)]
pub struct EyeOfEnder {
    pub base: EntityBase,
    pub item: ItemStack,
    /// Where it flies; not saved, so a reloaded eye drifts.
    target: Option<[f64; 3]>,
    life: u32,
    survive_after_death: bool,
}

impl EyeOfEnder {
    /// An eye at `position` carrying `item`, flying toward `target` (a
    /// structure's locate position).
    pub fn thrown(
        id: i32,
        position: [f64; 3],
        item: ItemStack,
        target: BlockPos,
        rng: &mut impl Rng,
    ) -> Self {
        let base = EntityBase::new(EntityType::EyeOfEnder, id, rng.r#gen(), position, [0.0; 3]);
        let mut eye = Self {
            base,
            item: item.keep(1, BurnReason::Consumed),
            target: None,
            life: 0,
            survive_after_death: false,
        };
        eye.signal_to(
            [
                f64::from(target.x),
                f64::from(target.y),
                f64::from(target.z),
            ],
            rng,
        );
        eye
    }

    fn signal_to(&mut self, target: [f64; 3], rng: &mut impl Rng) {
        let [x, y, z] = self.base.position;
        let (dx, dz) = (target[0] - x, target[2] - z);
        let horizontal = (dx * dx + dz * dz).sqrt();
        self.target = Some(if horizontal > TOO_FAR_DISTANCE {
            [
                x + dx / horizontal * TOO_FAR_DISTANCE,
                y + TOO_FAR_SIGNAL_HEIGHT,
                z + dz / horizontal * TOO_FAR_DISTANCE,
            ]
        } else {
            target
        });
        self.life = 0;
        self.survive_after_death = rng.gen_range(0..5) > 0;
    }

    pub fn target(&self) -> Option<[f64; 3]> {
        self.target
    }

    pub fn survives(&self) -> bool {
        self.survive_after_death
    }
}

/// The next velocity of an eye at `position` moving with `velocity`
/// toward `target`: it eases to the target's column and rises or sinks
/// toward its height.
fn steer(velocity: [f64; 3], position: [f64; 3], target: [f64; 3]) -> [f64; 3] {
    let (hx, hz) = (target[0] - position[0], target[2] - position[2]);
    let horizontal = (hx * hx + hz * hz).sqrt();
    let speed = (velocity[0] * velocity[0] + velocity[2] * velocity[2]).sqrt();
    let mut wanted = speed + 0.0025 * (horizontal - speed);
    let mut vy = velocity[1];
    if horizontal < 1.0 {
        wanted *= 0.8;
        vy *= 0.8;
    }
    let rise = if position[1] - velocity[1] < target[1] {
        1.0
    } else {
        -1.0
    };
    let scale = wanted / horizontal;
    [hx * scale, vy + (rise - vy) * 0.015, hz * scale]
}

impl RegionEntity for EyeOfEnder {
    fn take_items(&mut self) -> Vec<ItemStack> {
        let item = std::mem::replace(&mut self.item, ItemStack::empty());
        if item.is_empty() {
            Vec::new()
        } else {
            vec![item]
        }
    }

    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn base_mut(&mut self) -> &mut EntityBase {
        &mut self.base
    }

    fn save(&self, out: &mut Compound) {
        out.insert("Item", self.item.to_storage());
    }

    fn metadata(&self) -> Vec<(u8, MetadataValue)> {
        vec![(
            DATA_ITEM,
            MetadataValue::Slot(Some(tuff_item::item::ItemSnapshot::of(&self.item))),
        )]
    }

    fn tick(&mut self, cx: &mut TickContext) -> Tick {
        let [x, y, z] = self.base.position;
        let [vx, vy, vz] = self.base.velocity;
        let next = [x + vx, y + vy, z + vz];
        if let Some(target) = self.target {
            self.base.velocity = steer(self.base.velocity, next, target);
        }
        self.base.position = next;
        self.life += 1;
        if self.life <= LIFETIME {
            return Tick::Alive;
        }
        let sound = framed(&play::Sound {
            sound_event: SoundEvent::EntityEnderEyeDeath.id(),
            category: play::sound_category::NEUTRAL,
            position: next,
            volume: 1.0,
            pitch: 1.0,
            seed: cx.rng.r#gen(),
        });
        cx.effects.push(Effect::ChunkPackets(packets([sound])));
        if self.survive_after_death {
            let velocity = [
                cx.rng.r#gen::<f64>() * 0.2 - 0.1,
                0.2,
                cx.rng.r#gen::<f64>() * 0.2 - 0.1,
            ];
            cx.effects.push(Effect::DropItem {
                stack: std::mem::replace(&mut self.item, ItemStack::empty()),
                position: next,
                velocity,
                pickup_delay: 0,
            });
        } else {
            let event = framed(&play::LevelEvent {
                event: PARTICLES_EYE_OF_ENDER_DEATH,
                position: BlockPos::new(
                    next[0].floor() as i32,
                    next[1].floor() as i32,
                    next[2].floor() as i32,
                ),
                data: 0,
                global: false,
            });
            cx.effects.push(Effect::ChunkPackets(packets([event])));
        }
        Tick::Discard
    }
}

pub fn load(base: EntityBase, nbt: &mut Compound) -> Option<Box<dyn RegionEntity>> {
    #[expect(
        clippy::disallowed_methods,
        reason = "decoding a saved stack and item a mob spawns, wears or throws (load)"
    )]
    let item = take(nbt, "Item", |tag| {
        ItemStack::from_storage(tag.as_compound()?)
    })
    .unwrap_or_else(|| ItemStack::mint(Item::EnderEye, 1, MintReason::Spawn));
    Some(Box::new(EyeOfEnder {
        base,
        item,
        target: None,
        life: 0,
        survive_after_death: false,
    }))
}

#[cfg(test)]
mod tests {
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    use super::*;

    fn eye(target: BlockPos) -> EyeOfEnder {
        let mut rng = StdRng::seed_from_u64(3);
        EyeOfEnder::thrown(
            1,
            [0.5, 70.9, 0.5],
            ItemStack::mint(Item::EnderEye, 5, MintReason::Test),
            target,
            &mut rng,
        )
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_far_target_becomes_a_point_twelve_blocks_along_and_eight_up() {
        let eye = eye(BlockPos::new(1000, 0, 0));
        let target = eye.target().unwrap();
        assert!((target[0] - 12.5).abs() < 1e-2);
        assert!((target[1] - 78.9).abs() < 1e-9);
        assert!((target[2] - 0.5).abs() < 1e-2);
        assert_eq!(eye.item.count(), 1);
    }

    #[tuff_core::watchdog]
    #[test]
    fn it_flies_toward_the_stronghold_and_climbs() {
        let mut eye = eye(BlockPos::new(-500, 0, 300));
        let target = eye.target().unwrap();
        let start = eye.base.position;
        for _ in 0..LIFETIME {
            let [x, y, z] = eye.base.position;
            let [vx, vy, vz] = eye.base.velocity;
            let next = [x + vx, y + vy, z + vz];
            eye.base.velocity = steer(eye.base.velocity, next, target);
            eye.base.position = next;
        }
        let travelled = [
            eye.base.position[0] - start[0],
            eye.base.position[2] - start[2],
        ];
        let wanted = [target[0] - start[0], target[2] - start[2]];
        let dot = travelled[0] * wanted[0] + travelled[1] * wanted[1];
        assert!(dot > 0.0, "moved away from the stronghold");
        assert!(eye.base.position[1] > start[1], "did not climb");
        let cross = travelled[0] * wanted[1] - travelled[1] * wanted[0];
        assert!(cross.abs() < 1e-6, "left the line toward the target");
    }
}
