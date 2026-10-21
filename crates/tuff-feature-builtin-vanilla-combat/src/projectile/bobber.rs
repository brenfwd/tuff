//! A fishing bobber: cast along the player's view, it hooks the first thing
//! it meets or settles on water, where fish are lured and bite after a
//! random wait. Reeling in pulls a hooked entity or lands the catch.

use rand::Rng;
use tuff_core::world::contact;
use tuff_vanilla_data::registry::{EntityType, SoundEvent};
use tuff_vanilla_data::{Block, Item};

use tuff_core::protocol::play::{self, MetadataValue};
use tuff_core::world::entity::{Effect, EntityBase, NearbyPlayer, RegionEntity, Tick, TickContext};
use tuff_core::world::messages::{AdvancementEvent, PlayerEvent};
use tuff_core::world::predicate::EntityFacts;
use tuff_core::world::projectile::{
    Owner, block_hit, border_stop, bounce_off_border, lerp_rotation, rotation_of,
};
use tuff_core::world::{distance_sq, framed, packets, physics};
use tuff_item::item::ItemStack;
use tuff_nbt::Compound;
use tuff_world_model::BlockPos;

use super::bite::{Cue, Line, Surroundings, Turn};

const GRAVITY: f64 = 0.03;
const DRAG: f64 = 0.92;
const MAX_DISTANCE_SQ: f64 = 1024.0;
const MAX_LANDED_TICKS: u32 = 1200;
const MAX_OUT_OF_WATER: i32 = 10;
const DATA_HOOKED: u8 = 8;
const DATA_BITING: u8 = 9;
const PICKUP_DELAY: u32 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Flying,
    Hooked(i32),
    Bobbing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layer {
    Above,
    Inside,
    Invalid,
}

#[derive(Debug)]
pub struct Bobber {
    pub base: EntityBase,
    pub owner: Owner,
    state: State,
    rod: ItemStack,
    luck: f32,
    lure: i32,
    landed: bool,
    life: u32,
    line: Line,
    open_water: bool,
    out_of_water: i32,
    biting: bool,
    reel: bool,
    changed: Vec<u8>,
}

impl Bobber {
    /// A cast from a player whose eyes are at `eye` looking along
    /// `(yaw, pitch)`.
    pub fn cast(
        id: i32,
        eye: [f64; 3],
        (yaw, pitch): (f32, f32),
        owner: Owner,
        rod: ItemStack,
        owner_luck: f32,
        rng: &mut impl Rng,
    ) -> Self {
        let luck = (rod.enchantment_value(|e| &e.fishing_luck_bonus, 0.0) as i32).max(0) as f32
            + owner_luck;
        let lure = (rod
            .enchantment_value(|e| &e.fishing_time_reduction, 0.0)
            .max(0.0)
            * 20.0) as i32;
        let yaw_r = -yaw.to_radians() - std::f32::consts::PI;
        let (y_sin, y_cos) = (yaw_r.sin(), yaw_r.cos());
        let x_cos = -(-pitch.to_radians()).cos();
        let x_sin = (-pitch.to_radians()).sin();
        let start = [
            eye[0] - f64::from(y_sin) * 0.3,
            eye[1],
            eye[2] - f64::from(y_cos) * 0.3,
        ];
        let direction = [
            f64::from(-y_sin),
            f64::from((-(x_sin / x_cos)).clamp(-5.0, 5.0)),
            f64::from(-y_cos),
        ];
        let length = direction.iter().map(|c| c * c).sum::<f64>().sqrt();
        let velocity = direction.map(|c| c * (0.6 / length + triangle(rng, 0.5, 0.010_336_5)));
        let mut base = EntityBase::new(
            EntityType::FishingBobber,
            id,
            rand::random(),
            start,
            velocity,
        );
        let (yaw, pitch) = rotation_of(velocity);
        base.yaw = yaw;
        base.pitch = pitch;
        Self {
            base,
            owner,
            state: State::Flying,
            rod,
            luck,
            lure,
            landed: false,
            life: 0,
            line: Line::Slack,
            open_water: true,
            out_of_water: 0,
            biting: false,
            reel: false,
            changed: Vec::new(),
        }
    }

