//! Fish: passive water mobs that swim along paths through the water, flee
//! players, flop about on land and can be caught in buckets. Cod, salmon
//! and tropical fish swim in schools; pufferfish puff up and sting.

use rand::Rng;
use rand::rngs::StdRng;
use tuff_vanilla_data::Item;
use tuff_vanilla_data::damage_type;
use tuff_vanilla_data::entity::MAX_AIR_SUPPLY;
use tuff_vanilla_data::item::DyeColor;
use tuff_vanilla_data::registry::{EntityType, MobEffect, SoundEvent};

use super::control::MoveStyle;
use super::goal::{Flags, Goal, GoalSelector};
use super::goals::{AvoidPlayer, Panic, RandomStroll};
use super::movement::{Sinking, WaterTravel};
use super::navigation::NavKind;
use super::school::{self, FollowFlockLeader};
use super::spawning::{GroupData, SpawnGroup};
use super::{MobCore, MobSounds, OwnAirRules, Species, physics};
use tuff_core::protocol::play::{self, MetadataValue};
use tuff_core::world::effects::ActiveEffect;
use tuff_core::world::entity::{
    Culprit, Effect, EntityHit, EntityInteraction, InteractOutcome, TickContext, take_bool,
    take_number,
};
use tuff_core::world::messages::{HandEffect, PlayerEvent};
use tuff_core::world::{framed, packets};
use tuff_item::item::{ItemStack, MintReason};
use tuff_nbt::{Compound, Tag};
use tuff_vanilla_data::entity::PathType;

const SWIM_SPEED: f64 = 1.0;
const SWIM_INTERVAL: i32 = 40;
const FISH_STROKE: f32 = 0.01;
/// Fish are not ageable, so the slot ageable mobs keep their baby flag in
/// holds whether the fish came out of a bucket; a fish's own data follows.
const FROM_BUCKET: u8 = 16;
const FISH_DATA: u8 = 17;
const MAX_SCHOOL: u32 = 8;
const SALMON_MAX_SCHOOL: u32 = 5;

/// A fish out of water breathes by the same 20-tick drowning rhythm as the
/// common rule, just inverted (dry instead of submerged); it never refills
/// past the shared max air.
pub(crate) fn fish_air_rules() -> OwnAirRules {
    OwnAirRules {
        max: MAX_AIR_SUPPLY as i16,
        in_rain: false,
        damage_kind: &damage_type::DROWN,
        damage: 2.0,
    }
}

fn configure_fish(core: &mut MobCore, school: Option<u32>) {
    core.set_path_malus(PathType::Water, 0.0);
    core.navigation.kind = NavKind::WaterBound { breaching: false };
    core.move_style = MoveStyle::Fish;
    core.water_travel = WaterTravel::Swimmer {
        stroke: Some(FISH_STROKE),
        sinks: Sinking::WithoutTarget,
    };
    let mut goals = GoalSelector::default();
    goals.add(0, Panic::new(1.25));
    goals.add(2, AvoidPlayer::new(8.0, 1.6, 1.4));
    let stroll = RandomStroll::swimming(SWIM_SPEED, SWIM_INTERVAL);
    match school {
        Some(max) => {
            goals.add(4, stroll.unless(school::is_follower));
            goals.add(5, FollowFlockLeader::new(max));
        }
        None => goals.add(4, stroll),
    }
    core.goals = goals;
}

