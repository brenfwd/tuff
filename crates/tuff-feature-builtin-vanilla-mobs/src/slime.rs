//! Slimes and magma cubes: cubes of a size that sets their box, health,
//! speed and damage. They hop the way they face, turn towards players
//! within four blocks of their height, hurt players they touch (tiny slimes
//! do not), and split into two to four half-size cubes when they die.
//! Slimes spawn in slime chunks below height 40 and on swamp surfaces by
//! the moon. Sulfur cubes are their own family (see `sulfur_cube`).

use rand::Rng;
use rand::rngs::StdRng;
use tuff_vanilla_data::damage_type;
use tuff_vanilla_data::registry::{Attribute, EntityType, SoundEvent};

use super::control::{MoveStyle, Operation, rotate_towards_wrapped};
use super::goal::{Flags, Goal, GoalSelector, reduced_delay};
use super::monster::LocalDifficulty;
use super::spawning::{SpawnLevel, SpawnReason};
use super::target::NearestAttackablePlayer;
use super::{Mob, MobCore, MobSounds, Species, data};
use tuff_core::protocol::play::MetadataValue;
use tuff_core::world::drops::EntityFacts;
use tuff_core::world::entity::{
    Culprit, Effect, EntityBase, EntityHit, NearbyPlayer, TickContext, take_bool, take_number,
};
use tuff_item::random::{LegacyRandom, RandomSource};
use tuff_nbt::Compound;
use tuff_world_model::BlockPos;

const SIZE: u8 = data::SPECIES;
const MAX_SIZE: i32 = 127;
const SLIME_CHUNK_SALT: i64 = 987_234_911;

/// The hop state of a cube's move control.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Hop {
    /// The yaw it turns to.
    pub yaw: f32,
    pub jump_delay: i32,
    /// It hops three times as often when it can hurt its target.
    pub aggressive: bool,
    /// Magma cubes wait four times as long between hops.
    pub magma: bool,
    /// It jumped this tick (for the jump sound).
    pub jumped: bool,
}

impl MobCore {
    pub(crate) fn hop(&mut self) -> Option<&mut Hop> {
        match &mut self.move_style {
            MoveStyle::Hop(hop) => Some(hop),
            _ => None,
        }
    }

    /// Turns towards the hop direction and, when on the ground, waits out
    /// the delay between hops, then jumps.
    pub(super) fn tick_hop_control(&mut self, cx: &mut TickContext) {
        let Some(hop) = self.hop().copied() else {
            return;
        };
        self.base.yaw = rotate_towards_wrapped(self.base.yaw, hop.yaw, 90.0);
        self.head_yaw = self.base.yaw;
        self.body_yaw = self.base.yaw;
        if self.movement.operation != Operation::MoveTo {
            self.body.zza = 0.0;
            return;
        }
        self.movement.operation = Operation::Wait;
        let speed =
            (self.movement.speed_modifier * self.attribute(Attribute::MovementSpeed)) as f32;
        if !self.base.on_ground {
            self.set_speed(speed);
            return;
        }
        self.set_speed(speed);
        let delay = hop.jump_delay;
        let hop = self.hop().expect("checked above");
        hop.jump_delay -= 1;
        if delay <= 0 {
            let mut next = cx.rng.gen_range(0..20) + 10;
            if hop.magma {
                next *= 4;
            }
            if hop.aggressive {
                next /= 3;
            }
            hop.jump_delay = next;
            hop.jumped = true;
            self.jump.jump = true;
            return;
        }
        self.body.xxa = 0.0;
        self.body.zza = 0.0;
        self.set_speed(0.0);
    }

    pub(crate) fn set_hop_direction(&mut self, yaw: f32, aggressive: bool) {
        if let Some(hop) = self.hop() {
            hop.yaw = yaw;
            hop.aggressive = aggressive;
        }
    }

    pub(crate) fn want_hop(&mut self, speed_modifier: f64) {
        self.movement.speed_modifier = speed_modifier;
        self.movement.operation = Operation::MoveTo;
    }
}

/// Keeps it bobbing up in liquids.
#[derive(Debug, Default)]
pub(crate) struct CubeFloat;