    /// A bobber already floating with a fish on the line.
    pub fn with_fish_on(mut self) -> Self {
        self.state = State::Bobbing;
        self.line = Line::fish_on();
        self.biting = true;
        self
    }

    fn hooked_id(&self) -> Option<i32> {
        match self.state {
            State::Hooked(id) => Some(id),
            _ => None,
        }
    }

    fn set_biting(&mut self, biting: bool) {
        if self.biting != biting {
            self.biting = biting;
            self.changed.push(DATA_BITING);
        }
    }

    fn set_state(&mut self, state: State) {
        let before = self.hooked_id();
        self.state = state;
        if before != self.hooked_id() {
            self.changed.push(DATA_HOOKED);
        }
    }

    fn value(&self, index: u8) -> Option<MetadataValue> {
        match index {
            DATA_HOOKED => Some(MetadataValue::VarInt(
                self.hooked_id().map_or(0, |id| id + 1),
            )),
            DATA_BITING => Some(MetadataValue::Boolean(self.biting)),
            _ => None,
        }
    }

    fn gone(&self) {
        if let Some(handle) = &self.owner.handle {
            handle.send(PlayerEvent::BobberGone(self.base.id));
        }
    }

    fn block_pos(&self) -> BlockPos {
        let [x, y, z] = self.base.position;
        BlockPos::new(x.floor() as i32, y.floor() as i32, z.floor() as i32)
    }

    fn hooks_entity(
        &self,
        from: [f64; 3],
        to: [f64; 3],
        skip: Option<i32>,
        cx: &TickContext,
    ) -> Option<i32> {
        let bounds = self.base.bounds();
        let delta = [to[0] - from[0], to[1] - from[1], to[2] - from[2]];
        let area = physics::inflate(physics::expand_towards(bounds, delta), [1.0; 3]);
        let mut best: Option<(f64, i32)> = None;
        let mut consider = |id: i32, target: physics::Aabb| {
            if Some(id) == skip || id == self.base.id || !physics::intersects(area, target) {
                return;
            }
            if let Some(at) =
                tuff_core::world::projectile::clip(physics::inflate(target, [0.3; 3]), from, to)
            {
                let d = distance_sq(from, at);
                if best.is_none_or(|(b, _)| d < b) {
                    best = Some((d, id));
                }
            }
        };
        for player in cx.players {
            consider(player.id, player.bounds());
        }
        for entity in cx.entities {
            if entity.pickable || entity.item.is_some() || entity.living.is_some_and(|l| l.alive) {
                consider(entity.id, entity.bounds);
            }
        }
        best.map(|(_, id)| id)
    }

    fn tend_line(&mut self, cell: BlockPos, cx: &mut TickContext) {
        let (block, light) = (cx.block, cx.light);
        let env = Surroundings {
            raining: cx.raining,
            open_sky: |p: BlockPos| light(p).is_some_and(|(sky, _)| sky >= 15),
            water: |p: [f64; 3]| {
                let at = BlockPos::new(
                    p[0].floor() as i32,
                    p[1].floor() as i32,
                    p[2].floor() as i32,
                );
                block(at).is_some_and(|state| state.block() == Block::Water)
            },
        };
        let here = self.base.position;
        let effects = &mut *cx.effects;
        let turn = self
            .line
            .advance(here, cell, self.lure, &env, cx.rng, |cue| {
                effects.push(cue_effect(cue, here));
            });
        match turn {
            Some(Turn::Bit { sink }) => {
                self.set_biting(true);
                self.base.velocity[1] = sink;
            }
            Some(Turn::Escaped) => self.set_biting(false),
            None => {}
        }
    }

