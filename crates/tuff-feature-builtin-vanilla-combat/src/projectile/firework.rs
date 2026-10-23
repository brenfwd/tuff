//! Firework rockets: launched from a block, shot from a crossbow or
//! boosting a gliding player. A rocket climbs (or flies straight when shot
//! at an angle) until its flight time is up or it hits something, then
//! bursts: clients draw the stars, and a rocket with stars hurts the
//! living things around it that it can see.

use rand::Rng;
use tuff_vanilla_data::Item;
use tuff_vanilla_data::damage_type;
use tuff_vanilla_data::registry::{EntityType, SoundEvent};

use tuff_core::protocol::play::{self, MetadataValue};
use tuff_core::world::entity::{
    Effect, EntityBase, EntityHit, NeighbourTask, RegionEntity, Tick, TickContext, take, take_bool,
    take_number,
};
use tuff_core::world::physics;
use tuff_core::world::projectile::{Owner, block_hit, margin, rotation_of, targets_along};
use tuff_core::world::region_api::RegionWorld;
use tuff_core::world::{framed, packets};
use tuff_item::item::{ItemStack, MintReason};
use tuff_nbt::Compound;
use tuff_vanilla_data::{Block, BlockState};
use tuff_world_model::BlockPos;

const DATA_ITEM: u8 = 8;
const DATA_ATTACHED: u8 = 9;
const DATA_SHOT_AT_ANGLE: u8 = 10;
/// The entity event clients burst a rocket's stars on.
const EVENT_EXPLODE: i8 = 17;
const ACCELERATION: f64 = 1.15;
const LIFT: f64 = 0.04;
const BLAST_RADIUS: f64 = 5.0;

#[derive(Debug)]
pub struct FireworkRocket {
    pub base: EntityBase,
    pub owner: Owner,
    pub item: ItemStack,
    life: i32,
    lifetime: i32,
    shot_at_angle: bool,
    /// The gliding player it boosts, by entity id.
    attached: Option<i32>,
    ticks: u32,
}

impl FireworkRocket {
    /// A rocket launched straight up from `position` (a click on a block,
    /// a dispenser), drifting a little sideways.
    pub fn launched(
        id: i32,
        position: [f64; 3],
        item: ItemStack,
        owner: Owner,
        rng: &mut rand::rngs::StdRng,
    ) -> Self {
        let velocity = [
            physics::triangle(rng, 0.0, 0.002297),
            0.05,
            physics::triangle(rng, 0.0, 0.002297),
        ];
        let lifetime = tuff_core::world::projectile::firework_lifetime(&item, rng);
        Self::build(id, position, velocity, item, owner, lifetime)
    }

    /// A rocket shot from a crossbow along `velocity`.
    pub fn shot(
        id: i32,
        position: [f64; 3],
        velocity: [f64; 3],
        item: ItemStack,
        owner: Owner,
        rng: &mut rand::rngs::StdRng,
    ) -> Self {
        let lifetime = tuff_core::world::projectile::firework_lifetime(&item, rng);
        let mut rocket = Self::build(id, position, velocity, item, owner, lifetime);
        rocket.shot_at_angle = true;
        rocket
    }

    /// A rocket boosting the gliding player `player`, which bursts after
    /// `lifetime` ticks.
    pub fn attached(
        id: i32,
        player: i32,
        position: [f64; 3],
        item: ItemStack,
        owner: Owner,
        lifetime: i32,
    ) -> Self {
        let mut rocket = Self::build(id, position, [0.0; 3], item, owner, lifetime);
        rocket.attached = Some(player);
        rocket
    }

    fn build(
        id: i32,
        position: [f64; 3],
        velocity: [f64; 3],
        item: ItemStack,
        owner: Owner,
        lifetime: i32,
    ) -> Self {
        let mut base = EntityBase::new(
            EntityType::FireworkRocket,
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
            item: item.keep(1, tuff_item::item::BurnReason::Consumed),
            life: 0,
            lifetime,
            shot_at_angle: false,
            attached: None,
            ticks: 0,
        }
    }

    fn stars(&self) -> usize {
        tuff_core::world::projectile::firework_flight(&self.item).1
    }

    /// Bursts: clients draw it, and with stars it hurts what is near.
    fn explode(&self, cx: &mut TickContext) -> Tick {
        cx.effects.push(Effect::EntityPackets(packets([framed(
            &play::EntityEvent {
                entity_id: self.base.id,
                status: EVENT_EXPLODE,
            },
        )])));
        self.blast(cx);
        Tick::Discard
    }

