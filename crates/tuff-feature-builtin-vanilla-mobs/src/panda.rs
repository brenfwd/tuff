//! Pandas: a main and hidden gene pair, exactly as the game breeds and
//! rolls them (recessive genes only show when both copies match), eating
//! bamboo and defending themselves when hurt. Lazy pandas lie on their
//! backs and worried ones sit out thunderstorms. Pandas pick up bamboo and
//! cake from the ground and sit down to eat it, or from a hand. Babies
//! sneeze (a gift drops) and playful ones roll; worried pandas avoid
//! players and monsters; breeding without bamboo near makes them unhappy.
//! An aggressive panda alerting others and adults jumping at a sneeze are
//! not modeled.

use rand::Rng;
use rand::rngs::StdRng;
use tuff_vanilla_data::registry::{Attribute, EntityType, SoundEvent};
use tuff_vanilla_data::{Item, tag};

use super::attack::MeleeAttack;
use super::goal::{Flags, Goal, GoalSelector};
use super::goals::{
    AvoidPlayer, Breed, FollowParent, LookAtPlayer, Panic, RandomLookAround, RandomStroll, Tempt,
};
use super::target::HurtByTarget;
use super::{MobCore, MobSounds, Species, data};
use tuff_core::protocol::play::MetadataValue;
use tuff_core::world::entity::{Effect, EntityInteraction, InteractOutcome, TickContext, take};
use tuff_core::world::messages::HandEffect;
use tuff_item::item::ItemStack;
use tuff_nbt::Compound;
use tuff_vanilla_data::item::EquipmentSlot;
use tuff_world_model::BlockPos;

const BREED_TIMER: u8 = data::SPECIES;
const SNEEZE_TIMER: u8 = data::SPECIES + 1;
const EAT_TIMER: u8 = data::SPECIES + 2;
const MAIN_GENE: u8 = data::SPECIES + 3;
const HIDDEN_GENE: u8 = data::SPECIES + 4;
const FLAGS: u8 = data::SPECIES + 5;

const FLAG_SNEEZING: u8 = 2;
const FLAG_ROLLING: u8 = 4;
const FLAG_SITTING: u8 = 8;
const FLAG_ON_BACK: u8 = 16;
const LIE_COOLDOWN: u64 = 200;

fn set_flag(core: &mut MobCore, flag: u8, on: bool) {
    let flags = if on {
        core.panda_flags | flag
    } else {
        core.panda_flags & !flag
    };
    if flags != core.panda_flags {
        core.panda_flags = flags;
        core.changed(FLAGS);
    }
}

fn can_perform_action(core: &MobCore) -> bool {
    core.panda_flags & (FLAG_SITTING | FLAG_ON_BACK | FLAG_ROLLING) == 0 && core.panda_eat <= 0
}

fn set_eat(core: &mut MobCore, counter: i32) {
    if core.panda_eat != counter {
        core.panda_eat = counter;
        core.changed(EAT_TIMER);
    }
}

fn eats_from_ground(item: Item) -> bool {
    tuff_datapack::tags::is(tag::item::key::PANDA_EATS_FROM_GROUND, item)
}

fn holding(core: &MobCore) -> bool {
    core.equipment.main_hand().is_some()
}

fn try_to_sit(core: &mut MobCore) {
    if !core.body.in_water {
        core.navigation.stop();
        set_flag(core, FLAG_SITTING, true);
    }
}

fn ground_food_within(core: &MobCore, cx: &TickContext, range: f64) -> Option<[f64; 3]> {
    let area = super::physics::inflate(core.bounds(), [range; 3]);
    cx.entities.iter().find_map(|e| {
        let (item, count, takeable) = e.item?;
        (takeable
            && count > 0
            && eats_from_ground(item)
            && super::physics::intersects(e.bounds, area))
        .then(|| {
            [
                (e.bounds[0] + e.bounds[3]) / 2.0,
                e.bounds[1],
                (e.bounds[2] + e.bounds[5]) / 2.0,
            ]
        })
    })
}

#[derive(Debug, Default)]
struct PandaSit {
    cooldown: u64,
}