/// A fish stranded on the ground hops about at random.
pub(crate) fn flop(core: &mut MobCore, sound: SoundEvent, cx: &mut TickContext) {
    if core.body.in_water || !core.base.on_ground || !core.body.vertical_collision {
        return;
    }
    let dx = (cx.rng.r#gen::<f32>() * 2.0 - 1.0) * 0.05;
    let dz = (cx.rng.r#gen::<f32>() * 2.0 - 1.0) * 0.05;
    let velocity = &mut core.base.velocity;
    velocity[0] += f64::from(dx);
    velocity[1] += 0.400_000_005_960_464_5;
    velocity[2] += f64::from(dz);
    core.base.on_ground = false;
    core.impulse = true;
    let pitch = core.voice_pitch(cx.rng);
    core.sound(sound, 1.0, pitch, cx);
}

/// The `minecraft:bucket_entity_data` a captured mob's bucket carries, and
/// what a placed one's saved state restores: the fields vanilla's own
/// mob-bucket default data always writes (this server tracks only health,
/// no-AI and persistence among them) plus whatever a species adds.
pub(crate) fn capture(core: &mut MobCore, extra: impl FnOnce(&mut Compound)) -> Compound {
    let mut nbt = Compound::new();
    if let Some(name) = &core.base.custom_name {
        nbt.insert("CustomName", name.clone());
    }
    for (key, set) in [
        ("Silent", core.base.silent),
        ("NoGravity", core.base.no_gravity),
        ("Glowing", core.base.glowing),
        ("Invulnerable", core.base.invulnerable),
    ] {
        if set {
            nbt.insert(key, true);
        }
    }
    nbt.insert("Health", core.health);
    if core.no_ai {
        nbt.insert("NoAI", true);
    }
    if core.persistence_required {
        nbt.insert("PersistenceRequired", true);
    }
    extra(&mut nbt);
    nbt
}

/// The shared water-bucket capture: any species with no other click
/// behaviour uses this directly as its `interact`.
pub(crate) fn bucket_interact(
    kind: EntityType,
    core: &mut MobCore,
    interaction: &EntityInteraction,
    pickup_sound: SoundEvent,
    extra: impl FnOnce(&mut Compound),
    cx: &mut TickContext,
) -> InteractOutcome {
    let stack = interaction.stack.as_deref()?;
    if stack.item() != Item::WaterBucket || !core.is_alive() {
        return None;
    }
    let bucket_item = Item::of_bucket(kind)?;
    let nbt = capture(core, extra);
    let mut result = ItemStack::mint(bucket_item, 1, MintReason::Spawn);
    result
        .components
        .other
        .insert("minecraft:bucket_entity_data", Tag::Compound(nbt));
    core.sound_as(pickup_sound, play::sound_category::NEUTRAL, 1.0, 1.0, cx);
    super::leash::untie(&mut core.leash, &core.base, true, cx);
    cx.effects.push(Effect::Discard);
    Some(HandEffect::ExchangeStack(Box::new(result)))
}

/// What every fish shares besides its core: whether a player let it out
/// of a bucket (it then never despawns).
#[derive(Debug, Default, Clone, Copy)]
struct FishParts {
    from_bucket: bool,
}

impl FishParts {
    fn load(&mut self, nbt: &mut Compound) {
        self.from_bucket = take_bool(nbt, "FromBucket").unwrap_or(false);
    }

    fn save(self, out: &mut Compound) {
        out.insert("FromBucket", self.from_bucket);
    }

    fn metadata(self) -> (u8, MetadataValue) {
        (FROM_BUCKET, MetadataValue::Boolean(self.from_bucket))
    }
}

/// A schooling fish that spawns with others joins the first one's school.
fn join_spawn_school(core: &MobCore, cx: &mut TickContext) {
    let me = core.base.id;
    let leader = {
        let mut group = cx.state.get::<SpawnGroup>();
        match group.data {
            Some(GroupData::School { leader, .. }) => Some(leader),
            _ => {
                group.data = Some(GroupData::School {
                    leader: me,
                    variant: None,
                });
                None
            }
        }
    };
    if let Some(leader) = leader {
        cx.state.get::<school::Schools>().follow(me, leader);
    }
}

fn fish_sounds(ambient: Option<SoundEvent>, hurt: SoundEvent, death: SoundEvent) -> MobSounds {
    MobSounds {
        ambient,
        hurt: Some(hurt),
        death: Some(death),
        step: None,
        volume: 1.0,
    }
}

macro_rules! schooling_fish {
    ($name:ident, $kind:expr, $max:expr, $flop:expr, $ambient:expr, $hurt:expr, $death:expr) => {
        #[derive(Debug, Default)]
        pub struct $name {
            parts: FishParts,
        }

        impl Species for $name {
            fn pushed_by_fluid(&self) -> bool {
                false
            }

            fn create(_rng: &mut StdRng) -> Self {
                Self::default()
            }

            fn configure(core: &mut MobCore) {
                configure_fish(core, Some($max));
            }

            fn load(&mut self, _core: &mut MobCore, nbt: &mut Compound) {
                self.parts.load(nbt);
            }

            fn save(&self, _core: &MobCore, out: &mut Compound) {
                self.parts.save(out);
            }

            fn metadata(&self, _core: &MobCore) -> Vec<(u8, MetadataValue)> {
                vec![self.parts.metadata()]
            }

            fn finalize_spawn(&mut self, core: &mut MobCore, cx: &mut TickContext) {
                join_spawn_school(core, cx);
            }

            fn ai_step(&mut self, core: &mut MobCore, cx: &mut TickContext) {
                flop(core, $flop, cx);
                school::tick_leader(core, cx);
            }

            fn sounds(&self, _core: &MobCore) -> MobSounds {
                fish_sounds($ambient, $hurt, $death)
            }

            fn interact(
                &mut self,
                core: &mut MobCore,
                interaction: &EntityInteraction,
                cx: &mut TickContext,
            ) -> InteractOutcome {
                bucket_interact(
                    $kind,
                    core,
                    interaction,
                    SoundEvent::ItemBucketFillFish,
                    |_| {},
                    cx,
                )
            }

            fn remove_when_far_away(&self) -> bool {
                !self.parts.from_bucket
            }

            fn custom_persistence(&self, _core: &MobCore) -> bool {
                self.parts.from_bucket
            }

            fn air_rules(&self) -> Option<OwnAirRules> {
                Some(fish_air_rules())
            }
        }
    };
}

schooling_fish!(
    Cod,
    EntityType::Cod,
    MAX_SCHOOL,
    SoundEvent::EntityCodFlop,
    Some(SoundEvent::EntityCodAmbient),
    SoundEvent::EntityCodHurt,
    SoundEvent::EntityCodDeath
);

const SALMON_SIZE_KEY: &str = "minecraft:salmon_size";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SalmonSize {
    Small,
    Medium,
    Large,
}

impl SalmonSize {
    fn name(self) -> &'static str {
        match self {
            Self::Small => "small",
            Self::Medium => "medium",
            Self::Large => "large",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "small" => Self::Small,
            "medium" => Self::Medium,
            "large" => Self::Large,
            _ => return None,
        })
    }

    fn id(self) -> i32 {
        self as i32
    }

    fn scale(self) -> f32 {
        match self {
            Self::Small => 0.5,
            Self::Medium => 1.0,
            Self::Large => 1.5,
        }
    }

    fn random(rng: &mut StdRng) -> Self {
        match rng.gen_range(0..95) {
            0..30 => Self::Small,
            30..80 => Self::Medium,
            _ => Self::Large,
        }
    }
}