    fn open_water_at(&self, pos: BlockPos, cx: &TickContext) -> bool {
        let mut previous = Layer::Invalid;
        for dy in -1..=2 {
            let mut layer: Option<Layer> = None;
            for dx in -2..=2 {
                for dz in -2..=2 {
                    let kind = layer_of(BlockPos::new(pos.x + dx, pos.y + dy, pos.z + dz), cx);
                    layer = Some(match layer {
                        None => kind,
                        Some(l) if l == kind => l,
                        Some(_) => Layer::Invalid,
                    });
                }
            }
            match layer.unwrap_or(Layer::Invalid) {
                Layer::Above if previous == Layer::Invalid => return false,
                Layer::Inside if previous == Layer::Above => return false,
                Layer::Invalid => return false,
                other => previous = other,
            }
        }
        true
    }

    fn retrieve(&mut self, player: &NearbyPlayer<'_>, cx: &mut TickContext) -> Tick {
        let mut damage = 0;
        let mut fish = 0;
        let mut facts = EntityFacts {
            kind: Some(EntityType::FishingBobber),
            position: self.base.position,
            dimension: String::new(),
            ..Default::default()
        };
        let mut caught = Vec::new();
        if let Some(hooked) = self.hooked_id() {
            let target = cx
                .entities
                .iter()
                .find(|e| e.id == hooked)
                .map(|e| {
                    let baby = e.living.is_some_and(|l| l.age < 0);
                    (e.kind, e.position, e.item.is_some(), baby, false)
                })
                .or_else(|| {
                    cx.players
                        .iter()
                        .find(|p| p.id == hooked)
                        .map(|p| (EntityType::Player, p.position, false, false, p.sneaking))
                });
            if let Some((kind, position, is_item, baby, sneaking)) = target {
                let pull = [0, 1, 2].map(|i| (player.position[i] - self.base.position[i]) * 0.1);
                cx.effects.push(Effect::Push {
                    target: hooked,
                    push: pull,
                });
                facts.kind = Some(kind);
                facts.position = position;
                facts.flags.baby = baby;
                facts.flags.sneaking = sneaking;
                damage = if is_item { 3 } else { 5 };
            }
        } else if self.line.biting() {
            let items =
                cx.shared
                    .block_loot()
                    .fishing(self.luck, self.open_water, &self.rod, cx.rng);
            for stack in items {
                if tuff_datapack::tags::is(tuff_vanilla_data::tag::item::key::FISHES, stack.item) {
                    fish += 1;
                }
                caught.push(tuff_item::item::ItemSnapshot::of(&stack));
                let delta = [0, 1, 2].map(|i| player.position[i] - self.base.position[i]);
                let length = delta.iter().map(|c| c * c).sum::<f64>().sqrt();
                cx.effects.push(Effect::DropItem {
                    stack,
                    position: self.base.position,
                    velocity: [
                        delta[0] * 0.1,
                        delta[1] * 0.1 + length.sqrt() * 0.08,
                        delta[2] * 0.1,
                    ],
                    pickup_delay: PICKUP_DELAY,
                });
                cx.effects.push(Effect::Experience {
                    position: [
                        player.position[0],
                        player.position[1] + 0.5,
                        player.position[2] + 0.5,
                    ],
                    amount: cx.rng.gen_range(1..=6),
                });
            }
            damage = 1;
        }
        if self.landed {
            damage = 2;
        }
        if let Some(handle) = &self.owner.handle {
            if damage > 0
                && (self.hooked_id().is_some() || !caught.is_empty() || self.line.biting())
            {
                handle.send(PlayerEvent::Advancement(Box::new(
                    AdvancementEvent::FishingRodHooked {
                        entity: facts,
                        items: caught,
                    },
                )));
            }
            handle.send(PlayerEvent::Reeled { damage, fish });
        }
        Tick::Discard
    }
}