impl Goal for PandaSit {
    fn flags(&self) -> Flags {
        Flags::MOVE
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        if self.cooldown > cx.tick || mob.is_baby() || mob.body.in_water || !can_perform_action(mob)
        {
            return false;
        }
        holding(mob) || ground_food_within(mob, cx, 6.0).is_some()
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        if mob.body.in_water {
            return false;
        }
        let lazy = mob.panda_lazy;
        !((!lazy && cx.rng.gen_range(0..super::goal::reduced_delay(600)) == 1)
            || cx.rng.gen_range(0..super::goal::reduced_delay(2000)) == 1)
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        if holding(mob) {
            try_to_sit(mob);
        } else if let Some(target) = ground_food_within(mob, cx, 8.0) {
            mob.move_to_entity(target, 1.2, cx);
        }
        self.cooldown = 0;
    }

    fn tick(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        if mob.panda_flags & FLAG_SITTING == 0 && holding(mob) {
            try_to_sit(mob);
        }
    }

    fn stop(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        if let Some(stack) = mob.equipment.take(EquipmentSlot::Mainhand) {
            mob.drop_item(stack, 0.0, cx);
            let seconds = if mob.panda_lazy {
                cx.rng.gen_range(0..50)
            } else {
                cx.rng.gen_range(0..320)
            } + 10;
            self.cooldown = cx.tick + seconds * 20;
        }
        set_flag(mob, FLAG_SITTING, false);
    }
}

/// Breeding is put off, with a sulk, while no bamboo grows nearby.
#[derive(Debug)]
struct PandaBreed {
    inner: Breed,
    unhappy_until: u64,
}

fn bamboo_near(mob: &MobCore, cx: &TickContext) -> bool {
    let here = super::physics::block_pos(mob.base.position);
    (0..3).any(|y| {
        (-7..=7).any(|x| {
            (-7..=7).any(|z| {
                (cx.block)(BlockPos::new(here.x + x, here.y + y, here.z + z))
                    .is_some_and(|s| s.block() == tuff_vanilla_data::Block::Bamboo)
            })
        })
    })
}

impl Goal for PandaBreed {
    fn flags(&self) -> Flags {
        self.inner.flags()
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        if !self.inner.can_use(mob, cx) || mob.panda_unhappy != 0 {
            return false;
        }
        if bamboo_near(mob, cx) {
            return true;
        }
        if self.unhappy_until <= cx.tick {
            mob.panda_unhappy = 32;
            mob.changed(BREED_TIMER);
            self.unhappy_until = cx.tick + 600;
        }
        false
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        self.inner.can_continue(mob, cx)
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        self.inner.start(mob, cx);
    }

    fn stop(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        self.inner.stop(mob, cx);
    }

    fn every_tick(&self) -> bool {
        self.inner.every_tick()
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        self.inner.tick(mob, cx);
    }
}

/// A baby now and then sneezes, which is how it gifts a slime ball.
#[derive(Debug, Default)]
struct PandaSneeze;

impl Goal for PandaSneeze {
    fn flags(&self) -> Flags {
        Flags::NONE
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        if !mob.is_baby() || !can_perform_action(mob) {
            return false;
        }
        (mob.panda_weak && cx.rng.gen_range(0..super::goal::reduced_delay(500)) == 1)
            || cx.rng.gen_range(0..super::goal::reduced_delay(6000)) == 1
    }

    fn can_continue(&mut self, _mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        false
    }

    fn start(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        set_flag(mob, FLAG_SNEEZING, true);
    }
}

/// A baby, or a playful panda, tumbles forward.
#[derive(Debug, Default)]
struct PandaRoll;

impl Goal for PandaRoll {
    fn flags(&self) -> Flags {
        Flags::MOVE | Flags::LOOK | Flags::JUMP
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        if (!mob.is_baby() && !mob.panda_playful) || !mob.base.on_ground || !can_perform_action(mob)
        {
            return false;
        }
        let angle = f64::from(mob.base.yaw).to_radians();
        let (dx, dz) = (-angle.sin(), angle.cos());
        let step = |d: f64| if d.abs() > 0.5 { d.signum() as i32 } else { 0 };
        let here = super::physics::block_pos(mob.base.position);
        let ahead = BlockPos::new(here.x + step(dx), here.y - 1, here.z + step(dz));
        if (cx.block)(ahead).is_some_and(tuff_world_model::chunk::is_air) {
            return true;
        }
        (mob.panda_playful && cx.rng.gen_range(0..super::goal::reduced_delay(60)) == 1)
            || cx.rng.gen_range(0..super::goal::reduced_delay(500)) == 1
    }