#[derive(Debug)]
pub struct Salmon {
    parts: FishParts,
    size: SalmonSize,
}

impl Salmon {
    fn set_size(&mut self, core: &mut MobCore, size: SalmonSize) {
        self.size = size;
        core.size_scale = size.scale();
        core.refresh_size();
        core.changed(FISH_DATA);
    }
}

impl Species for Salmon {
    fn pushed_by_fluid(&self) -> bool {
        false
    }

    fn create(_rng: &mut StdRng) -> Self {
        Self {
            parts: FishParts::default(),
            size: SalmonSize::Medium,
        }
    }

    fn configure(core: &mut MobCore) {
        configure_fish(core, Some(SALMON_MAX_SCHOOL));
    }

    fn load(&mut self, core: &mut MobCore, nbt: &mut Compound) {
        self.parts.load(nbt);
        let size = nbt
            .get_str("type")
            .and_then(SalmonSize::from_name)
            .unwrap_or(SalmonSize::Medium);
        nbt.remove("type");
        self.set_size(core, size);
    }

    fn save(&self, _core: &MobCore, out: &mut Compound) {
        self.parts.save(out);
        out.insert("type", self.size.name());
    }

    fn metadata(&self, _core: &MobCore) -> Vec<(u8, MetadataValue)> {
        vec![
            self.parts.metadata(),
            (FISH_DATA, MetadataValue::VarInt(self.size.id())),
        ]
    }

    fn finalize_spawn(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        let size = SalmonSize::random(cx.rng);
        self.set_size(core, size);
        join_spawn_school(core, cx);
    }

    fn ai_step(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        flop(core, SoundEvent::EntitySalmonFlop, cx);
        school::tick_leader(core, cx);
    }

    fn sounds(&self, _core: &MobCore) -> MobSounds {
        fish_sounds(
            Some(SoundEvent::EntitySalmonAmbient),
            SoundEvent::EntitySalmonHurt,
            SoundEvent::EntitySalmonDeath,
        )
    }

    fn interact(
        &mut self,
        core: &mut MobCore,
        interaction: &EntityInteraction,
        cx: &mut TickContext,
    ) -> InteractOutcome {
        let size = self.size;
        let mut outcome = bucket_interact(
            EntityType::Salmon,
            core,
            interaction,
            SoundEvent::ItemBucketFillFish,
            |_| {},
            cx,
        );
        if let Some(HandEffect::ExchangeStack(stack)) = &mut outcome {
            stack
                .components
                .other
                .insert(SALMON_SIZE_KEY, Tag::from(size.name()));
        }
        outcome
    }

    fn remove_when_far_away(&self) -> bool {
        !self.parts.from_bucket
    }

    fn custom_persistence(&self, _core: &MobCore) -> bool {
        self.parts.from_bucket
    }

    fn air_rules(&self) -> Option<OwnAirRules> {
        Some(fish_air_rules())
    }
}

const PUFF_STATE: u8 = FISH_DATA;
/// Ticks between stings on one victim: its hurt cooldown would swallow
/// any sooner one.
const STING_COOLDOWN: u64 = 10;