fn layer_of(pos: BlockPos, cx: &TickContext) -> Layer {
    let Some(state) = (cx.block)(pos) else {
        return Layer::Invalid;
    };
    let block = state.block();
    if block == Block::Air
        || block == Block::CaveAir
        || block == Block::VoidAir
        || block == Block::LilyPad
    {
        return Layer::Above;
    }
    let fluid = state.fluid_state();
    if block == Block::Water
        && fluid.is_source()
        && tuff_datapack::tags::is(tuff_vanilla_data::tag::fluid::key::WATER, fluid.fluid())
    {
        Layer::Inside
    } else {
        Layer::Invalid
    }
}

fn cue_effect(cue: Cue, origin: [f64; 3]) -> Effect {
    let packet = match cue {
        Cue::Splash { pitch, seed } => framed(&play::Sound {
            sound_event: SoundEvent::EntityFishingBobberSplash.id(),
            category: play::sound_category::NEUTRAL,
            position: origin,
            volume: 0.25,
            pitch,
            seed,
        }),
        Cue::Particles {
            kind,
            at,
            count,
            spread,
            speed,
        } => framed(&play::Particles {
            particle: kind.id(),
            position: at,
            spread: spread.map(|c| c as f32),
            speed,
            count,
            alternative: false,
        }),
    };
    Effect::ChunkPackets(packets([packet]))
}