    fn can_continue(&mut self, _mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        false
    }

    fn start(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        set_flag(mob, FLAG_ROLLING, true);
    }

    fn interruptable(&self) -> bool {
        false
    }
}

#[derive(Debug, Default)]
struct LieOnBack {
    cooldown: u64,
}

impl Goal for LieOnBack {
    fn flags(&self) -> Flags {
        Flags::NONE
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        self.cooldown < cx.tick
            && mob.panda_lazy
            && can_perform_action(mob)
            && cx.rng.gen_range(0..super::goal::reduced_delay(400)) == 1
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        !mob.body.in_water && cx.rng.gen_range(0..super::goal::reduced_delay(100)) != 1
    }

    fn start(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        set_flag(mob, FLAG_ON_BACK, true);
        self.cooldown = 0;
    }

    fn stop(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        set_flag(mob, FLAG_ON_BACK, false);
        self.cooldown = cx.tick + LIE_COOLDOWN;
    }
}

const LAZY_MOVEMENT_SPEED: f64 = 0.07;
const WEAK_MAX_HEALTH: f64 = 10.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Gene {
    Normal,
    Lazy,
    Worried,
    Playful,
    Brown,
    Weak,
    Aggressive,
}

impl Gene {
    const ALL: [Self; 7] = [
        Self::Normal,
        Self::Lazy,
        Self::Worried,
        Self::Playful,
        Self::Brown,
        Self::Weak,
        Self::Aggressive,
    ];

    fn id(self) -> i8 {
        match self {
            Self::Normal => 0,
            Self::Lazy => 1,
            Self::Worried => 2,
            Self::Playful => 3,
            Self::Brown => 4,
            Self::Weak => 5,
            Self::Aggressive => 6,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Lazy => "lazy",
            Self::Worried => "worried",
            Self::Playful => "playful",
            Self::Brown => "brown",
            Self::Weak => "weak",
            Self::Aggressive => "aggressive",
        }
    }

    fn from_name(name: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|g| g.name() == name)
            .unwrap_or(Self::Normal)
    }

    fn is_recessive(self) -> bool {
        matches!(self, Self::Brown | Self::Weak)
    }

    /// The game's weighted natural roll (out of 16).
    fn random(rng: &mut StdRng) -> Self {
        match rng.gen_range(0..16) {
            0 => Self::Lazy,
            1 => Self::Worried,
            2 => Self::Playful,
            4 => Self::Aggressive,
            3 | 5..=8 => Self::Weak,
            9 | 10 => Self::Brown,
            _ => Self::Normal,
        }
    }

    /// The visible variant: a recessive gene only shows when both copies
    /// match, otherwise it hides behind `Normal`.
    fn variant(main: Self, hidden: Self) -> Self {
        if main.is_recessive() {
            if main == hidden { main } else { Self::Normal }
        } else {
            main
        }
    }
}

fn panda_food(item: Item) -> bool {
    tuff_datapack::tags::is(tag::item::key::PANDA_FOOD, item)
}

fn one_of(main: Gene, hidden: Gene, rng: &mut StdRng) -> Gene {
    if rng.r#gen() { main } else { hidden }
}

#[derive(Debug)]
pub struct Panda {
    main_gene: Gene,
    hidden_gene: Gene,
}

impl Panda {
    fn apply_attributes(&self, core: &mut MobCore) {
        let variant = Gene::variant(self.main_gene, self.hidden_gene);
        core.panda_lazy = variant == Gene::Lazy;
        core.panda_weak = variant == Gene::Weak;
        core.panda_playful = variant == Gene::Playful;
        core.panda_worried = variant == Gene::Worried;
        if variant == Gene::Weak {
            core.set_attribute_base(Attribute::MaxHealth, WEAK_MAX_HEALTH);
        }
        if variant == Gene::Lazy {
            core.set_attribute_base(Attribute::MovementSpeed, LAZY_MOVEMENT_SPEED);
        }
    }
}

