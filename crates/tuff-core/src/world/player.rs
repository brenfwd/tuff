//! Vanilla rules for players: game mode abilities, hunger, health and fall
//! damage, kept free of networking so they can be tested directly.

use tuff_vanilla_data::damage_type::DamageType;
use tuff_vanilla_data::registry::{Attribute, EntityType};
use tuff_vanilla_data::{Block, BlockState, Item};

use crate::protocol::play::{GameMode, abilities, level_event};
use crate::world::fall;
use crate::world::{PLAYER_HALF_WIDTH, PLAYER_HEIGHT};
use tuff_item::item::ItemStack;
use tuff_world_model::BlockPos;

/// Ticks before anyone can pick up an item a player threw.
pub const THROWN_PICKUP_DELAY: u32 = 40;

pub const MAX_HEALTH: f32 = 20.0;
pub const MAX_FOOD: i32 = 20;
const MAX_EXHAUSTION: f32 = 40.0;
const EXHAUSTION_PER_FOOD_POINT: f32 = 4.0;
const SATURATED_REGEN_TICKS: u32 = 10;
const REGEN_TICKS: u32 = 80;
const REGEN_FOOD_THRESHOLD: i32 = 18;
const SATURATED_HEAL_CAP: f32 = 6.0;
const REGEN_EXHAUSTION: f32 = 6.0;
const INITIAL_SATURATION: f32 = 5.0;
const INVULNERABLE_TICKS: u32 = 20;
const SAFE_FALL_DISTANCE: f64 = 3.0;

pub mod exhaustion {
    pub const SPRINT_PER_METER: f32 = 0.1;
    pub const JUMP: f32 = 0.05;
    pub const SPRINT_JUMP: f32 = 0.2;
    pub const ATTACK: f32 = 0.1;
    pub const BLOCK_BREAK: f32 = 0.005;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Difficulty {
    Peaceful,
    Easy,
    Normal,
    Hard,
}

impl Difficulty {
    pub const ALL: [Self; 4] = [Self::Peaceful, Self::Easy, Self::Normal, Self::Hard];

    pub fn id(self) -> u8 {
        self as u8
    }

    pub fn from_id(id: u8) -> Option<Self> {
        Self::ALL.get(usize::from(id)).copied()
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Peaceful => "peaceful",
            Self::Easy => "easy",
            Self::Normal => "normal",
            Self::Hard => "hard",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|d| d.name() == name)
    }
}

/// Vanilla `Abilities`, as each game mode sets them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Abilities {
    pub invulnerable: bool,
    pub flying: bool,
    pub may_fly: bool,
    /// Creative: blocks break instantly without drops and placing blocks
    /// uses no items.
    pub instabuild: bool,
    pub may_build: bool,
}

impl Abilities {
    /// A mode's abilities; `flying` is the client's flight state, kept where
    /// the mode allows flight (spectators always fly).
    pub fn new(mode: GameMode, flying: bool) -> Self {
        let creative = mode == GameMode::Creative;
        let spectator = mode == GameMode::Spectator;
        Self {
            invulnerable: creative || spectator,
            flying: spectator || (creative && flying),
            may_fly: creative || spectator,
            instabuild: creative,
            may_build: !matches!(mode, GameMode::Adventure | GameMode::Spectator),
        }
    }

    /// After a mode change, flight ends when the player is within a block of
    /// the ground, except for spectators.
    pub fn landed(mut self, mode: GameMode, near_ground: bool) -> Self {
        if self.flying && mode != GameMode::Spectator && near_ground {
            self.flying = false;
        }
        self
    }

    pub fn flags(self) -> i8 {
        [
            (self.invulnerable, abilities::INVULNERABLE),
            (self.flying, abilities::FLYING),
            (self.may_fly, abilities::MAY_FLY),
            (self.instabuild, abilities::INSTABUILD),
        ]
        .into_iter()
        .filter(|(set, _)| *set)
        .fold(0, |flags, (_, flag)| flags | flag)
    }

    pub fn is_immune_to(self, kind: &DamageType) -> bool {
        self.invulnerable
            && !tuff_datapack::tags::has(
                tuff_vanilla_data::tag::damage_type::key::BYPASSES_INVULNERABILITY,
                kind.name,
            )
    }
}