fn scares_pufferfish(kind: EntityType) -> bool {
    !tuff_core::world::entity_in_tag(kind, "not_scary_for_pufferfish")
}

/// Puffs its pufferfish up while something scary (a player not in creative,
/// or a mob not tagged harmless to it) is within two blocks: it raises
/// `signal` while it runs.
#[derive(Debug, Default)]
struct PuffUp;

impl Goal for PuffUp {
    fn flags(&self) -> Flags {
        Flags::NONE
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        let area = physics::inflate(mob.bounds(), [2.0; 3]);
        cx.players
            .iter()
            .any(|p| !p.creative && physics::intersects(area, p.bounds()))
            || cx.entities.iter().any(|e| {
                e.living.is_some_and(|l| l.alive)
                    && scares_pufferfish(e.kind)
                    && physics::intersects(area, e.bounds)
            })
    }

    fn start(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        mob.signal = 1;
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        mob.signal = 0;
    }
}

#[derive(Debug, Default)]
pub struct Pufferfish {
    parts: FishParts,
    puff: u8,
    inflate_counter: i32,
    deflate_timer: i32,
    scared: bool,
    stung: Vec<(i32, u64)>,
}

impl Pufferfish {
    fn scale(puff: u8) -> f32 {
        match puff {
            0 => 0.5,
            1 => 0.7,
            _ => 1.0,
        }
    }

    fn set_puff(&mut self, core: &mut MobCore, puff: u8) {
        self.puff = puff;
        core.size_scale = Self::scale(puff);
        core.refresh_size();
        core.changed(PUFF_STATE);
    }

    /// Whether `victim` may be stung again now, noting the sting.
    fn sting_ready(&mut self, victim: i32, tick: u64) -> bool {
        self.stung
            .retain(|(_, at)| tick.saturating_sub(*at) < STING_COOLDOWN);
        if self.stung.iter().any(|(id, _)| *id == victim) {
            return false;
        }
        self.stung.push((victim, tick));
        true
    }

    fn sting(&self, core: &MobCore) -> EntityHit {
        let mut hit = EntityHit::new(1.0 + f32::from(self.puff), &damage_type::MOB_ATTACK);
        hit.attacker = Some(Culprit::entity(&core.base));
        hit.position = Some(core.base.position);
        hit.effects.push(ActiveEffect::timed(
            MobEffect::Poison,
            60 * i64::from(self.puff),
            0,
        ));
        hit
    }

    /// Stings the mobs and players touching it while it is puffed up.
    fn touch(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if self.puff == 0 {
            return;
        }
        let reach = physics::inflate(core.bounds(), [0.3; 3]);
        let mobs: Vec<i32> = cx
            .entities
            .iter()
            .filter(|e| {
                e.living.is_some_and(|l| l.alive)
                    && scares_pufferfish(e.kind)
                    && physics::intersects(reach, e.bounds)
            })
            .map(|e| e.id)
            .collect();
        for id in mobs {
            if self.sting_ready(id, cx.tick) {
                cx.effects.push(Effect::Hurt {
                    target: id,
                    hit: Box::new(self.sting(core)),
                });
                core.sound(SoundEvent::EntityPufferFishSting, 1.0, 1.0, cx);
            }
        }
        let bounds = core.bounds();
        let players: Vec<(i32, bool)> = cx
            .players
            .iter()
            .filter(|p| physics::intersects(physics::inflate(p.bounds(), [1.0, 0.5, 1.0]), bounds))
            .map(|p| (p.id, p.creative || p.invulnerable))
            .collect();
        for (id, safe) in players {
            if !self.sting_ready(id, cx.tick) {
                continue;
            }
            cx.effects.push(Effect::Hurt {
                target: id,
                hit: Box::new(self.sting(core)),
            });
            if !safe && !core.base.silent {
                let packet = framed(&play::GameEvent {
                    event: play::game_event::PUFFER_FISH_STING,
                    value: 0.0,
                });
                cx.effects.push(Effect::TellPlayer {
                    player: id,
                    event: Box::new(PlayerEvent::Packets(packets([packet]))),
                });
            }
        }
    }
}

impl Species for Pufferfish {
    fn pushed_by_fluid(&self) -> bool {
        false
    }

    fn create(_rng: &mut StdRng) -> Self {
        Self::default()
    }

    fn configure(core: &mut MobCore) {
        configure_fish(core, None);
        core.goals.add(1, PuffUp);
        core.size_scale = Self::scale(0);
    }

    fn load(&mut self, core: &mut MobCore, nbt: &mut Compound) {
        self.parts.load(nbt);
        let puff = take_number(nbt, "PuffState").map_or(0, |v| (v as i32).clamp(0, 2) as u8);
        self.set_puff(core, puff);
    }

