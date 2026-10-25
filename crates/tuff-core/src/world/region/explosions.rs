//! Explosions in a region. The math is in [`crate::world::explosion`];
//! this applies it to the region's blocks, players and entities, and hands
//! the parts that reach into neighbouring regions over as [`Blast`]s and
//! [`BlockUpdate::Exploded`]. Primed TNT comes from the feature that owns
//! it, through [`crate::world::explosion::TntPrimer`].

use std::collections::HashMap;
use std::sync::Arc;

use rand::Rng;
use rand::seq::SliceRandom;
use tuff_vanilla_data::Block;
use tuff_vanilla_data::registry::{ParticleType, SoundEvent};

use super::{AIR, ChunkPos, RegionActor, RegionPos, framed, packets};
use crate::protocol::play;
use crate::world::explosion::{self, BlockInteraction, Explosion};
use crate::world::messages::{Blast, BlockUpdate, Igniter, PlayerEvent, RegionInternal};
use crate::world::region_api::BlockCause;
use crate::world::{EYE_HEIGHT, PLAYER_HALF_WIDTH, PLAYER_HEIGHT};
use tuff_item::item::ItemStack;
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::is_air;

/// Players within this distance of an explosion see it.
const EXPLOSION_VIEW_DISTANCE: f64 = 64.0;

impl RegionActor {
    /// Lights TNT at `pos` (the block is already gone) with `fuse` ticks,
    /// or the TNT feature's usual fuse. Nothing lights with no such feature.
    pub(super) fn spawn_primed_tnt(
        &mut self,
        origin: crate::world::block_ledger::TntOrigin,
        pos: BlockPos,
        fuse: Option<i32>,
        igniter: Option<Igniter>,
    ) {
        origin.spend();
        let Some(primer) = self.shared.tnt.clone() else {
            return;
        };
        let context =
            igniter
                .as_ref()
                .map_or_else(crate::world::game_event::Context::default, |i| {
                    crate::world::game_event::Context::from(
                        crate::world::game_event::Source::player(i.id),
                    )
                });
        crate::world::region_api::RegionWorld::game_event(
            self,
            crate::world::game_event::GameEvent::PrimeFuse,
            pos.center(),
            context,
        );
        let id = self.shared.next_entity_id();
        let uuid = self.rng.r#gen();
        let tnt = primer.prime(id, uuid, pos, fuse, igniter, &mut self.rng);
        let position = tnt.base().position;
        self.receive_entity(tnt, 0);
        let packet = packets([framed(&play::Sound {
            sound_event: SoundEvent::EntityTntPrimed.id(),
            category: play::sound_category::BLOCKS,
            position,
            volume: 1.0,
            pitch: 1.0,
            seed: self.rng.r#gen(),
        })]);
        self.send_sound_near(position, 1.0, None, packet);
    }