/// Ticks after a level-up sound before the next one can play.
const LEVEL_UP_SOUND_COOLDOWN: u64 = 100;
const MAX_DEATH_EXPERIENCE: i32 = 100;
const DEATH_EXPERIENCE_PER_LEVEL: i32 = 7;

/// A player's experience: the level, the bar's progress and the lifetime
/// total, with the game's level cost curve.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Experience {
    pub level: i32,
    pub progress: f32,
    pub total: i32,
    /// The enchantment seed (`XpSeed`), kept for the enchanting table.
    pub seed: i32,
    last_level_up: Option<u64>,
}

impl Default for Experience {
    fn default() -> Self {
        Self {
            level: 0,
            progress: 0.0,
            total: 0,
            seed: 0,
            last_level_up: None,
        }
    }
}

impl Experience {
    pub fn new(level: i32, progress: f32, total: i32, seed: i32) -> Self {
        Self {
            level,
            progress,
            total,
            seed,
            last_level_up: None,
        }
    }

    /// Points needed to go from the current level to the next.
    pub fn needed_for_next_level(&self) -> i32 {
        Self::needed_at(self.level)
    }

    pub fn needed_at(level: i32) -> i32 {
        if level >= 30 {
            112 + (level - 30) * 9
        } else if level >= 15 {
            37 + (level - 15) * 5
        } else {
            7 + level * 2
        }
    }

    /// Adds (or removes) points, carrying the bar over levels. Returns the
    /// volume of a level-up sound to play, if one is due at `tick`.
    pub fn give_points(&mut self, points: i32, tick: u64) -> Option<f32> {
        self.progress += points as f32 / self.needed_for_next_level() as f32;
        self.total = self.total.saturating_add(points).max(0);
        let mut sound = None;
        while self.progress < 0.0 {
            let remaining = self.progress * self.needed_for_next_level() as f32;
            if self.level > 0 {
                self.give_levels(-1, tick);
                self.progress = 1.0 + remaining / self.needed_for_next_level() as f32;
            } else {
                self.give_levels(-1, tick);
                self.progress = 0.0;
            }
        }
        while self.progress >= 1.0 {
            self.progress = (self.progress - 1.0) * self.needed_for_next_level() as f32;
            sound = self.give_levels(1, tick).or(sound);
            self.progress /= self.needed_for_next_level() as f32;
        }
        sound
    }

    /// Adds levels; a negative total empties the bar. Returns the volume
    /// of the level-up sound every fifth level plays.
    pub fn give_levels(&mut self, amount: i32, tick: u64) -> Option<f32> {
        self.level = self.level.saturating_add(amount);
        if self.level < 0 {
            self.level = 0;
            self.progress = 0.0;
            self.total = 0;
        }
        let due = self
            .last_level_up
            .is_none_or(|last| last + LEVEL_UP_SOUND_COOLDOWN < tick);
        if amount > 0 && self.level % 5 == 0 && due {
            self.last_level_up = Some(tick);
            let volume = if self.level > 30 {
                1.0
            } else {
                self.level as f32 / 30.0
            };
            return Some(volume * 0.75);
        }
        None
    }

    /// The experience a dying player drops.
    pub fn death_reward(&self) -> i32 {
        (self.level * DEATH_EXPERIENCE_PER_LEVEL).min(MAX_DEATH_EXPERIENCE)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FoodEffect {
    None,
    Heal(f32),
    Starve,
}

/// Vanilla `FoodData`: exhaustion drains saturation, then food; a full or
/// nearly full hunger bar regenerates health, and an empty one starves.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Food {
    pub level: i32,
    pub saturation: f32,
    pub exhaustion: f32,
    pub tick_timer: u32,
}

impl Default for Food {
    fn default() -> Self {
        Self {
            level: MAX_FOOD,
            saturation: INITIAL_SATURATION,
            exhaustion: 0.0,
            tick_timer: 0,
        }
    }
}

impl Food {
    pub fn add_exhaustion(&mut self, amount: f32) {
        self.exhaustion = (self.exhaustion + amount).min(MAX_EXHAUSTION);
    }

    pub fn needs_food(&self) -> bool {
        self.level < MAX_FOOD
    }

    /// Restores food and saturation the way eating does; saturation never
    /// exceeds the food level.
    pub fn eat(&mut self, nutrition: i32, saturation: f32) {
        self.level = (self.level + nutrition).clamp(0, MAX_FOOD);
        self.saturation = (self.saturation + saturation).clamp(0.0, self.level as f32);
    }