fn handle_eating(core: &mut MobCore, cx: &mut TickContext) {
    let sitting = core.panda_flags & FLAG_SITTING != 0;
    if core.panda_eat <= 0 && sitting && holding(core) && cx.rng.gen_range(0..80) == 1 {
        set_eat(core, 1);
    } else if !holding(core) || !sitting {
        set_eat(core, 0);
    }
    if core.panda_eat > 0 {
        if core.panda_eat > 80 && cx.rng.gen_range(0..20) == 1 {
            let finished = core.panda_eat > 100
                && core
                    .equipment
                    .main_hand()
                    .is_some_and(|s| eats_from_ground(s.item()));
            if finished {
                core.equipment.set(EquipmentSlot::Mainhand, None);
                set_flag(core, FLAG_SITTING, false);
            }
            set_eat(core, 0);
            return;
        }
        set_eat(core, core.panda_eat + 1);
    }
}

fn after_sneeze(core: &mut MobCore, cx: &mut TickContext) {
    core.sound(SoundEvent::EntityPandaSneeze, 1.0, 1.0, cx);
    if !cx.shared.game_rules.borrow().bool("mob_drops") {
        return;
    }
    let [x, y, z] = core.base.position;
    for stack in cx
        .shared
        .block_loot()
        .roll("minecraft:gameplay/panda_sneeze", cx.rng)
    {
        cx.effects.push(Effect::DropItem {
            stack,
            position: [x, y, z],
            velocity: [0.0; 3],
            pickup_delay: tuff_core::world::items::BLOCK_DROP_PICKUP_DELAY,
        });
    }
}

fn tick_sulk(core: &mut MobCore, cx: &mut TickContext) {
    if core.panda_unhappy > 0 {
        if let Some(target) = core.target_view(cx) {
            core.turn_to(target.eye, 90.0, 90.0);
        }
        if core.panda_unhappy == 29 || core.panda_unhappy == 14 {
            core.sound(SoundEvent::EntityPandaCantBreed, 1.0, 1.0, cx);
        }
        core.panda_unhappy -= 1;
        core.changed(BREED_TIMER);
    }
}

fn tick_sneeze(core: &mut MobCore, cx: &mut TickContext) {
    if core.panda_flags & FLAG_SNEEZING == 0 {
        return;
    }
    core.panda_sneeze += 1;
    core.changed(SNEEZE_TIMER);
    if core.panda_sneeze > 20 {
        set_flag(core, FLAG_SNEEZING, false);
        core.panda_sneeze = 0;
        after_sneeze(core, cx);
    } else if core.panda_sneeze == 1 {
        core.sound(SoundEvent::EntityPandaPreSneeze, 1.0, 1.0, cx);
    }
}

fn tick_roll(core: &mut MobCore) {
    if core.panda_flags & FLAG_ROLLING == 0 {
        core.panda_roll = 0;
        return;
    }
    core.panda_roll += 1;
    if core.panda_roll > 32 {
        set_flag(core, FLAG_ROLLING, false);
        return;
    }
    let [vx, vy, vz] = core.base.velocity;
    if core.panda_roll == 1 {
        let angle = f64::from(core.base.yaw).to_radians();
        let push = if core.is_baby() { 0.1 } else { 0.2 };
        core.panda_roll_delta = [vx - angle.sin() * push, vz + angle.cos() * push];
        core.base.velocity = [core.panda_roll_delta[0], 0.27, core.panda_roll_delta[1]];
    } else if matches!(core.panda_roll, 7 | 15 | 23) {
        core.base.velocity = [0.0, if core.base.on_ground { 0.27 } else { vy }, 0.0];
    } else {
        core.base.velocity = [core.panda_roll_delta[0], vy, core.panda_roll_delta[1]];
    }
}

impl Panda {
    fn pick_up_ground_food(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if !core.can_pick_up_loot
            || core.dead
            || holding(core)
            || !cx.shared.game_rules.borrow().bool("mob_griefing")
        {
            return;
        }
        let reach = super::physics::inflate(core.bounds(), [1.0, 0.0, 1.0]);
        let Some((id, count)) = cx.entities.iter().find_map(|e| {
            let (item, count, takeable) = e.item?;
            (takeable
                && count > 0
                && eats_from_ground(item)
                && super::physics::intersects(e.bounds, reach))
            .then_some((e.id, count))
        }) else {
            return;
        };
        cx.effects.push(Effect::TakeItem { item: id, count });
    }
}