    fn save(&self, _core: &MobCore, out: &mut Compound) {
        self.parts.save(out);
        out.insert("PuffState", i32::from(self.puff));
    }

    fn metadata(&self, _core: &MobCore) -> Vec<(u8, MetadataValue)> {
        vec![
            self.parts.metadata(),
            (PUFF_STATE, MetadataValue::VarInt(i32::from(self.puff))),
        ]
    }

    fn tick(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        let scared = core.signal != 0;
        if scared != self.scared {
            self.scared = scared;
            self.inflate_counter = i32::from(scared);
            if scared {
                self.deflate_timer = 0;
            }
        }
        if !core.is_alive() || core.no_ai {
            return;
        }
        if self.inflate_counter > 0 {
            if self.puff == 0 {
                core.sound(SoundEvent::EntityPufferFishBlowUp, 1.0, 1.0, cx);
                self.set_puff(core, 1);
            } else if self.inflate_counter > 40 && self.puff == 1 {
                core.sound(SoundEvent::EntityPufferFishBlowUp, 1.0, 1.0, cx);
                self.set_puff(core, 2);
            }
            self.inflate_counter += 1;
        } else if self.puff != 0 {
            if self.deflate_timer > 60 && self.puff == 2 {
                core.sound(SoundEvent::EntityPufferFishBlowOut, 1.0, 1.0, cx);
                self.set_puff(core, 1);
            } else if self.deflate_timer > 100 && self.puff == 1 {
                core.sound(SoundEvent::EntityPufferFishBlowOut, 1.0, 1.0, cx);
                self.set_puff(core, 0);
            }
            self.deflate_timer += 1;
        }
    }

    fn ai_step(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        flop(core, SoundEvent::EntityPufferFishFlop, cx);
        self.touch(core, cx);
    }

    fn sounds(&self, _core: &MobCore) -> MobSounds {
        fish_sounds(
            None,
            SoundEvent::EntityPufferFishHurt,
            SoundEvent::EntityPufferFishDeath,
        )
    }

    fn interact(
        &mut self,
        core: &mut MobCore,
        interaction: &EntityInteraction,
        cx: &mut TickContext,
    ) -> InteractOutcome {
        bucket_interact(
            EntityType::Pufferfish,
            core,
            interaction,
            SoundEvent::ItemBucketFillFish,
            |_| {},
            cx,
        )
    }

    fn remove_when_far_away(&self) -> bool {
        !self.parts.from_bucket
    }

    fn custom_persistence(&self, _core: &MobCore) -> bool {
        self.parts.from_bucket
    }

    fn air_rules(&self) -> Option<OwnAirRules> {
        Some(fish_air_rules())
    }
}

/// A tropical fish's look: a pattern (0..12, six small then six large
/// shapes) and two dye colors, packed into the single entity-data int the
/// game renders it from. Packing: a 16-bit pattern id (a 0/1 "small or
/// large" flag in the low byte, the 0..6 shape index in the next byte),
/// then the base color id and the pattern color id, one per byte, from low
/// to high.
#[derive(Debug, Clone, Copy, PartialEq)]
struct FishVariant {
    pattern: u8,
    base_color: DyeColor,
    pattern_color: DyeColor,
}

const PATTERN_COUNT: u8 = 12;

/// The looks most tropical fish spawn with, by pattern index and colors.
const COMMON_VARIANTS: [(u8, DyeColor, DyeColor); 22] = [
    (7, DyeColor::Orange, DyeColor::Gray),
    (6, DyeColor::Gray, DyeColor::Gray),
    (6, DyeColor::Gray, DyeColor::Blue),
    (11, DyeColor::White, DyeColor::Gray),
    (1, DyeColor::Blue, DyeColor::Gray),
    (0, DyeColor::Orange, DyeColor::White),
    (5, DyeColor::Pink, DyeColor::LightBlue),
    (9, DyeColor::Purple, DyeColor::Yellow),
    (11, DyeColor::White, DyeColor::Red),
    (5, DyeColor::White, DyeColor::Yellow),
    (8, DyeColor::White, DyeColor::Gray),
    (11, DyeColor::White, DyeColor::Orange),
    (3, DyeColor::Cyan, DyeColor::Pink),
    (4, DyeColor::Lime, DyeColor::LightBlue),
    (10, DyeColor::Red, DyeColor::White),
    (2, DyeColor::Gray, DyeColor::Red),
    (9, DyeColor::Red, DyeColor::White),
    (6, DyeColor::White, DyeColor::Yellow),
    (0, DyeColor::Red, DyeColor::White),
    (1, DyeColor::Gray, DyeColor::White),
    (3, DyeColor::Cyan, DyeColor::Yellow),
    (6, DyeColor::Yellow, DyeColor::Yellow),
];