    /// Eating by nutrition and a saturation modifier, the form some blocks
    /// (cake) and effects (saturation) use.
    pub fn eat_with_modifier(&mut self, nutrition: i32, modifier: f32) {
        self.eat(nutrition, nutrition as f32 * modifier * 2.0);
    }

    /// Advances one game tick. The caller applies the returned effect, which
    /// already accounts for the exhaustion that healing costs.
    pub fn tick(
        &mut self,
        health: f32,
        max_health: f32,
        difficulty: Difficulty,
        natural_regen: bool,
    ) -> FoodEffect {
        if self.exhaustion > EXHAUSTION_PER_FOOD_POINT {
            self.exhaustion -= EXHAUSTION_PER_FOOD_POINT;
            if self.saturation > 0.0 {
                self.saturation = (self.saturation - 1.0).max(0.0);
            } else if difficulty != Difficulty::Peaceful {
                self.level = (self.level - 1).max(0);
            }
        }
        let hurt = health > 0.0 && health < max_health;
        if natural_regen && self.saturation > 0.0 && hurt && self.level >= MAX_FOOD {
            self.tick_timer += 1;
            if self.tick_timer >= SATURATED_REGEN_TICKS {
                let amount = self.saturation.min(SATURATED_HEAL_CAP);
                self.add_exhaustion(amount);
                self.tick_timer = 0;
                return FoodEffect::Heal(amount / SATURATED_HEAL_CAP);
            }
        } else if natural_regen && self.level >= REGEN_FOOD_THRESHOLD && hurt {
            self.tick_timer += 1;
            if self.tick_timer >= REGEN_TICKS {
                self.add_exhaustion(REGEN_EXHAUSTION);
                self.tick_timer = 0;
                return FoodEffect::Heal(1.0);
            }
        } else if self.level <= 0 {
            self.tick_timer += 1;
            if self.tick_timer >= REGEN_TICKS {
                self.tick_timer = 0;
                let starves = health > 10.0
                    || difficulty == Difficulty::Hard
                    || (health > 1.0 && difficulty == Difficulty::Normal);
                if starves {
                    return FoodEffect::Starve;
                }
            }
        } else {
            self.tick_timer = 0;
        }
        FoodEffect::None
    }
}

/// Health plus the post-hit invulnerability window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Health {
    pub value: f32,
    invulnerable_ticks: u32,
    last_hurt: f32,
}

impl Default for Health {
    fn default() -> Self {
        Self {
            value: MAX_HEALTH,
            invulnerable_ticks: 0,
            last_hurt: 0.0,
        }
    }
}

impl Health {
    pub fn is_dead(&self) -> bool {
        self.value <= 0.0
    }

    pub fn tick(&mut self) {
        self.invulnerable_ticks = self.invulnerable_ticks.saturating_sub(1);
    }

    pub fn heal(&mut self, amount: f32, max_health: f32) {
        if !self.is_dead() {
            self.value = (self.value + amount).min(max_health);
        }
    }

    /// Applies damage the vanilla way: during the second half of the
    /// invulnerability window only a hit stronger than the last one lands, and
    /// only for the difference. Returns the damage actually dealt, if any.
    pub fn hurt(&mut self, amount: f32) -> Option<f32> {
        if self.is_dead() || amount <= 0.0 {
            return None;
        }
        let dealt = if self.invulnerable_ticks > INVULNERABLE_TICKS / 2 {
            if amount <= self.last_hurt {
                return None;
            }
            let dealt = amount - self.last_hurt;
            self.last_hurt = amount;
            dealt
        } else {
            self.last_hurt = amount;
            self.invulnerable_ticks = INVULNERABLE_TICKS;
            amount
        };
        self.value = (self.value - dealt).max(0.0);
        Some(dealt)
    }

    /// Runs a hit of `amount` through the invulnerability window: the part
    /// that gets through, or `None` if the window absorbs it, and whether
    /// the hit was a full one (starting the window: it flashes and knocks
    /// back). During the window's second half only a stronger hit lands,
    /// for the difference. `bypasses` ignores the window.
    pub fn window(&mut self, amount: f32, bypasses: bool) -> Option<(f32, bool)> {
        if self.is_dead() {
            return None;
        }
        if self.invulnerable_ticks > INVULNERABLE_TICKS / 2 && !bypasses {
            if amount <= self.last_hurt {
                return None;
            }
            let dealt = amount - self.last_hurt;
            self.last_hurt = amount;
            return Some((dealt, false));
        }
        self.last_hurt = amount;
        self.invulnerable_ticks = INVULNERABLE_TICKS;
        Some((amount, true))
    }