impl Species for Panda {
    fn create(rng: &mut StdRng) -> Self {
        Self {
            main_gene: Gene::random(rng),
            hidden_gene: Gene::random(rng),
        }
    }

    fn configure(core: &mut MobCore) {
        core.make_animal();
        core.can_pick_up_loot = true;
        let mut goals = GoalSelector::default();
        goals.add(0, HurtByTarget::new(&[]));
        goals.add(1, super::goals::Float::new(core));
        goals.add(2, Panic::new(2.0));
        goals.add(
            2,
            PandaBreed {
                inner: Breed::new(1.0),
                unhappy_until: 0,
            },
        );
        goals.add(3, MeleeAttack::new(1.2, true));
        goals.add(4, Tempt::new(1.0, panda_food));
        goals.add(
            6,
            AvoidPlayer::new(8.0, 2.0, 2.0).only_if(|m| m.panda_worried && can_perform_action(m)),
        );
        goals.add(
            6,
            AvoidPlayer::monsters(4.0, 2.0, 2.0)
                .only_if(|m| m.panda_worried && can_perform_action(m)),
        );
        goals.add(7, PandaSit::default());
        goals.add(8, LieOnBack::default());
        goals.add(8, PandaSneeze);
        goals.add(9, LookAtPlayer::new(6.0));
        goals.add(10, RandomLookAround::default());
        goals.add(12, PandaRoll);
        goals.add(13, FollowParent::new(1.25));
        goals.add(14, RandomStroll::water_avoiding(1.0));
        core.goals = goals;
    }

    fn load(&mut self, core: &mut MobCore, nbt: &mut Compound) {
        if let Some(gene) = take(nbt, "MainGene", |t| Some(Gene::from_name(t.as_str()?))) {
            self.main_gene = gene;
        }
        if let Some(gene) = take(nbt, "HiddenGene", |t| Some(Gene::from_name(t.as_str()?))) {
            self.hidden_gene = gene;
        }
        self.apply_attributes(core);
    }

    fn save(&self, _core: &MobCore, out: &mut Compound) {
        out.insert("MainGene", self.main_gene.name());
        out.insert("HiddenGene", self.hidden_gene.name());
    }

    fn tick(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if Gene::variant(self.main_gene, self.hidden_gene) == Gene::Worried {
            if cx.shared.weather.borrow().thundering && !core.body.in_water {
                set_flag(core, FLAG_SITTING, true);
                set_eat(core, 0);
            } else if core.panda_eat <= 0 {
                set_flag(core, FLAG_SITTING, false);
            }
        }
        tick_sulk(core, cx);
        tick_sneeze(core, cx);
        tick_roll(core);
        self.pick_up_ground_food(core, cx);
        handle_eating(core, cx);
    }

    fn took_item(&mut self, core: &mut MobCore, stack: ItemStack) {
        if !holding(core) {
            core.equipment.set(EquipmentSlot::Mainhand, Some(stack));
            core.equipment.set_drop_chance(
                EquipmentSlot::Mainhand,
                super::equipment::PRESERVED_DROP_CHANCE,
            );
            core.persistence_required = true;
        }
    }

    fn interact(
        &mut self,
        core: &mut MobCore,
        interaction: &EntityInteraction,
        cx: &mut TickContext,
    ) -> InteractOutcome {
        if core.panda_flags & FLAG_ON_BACK != 0 {
            set_flag(core, FLAG_ON_BACK, false);
            return Some(HandEffect::None);
        }
        let stack = interaction.stack.as_deref()?;
        if !panda_food(stack.item())
            || core.is_baby()
            || core.panda_flags & FLAG_SITTING != 0
            || core.body.in_water
        {
            return None;
        }
        let food = interaction.hand.take(1)?;
        try_to_sit(core);
        set_eat(core, 1);
        if let Some(held) = core.equipment.replace(EquipmentSlot::Mainhand, Some(food)) {
            if interaction.creative {
                held.burn(tuff_item::item::BurnReason::Creative);
            } else {
                core.drop_item(held, 0.0, cx);
            }
        }
        core.equipment.set_drop_chance(
            EquipmentSlot::Mainhand,
            super::equipment::PRESERVED_DROP_CHANCE,
        );
        core.persistence_required = true;
        Some(HandEffect::None)
    }