fn triangle(rng: &mut impl Rng, center: f64, spread: f64) -> f64 {
    center + spread * (rng.r#gen::<f64>() - rng.r#gen::<f64>())
}

impl RegionEntity for Bobber {
    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn base_mut(&mut self) -> &mut EntityBase {
        &mut self.base
    }

    fn save(&self, _out: &mut Compound) {}

    fn facts(&self) -> EntityFacts {
        EntityFacts {
            kind: Some(EntityType::FishingBobber),
            position: self.base.position,
            open_water: Some(self.open_water),
            ..EntityFacts::default()
        }
    }

    fn metadata(&self) -> Vec<(u8, MetadataValue)> {
        vec![
            (
                DATA_HOOKED,
                MetadataValue::VarInt(self.hooked_id().map_or(0, |id| id + 1)),
            ),
            (DATA_BITING, MetadataValue::Boolean(self.biting)),
        ]
    }

    fn take_changed_data(&mut self) -> Vec<(u8, MetadataValue)> {
        let mut changed = std::mem::take(&mut self.changed);
        changed.sort_unstable();
        changed.dedup();
        changed
            .into_iter()
            .filter_map(|i| Some((i, self.value(i)?)))
            .collect()
    }

    fn resend_data(&mut self, index: u8) {
        self.changed.push(index);
    }

    fn spawn_data(&self) -> i32 {
        self.owner.culprit.as_ref().map_or(self.base.id, |c| c.id)
    }

    fn notice(&mut self, notice: tuff_core::world::entity::Notice) {
        if notice == tuff_core::world::entity::Notice::Reel {
            self.reel = true;
        }
    }

    fn tick(&mut self, cx: &mut TickContext) -> Tick {
        self.owner.has_been_shot = true;
        let players = cx.players;
        let owner = self
            .owner
            .uuid
            .and_then(|uuid| players.iter().find(|p| p.uuid == uuid));
        let Some(owner) = owner else {
            self.gone();
            return Tick::Discard;
        };
        let holds = owner.held.contains(&Some(Item::FishingRod));
        if !holds || distance_sq(self.base.position, owner.position) > MAX_DISTANCE_SQ {
            self.gone();
            return Tick::Discard;
        }
        if self.reel {
            return self.retrieve(owner, cx);
        }
        if self.landed {
            self.life += 1;
            if self.life >= MAX_LANDED_TICKS {
                self.gone();
                return Tick::Discard;
            }
        } else {
            self.life = 0;
        }
        let pos = self.block_pos();
        let level = |p: BlockPos| (cx.block)(p);
        let liquid =
            contact::surface(pos, tuff_vanilla_data::tag::fluid::key::WATER, level).unwrap_or(0.0);
        let in_water = liquid > 0.0;
        let from = self.base.position;
        match self.state {
            State::Flying => {
                let end = [0, 1, 2].map(|i| from[i] + self.base.velocity[i]);
                let block = block_hit(from, end, cx)
                    .filter(|hit| border_stop(from, hit.location, cx).is_none());
                let to = block.map_or(end, |hit| hit.location);
                let skip = owner.id;
                let skip = (!self.owner.left).then_some(skip);
                if let Some(id) = self.hooks_entity(from, to, skip, cx) {
                    self.set_state(State::Hooked(id));
                    self.base.velocity = [0.0; 3];
                    return Tick::Alive;
                }
                if in_water {
                    self.base.velocity[0] *= 0.3;
                    self.base.velocity[1] *= 0.2;
                    self.base.velocity[2] *= 0.3;
                    self.set_state(State::Bobbing);
                    return Tick::Alive;
                }
                if let Some(hit) = block {
                    self.base.position = hit.location;
                    self.base.velocity = [0.0; 3];
                    self.landed = true;
                    return Tick::Alive;
                }
                self.owner
                    .check_left(self.base.bounds(), self.base.velocity, cx);
            }
            State::Hooked(id) => {
                let target = cx
                    .entities
                    .iter()
                    .find(|e| e.id == id)
                    .map(|e| e.bounds)
                    .or_else(|| players.iter().find(|p| p.id == id).map(|p| p.bounds()));
                match target {
                    Some(b) => {
                        self.base.position = [
                            (b[0] + b[3]) / 2.0,
                            b[1] + (b[4] - b[1]) * 0.8,
                            (b[2] + b[5]) / 2.0,
                        ]
                    }
                    None => self.set_state(State::Flying),
                }
                return Tick::Alive;
            }
            State::Bobbing => {
                let velocity = self.base.velocity;
                let mut force = (self.base.position[1] + velocity[1]) - f64::from(pos.y) - liquid;
                if force.abs() < 0.01 {
                    force += force.signum() * 0.1;
                }
                self.base.velocity = [
                    velocity[0] * 0.9,
                    velocity[1] - force * f64::from(cx.rng.r#gen::<f32>()) * 0.2,
                    velocity[2] * 0.9,
                ];
                if self.line.fish_close() {
                    self.open_water = self.open_water
                        && self.out_of_water < MAX_OUT_OF_WATER
                        && self.open_water_at(pos, cx);
                } else {
                    self.open_water = true;
                }
                if in_water {
                    self.out_of_water = (self.out_of_water - 1).max(0);
                    if self.biting {
                        let a = f64::from(cx.rng.r#gen::<f32>());
                        let b = f64::from(cx.rng.r#gen::<f32>());
                        self.base.velocity[1] += -0.1 * a * b;
                    }
                    self.tend_line(pos, cx);
                } else {
                    self.out_of_water = (self.out_of_water + 1).min(MAX_OUT_OF_WATER);
                }
            }
        }
        if !in_water && !self.landed {
            self.base.velocity[1] -= GRAVITY;
        }
        let end = [0, 1, 2].map(|i| from[i] + self.base.velocity[i]);
        let block = block_hit(from, end, cx);
        let border = border_stop(from, block.map_or(end, |hit| hit.location), cx);
        match (border, block) {
            (Some(stop), _) => {
                self.base.position = stop;
                bounce_off_border(&mut self.base, cx.rng);
            }
            (None, Some(hit)) => {
                self.base.position = hit.location;
                self.base.velocity = [0.0; 3];
                self.landed = true;
            }
            (None, None) => self.base.position = end,
        }
        self.base.velocity = self.base.velocity.map(|c| c * DRAG);
        let (yaw, pitch) = rotation_of(self.base.velocity);
        if self.base.velocity.iter().any(|c| c.abs() > 1.0e-6) {
            self.base.pitch = lerp_rotation(self.base.pitch, pitch);
            self.base.yaw = lerp_rotation(self.base.yaw, yaw);
        }
        Tick::Alive
    }
}