    /// Takes `amount` off the health, after all reductions.
    pub fn lose(&mut self, amount: f32) {
        self.value = (self.value - amount).max(0.0);
    }

    /// Whether a new hit would currently be absorbed in full.
    pub fn in_full_cooldown(&self, amount: f32) -> bool {
        self.invulnerable_ticks > INVULNERABLE_TICKS / 2 && amount <= self.last_hurt
    }

    /// Like [`Self::hurt`], but absorption hearts soak up damage first.
    /// Returns the damage that got through the invulnerability window, even
    /// if absorption then took all of it.
    pub fn hurt_absorbing(&mut self, amount: f32, absorption: &mut f32) -> Option<f32> {
        if self.is_dead() || amount <= 0.0 {
            return None;
        }
        let dealt = if self.invulnerable_ticks > INVULNERABLE_TICKS / 2 {
            if amount <= self.last_hurt {
                return None;
            }
            let dealt = amount - self.last_hurt;
            self.last_hurt = amount;
            dealt
        } else {
            self.last_hurt = amount;
            self.invulnerable_ticks = INVULNERABLE_TICKS;
            amount
        };
        let remaining = (dealt - *absorption).max(0.0);
        *absorption = (*absorption - (dealt - remaining)).max(0.0);
        self.value = (self.value - remaining).max(0.0);
        Some(dealt)
    }
}

/// Using (eating, drinking, blocking with) a held item, as vanilla counts it
/// down. Consumables finish after their consume time; a shield is held up
/// until released.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsingItem {
    pub off_hand: bool,
    pub item: Item,
    pub total: u32,
    pub remaining: u32,
}

impl UsingItem {
    /// The use duration of items that are held rather than consumed.
    pub const HELD: u32 = 72_000;
    const EFFECTS_START_FRACTION: f32 = 0.21875;
    const EFFECTS_INTERVAL: u32 = 4;

    pub fn new(off_hand: bool, item: Item, ticks: u32) -> Self {
        Self {
            off_hand,
            item,
            total: ticks,
            remaining: ticks,
        }
    }

    /// Whether a consumable plays its sound and particles on this tick.
    pub fn emits_effects(&self) -> bool {
        let used = self.total - self.remaining;
        let wait = (self.total as f32 * Self::EFFECTS_START_FRACTION) as u32;
        used > wait && self.remaining.is_multiple_of(Self::EFFECTS_INTERVAL)
    }

    /// Counts down one tick; true once the use completes.
    pub fn tick(&mut self) -> bool {
        self.remaining = self.remaining.saturating_sub(1);
        self.remaining == 0 && self.total != Self::HELD
    }
}

/// A survival block break in progress. Vanilla shows it to everyone nearby,
/// the breaker included, with crack particles every tick and the block's hit
/// sound every fourth tick after the start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Destroying {
    pub pos: BlockPos,
    direction: i32,
    started: u64,
}

impl Destroying {
    pub fn new(pos: BlockPos, face: i8, tick: u64) -> Self {
        let mut destroying = Self {
            pos,
            direction: 0,
            started: tick,
        };
        destroying.set_face(face);
        destroying
    }

    /// Mirrors the protocol's lenient direction decoding (unsigned byte,
    /// modulo 6), so any byte yields a valid direction for clients.
    pub fn set_face(&mut self, face: i8) {
        self.direction = i32::from(face as u8 % 6);
    }

    /// Ticks since the break started.
    pub fn ticks(&self, tick: u64) -> u64 {
        tick.wrapping_sub(self.started)
    }

    /// The Level Event `(event, data)` to show on `tick`.
    pub fn event(&self, tick: u64) -> (i32, i32) {
        let event = if tick.wrapping_sub(self.started).is_multiple_of(4) {
            level_event::DESTROY_PROGRESS_SOUND
        } else {
            level_event::DESTROY_PROGRESS
        };
        (event, self.direction)
    }
}

/// The crack stage (0-9, higher once done) a break at `progress` per tick
/// shows `ticks` after it started.
pub fn crack_stage(progress: f32, ticks: u64) -> i32 {
    (progress * (ticks as f32 + 1.0) * 10.0) as i32
}