impl FishVariant {
    fn packed(self) -> i32 {
        let base = i32::from(self.pattern >= 6);
        let index = i32::from(self.pattern % 6);
        let pattern_id = base | (index << 8);
        (pattern_id & 0xFFFF)
            | ((self.base_color.id() as i32 & 0xFF) << 16)
            | ((self.pattern_color.id() as i32 & 0xFF) << 24)
    }

    fn unpacked(packed: i32) -> Self {
        let pattern_id = packed & 0xFFFF;
        let base = (pattern_id & 0xFF).min(1) as u8;
        let index = ((pattern_id >> 8) & 0xFF) as u8;
        Self {
            pattern: base * 6 + index.min(5),
            base_color: DyeColor::from_id(((packed >> 16) & 0xFF) as u32)
                .unwrap_or(DyeColor::White),
            pattern_color: DyeColor::from_id(((packed >> 24) & 0xFF) as u32)
                .unwrap_or(DyeColor::White),
        }
    }

    fn random(rng: &mut StdRng) -> Self {
        Self {
            pattern: rng.gen_range(0..PATTERN_COUNT),
            base_color: DyeColor::ALL[rng.gen_range(0..DyeColor::ALL.len())],
            pattern_color: DyeColor::ALL[rng.gen_range(0..DyeColor::ALL.len())],
        }
    }

    fn common(rng: &mut StdRng) -> Self {
        let (pattern, base_color, pattern_color) =
            COMMON_VARIANTS[rng.gen_range(0..COMMON_VARIANTS.len())];
        Self {
            pattern,
            base_color,
            pattern_color,
        }
    }
}

#[derive(Debug)]
pub struct TropicalFish {
    parts: FishParts,
    variant: FishVariant,
}

impl Default for TropicalFish {
    fn default() -> Self {
        Self {
            parts: FishParts::default(),
            variant: FishVariant {
                pattern: 0,
                base_color: DyeColor::White,
                pattern_color: DyeColor::White,
            },
        }
    }
}

impl Species for TropicalFish {
    fn pushed_by_fluid(&self) -> bool {
        false
    }

    fn create(rng: &mut StdRng) -> Self {
        Self {
            parts: FishParts::default(),
            variant: FishVariant::random(rng),
        }
    }

    fn configure(core: &mut MobCore) {
        configure_fish(core, Some(MAX_SCHOOL));
    }

    /// A school shares its first fish's look; nine in ten groups start
    /// from one of the common looks, the rest is one lone fish of any look.
    fn finalize_spawn(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        let me = core.base.id;
        let data = cx.state.get::<SpawnGroup>().data;
        if let Some(GroupData::School { leader, variant }) = data {
            if let Some(variant) = variant {
                self.variant = FishVariant::unpacked(variant);
            }
            cx.state.get::<school::Schools>().follow(me, leader);
            return;
        }
        let group = if f64::from(cx.rng.r#gen::<f32>()) < 0.9 {
            self.variant = FishVariant::common(cx.rng);
            SpawnGroup {
                data: Some(GroupData::School {
                    leader: me,
                    variant: Some(self.variant.packed()),
                }),
                ..SpawnGroup::default()
            }
        } else {
            self.variant = FishVariant::random(cx.rng);
            SpawnGroup {
                data: Some(GroupData::School {
                    leader: me,
                    variant: None,
                }),
                full: true,
                ..SpawnGroup::default()
            }
        };
        let index = cx.state.get::<SpawnGroup>().index;
        *cx.state.get::<SpawnGroup>() = SpawnGroup { index, ..group };
    }

    fn ai_step(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        flop(core, SoundEvent::EntityTropicalFishFlop, cx);
        school::tick_leader(core, cx);
    }

    fn load(&mut self, _core: &mut MobCore, nbt: &mut Compound) {
        self.parts.load(nbt);
        if let Some(packed) = take_number(nbt, "Variant").or_else(|| take_number(nbt, "variant")) {
            self.variant = FishVariant::unpacked(packed as i32);
        }
    }

    fn save(&self, _core: &MobCore, out: &mut Compound) {
        self.parts.save(out);
        out.insert("Variant", self.variant.packed());
    }

    fn metadata(&self, _core: &MobCore) -> Vec<(u8, MetadataValue)> {
        vec![
            self.parts.metadata(),
            (FISH_DATA, MetadataValue::VarInt(self.variant.packed())),
        ]
    }

    fn sounds(&self, _core: &MobCore) -> MobSounds {
        fish_sounds(
            Some(SoundEvent::EntityTropicalFishAmbient),
            SoundEvent::EntityTropicalFishHurt,
            SoundEvent::EntityTropicalFishDeath,
        )
    }