impl Goal for CubeFloat {
    fn flags(&self) -> Flags {
        Flags::JUMP | Flags::MOVE
    }

    fn can_use(&mut self, mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        mob.in_liquid()
    }

    fn every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        if cx.rng.r#gen::<f32>() < 0.8 {
            mob.jump.jump = true;
        }
        mob.want_hop(1.2);
    }
}

/// Faces its target, hopping faster when it can hurt it; it tires after
/// fifteen seconds.
#[derive(Debug, Default)]
struct CubeAttack {
    tired: i32,
}

impl Goal for CubeAttack {
    fn flags(&self) -> Flags {
        Flags::LOOK
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        mob.target_view(cx).is_some_and(|t| mob.can_attack(&t, cx))
    }

    fn start(&mut self, _mob: &mut MobCore, _cx: &mut TickContext) {
        self.tired = reduced_delay(300);
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        if !self.can_use(mob, cx) {
            return false;
        }
        self.tired -= 1;
        self.tired > 0
    }

    fn every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        if let Some(target) = mob.target_view(cx) {
            mob.turn_to(target.eye, 10.0, 10.0);
        }
        let aggressive = can_deal_damage(mob);
        mob.set_hop_direction(mob.base.yaw, aggressive);
    }
}

/// Without a target it turns to a random direction every two to five
/// seconds.
#[derive(Debug, Default)]
pub(crate) struct CubeRandomDirection {
    degrees: f32,
    next: i32,
}

impl Goal for CubeRandomDirection {
    fn flags(&self) -> Flags {
        Flags::LOOK
    }

    fn can_use(&mut self, mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        mob.target.is_none() && (mob.base.on_ground || mob.in_liquid())
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        self.next -= 1;
        if self.next <= 0 {
            self.next = self.adjusted_delay(40 + cx.rng.gen_range(0..60));
            self.degrees = cx.rng.gen_range(0..360) as f32;
        }
        mob.set_hop_direction(self.degrees, false);
    }
}

/// Always wants to hop on.
#[derive(Debug, Default)]
pub(crate) struct CubeKeepJumping;

impl Goal for CubeKeepJumping {
    fn flags(&self) -> Flags {
        Flags::JUMP | Flags::MOVE
    }

    fn can_use(&mut self, _mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        true
    }

    fn tick(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        mob.want_hop(1.0);
    }
}

fn within_height(mob: &MobCore, player: &NearbyPlayer) -> bool {
    (player.position[1] - mob.base.position[1]).abs() <= 4.0
}

fn is_magma(mob: &MobCore) -> bool {
    mob.kind() == EntityType::MagmaCube
}

fn can_deal_damage(mob: &MobCore) -> bool {
    is_magma(mob) || mob.size_scale > 1.0
}

/// A slime or magma cube.
#[derive(Debug)]
pub struct Cube {
    size: i32,
    was_on_ground: bool,
}

impl Cube {
    fn is_tiny(&self) -> bool {
        self.size <= 1
    }

    /// Sets the size and everything that follows from it; `heal` refills
    /// its health to the new maximum.
    pub fn set_size(&mut self, core: &mut MobCore, size: i32, heal: bool) {
        let size = size.clamp(1, MAX_SIZE);
        self.size = size;
        core.size_scale = size as f32;
        core.refresh_size();
        core.set_attribute_base(Attribute::MaxHealth, f64::from(size * size));
        core.set_attribute_base(
            Attribute::MovementSpeed,
            f64::from(0.2f32 + 0.1f32 * size as f32),
        );
        core.set_attribute_base(Attribute::AttackDamage, f64::from(size));
        if is_magma(core) {
            core.set_attribute_base(Attribute::Armor, f64::from(size * 3));
            core.jump_boost = f64::from(size as f32 * 0.1);
            core.lava_jump = Some(f64::from(0.22f32 + size as f32 * 0.05));
        }
        core.xp_reward = size;
        if heal {
            core.health = core.max_health();
        }
        core.changed(SIZE);
    }