    /// Takes the TNT block at `pos` out of the world and lights it, when
    /// this region owns it and TNT is allowed to explode.
    #[expect(
        clippy::disallowed_methods,
        reason = "TNT block primed by the duplication glitch (prime_tnt_block)"
    )]
    pub(super) fn prime_tnt_block(&mut self, pos: BlockPos) {
        if !self.owns(pos)
            || !self.shared.rule_bool("tnt_explodes")
            || RegionActor::block(self, pos).map(|state| state.block()) != Some(Block::Tnt)
        {
            return;
        }
        if self.ghost_dupe(pos) == Some(crate::world::block_ledger::VanillaDupe::Tnt) {
            self.set_block_flags(
                pos,
                AIR,
                crate::world::redstone::UPDATE_ALL,
                crate::world::redstone::UPDATE_LIMIT,
            );
            if let Some(origin) = crate::world::block_ledger::duplicate_tnt(&self.shared.config) {
                self.spawn_primed_tnt(origin, pos, None, None);
            }
        } else if let Some(broken) = self.claim(
            pos,
            crate::world::block_ledger::Remains::Air,
            crate::world::block_ledger::ClaimReason::Entity,
        ) {
            self.spawn_primed_tnt(
                crate::world::block_ledger::TntOrigin::Block(broken),
                pos,
                None,
                None,
            );
        }
    }

    /// Sets off an explosion centred in this region.
    pub(super) fn explode(&mut self, explosion: Explosion) {
        if !self.allows_detonation(&explosion) {
            return;
        }
        self.announce_explosion(&explosion);
        let cause = explosion_cause(&explosion);
        self.with_cause(cause, |region| {
            region.batch_block_changes(|region| region.detonate(explosion, None))
        });
    }

    /// Sets off an explosion whose rays meet water's resistance at
    /// `shield`, as a respawn anchor's does beside flowing water.
    pub(super) fn explode_shielded(&mut self, explosion: Explosion, shield: BlockPos) {
        if !self.allows_detonation(&explosion) {
            return;
        }
        self.announce_explosion(&explosion);
        let cause = explosion_cause(&explosion);
        self.with_cause(cause, |region| {
            region.batch_block_changes(|region| region.detonate(explosion, Some(shield)))
        });
    }

    fn allows_detonation(&mut self, explosion: &Explosion) -> bool {
        if self.shared.region_hooks.is_empty() {
            return true;
        }
        let hooks = self.shared.region_hooks.clone();
        let cause = explosion.cause();
        hooks
            .iter()
            .all(|hook| hook.allows_detonation(self, &cause))
    }

    fn announce_explosion(&mut self, explosion: &Explosion) {
        use crate::world::game_event::{Context, GameEvent, Source};
        let context = explosion
            .igniter
            .as_ref()
            .map_or_else(Context::default, |i| Context::from(Source::player(i.id)));
        crate::world::region_api::RegionWorld::game_event(
            self,
            GameEvent::Explode,
            explosion.center,
            context,
        );
    }

    fn detonate(&mut self, explosion: Explosion, shield: Option<BlockPos>) {
        let shape = self.dimension().shape;
        let mut rays: rand::rngs::StdRng = rand::SeedableRng::seed_from_u64(self.rng.r#gen());
        let rules = self.shared.explosion_rules.clone();
        let world = |pos: BlockPos| self.block(pos);
        let spared = |pos: BlockPos| rules.spares(&explosion, pos, &world);
        let wind = explosion.blocks.wind();
        let mut positions = explosion::exploded_positions_with(
            explosion.center,
            explosion.radius,
            &mut rays,
            |pos| {
                if !shape.contains_y(pos.y) {
                    None
                } else if shield == Some(pos) {
                    Some(Block::Water.default_state())
                } else {
                    self.block(pos)
                }
            },
            |pos, state| rules.ray_resistance(&explosion, pos, state, &world),
        );
        positions.retain(|pos| !spared(*pos));
        if !self.shared.region_hooks.is_empty() {
            let hooks = self.shared.region_hooks.clone();
            let cause = explosion.cause();
            for hook in hooks.iter() {
                hook.explosion_detonated(self, &cause, explosion.radius, positions.len());
            }
        }
        let blast = Blast {
            center: explosion.center,
            radius: explosion.radius,
            damage_type: explosion.damage_type,
            igniter: explosion.igniter.clone(),
            culprit: explosion.culprit.as_deref().cloned(),
            wind,
        };
        let pushed = self.apply_blast(&blast, true);
        for region in self.regions_reached(&blast) {
            if let Some(neighbor) = self.border.neighbors.get(&region) {
                neighbor.internal(RegionInternal::Blast(blast.clone()));
            }
        }
        let mut remote: HashMap<RegionPos, Vec<BlockUpdate>> = HashMap::new();
        if wind.is_some() {
            let mut elsewhere = Vec::new();
            for &pos in &positions {
                if self.owns(pos) {
                    self.wind_reached(pos);
                } else {
                    elsewhere.push(pos);
                }
            }
            if !elsewhere.is_empty() {
                let elsewhere: Arc<[BlockPos]> = elsewhere.into();
                let reach = f64::from(explosion.radius) + 1.0;
                crate::world::area::broadcast(
                    self,
                    crate::world::area::around(explosion.center, reach),
                    move |world| {
                        for &pos in elsewhere.iter() {
                            world.wind_reached(pos);
                        }
                    },
                );
            }
        }
        if !matches!(
            explosion.blocks,
            BlockInteraction::Keep | BlockInteraction::Trigger(_)
        ) {
            let decay = explosion.blocks.decays().then_some(explosion.radius);
            let mut shuffled = positions.clone();
            shuffled.shuffle(&mut self.rng);
            let mut drops: Vec<(BlockPos, ItemStack)> = Vec::new();
            let cause = explosion.cause();
            for pos in shuffled {
                let chunk = ChunkPos::of_block(pos);
                if !self.pos.contains(chunk) {
                    remote
                        .entry(chunk.region())
                        .or_default()
                        .push(BlockUpdate::Exploded {
                            pos,
                            decay,
                            cause: Box::new(cause.clone()),
                        });
                    continue;
                }
                for stack in self.explode_block(pos, decay, &cause) {
                    collect_drop(&mut drops, pos, stack);
                }
            }
            for (pos, stack) in drops {
                self.drop_items(pos, vec![stack]);
            }
        }
        if explosion.fire {
            for pos in &positions {
                if self.rng.gen_range(0..3) != 0 {
                    continue;
                }
                let below = BlockPos::new(pos.x, pos.y - 1, pos.z);
                let (Some(state), Some(ground)) = (self.block(*pos), self.block(below)) else {
                    continue;
                };
                if !is_air(state) || !ground.is_solid_render() {
                    continue;
                }
                let Some(fire) = self.shared.explosion_rules.fire(ground) else {
                    continue;
                };
                let chunk = ChunkPos::of_block(*pos);
                if self.pos.contains(chunk) {
                    self.put_block(*pos, fire);
                } else {
                    remote
                        .entry(chunk.region())
                        .or_default()
                        .push(BlockUpdate::Place {
                            pos: *pos,
                            state: fire,
                            cause: self.sim.cause,
                        });
                }
            }
        }
        self.send_block_updates(remote);
        self.send_explosion_packets(&explosion, positions.len(), &pushed);
    }

    /// The Explosion packet for everyone viewing the blast's chunk, with
    /// the push for players this region moved.
    fn send_explosion_packets(
        &self,
        explosion: &Explosion,
        block_count: usize,
        pushed: &HashMap<crate::world::messages::PlayerId, [f64; 3]>,
    ) {
        let small = explosion.radius < explosion::LARGE_EXPLOSION_RADIUS
            || explosion.blocks == BlockInteraction::Keep;
        let wind = explosion.blocks.wind();
        let particle = match (wind.is_some(), small) {
            (true, true) => ParticleType::GustEmitterSmall,
            (true, false) => ParticleType::GustEmitterLarge,
            (false, true) => ParticleType::Explosion,
            (false, false) => ParticleType::ExplosionEmitter,
        };
        let sound = wind.map_or(SoundEvent::EntityGenericExplode, |wind| wind.sound);
        let block_particles: Vec<(u32, f32, f32, u32)> = if wind.is_some() {
            Vec::new()
        } else {
            tuff_vanilla_data::world::EXPLOSION_BLOCK_PARTICLES
                .iter()
                .map(|(particle, scaling, speed, weight)| {
                    (particle.id(), *scaling, *speed, *weight)
                })
                .collect()
        };
        let chunk = ChunkPos::of_position(explosion.center);
        let dimension = self.dimension;
        let center = explosion.center;
        self.fanout_chunk(chunk, None, |viewer| {
            let seen = self.ents.presence.get(&viewer.id).is_none_or(|p| {
                crate::world::distance_sq(p.position, center) < EXPLOSION_VIEW_DISTANCE.powi(2)
            });
            let packet = seen.then(|| {
                framed(&play::Explode {
                    center,
                    radius: explosion.radius,
                    block_count: block_count as i32,
                    knockback: pushed.get(&viewer.id).copied(),
                    particle: particle.id(),
                    sound: sound.id(),
                    block_particles: &block_particles,
                })
            });
            PlayerEvent::ChunkPackets {
                dimension,
                pos: chunk,
                packets: packets(packet),
            }
        });
    }

    /// A wind burst reached the block at `pos`: a lever flips, a button
    /// presses. Nothing happens unless this region owns it.
    pub(super) fn wind_reached(&mut self, pos: BlockPos) {
        if self.owns(pos)
            && let Some(state) = self.block(pos)
        {
            let rules = self.shared.explosion_rules.clone();
            if rules.wind_reached(self, pos, state) {
                return;
            }
            self.block_behavior(state.block())
                .exploded(self, pos, state);
        }
    }

    /// Regions other than this one within the blast's reach of entities.
    fn regions_reached(&self, blast: &Blast) -> Vec<RegionPos> {
        let reach = blast.radius as f64 * 2.0 + 1.0;
        let mut regions = Vec::new();
        for dx in [-reach, reach] {
            for dz in [-reach, reach] {
                let region = ChunkPos::of_position([
                    blast.center[0] + dx,
                    blast.center[1],
                    blast.center[2] + dz,
                ])
                .region();
                if region != self.pos && !regions.contains(&region) {
                    regions.push(region);
                }
            }
        }
        regions
    }

    /// Hurts and pushes this region's own players and entities
    /// caught in a blast. Returns the push of each player hurt, and tells
    /// those players; `push_sent` says the caller sends them the Explosion
    /// packet carrying it.
    pub(super) fn apply_blast(
        &mut self,
        blast: &Blast,
        push_sent: bool,
    ) -> HashMap<crate::world::messages::PlayerId, [f64; 3]> {
        let mut pushed = HashMap::new();
        if blast.radius < 1.0e-5 {
            return pushed;
        }
        let level_positions: Vec<_> = self
            .ents
            .presence
            .iter()
            .filter(|(_, p)| p.is_physical() && self.pos.contains(p.chunk))
            .map(|(id, p)| {
                (
                    *id,
                    p.position,
                    p.handle.clone(),
                    p.explosion_knockback_resistance,
                    p.creative_flying,
                )
            })
            .collect();
        for (id, position, handle, resistance, flying) in level_positions {
            let bounds = [
                position[0] - PLAYER_HALF_WIDTH,
                position[1],
                position[2] - PLAYER_HALF_WIDTH,
                position[0] + PLAYER_HALF_WIDTH,
                position[1] + PLAYER_HEIGHT,
                position[2] + PLAYER_HALF_WIDTH,
            ];
            let eye = [position[0], position[1] + EYE_HEIGHT, position[2]];
            let Some((damage, push)) = self.blast_impact(blast, position, eye, bounds) else {
                continue;
            };
            let push = push.map(|v| v * (1.0 - resistance));
            if !flying {
                pushed.insert(id, push);
            }
            handle.send(PlayerEvent::Exploded {
                damage,
                blast: blast.clone(),
                push,
                push_sent,
            });
        }
        let mut ids: Vec<i32> = self.ents.entities.keys().copied().collect();
        ids.sort_unstable();
        for id in ids {
            let Some(tracked) = self.ents.entities.get(&id) else {
                continue;
            };
            if !self.pos.contains(tracked.chunk()) {
                continue;
            }
            let parts: Vec<(usize, f32)> = if blast.wind.is_some() {
                Vec::new()
            } else {
                tracked
                    .entity
                    .part_positions()
                    .into_iter()
                    .zip(tracked.entity.parts())
                    .enumerate()
                    .filter_map(|(part, (at, bounds))| {
                        self.blast_impact(blast, at, at, bounds)
                            .map(|(damage, _)| (part, damage))
                    })
                    .collect()
            };
            if !parts.is_empty() {
                let tracked = self.ents.entities.get_mut(&id).expect("looked up above");
                for (part, damage) in parts {
                    tracked.entity.hurt_part(part, damage, blast.damage_type);
                }
            }
            let Some(tracked) = self.ents.entities.get(&id) else {
                continue;
            };
            let (position, bounds) = (tracked.entity.base().position, tracked.entity.bounds());
            let kind = tracked.entity.base().kind;
            let origin = if kind == tuff_vanilla_data::registry::EntityType::Tnt {
                position
            } else {
                [
                    position[0],
                    position[1] + crate::world::entity::eye_height(kind, bounds),
                    position[2],
                ]
            };
            let Some((damage, push)) = self.blast_impact(blast, position, origin, bounds) else {
                continue;
            };
            let tracked = self.ents.entities.get_mut(&id).expect("looked up above");
            let base = tracked.entity.base_mut();
            for axis in 0..3 {
                base.velocity[axis] += push[axis];
            }
            tracked.sync.impulse();
            if let Some(owner) = blast
                .igniter
                .as_ref()
                .map(|i| i.uuid)
                .or(blast.culprit.as_ref().map(|c| c.uuid))
            {
                tracked.entity.redirect_owner(owner);
            }
            if blast.wind.is_none()
                && !tracked.entity.base().invulnerable
                && tracked
                    .entity
                    .hurt_blamed(damage, blast.damage_type, blast.culprit.as_ref())
            {
                self.remove_entity(id, &[]);
            }
        }
        pushed
    }

    fn blast_impact(
        &self,
        blast: &Blast,
        position: [f64; 3],
        origin: [f64; 3],
        bounds: [f64; 6],
    ) -> Option<(f32, [f64; 3])> {
        let reach = blast.radius as f64 * 2.0;
        if crate::world::distance_sq(position, blast.center) > reach * reach {
            return None;
        }
        let exposure = {
            let level = |p: BlockPos| self.block(p).unwrap_or(AIR);
            explosion::seen_percent(blast.center, bounds, &level)
        };
        let knockback = blast.wind.map_or(1.0, |wind| f64::from(wind.knockback));
        explosion::impact(blast.center, blast.radius, position, origin, exposure)
            .map(|(damage, push)| (damage, push.map(|v| v * knockback)))
    }

    /// Breaks one block in an explosion, returning its drops. TNT is lit
    /// with a short fuse instead of dropping. Features hear of the block
    /// before it goes.
    pub(super) fn explode_block(
        &mut self,
        pos: BlockPos,
        decay: Option<f32>,
        cause: &crate::world::explosion::ExplosionCause,
    ) -> Vec<ItemStack> {
        let Some(state) = self.block(pos) else {
            return Vec::new();
        };
        if is_air(state) || !self.owns(pos) {
            return Vec::new();
        }
        let hooks = self.shared.region_hooks.clone();
        if !hooks
            .iter()
            .all(|hook| hook.allows_explosion(self, pos, state, cause))
        {
            return Vec::new();
        }
        for hook in hooks.iter() {
            hook.block_exploded(self, pos, state, cause);
        }
        let igniter = cause.igniter.clone();
        let Some(broken) = self.with_cause(BlockCause::explosion(cause), |region| {
            region.claim(
                pos,
                crate::world::block_ledger::Remains::Air,
                crate::world::block_ledger::ClaimReason::Exploded,
            )
        }) else {
            return Vec::new();
        };
        if state.block() == Block::Tnt
            && self.shared.rule_bool("tnt_explodes")
            && let Some(primer) = self.shared.tnt.clone()
        {
            let fuse = primer.chain_fuse(&mut self.rng);
            self.spawn_primed_tnt(
                crate::world::block_ledger::TntOrigin::Block(broken),
                pos,
                Some(fuse),
                igniter,
            );
            return Vec::new();
        }
        if !self.shared.rule_bool("block_drops") {
            return Vec::new();
        }
        self.release_infestation(pos, state, None);
        let loot = self.shared.block_loot();
        loot.exploded(broken, decay, &mut self.rng)
    }
}

/// Merges an explosion's drop into a stack already collected, up to 16 per
/// stack, as the game does; otherwise it drops where its block was.
fn explosion_cause(explosion: &Explosion) -> BlockCause {
    BlockCause::Explosion {
        igniter: explosion.igniter.as_ref().map(|igniter| igniter.uuid),
        culprit: explosion.culprit.as_ref().map(|culprit| culprit.kind),
    }
}

fn collect_drop(drops: &mut Vec<(BlockPos, ItemStack)>, pos: BlockPos, mut stack: ItemStack) {
    const MAX_MERGED: u8 = 16;
    for (_, collected) in drops.iter_mut() {
        if collected.stacks_with(&stack) {
            let limit = MAX_MERGED.min(collected.max_stack_size());
            let room = limit.saturating_sub(collected.count());
            collected.transfer(&mut stack, room);
            if stack.count() == 0 {
                return;
            }
        }
    }
    drops.push((pos, stack));
}