    fn interact(
        &mut self,
        core: &mut MobCore,
        interaction: &EntityInteraction,
        cx: &mut TickContext,
    ) -> InteractOutcome {
        let variant = self.variant;
        bucket_interact(
            EntityType::TropicalFish,
            core,
            interaction,
            SoundEvent::ItemBucketFillFish,
            |nbt| {
                nbt.insert("Variant", variant.packed());
            },
            cx,
        )
    }

    fn remove_when_far_away(&self) -> bool {
        !self.parts.from_bucket
    }

    fn custom_persistence(&self, _core: &MobCore) -> bool {
        self.parts.from_bucket
    }

    fn air_rules(&self) -> Option<OwnAirRules> {
        Some(fish_air_rules())
    }
}

/// Loads a captured mob's `minecraft:bucket_entity_data` (or an empty
/// compound for a bucket with none) the same way a saved chunk's entity
/// NBT loads, and places it in the world, marked as let out of a bucket.
pub fn spawn_from_bucket(
    kind: EntityType,
    base: tuff_core::world::entity::EntityBase,
    data: Option<Compound>,
    components: Option<&Compound>,
) -> Option<Box<dyn tuff_core::world::entity::RegionEntity>> {
    if kind == EntityType::SulfurCube {
        return super::sulfur_cube::from_bucket(base, data, components);
    }
    let mut nbt = data.unwrap_or_default();
    nbt.insert("FromBucket", true);
    if kind == EntityType::Salmon
        && let Some(size) = components.and_then(|c| c.get_str(SALMON_SIZE_KEY))
    {
        nbt.insert("type", size);
    }
    match kind {
        EntityType::Cod => super::load::<Cod>(base, &mut nbt),
        EntityType::Salmon => super::load::<Salmon>(base, &mut nbt),
        EntityType::Pufferfish => super::load::<Pufferfish>(base, &mut nbt),
        EntityType::TropicalFish => super::load::<TropicalFish>(base, &mut nbt),
        EntityType::Axolotl => super::load::<super::axolotl::Axolotl>(base, &mut nbt),
        EntityType::Tadpole => super::load::<super::tadpole::Tadpole>(base, &mut nbt),
        _ => None,
    }
}

fn released(
    base: tuff_core::world::entity::EntityBase,
    data: Option<Compound>,
    components: Option<&Compound>,
) -> Option<Box<dyn tuff_core::world::entity::RegionEntity>> {
    spawn_from_bucket(base.kind, base, data, components)
}