    /// A size-two slime at `position`, as an oozing death leaves.
    pub(crate) fn oozed(position: [f64; 3], cx: &mut TickContext) -> Mob<Cube> {
        let mut base = EntityBase::new(
            EntityType::Slime,
            cx.shared.next_entity_id(),
            cx.rng.r#gen(),
            position,
            [0.0; 3],
        );
        base.yaw = cx.rng.r#gen::<f32>() * 360.0;
        let mut slime = Mob::<Cube>::new(base, cx.rng);
        slime.species.set_size(&mut slime.core, 2, true);
        slime.core.changed.clear();
        slime
    }

    fn sound_volume(&self) -> f32 {
        0.4 * self.size as f32
    }

    fn sound_pitch(&self, rng: &mut StdRng) -> f32 {
        let adjust = if self.is_tiny() { 1.4 } else { 0.8 };
        ((rng.r#gen::<f32>() - rng.r#gen::<f32>()) * 0.2 + 1.0) * adjust
    }

    /// Hurts the players it touches, if it is big enough, they are in reach
    /// and in sight.
    fn touch_players(&self, core: &MobCore, cx: &mut TickContext) {
        if !can_deal_damage(core) {
            return;
        }
        let bounds = core.bounds();
        let damage =
            core.attribute(Attribute::AttackDamage) as f32 + if is_magma(core) { 2.0 } else { 0.0 };
        let touching: Vec<i32> = cx
            .players
            .iter()
            .filter(|p| {
                let reach = super::physics::inflate(p.bounds(), [1.0, 0.5, 1.0]);
                super::physics::intersects(reach, bounds)
            })
            .map(|p| p.id)
            .collect();
        for id in touching {
            let Some(target) = super::target::view(cx, id) else {
                continue;
            };
            if !core.can_attack(&target, cx)
                || !core.within_melee_range(&target)
                || !core.has_line_of_sight(&target, cx)
            {
                continue;
            }
            let mut hit = EntityHit::new(damage, &damage_type::MOB_ATTACK);
            hit.attacker = Some(Culprit::entity(&core.base));
            hit.position = Some(core.base.position);
            hit.report_landed = true;
            cx.effects.push(Effect::Hurt {
                target: id,
                hit: Box::new(hit),
            });
        }
    }
}

impl Species for Cube {
    fn create(_rng: &mut StdRng) -> Self {
        Self {
            size: 1,
            was_on_ground: false,
        }
    }

    fn configure(core: &mut MobCore) {
        core.make_monster();
        core.move_style = MoveStyle::Hop(Hop {
            magma: is_magma(core),
            ..Hop::default()
        });
        core.navigation.can_float = true;
        let mut goals = GoalSelector::default();
        goals.add(
            1,
            NearestAttackablePlayer::with_interval(10, true, Some(within_height)),
        );
        goals.add(1, CubeFloat);
        goals.add(2, CubeAttack::default());
        goals.add(4, CubeRandomDirection::default());
        goals.add(5, CubeKeepJumping);
        core.goals = goals;
    }

    fn load(&mut self, core: &mut MobCore, nbt: &mut Compound) {
        let size = take_number(nbt, "Size").map_or(0, |v| v as i32);
        let health = core.health;
        self.set_size(core, size.saturating_add(1), false);
        core.health = health.min(core.max_health());
        if let Some(saved) = take_number(nbt, "Health") {
            core.health = (saved as f32).clamp(0.0, core.max_health());
        }
        self.was_on_ground = take_bool(nbt, "wasOnGround").unwrap_or(false);
    }

    fn save(&self, _core: &MobCore, out: &mut Compound) {
        out.insert("Size", self.size - 1);
        out.insert("wasOnGround", self.was_on_ground);
    }

    fn metadata(&self, _core: &MobCore) -> Vec<(u8, MetadataValue)> {
        vec![(SIZE, MetadataValue::VarInt(self.size))]
    }

    fn sounds(&self, core: &MobCore) -> MobSounds {
        let (hurt, death) = match (is_magma(core), self.is_tiny()) {
            (true, true) => (
                SoundEvent::EntityMagmaCubeHurtSmall,
                SoundEvent::EntityMagmaCubeDeathSmall,
            ),
            (true, false) => (
                SoundEvent::EntityMagmaCubeHurt,
                SoundEvent::EntityMagmaCubeDeath,
            ),
            (false, true) => (
                SoundEvent::EntitySlimeHurtSmall,
                SoundEvent::EntitySlimeDeathSmall,
            ),
            (false, false) => (SoundEvent::EntitySlimeHurt, SoundEvent::EntitySlimeDeath),
        };
        MobSounds {
            ambient: None,
            hurt: Some(hurt),
            death: Some(death),
            step: None,
            volume: self.sound_volume(),
        }
    }

    fn tick(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        for _ in 0..std::mem::take(&mut core.landed_hits) {
            let pitch = (cx.rng.r#gen::<f32>() - cx.rng.r#gen::<f32>()) * 0.2 + 1.0;
            core.sound(SoundEvent::EntitySlimeAttack, 1.0, pitch, cx);
        }
        if core.base.on_ground && !self.was_on_ground {
            let squish = match (is_magma(core), self.is_tiny()) {
                (true, true) => SoundEvent::EntityMagmaCubeSquishSmall,
                (true, false) => SoundEvent::EntityMagmaCubeSquish,
                (false, true) => SoundEvent::EntitySlimeSquishSmall,
                (false, false) => SoundEvent::EntitySlimeSquish,
            };
            let pitch = ((cx.rng.r#gen::<f32>() - cx.rng.r#gen::<f32>()) * 0.2 + 1.0) / 0.8;
            core.sound(squish, self.sound_volume(), pitch, cx);
        }
        self.was_on_ground = core.base.on_ground;
    }

    fn ai_step(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if let Some(hop) = core.hop()
            && std::mem::take(&mut hop.jumped)
        {
            let sound = match (is_magma(core), self.is_tiny()) {
                (true, _) => SoundEvent::EntityMagmaCubeJump,
                (false, true) => SoundEvent::EntitySlimeJumpSmall,
                (false, false) => SoundEvent::EntitySlimeJump,
            };
            let pitch = self.sound_pitch(cx.rng);
            core.sound(sound, self.sound_volume(), pitch, cx);
        }
        self.touch_players(core, cx);
    }

    fn finalize_spawn(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        let special = LocalDifficulty::at(cx, core.base.position).special_multiplier();
        let mut scale = cx.rng.gen_range(0..3);
        if scale < 2 && cx.rng.r#gen::<f32>() < 0.5 * special {
            scale += 1;
        }
        self.set_size(core, 1 << scale, true);
    }

    fn loot_facts(&self, facts: &mut EntityFacts) {
        facts.cube_size = Some(self.size);
    }

    fn experience(&self, core: &MobCore, rng: &mut StdRng) -> i32 {
        core.xp_reward + core.equipment.experience_bonus(rng)
    }

    fn remove_dead(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if self.size <= 1 {
            return;
        }
        let offset = core.size.width / 2.0;
        let half = self.size / 2;
        let count = 2 + cx.rng.gen_range(0..3);
        for i in 0..count {
            let dx = ((i % 2) as f32 - 0.5) * offset;
            let dz = ((i / 2) as f32 - 0.5) * offset;
            let [x, y, z] = core.base.position;
            let position = [x + f64::from(dx), y + 0.5, z + f64::from(dz)];
            let mut base = EntityBase::new(
                core.kind(),
                cx.shared.next_entity_id(),
                cx.rng.r#gen(),
                position,
                [0.0; 3],
            );
            base.yaw = cx.rng.r#gen::<f32>() * 360.0;
            base.custom_name = core.base.custom_name.clone();
            base.invulnerable = core.base.invulnerable;
            base.silent = core.base.silent;
            let mut child = Mob::<Cube>::new(base, cx.rng);
            child.core.persistence_required = core.persistence_required;
            child.core.no_ai = core.no_ai;
            child.species.set_size(&mut child.core, half, true);
            child.core.changed.clear();
            cx.effects.push(Effect::Spawn(Box::new(child)));
        }
    }
}

pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    registry.register(EntityType::Slime, super::load::<Cube>);
    registry.register(EntityType::MagmaCube, super::load::<Cube>);
    registry.register_spawn_rule(EntityType::Slime, slime_rule);
    registry.register_spawn_rule(EntityType::MagmaCube, magma_rule);
}

/// Whether chunk `(x, z)` of a world with `seed` is a slime chunk.
pub fn is_slime_chunk(seed: i64, x: i32, z: i32) -> bool {
    slime_chunk_roll(seed, x, z) == 0
}

/// The chunk's roll out of ten; zero makes a slime chunk. The products
/// wrap as the game's int arithmetic does.
fn slime_chunk_roll(seed: i64, x: i32, z: i32) -> i32 {
    let mixed = seed
        .wrapping_add(i64::from(x.wrapping_mul(x).wrapping_mul(4_987_142)))
        .wrapping_add(i64::from(x.wrapping_mul(5_947_611)))
        .wrapping_add(i64::from(z.wrapping_mul(z)).wrapping_mul(4_392_871))
        .wrapping_add(i64::from(z.wrapping_mul(389_711)))
        ^ SLIME_CHUNK_SALT;
    LegacyRandom::new(mixed).next_int_bounded(10)
}

/// Slimes: from spawners anywhere they can stand; naturally on swamp
/// surfaces (heights 51 to 69) by the moon in the dark, or in slime chunks
/// below height 40.
fn slime_rule(
    kind: EntityType,
    level: &SpawnLevel,
    pos: BlockPos,
    reason: SpawnReason,
    rng: &mut StdRng,
) -> bool {
    let stands = |rng: &mut StdRng| super::spawning::mob_rule(kind, level, pos, reason, rng);
    if reason.is_spawner() {
        return stands(rng);
    }
    let swamp = (level.biome)(pos).is_some_and(|biome| {
        tuff_datapack::tags::has(
            tuff_vanilla_data::tag::worldgen::biome::key::ALLOWS_SURFACE_SLIME_SPAWNS,
            biome,
        )
    });
    if swamp
        && pos.y > 50
        && pos.y < 70
        && rng.r#gen::<f32>() < level.surface_slime_chance
        && i32::from(level.raw_brightness(pos, level.sky_darken)) <= rng.gen_range(0..8)
    {
        return stands(rng);
    }
    let chunk = tuff_world_model::pos::ChunkPos::of_block(pos);
    let slime_chunk = is_slime_chunk(level.seed, chunk.x, chunk.z);
    if rng.gen_range(0..10) == 0 && slime_chunk && pos.y < 40 {
        return stands(rng);
    }
    false
}

/// Magma cubes spawn wherever they are placed (the region already keeps
/// monsters out of peaceful).
fn magma_rule(
    _kind: EntityType,
    _level: &SpawnLevel,
    _pos: BlockPos,
    _reason: SpawnReason,
    _rng: &mut StdRng,
) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn cube_data_indices_match_the_dumped_layouts() {
        let varint = MetadataValue::VarInt(0).type_id() as u8;
        for kind in [EntityType::Slime, EntityType::MagmaCube] {
            let layout = kind.data_layout();
            assert_eq!(layout.len(), SIZE as usize + 1);
            assert_eq!(layout[SIZE as usize], varint);
        }
        let boolean = MetadataValue::Boolean(false).type_id() as u8;
        assert_eq!(
            EntityType::Ghast.data_layout()[data::BABY as usize..],
            [boolean]
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn slimes_spawn_in_dark_swamps_by_the_moon_and_deep_in_slime_chunks() {
        use rand::SeedableRng;
        use tuff_vanilla_data::Block;
        let ground = |pos: BlockPos| {
            Some(if pos.y == 62 || pos.y == 20 {
                Block::GrassBlock.default_state()
            } else {
                Block::Air.default_state()
            })
        };
        let dark = |_: BlockPos| Some((0, 0));
        let overworld =
            tuff_vanilla_data::dimension_type::DimensionType::get("minecraft:overworld").unwrap();
        let level =
            |biome: &'static dyn Fn(BlockPos) -> Option<&'static str>, chance: f32| SpawnLevel {
                block: &ground,
                light: &dark,
                sky_darken: 0,
                min_y: -64,
                sea_level: 63,
                dimension: overworld,
                thundering: false,
                biome,
                seed: 12345,
                surface_slime_chance: chance,
                difficulty: tuff_core::world::player::Difficulty::Normal,
                player_within: &|_, _| false,
            };
        let mut rng = StdRng::seed_from_u64(9);
        let mut spawns = |level: &SpawnLevel, pos: BlockPos| {
            (0..200)
                .filter(|_| {
                    slime_rule(
                        EntityType::Slime,
                        level,
                        pos,
                        SpawnReason::Natural,
                        &mut rng,
                    )
                })
                .count()
        };
        let surface = BlockPos::new(8, 63, 8);
        assert!(spawns(&level(&|_| Some("minecraft:swamp"), 0.5), surface) > 0);
        assert_eq!(
            spawns(&level(&|_| Some("minecraft:swamp"), 0.0), surface),
            0
        );
        assert_eq!(
            spawns(&level(&|_| Some("minecraft:plains"), 1.0), surface),
            0
        );
        let deep_in_slime_chunk = BlockPos::new(-5 * 16 + 8, 21, 2 * 16 + 8);
        assert!(spawns(&level(&|_| None, 0.0), deep_in_slime_chunk) > 0);
        let deep_elsewhere = BlockPos::new(-4 * 16 + 8, 21, 2 * 16 + 8);
        assert!(!is_slime_chunk(12345, -4, 2));
        assert_eq!(spawns(&level(&|_| None, 0.0), deep_elsewhere), 0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn slime_chunks_match_the_game() {
        let expected: [(i64, &[(i32, i32)]); 2] = [
            (
                12345,
                &[
                    (-40, -16),
                    (-40, 38),
                    (-33, -28),
                    (-33, 32),
                    (-26, -22),
                    (-26, -13),
                    (-26, 29),
                    (-19, 5),
                    (-19, 11),
                    (-19, 26),
                    (-12, -40),
                    (-12, -31),
                    (-12, -28),
                    (-12, -19),
                    (-12, -1),
                    (-12, 32),
                    (-5, 2),
                    (2, -40),
                    (2, -25),
                    (2, 11),
                    (9, -28),
                    (9, 5),
                    (9, 35),
                    (16, -40),
                    (16, -37),
                    (16, -25),
                    (16, 26),
                    (16, 32),
                    (23, -7),
                    (23, 11),
                    (23, 20),
                    (30, -40),
                    (30, -37),
                    (30, -19),
                    (30, 8),
                    (37, -28),
                    (37, 2),
                    (37, 14),
                    (37, 29),
                ],
            ),
            (
                -4_172_144_997_902_289_642,
                &[
                    (-40, -25),
                    (-40, 11),
                    (-33, 35),
                    (-26, -1),
                    (-26, 2),
                    (-26, 14),
                    (-26, 35),
                    (-19, 2),
                    (-19, 8),
                    (-19, 20),
                    (-12, -40),
                    (-12, -37),
                    (-12, 5),
                    (-12, 29),
                    (-12, 32),
                    (-5, -34),
                    (-5, 2),
                    (2, -34),
                    (2, -13),
                    (2, 8),
                    (9, 5),
                    (16, -37),
                    (16, -16),
                    (16, 38),
                    (23, -4),
                    (23, 11),
                    (23, 14),
                    (30, 14),
                    (30, 35),
                    (37, -22),
                    (37, -7),
                    (37, 26),
                ],
            ),
        ];
        for (seed, chunks) in expected {
            let found: Vec<(i32, i32)> = (-40..40)
                .step_by(7)
                .flat_map(|x| (-40..40).step_by(3).map(move |z| (x, z)))
                .filter(|&(x, z)| is_slime_chunk(seed, x, z))
                .collect();
            assert_eq!(found, chunks, "seed {seed}");
        }
        assert_eq!(
            slime_chunk_roll(7, 46341, -46341),
            3,
            "int products wrap as in the game"
        );
    }
}