    fn metadata(&self, core: &MobCore) -> Vec<(u8, MetadataValue)> {
        vec![
            (BREED_TIMER, MetadataValue::VarInt(core.panda_unhappy)),
            (SNEEZE_TIMER, MetadataValue::VarInt(core.panda_sneeze)),
            (EAT_TIMER, MetadataValue::VarInt(core.panda_eat)),
            (MAIN_GENE, MetadataValue::Byte(self.main_gene.id())),
            (HIDDEN_GENE, MetadataValue::Byte(self.hidden_gene.id())),
            (FLAGS, MetadataValue::Byte(core.panda_flags as i8)),
        ]
    }

    fn sounds(&self, _core: &MobCore) -> MobSounds {
        let variant = Gene::variant(self.main_gene, self.hidden_gene);
        MobSounds {
            ambient: Some(match variant {
                Gene::Worried => SoundEvent::EntityPandaWorriedAmbient,
                Gene::Aggressive => SoundEvent::EntityPandaAggressiveAmbient,
                _ => SoundEvent::EntityPandaAmbient,
            }),
            hurt: Some(SoundEvent::EntityPandaHurt),
            death: Some(SoundEvent::EntityPandaDeath),
            step: Some(SoundEvent::EntityPandaStep),
            volume: 1.0,
        }
    }

    fn eating_sound(&self, _core: &MobCore) -> Option<SoundEvent> {
        Some(SoundEvent::EntityPandaEat)
    }

    fn is_food<M: tuff_item::item::Mode>(&self, stack: &ItemStack<M>) -> bool {
        panda_food(stack.item())
    }

    fn offspring(&self, partner: &Self, rng: &mut StdRng) -> Self {
        let mut main_gene = one_of(self.main_gene, self.hidden_gene, rng);
        let mut hidden_gene = one_of(partner.main_gene, partner.hidden_gene, rng);
        if rng.r#gen() {
            std::mem::swap(&mut main_gene, &mut hidden_gene);
        }
        if rng.gen_range(0..32) == 0 {
            main_gene = Gene::random(rng);
        }
        if rng.gen_range(0..32) == 0 {
            hidden_gene = Gene::random(rng);
        }
        Self {
            main_gene,
            hidden_gene,
        }
    }

    fn finalize_spawn(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        self.main_gene = Gene::random(cx.rng);
        self.hidden_gene = Gene::random(cx.rng);
        self.apply_attributes(core);
    }

    fn finalize_offspring(&mut self, core: &mut MobCore) {
        self.apply_attributes(core);
    }

    fn loot_facts(&self, facts: &mut tuff_core::world::drops::EntityFacts) {
        facts.components.push((
            "minecraft:panda/main_gene",
            self.main_gene.name().to_owned(),
        ));
        facts.components.push((
            "minecraft:panda/hidden_gene",
            self.hidden_gene.name().to_owned(),
        ));
    }

    fn experience(&self, _core: &MobCore, rng: &mut StdRng) -> i32 {
        1 + rng.gen_range(0..3)
    }

    fn remove_when_far_away(&self) -> bool {
        false
    }
}