/// Whether a player standing at `feet` sees others' cracks at `pos`.
pub fn within_crack_range(pos: BlockPos, feet: [f64; 3]) -> bool {
    let d = [
        f64::from(pos.x) - feet[0],
        f64::from(pos.y) - feet[1],
        f64::from(pos.z) - feet[2],
    ];
    d[0] * d[0] + d[1] * d[1] + d[2] * d[2] < 1024.0
}

/// Server-side fall tracking from the client's movement packets.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Fall {
    pub distance: f64,
    /// How far the last landing that hurt fell, for the combat log.
    pub impact_distance: f64,
    /// How far a landing on powder snow fell, until its sound is played.
    pub powder_landing: Option<f64>,
    in_water: bool,
    landing: Option<Landing>,
    landed: Option<f64>,
    /// The height a wind burst launched it from: a landing counts only
    /// the drop below it.
    impulse: Option<f64>,
    /// Ticks the impulse survives touching the ground.
    impulse_grace: u32,
}

/// Ticks after a wind burst during which landing keeps its impulse.
const IMPULSE_GRACE: u32 = 40;

#[derive(Debug, Clone, Copy, PartialEq)]
struct Landing {
    seq: u64,
    distance: f64,
}

/// What the home region saw around a player as of one movement update,
/// as far as its fall is concerned.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct FallReport {
    pub in_water: bool,
    /// Climbing, or inside a cobweb, bush or powder snow.
    pub resets: bool,
    /// The block under the feet.
    pub landed_on: Option<BlockState>,
    /// Sneaking, which keeps slime blocks from taking the landing.
    pub suppressing_bounce: bool,
}

impl Fall {
    pub fn new(distance: f64) -> Self {
        Self {
            distance,
            ..Self::default()
        }
    }

    /// Feeds one movement step, made as of player update `seq`. A landing
    /// that could hurt is held until the surroundings report for `seq`
    /// says what the player landed on; returns whether one is now held.
    pub fn step(&mut self, dy: f64, on_ground: bool, seq: u64, y: f64) -> bool {
        if !self.in_water && dy < 0.0 {
            self.distance -= dy;
        }
        let mut held = false;
        if on_ground {
            if self.distance > 0.0 {
                self.landed = Some(self.distance);
                if let Some(launch) = self.impulse {
                    let below = self.distance.min(launch - y);
                    if below <= 0.0 {
                        self.reset_impulse();
                    }
                    self.distance = below.max(0.0);
                }
            }
            if fall::could_hurt(self.distance, SAFE_FALL_DISTANCE) {
                self.reset_impulse();
                self.landing = Some(Landing {
                    seq,
                    distance: self.distance,
                });
                held = true;
            }
            self.distance = 0.0;
            self.try_reset_impulse();
        }
        if dy > 0.0 {
            self.distance = 0.0;
        }
        held
    }

    /// An explosion reached the player standing at height `y`: a wind
    /// burst makes its later landing count only from there.
    pub fn explosion_hit(&mut self, wind: bool, y: f64) {
        if wind {
            self.impulse_grace = self.impulse_grace.max(IMPULSE_GRACE);
            self.impulse = Some(y);
        } else {
            self.impulse_grace = 0;
        }
    }

    pub fn in_impulse_grace(&self) -> bool {
        self.impulse_grace > 0
    }

    pub fn tick_impulse(&mut self) {
        self.impulse_grace = self.impulse_grace.saturating_sub(1);
    }

    /// Drops the impulse once its grace is over (touching ground, water,
    /// a climbable or gliding).
    pub fn try_reset_impulse(&mut self) {
        if self.impulse_grace == 0 {
            self.reset_impulse();
        }
    }

    pub fn reset_impulse(&mut self) {
        self.impulse = None;
        self.impulse_grace = 0;
    }

    pub fn has_impulse(&self) -> bool {
        self.impulse.is_some()
    }

    pub fn in_water(&self) -> bool {
        self.in_water
    }

    /// The fall distance of the last landing, once.
    pub fn take_landed(&mut self) -> Option<f64> {
        self.landed.take()
    }