    fn blast(&self, cx: &mut TickContext) {
        let stars = self.stars();
        if stars == 0 {
            return;
        }
        let damage = 5.0 + 2.0 * stars as f32;
        let attacker = self.owner.culprit(cx);
        let center = self.base.position;
        let hit = |amount: f32| {
            let mut hit = EntityHit::new(amount, &damage_type::FIREWORKS);
            hit.attacker = attacker.clone();
            hit.projectile = Some(EntityType::FireworkRocket);
            hit.position = Some(center);
            hit
        };
        if let Some(player) = self.attached {
            cx.effects.push(Effect::Hurt {
                target: player,
                hit: Box::new(hit(damage)),
            });
        }
        let area = physics::inflate(self.base.bounds(), [BLAST_RADIUS; 3]);
        let mut targets: Vec<(i32, [f64; 3], f64)> = cx
            .players
            .iter()
            .filter(|p| physics::intersects(area, p.bounds()))
            .map(|p| (p.id, p.position, p.height))
            .collect();
        targets.extend(
            cx.entities
                .iter()
                .filter(|e| e.living.is_some_and(|l| l.alive))
                .filter(|e| physics::intersects(area, e.bounds))
                .map(|e| (e.id, e.position, e.bounds[4] - e.bounds[1])),
        );
        let level = |pos| (cx.block)(pos).unwrap_or(Block::Stone.default_state());
        for (id, position, height) in targets {
            if Some(id) == self.attached {
                continue;
            }
            if let Some(scale) = blast_scale(center, position, height, &level) {
                cx.effects.push(Effect::Hurt {
                    target: id,
                    hit: Box::new(hit(damage * scale)),
                });
            }
        }
        let attached = self.attached;
        let blast = hit(damage);
        cx.effects.push(Effect::InNeighbours {
            area,
            run: NeighbourTask(std::sync::Arc::new(move |world| {
                neighbour_blast(world, center, area, attached, &blast);
            })),
        });
    }

    /// Follows the player it boosts, where the region sees it.
    fn follow(&mut self, cx: &TickContext) {
        let Some(player) = self.attached else {
            return;
        };
        if let Some(player) = cx.players.iter().find(|p| p.id == player) {
            self.base.position = player.position;
        }
    }
}

/// How much of a burst at `center` reaches a thing at `position`, `None`
/// if it is out of reach or both its feet and middle are hidden.
fn blast_scale(
    center: [f64; 3],
    position: [f64; 3],
    height: f64,
    level: &dyn Fn(BlockPos) -> BlockState,
) -> Option<f32> {
    let distance = tuff_core::world::distance_sq(center, position).sqrt();
    if distance > BLAST_RADIUS {
        return None;
    }
    let seen = (0..2).any(|step| {
        let to = [
            position[0],
            position[1] + height * 0.5 * f64::from(step),
            position[2],
        ];
        tuff_core::world::raycast::cast_colliders(center, to, level).is_none()
    });
    seen.then(|| ((BLAST_RADIUS - distance) / BLAST_RADIUS).sqrt() as f32)
}

/// A burst reaching over a border hurts the living things the
/// neighbour holds (players are hurt from the rocket's own region).
fn neighbour_blast(
    world: &mut dyn RegionWorld,
    center: [f64; 3],
    area: [f64; 6],
    attached: Option<i32>,
    blast: &EntityHit,
) {
    let mut hits = Vec::new();
    {
        let level = |pos| world.block(pos).unwrap_or(Block::Stone.default_state());
        for id in world.entity_ids() {
            let Some(tracked) = world.entity(id) else {
                continue;
            };
            let bounds = tracked.entity.bounds();
            let alive = tracked.entity.living().is_some_and(|l| l.alive);
            if !alive || Some(id) == attached || !physics::intersects(area, bounds) {
                continue;
            }
            let position = tracked.entity.base().position;
            if let Some(scale) = blast_scale(center, position, bounds[4] - bounds[1], &level) {
                hits.push((id, scale));
            }
        }
    }
    for (id, scale) in hits {
        let mut hit = blast.clone();
        hit.amount *= scale;
        world.hit_entity(id, hit);
    }
}