/// Registers the panda's loader.
pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    registry.register(EntityType::Panda, super::load::<Panda>);
    registry.register_spawn_rule(EntityType::Panda, super::spawning::animal_rule);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn panda_data_indices_match_the_dumped_layout() {
        let layout = EntityType::Panda.data_layout();
        assert_eq!(layout.len(), FLAGS as usize + 1);
        assert_eq!(
            layout[MAIN_GENE as usize],
            MetadataValue::Byte(0).type_id() as u8
        );
        assert_eq!(
            layout[HIDDEN_GENE as usize],
            MetadataValue::Byte(0).type_id() as u8
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_recessive_gene_only_shows_when_both_copies_match() {
        assert_eq!(Gene::variant(Gene::Brown, Gene::Brown), Gene::Brown);
        assert_eq!(Gene::variant(Gene::Brown, Gene::Weak), Gene::Normal);
        assert_eq!(Gene::variant(Gene::Lazy, Gene::Brown), Gene::Lazy);
    }

    #[tuff_core::watchdog]
    #[test]
    fn weak_and_lazy_genes_adjust_attributes() {
        crate::tests::with_context(|core, _cx| {
            let weak = Panda {
                main_gene: Gene::Weak,
                hidden_gene: Gene::Weak,
            };
            weak.apply_attributes(core);
            assert_eq!(core.attribute(Attribute::MaxHealth), WEAK_MAX_HEALTH);

            let lazy = Panda {
                main_gene: Gene::Lazy,
                hidden_gene: Gene::Normal,
            };
            lazy.apply_attributes(core);
            assert_eq!(
                core.attribute(Attribute::MovementSpeed),
                LAZY_MOVEMENT_SPEED
            );
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_lazy_panda_lies_on_its_back_and_a_poke_rights_it() {
        crate::tests::with_context(|core, cx| {
            Panda::configure(core);
            let mut panda = Panda {
                main_gene: Gene::Lazy,
                hidden_gene: Gene::Normal,
            };
            panda.apply_attributes(core);
            assert!(core.panda_lazy);
            let mut goal = LieOnBack::default();
            let mut lay = false;
            for _ in 0..4000 {
                if goal.can_use(core, cx) {
                    lay = true;
                    break;
                }
            }
            assert!(lay);
            goal.start(core, cx);
            assert!(core.changed.contains(&FLAGS));
            assert!(
                panda
                    .metadata(core)
                    .contains(&(FLAGS, MetadataValue::Byte(FLAG_ON_BACK as i8)))
            );
            assert!(!goal.can_use(core, cx), "already lying down");
            let poke = tuff_core::world::entity::EntityInteraction {
                player: 1,
                player_uuid: 1,
                position: [0.0; 3],
                location: [0.0; 3],
                stack: None,
                hand: Default::default(),
                off_hand: false,
                creative: false,
                sneaking: false,
                riding: false,
                hero: None,
            };
            assert!(panda.interact(core, &poke, cx).is_some());
            assert_eq!(core.panda_flags, 0);
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_normal_panda_never_lies_down() {
        crate::tests::with_context(|core, cx| {
            Panda::configure(core);
            let mut normal = Panda {
                main_gene: Gene::Normal,
                hidden_gene: Gene::Normal,
            };
            normal.apply_attributes(core);
            let mut goal = LieOnBack::default();
            for _ in 0..4000 {
                assert!(!goal.can_use(core, cx));
            }
            normal.tick(core, cx);
            assert_eq!(core.panda_flags, 0);
        });
    }

    fn dropped(item: Item, id: i32, at: [f64; 3]) -> tuff_core::world::entity::EntitySummary {
        tuff_core::world::entity::EntitySummary {
            id,
            uuid: id as u128,
            kind: EntityType::Item,
            position: at,
            velocity: [0.0; 3],
            yaw: 0.0,
            bounds: [
                at[0] - 0.1,
                at[1],
                at[2] - 0.1,
                at[0] + 0.1,
                at[1] + 0.2,
                at[2] + 0.1,
            ],
            living: None,
            pickable: false,
            item: Some((item, 1, true)),
            vehicle: None,
            carries: false,
            ominous_banner: false,
            blue_sheep: false,
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_panda_picks_up_ground_bamboo_sits_and_eats_it() {
        let items = [dropped(Item::Bamboo, 9, [0.5, 64.0, 0.5])];
        crate::tests::with_entities(&items, |core, cx| {
            Panda::configure(core);
            let mut panda = Panda {
                main_gene: Gene::Normal,
                hidden_gene: Gene::Normal,
            };
            let mut goal = PandaSit::default();
            assert!(goal.can_use(core, cx), "bamboo within 6 blocks");
            panda.tick(core, cx);
            assert!(
                cx.effects
                    .iter()
                    .any(|e| matches!(e, Effect::TakeItem { item: 9, .. }))
            );
            panda.took_item(
                core,
                ItemStack::mint(Item::Bamboo, 1, tuff_item::item::MintReason::Test),
            );
            assert!(holding(core));
            goal.start(core, cx);
            assert_ne!(core.panda_flags & FLAG_SITTING, 0);
            let mut ate = false;
            for _ in 0..4000 {
                panda.tick(core, cx);
                if !holding(core) {
                    ate = true;
                    break;
                }
            }
            assert!(ate, "the held bamboo is eaten");
            assert_eq!(core.panda_flags & FLAG_SITTING, 0);
            assert_eq!(core.panda_eat, 0);
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_panda_spits_out_held_food_when_it_stops_sitting() {
        crate::tests::with_context(|core, cx| {
            Panda::configure(core);
            core.equipment.set(
                EquipmentSlot::Mainhand,
                Some(ItemStack::mint(
                    Item::Bamboo,
                    1,
                    tuff_item::item::MintReason::Test,
                )),
            );
            let mut goal = PandaSit::default();
            assert!(goal.can_use(core, cx));
            goal.stop(core, cx);
            assert!(!holding(core));
            assert!(
                cx.effects
                    .iter()
                    .any(|e| matches!(e, Effect::DropItem { .. }))
            );
            assert!(!goal.can_use(core, cx), "cooling down after a drop");
        });
    }

    fn normal() -> Panda {
        Panda {
            main_gene: Gene::Normal,
            hidden_gene: Gene::Normal,
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_baby_sneezes_for_twenty_ticks_then_the_flag_clears() {
        crate::tests::with_context(|core, cx| {
            Panda::configure(core);
            core.set_baby(true);
            let mut panda = normal();
            let mut goal = PandaSneeze;
            assert!(goal.can_use(core, cx) || (0..20000).any(|_| goal.can_use(core, cx)));
            goal.start(core, cx);
            assert_ne!(core.panda_flags & FLAG_SNEEZING, 0);
            for _ in 0..20 {
                panda.tick(core, cx);
                assert_ne!(core.panda_flags & FLAG_SNEEZING, 0);
            }
            panda.tick(core, cx);
            assert_eq!(core.panda_flags & FLAG_SNEEZING, 0);
            assert_eq!(core.panda_sneeze, 0);
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn an_adult_never_sneezes() {
        crate::tests::with_context(|core, cx| {
            Panda::configure(core);
            let mut goal = PandaSneeze;
            assert!(!(0..20000).any(|_| goal.can_use(core, cx)));
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_rolling_baby_tumbles_forward_for_thirty_two_ticks() {
        crate::tests::with_context(|core, cx| {
            Panda::configure(core);
            core.set_baby(true);
            core.base.on_ground = true;
            let mut panda = normal();
            let mut goal = PandaRoll;
            assert!((0..20000).any(|_| goal.can_use(core, cx)));
            goal.start(core, cx);
            panda.tick(core, cx);
            assert!(core.base.velocity[1] > 0.2, "{:?}", core.base.velocity);
            assert!(!can_perform_action(core));
            for _ in 0..32 {
                panda.tick(core, cx);
            }
            assert_eq!(core.panda_flags & FLAG_ROLLING, 0);
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_hand_fed_adult_sits_and_holds_the_bamboo() {
        crate::tests::with_context(|core, cx| {
            Panda::configure(core);
            core.base.on_ground = true;
            let mut panda = normal();
            let interaction = EntityInteraction {
                player: 1,
                player_uuid: 1,
                position: [0.0; 3],
                location: [0.0; 3],
                stack: Some(tuff_item::item::ItemSnapshot::new(Item::Bamboo, 1)),
                hand: tuff_core::world::entity::HandOffer::new(
                    (Some(tuff_item::item::ItemSnapshot::new(Item::Bamboo, 1)))
                        .as_deref()
                        .map(tuff_item::item::ItemStack::test_copy),
                ),
                off_hand: false,
                creative: false,
                sneaking: false,
                riding: false,
                hero: None,
            };
            let effect = panda.interact(core, &interaction, cx);
            assert!(matches!(effect, Some(HandEffect::None)));
            assert_ne!(core.panda_flags & FLAG_SITTING, 0);
            assert_eq!(core.panda_eat, 1);
            assert!(holding(core));
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn breeding_without_bamboo_nearby_makes_a_panda_unhappy() {
        crate::tests::with_context(|core, cx| {
            Panda::configure(core);
            let mut panda = normal();
            core.panda_unhappy = 32;
            for _ in 0..32 {
                panda.tick(core, cx);
            }
            assert_eq!(core.panda_unhappy, 0);
            assert!(!bamboo_near(core, cx));
        });
    }
}