    /// Applies the region's report as of update `seq`; returns the damage
    /// and damage type of a held landing the report settles.
    pub fn surroundings(
        &mut self,
        seq: u64,
        report: FallReport,
    ) -> Option<(f32, &'static DamageType)> {
        self.in_water = report.in_water;
        let cancels = report.in_water || report.resets;
        if cancels {
            self.distance = 0.0;
        }
        let landing = self.landing.filter(|landing| seq >= landing.seq)?;
        self.landing = None;
        if cancels {
            return None;
        }
        if report
            .landed_on
            .is_some_and(|s| s.block() == Block::PowderSnow)
        {
            self.powder_landing = Some(landing.distance);
        }
        let impact = fall::impact(
            report.landed_on,
            landing.distance,
            report.suppressing_bounce,
        )?;
        let damage = impact.damage(SAFE_FALL_DISTANCE, 1.0);
        self.impact_distance = landing.distance;
        (damage > 0).then_some((damage as f32, impact.kind))
    }

    /// A liquid change touched the player between movement reports: water
    /// cancels its fall, including a held landing.
    pub fn fluid_contact(&mut self, in_water: bool) {
        self.in_water = in_water;
        if in_water {
            self.distance = 0.0;
            self.landing = None;
        }
    }
}

/// Elytra flight: whether a player may glide, and the durability tick.
///
/// Movement itself is client-authoritative (the client runs the same
/// lift/drag model, see [`glide_step`]); the server only needs to know
/// whether a player is gliding for its metadata flag, to suppress fall
/// damage, to wear the elytra, and to give a firework boost a velocity to
/// start from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Glide {
    pub active: bool,
    /// Ticks spent gliding since the last durability point was spent.
    ticks: u32,
}

impl Glide {
    /// Whether gliding is possible: not on the ground, not riding, not levitating,
    /// not creative-flying, and the chest slot carries a usable glider.
    pub fn can_glide(
        on_ground: bool,
        riding: bool,
        levitating: bool,
        flying: bool,
        chest: Option<&ItemStack>,
    ) -> bool {
        !on_ground && !riding && !levitating && !flying && chest.is_some_and(ItemStack::can_glide)
    }

    /// Player Command's "start fall flying": only takes effect while not
    /// already gliding, able to glide, and out of a liquid.
    pub fn try_start(&mut self, can_glide: bool, in_liquid: bool) -> bool {
        if !self.active && can_glide && !in_liquid {
            self.active = true;
            self.ticks = 0;
            true
        } else {
            false
        }
    }

    /// Advances one tick of flight; stops on its own once `can_glide` no
    /// longer holds (landing, mounting, Levitation, a broken elytra, ...).
    /// Returns whether the elytra should wear by one point this tick
    /// (vanilla spends one every 20 ticks of flight).
    pub fn tick(&mut self, can_glide: bool) -> bool {
        if !self.active {
            return false;
        }
        if !can_glide {
            self.active = false;
            self.ticks = 0;
            return false;
        }
        self.ticks += 1;
        self.ticks.is_multiple_of(20)
    }

    /// Ends flight immediately (landing, a stop-fall-flying packet, ...).
    pub fn stop(&mut self) -> bool {
        let was_active = self.active;
        self.active = false;
        self.ticks = 0;
        was_active
    }
}

/// One tick of vanilla's elytra lift/drag/pitch model: gravity pulls down,
/// a shallow dive (or looking up out of one) converts speed to lift, and
/// horizontal velocity turns to face the look direction. `gravity` is the
/// `minecraft:gravity` attribute (0.08 by default).
pub fn glide_step(velocity: [f64; 3], yaw: f32, pitch: f32, gravity: f64) -> [f64; 3] {
    let look = crate::world::raycast::view_vector(yaw, pitch);
    let lean = f64::from(pitch) * std::f64::consts::PI / 180.0;
    let look_horizontal = (look[0] * look[0] + look[2] * look[2]).sqrt();
    let move_horizontal = (velocity[0] * velocity[0] + velocity[2] * velocity[2]).sqrt();
    let lift = lean.cos() * lean.cos();
    let mut v = [
        velocity[0],
        velocity[1] + gravity * (-1.0 + lift * 0.75),
        velocity[2],
    ];
    if v[1] < 0.0 && look_horizontal > 0.0 {
        let convert = v[1] * -0.1 * lift;
        v[0] += look[0] * convert / look_horizontal;
        v[1] += convert;
        v[2] += look[2] * convert / look_horizontal;
    }
    if lean < 0.0 && look_horizontal > 0.0 {
        let convert = move_horizontal * -lean.sin() * 0.04;
        v[0] += -look[0] * convert / look_horizontal;
        v[1] += convert * 3.2;
        v[2] += -look[2] * convert / look_horizontal;
    }
    if look_horizontal > 0.0 {
        v[0] += (look[0] / look_horizontal * move_horizontal - v[0]) * 0.1;
        v[2] += (look[2] / look_horizontal * move_horizontal - v[2]) * 0.1;
    }
    [v[0] * 0.99, v[1] * 0.98, v[2] * 0.99]
}