/// Registers the fish's loaders, and the axolotl's, and how every mob that
/// lives in a bucket comes out of one.
pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    for kind in [
        EntityType::Cod,
        EntityType::Salmon,
        EntityType::Pufferfish,
        EntityType::TropicalFish,
        EntityType::Axolotl,
        EntityType::Tadpole,
        EntityType::SulfurCube,
    ] {
        registry.register_bucket_release(kind, released);
    }
    registry.register(EntityType::Cod, super::load::<Cod>);
    registry.register(EntityType::Salmon, super::load::<Salmon>);
    registry.register(EntityType::Pufferfish, super::load::<Pufferfish>);
    registry.register(EntityType::TropicalFish, super::load::<TropicalFish>);
    for kind in [EntityType::Cod, EntityType::Salmon, EntityType::Pufferfish] {
        registry.register_spawn_rule(kind, super::spawning::surface_water_rule);
    }
    registry.register_spawn_rule(
        EntityType::TropicalFish,
        super::spawning::tropical_fish_rule,
    );
    super::axolotl::register(registry);
    super::tadpole::register(registry);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn capture_keeps_name_and_entity_flags() {
        use tuff_core::world::entity::EntityBase;
        let mut core = MobCore::new(EntityBase::new(EntityType::Cod, 1, 1, [0.0; 3], [0.0; 3]));
        core.base.custom_name = Some(Tag::from("\"Finn\""));
        core.base.silent = true;
        core.base.glowing = true;
        core.base.invulnerable = true;
        let nbt = capture(&mut core, |_| {});
        assert!(nbt.get("CustomName").is_some());
        for key in ["Silent", "Glowing", "Invulnerable"] {
            assert_eq!(nbt.get_bool(key), Some(true), "{key}");
        }
        assert_eq!(nbt.get_bool("NoGravity"), None);
    }

    #[tuff_core::watchdog]
    #[test]
    fn fish_data_indices_match_the_dumped_layouts() {
        let boolean = MetadataValue::Boolean(false).type_id() as u8;
        let varint = MetadataValue::VarInt(0).type_id() as u8;
        for kind in [EntityType::Cod, EntityType::Salmon] {
            assert_eq!(kind.data_layout()[FROM_BUCKET as usize], boolean);
        }
        let puffer = EntityType::Pufferfish.data_layout();
        assert_eq!(puffer.len(), PUFF_STATE as usize + 1);
        assert_eq!(puffer[FROM_BUCKET as usize], boolean);
        assert_eq!(puffer[PUFF_STATE as usize], varint);
        let tropical = EntityType::TropicalFish.data_layout();
        assert_eq!(tropical.len(), FISH_DATA as usize + 1);
        assert_eq!(tropical[FISH_DATA as usize], varint);
        let salmon = EntityType::Salmon.data_layout();
        assert_eq!(salmon.len(), FISH_DATA as usize + 1);
        assert_eq!(salmon[FISH_DATA as usize], varint);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_large_salmon_scales_its_box_and_keeps_its_type() {
        use tuff_core::world::entity::EntityBase;
        let base = EntityBase::new(EntityType::Salmon, 1, 1, [0.0; 3], [0.0; 3]);
        let mut nbt = Compound::new().with("type", "large");
        let released = spawn_from_bucket(EntityType::Salmon, base, Some(nbt.clone()), None);
        let mut saved = Compound::new();
        released.unwrap().save(&mut saved);
        assert_eq!(saved.get_str("type"), Some("large"));
        let normal = EntityType::Salmon.size();
        let mut core = MobCore::new(EntityBase::new(
            EntityType::Salmon,
            1,
            1,
            [0.0; 3],
            [0.0; 3],
        ));
        let mut salmon = Salmon::create(&mut <StdRng as rand::SeedableRng>::seed_from_u64(1));
        salmon.load(&mut core, &mut nbt);
        assert_eq!(core.size.width, normal.width * 1.5);
        assert_eq!(salmon.size.id(), 2);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_bucket_component_sets_the_released_salmon_size() {
        use tuff_core::world::entity::EntityBase;
        let base = EntityBase::new(EntityType::Salmon, 1, 1, [0.0; 3], [0.0; 3]);
        let components = Compound::new().with(SALMON_SIZE_KEY, "small");
        let released = spawn_from_bucket(EntityType::Salmon, base, None, Some(&components));
        let mut saved = Compound::new();
        released.unwrap().save(&mut saved);
        assert_eq!(saved.get_str("type"), Some("small"));
    }

    #[tuff_core::watchdog]
    #[test]
    fn tropical_fish_variants_round_trip_their_packing() {
        for (pattern, base_color, pattern_color) in COMMON_VARIANTS {
            let variant = FishVariant {
                pattern,
                base_color,
                pattern_color,
            };
            assert_eq!(FishVariant::unpacked(variant.packed()), variant);
        }
        let stripey = FishVariant::common(&mut <StdRng as rand::SeedableRng>::seed_from_u64(0));
        assert!(stripey.packed() & 0xFF <= 1);
    }

    #[tuff_core::watchdog]
    #[test]
    fn tropical_fish_spawned_together_share_a_look_and_school() {
        use tuff_core::world::entity::{EntityBase, RegionEntity};
        crate::tests::with_context(|_, cx| {
            let fish = |id| {
                let base =
                    EntityBase::new(EntityType::TropicalFish, id, id as u128, [0.5; 3], [0.0; 3]);
                let mut rng = <StdRng as rand::SeedableRng>::seed_from_u64(id as u64);
                super::super::Mob::<TropicalFish>::new(base, &mut rng)
            };
            let mut schools = 0;
            for round in 0..20 {
                let (mut first, mut second) = (fish(100 + round * 2), fish(101 + round * 2));
                first.finalize_spawn(cx, 0);
                if cx.state.get::<SpawnGroup>().full {
                    continue;
                }
                schools += 1;
                second.finalize_spawn(cx, 1);
                assert_eq!(second.species.variant, first.species.variant);
                assert!(COMMON_VARIANTS.iter().any(|&(p, b, c)| {
                    first.species.variant
                        == FishVariant {
                            pattern: p,
                            base_color: b,
                            pattern_color: c,
                        }
                }));
                assert_eq!(
                    cx.state
                        .get::<school::Schools>()
                        .leader_of(second.core.base.id),
                    Some(first.core.base.id)
                );
            }
            assert!(schools > 10, "nine in ten groups are schools");
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_puffed_pufferfish_stings_each_victim_once_per_cooldown() {
        let mut fish = Pufferfish::default();
        assert!(fish.sting_ready(7, 100));
        assert!(!fish.sting_ready(7, 105));
        assert!(fish.sting_ready(8, 105));
        assert!(fish.sting_ready(7, 110));
    }
}