impl RegionEntity for FireworkRocket {
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
        self.owner.save(out);
        out.insert("Life", self.life);
        out.insert("LifeTime", self.lifetime);
        out.insert("FireworksItem", self.item.to_storage());
        out.insert("ShotAtAngle", self.shot_at_angle);
    }

    fn metadata(&self) -> Vec<(u8, MetadataValue)> {
        let mut values = vec![(
            DATA_ITEM,
            MetadataValue::Slot(Some(tuff_item::item::ItemSnapshot::of(&self.item))),
        )];
        if self.attached.is_some() {
            values.push((DATA_ATTACHED, MetadataValue::OptionalVarInt(self.attached)));
        }
        if self.shot_at_angle {
            values.push((DATA_SHOT_AT_ANGLE, MetadataValue::Boolean(true)));
        }
        values
    }

    fn spawn_data(&self) -> i32 {
        self.owner.spawn_data()
    }

    fn tick(&mut self, cx: &mut TickContext) -> Tick {
        self.ticks += 1;
        if self.attached.is_some() {
            self.follow(cx);
        } else {
            if !self.shot_at_angle {
                let v = self.base.velocity;
                self.base.velocity = [v[0] * ACCELERATION, v[1] + LIFT, v[2] * ACCELERATION];
            }
            let from = self.base.position;
            let movement = self.base.velocity;
            let end = [
                from[0] + movement[0],
                from[1] + movement[1],
                from[2] + movement[2],
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
                |id| !left && Some(id) == owner,
                cx,
            )
            .into_iter()
            .next()
            .filter(|t| !tuff_core::world::projectile::deflect_off(&mut self.base, t, cx));
            self.owner.check_left(self.base.bounds(), movement, cx);
            if let Some(target) = target {
                self.base.position = target.location;
                return self.explode(cx);
            }
            if let Some(hit) = block {
                tuff_core::world::projectile::struck_block(hit, &self.base, &self.owner, cx);
                if self.stars() > 0 {
                    self.base.position = hit.location;
                    return self.explode(cx);
                }
            }
            let moved = physics::collide(self.base.bounds(), movement, 0.0, false, cx.block);
            for (axis, step) in moved.iter().enumerate() {
                self.base.position[axis] += step;
            }
            let (yaw, pitch) = rotation_of(movement);
            self.base.yaw = yaw;
            self.base.pitch = pitch;
        }
        if self.life == 0 && !self.base.silent {
            cx.effects
                .push(Effect::ChunkPackets(packets([framed(&play::Sound {
                    sound_event: SoundEvent::EntityFireworkRocketLaunch.id(),
                    category: play::sound_category::AMBIENT,
                    position: self.base.position,
                    volume: 3.0,
                    pitch: 1.0,
                    seed: cx.rng.r#gen(),
                })])));
        }
        self.life += 1;
        if self.life > self.lifetime {
            return self.explode(cx);
        }
        Tick::Alive
    }
}

/// Loads a saved rocket. One attached to a player comes back loose.
pub fn load(base: EntityBase, nbt: &mut Compound) -> Option<Box<dyn RegionEntity>> {
    let mut owner = Owner::default();
    owner.load(nbt);
    let item = take(nbt, "FireworksItem", |tag| {
        ItemStack::from_storage(tag.as_compound()?)
    })
    .unwrap_or_else(|| ItemStack::mint(Item::FireworkRocket, 1, MintReason::Spawn));
    Some(Box::new(FireworkRocket {
        base,
        owner,
        item,
        life: take_number(nbt, "Life").map_or(0, |v| v as i32),
        lifetime: take_number(nbt, "LifeTime").map_or(0, |v| v as i32),
        shot_at_angle: take_bool(nbt, "ShotAtAngle").unwrap_or(false),
        attached: None,
        ticks: 0,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn rockets_without_a_fireworks_patch_fly_for_one() {
        let plain = ItemStack::mint(Item::FireworkRocket, 1, MintReason::Test);
        assert_eq!(
            tuff_core::world::projectile::firework_flight(&plain),
            (1, 0)
        );
        let mut rng = rand::rngs::StdRng::seed_from_u64(3);
        for _ in 0..50 {
            assert!(
                (20..=31).contains(&tuff_core::world::projectile::firework_lifetime(
                    &plain, &mut rng
                ))
            );
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn rocket_data_indices_match_the_dumped_layout() {
        let slot = MetadataValue::Slot(None).type_id() as u8;
        let optional = MetadataValue::OptionalVarInt(None).type_id() as u8;
        let boolean = MetadataValue::Boolean(false).type_id() as u8;
        assert_eq!(
            EntityType::FireworkRocket.data_layout()[8..],
            [slot, optional, boolean]
        );
    }

    use rand::SeedableRng;
}