/// One tick of a firework rocket's boost while attached to a gliding
/// player: velocity eases halfway towards 1.5x the look direction, plus a
/// small constant push along it.
pub fn firework_boost(velocity: [f64; 3], look: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|i| velocity[i] + look[i] * 0.1 + (look[i] * 1.5 - velocity[i]) * 0.5)
}

/// Whether a player standing at `position` is on something it can climb:
/// a block in the climbable tag, or an open trapdoor above a ladder facing
/// the same way.
pub fn on_climbable(position: [f64; 3], block: impl Fn(BlockPos) -> Option<BlockState>) -> bool {
    climbable_block(position, block).is_some()
}

/// The block a player at `position` climbs, if any: the climbable block it
/// stands in, or the open trapdoor over a ladder.
pub fn climbable_block(
    position: [f64; 3],
    block: impl Fn(BlockPos) -> Option<BlockState>,
) -> Option<Block> {
    let pos = BlockPos::new(
        position[0].floor() as i32,
        position[1].floor() as i32,
        position[2].floor() as i32,
    );
    let state = block(pos)?;
    if tuff_datapack::tags::is(tuff_vanilla_data::tag::block::key::CLIMBABLE, state.block()) {
        return Some(state.block());
    }
    if state.block().kind() != tuff_vanilla_data::BlockKind::Trapdoor || state.open() != Some(true)
    {
        return None;
    }
    let below = BlockPos::new(pos.x, pos.y - 1, pos.z);
    block(below)
        .is_some_and(|below| {
            below.block() == Block::Ladder && below.get("facing") == state.get("facing")
        })
        .then(|| state.block())
}

/// Whether a player standing at `position` has its bounding box in water.
pub fn touches_water(position: [f64; 3], block: impl Fn(BlockPos) -> Option<BlockState>) -> bool {
    touches_fluid(
        position,
        0.001,
        tuff_vanilla_data::tag::fluid::key::WATER,
        block,
    )
}

/// The damage of the fire the player stands in, if any.
pub fn fire_contact(
    position: [f64; 3],
    block: impl Fn(BlockPos) -> Option<BlockState>,
) -> Option<f32> {
    let bounds = [
        position[0] - PLAYER_HALF_WIDTH,
        position[1],
        position[2] - PLAYER_HALF_WIDTH,
        position[0] + PLAYER_HALF_WIDTH,
        position[1] + PLAYER_HEIGHT,
        position[2] + PLAYER_HALF_WIDTH,
    ];
    crate::world::contact::fire_contact(bounds, block)
}

/// Whether a player standing at `position` is inside lava, which sets it
/// alight and hurts it.
pub fn touches_lava(position: [f64; 3], block: impl Fn(BlockPos) -> Option<BlockState>) -> bool {
    touches_fluid(
        position,
        1.0e-5,
        tuff_vanilla_data::tag::fluid::key::LAVA,
        block,
    )
}

/// Whether the player's box, shrunk by `inset`, reaches below the surface
/// of a fluid in `kind`.
fn touches_fluid(
    position: [f64; 3],
    inset: f64,
    kind: tuff_vanilla_data::tag::Key<tuff_vanilla_data::fluid::Fluid>,
    block: impl Fn(BlockPos) -> Option<BlockState>,
) -> bool {
    let bounds = [
        position[0] - PLAYER_HALF_WIDTH,
        position[1],
        position[2] - PLAYER_HALF_WIDTH,
        position[0] + PLAYER_HALF_WIDTH,
        position[1] + PLAYER_HEIGHT,
        position[2] + PLAYER_HALF_WIDTH,
    ];
    crate::world::contact::box_touches(bounds, inset, kind, block)
}

/// Whether eyes at `eye` are under water, which slows mining and uses up
/// air.
pub fn eye_in_water(eye: [f64; 3], block: impl Fn(BlockPos) -> Option<BlockState>) -> bool {
    crate::world::contact::eyes_in(eye, tuff_vanilla_data::tag::fluid::key::WATER, block)
}

/// How far into water a player is, beyond touching it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Submersion {
    /// The eyes are under water.
    pub eyes: bool,
    /// The eyes are in a bubble column, which gives air instead.
    pub bubble_column: bool,
    /// The block at the feet holds water, where swimming can start.
    pub feet: bool,
}

impl Submersion {
    /// Whether the player is using up air.
    pub fn holding_breath(self) -> bool {
        self.eyes && !self.bubble_column
    }
}

/// The submersion of a player at `position` with its eyes `eye_height`
/// above its feet.
pub fn submersion(
    position: [f64; 3],
    eye_height: f64,
    block: impl Fn(BlockPos) -> Option<BlockState>,
) -> Submersion {
    let eye = [position[0], position[1] + eye_height, position[2]];
    let at = |p: [f64; 3]| {
        BlockPos::new(
            p[0].floor() as i32,
            p[1].floor() as i32,
            p[2].floor() as i32,
        )
    };
    let water = tuff_vanilla_data::tag::fluid::key::WATER;
    Submersion {
        eyes: eye_in_water(eye, &block),
        bubble_column: block(at(eye)).is_some_and(|state| state.block() == Block::BubbleColumn),
        feet: block(at(position))
            .is_some_and(|state| tuff_datapack::tags::is(water, state.fluid_state().fluid())),
    }
}

/// What a player adds to the speed it mines at: the ground under it, the
/// mining attributes of its gear and the Haste and Mining Fatigue effects.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Miner {
    pub on_ground: bool,
    /// Added to a tool's speed on blocks it is fast on (Efficiency).
    pub efficiency: f32,
    /// The Haste (or Conduit Power) amplifier.
    pub haste: Option<u8>,
    pub fatigue: Option<u8>,
    pub break_speed: f32,
    /// The speed factor with the eyes under water (Aqua Affinity raises it).
    pub submerged_speed: f32,
}

impl Miner {
    /// A player with no gear or effects that change how fast it mines.
    pub fn plain(on_ground: bool) -> Self {
        let base = |attribute: Attribute| {
            EntityType::Player
                .base_attribute(attribute)
                .unwrap_or(attribute.range().default) as f32
        };
        Self {
            on_ground,
            efficiency: base(Attribute::MiningEfficiency),
            haste: None,
            fatigue: None,
            break_speed: base(Attribute::BlockBreakSpeed),
            submerged_speed: base(Attribute::SubmergedMiningSpeed),
        }
    }
}

/// The fraction of `state` one tick of mining removes; the block breaks
/// once the total reaches 1, on the first tick if one is enough.
pub fn destroy_progress<M: tuff_item::item::Mode>(
    state: BlockState,
    tool: Option<&ItemStack<M>>,
    miner: &Miner,
    eye_in_water: bool,
) -> f32 {
    destroy_progress_at(state, state.destroy_speed(), tool, miner, eye_in_water)
}

/// [`destroy_progress`] with `hardness` in place of the state's own, tool
/// rules still those of `state`.
pub fn destroy_progress_at<M: tuff_item::item::Mode>(
    state: BlockState,
    hardness: Option<f32>,
    tool: Option<&ItemStack<M>>,
    miner: &Miner,
    eye_in_water: bool,
) -> f32 {
    let Some(hardness) = hardness else {
        return 0.0;
    };
    let block = state.block();
    let mut speed = tool
        .and_then(ItemStack::tool)
        .map_or(1.0, |tool| tool.mining_speed(block));
    if speed > 1.0 {
        speed += miner.efficiency;
    }
    if let Some(amplifier) = miner.haste {
        speed *= 1.0 + (amplifier as f32 + 1.0) * 0.2;
    }
    if let Some(amplifier) = miner.fatigue {
        speed *= 0.3f64.powi(amplifier as i32 + 1) as f32;
    }
    speed *= miner.break_speed;
    if eye_in_water {
        speed *= miner.submerged_speed;
    }
    if !miner.on_ground {
        speed /= 5.0;
    }
    let correct = !state.requires_correct_tool_for_drops()
        || tool.is_some_and(|stack| stack.is_correct_tool_for_drops(block));
    speed / hardness / if correct { 30.0 } else { 100.0 }
}

#[cfg(test)]
#[path = "player_tests.rs"]
mod tests;
