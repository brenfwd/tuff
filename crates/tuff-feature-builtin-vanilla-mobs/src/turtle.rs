//! Turtles: amphibious grazers bound to the beach they hatched on. Bred
//! turtles carry eggs home and lay them in the sand; out at sea they swim
//! long trips and now and then head back home. Babies drop a scute when
//! they grow up.

use rand::Rng;
use rand::rngs::StdRng;
use tuff_vanilla_data::entity::PathType;
use tuff_vanilla_data::registry::{Attribute, EntityType, SoundEvent};
use tuff_vanilla_data::{Block, Item, tag};

use super::control::MoveStyle;
use super::goal::{Flags, Goal, GoalSelector};
use super::goals::{Breed, LookAtPlayer, MoveToBlock, Panic, RandomStroll, Tempt};
use super::movement::WaterTravel;
use super::navigation::NavKind;
use super::physics::block_pos;
use super::{Mob, MobCore, MobSounds, Species, data, random_pos};
use tuff_core::protocol::play::{self, MetadataValue};
use tuff_core::world::entity::{Effect, RegionEntity, TickContext, take, take_bool};
use tuff_core::world::{framed, packets};
use tuff_item::item::ItemStack;
use tuff_nbt::{Compound, Tag};
use tuff_world_model::BlockPos;

const HAS_EGG: u8 = data::SPECIES;
const LAYING_EGG: u8 = data::SPECIES + 1;
/// `MobCore::signal` bits its goals share: carrying eggs, and laying them.
const SIGNAL_HAS_EGG: i32 = 1;
const SIGNAL_LAYING: i32 = 2;
const LAY_TICKS: i32 = 200;
const NARROW: f64 = 0.314_159_274_101_257_3;
const WIDE: f64 = std::f64::consts::FRAC_PI_2;
const GROW_TABLE: &str = "minecraft:gameplay/turtle_grow";

fn turtle_food(item: Item) -> bool {
    tuff_datapack::tags::is(tag::item::key::TURTLE_FOOD, item)
}

fn has_egg(mob: &MobCore) -> bool {
    mob.signal & SIGNAL_HAS_EGG != 0
}

fn set_signal(mob: &mut MobCore, bit: i32, on: bool) {
    let index = if bit == SIGNAL_HAS_EGG {
        HAS_EGG
    } else {
        LAYING_EGG
    };
    if (mob.signal & bit != 0) != on {
        mob.signal ^= bit;
        mob.changed(index);
    }
}

fn home(mob: &MobCore) -> BlockPos {
    mob.bound_origin
        .unwrap_or_else(|| block_pos(mob.base.position))
}

fn home_bottom(mob: &MobCore) -> [f64; 3] {
    let home = home(mob);
    [
        f64::from(home.x) + 0.5,
        f64::from(home.y),
        f64::from(home.z) + 0.5,
    ]
}

fn sand(cx: &TickContext, pos: BlockPos) -> bool {
    (cx.block)(pos).is_some_and(|s| tuff_datapack::tags::is(tag::block::key::SAND, s.block()))
}

fn water_block(cx: &TickContext, pos: BlockPos) -> bool {
    (cx.block)(pos).is_some_and(|s| s.block() == Block::Water)
}

/// A step towards `target`: within a narrow cone if it can, else wider.
fn step_towards(mob: &MobCore, target: [f64; 3], cx: &mut TickContext) -> Option<[f64; 3]> {
    random_pos::pos_towards_within(mob, 16, 3, target, NARROW, cx)
        .or_else(|| random_pos::pos_towards_within(mob, 8, 7, target, WIDE, cx))
}

/// Carrying eggs, or now and then when far from home, swims and walks
/// back to its home beach.
#[derive(Debug, Default)]
struct GoHome {
    stuck: bool,
    close_ticks: i32,
}

impl Goal for GoHome {
    fn flags(&self) -> Flags {
        Flags::NONE
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        if mob.is_baby() {
            return false;
        }
        if has_egg(mob) {
            return true;
        }
        cx.rng.gen_range(0..super::goal::reduced_delay(700)) == 0 && !mob.near_home(64.0)
    }

    fn start(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        mob.homeward = true;
        self.stuck = false;
        self.close_ticks = 0;
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        mob.homeward = false;
    }

    fn can_continue(&mut self, mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        !(mob.near_home(7.0) || self.stuck || self.close_ticks > self.adjusted_delay(600))
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let close = mob.near_home(16.0);
        if close {
            self.close_ticks += 1;
        }
        if !mob.navigation.is_done() {
            return;
        }
        let target = home_bottom(mob);
        let mut next = step_towards(mob, target, cx);
        if next.is_some_and(|n| !close && !water_block(cx, block_pos(n))) {
            next = random_pos::pos_towards_within(mob, 16, 5, target, WIDE, cx);
        }
        match next {
            Some(next) => {
                mob.move_to_position(next, 1.0, cx);
            }
            None => self.stuck = true,
        }
    }
}

/// Out at sea with nothing else to do, swims a long way off.
#[derive(Debug, Default)]
struct Travel {
    target: Option<[f64; 3]>,
    stuck: bool,
}

impl Goal for Travel {
    fn flags(&self) -> Flags {
        Flags::NONE
    }

    fn can_use(&mut self, mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        !mob.homeward && !has_egg(mob) && mob.body.in_water
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let x = cx.rng.gen_range(0..1025) - 512;
        let mut y = cx.rng.gen_range(0..9) - 4;
        let z = cx.rng.gen_range(0..1025) - 512;
        let sea_level = cx.shared.dimension(cx.dimension).sea_level();
        if f64::from(y) + mob.base.position[1] > f64::from(sea_level - 1) {
            y = 0;
        }
        let [px, py, pz] = mob.base.position;
        let target = block_pos([px + f64::from(x), py + f64::from(y), pz + f64::from(z)]);
        self.target = Some([
            f64::from(target.x) + 0.5,
            f64::from(target.y),
            f64::from(target.z) + 0.5,
        ]);
        self.stuck = false;
        mob.navigation.water_destinations = true;
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let Some(target) = self.target else {
            self.stuck = true;
            return;
        };
        if !mob.navigation.is_done() {
            return;
        }
        let next = step_towards(mob, target, cx).filter(|next| {
            let at = block_pos(*next);
            [(-34, -34), (34, -34), (-34, 34), (34, 34)]
                .iter()
                .all(|(dx, dz)| (cx.block)(BlockPos::new(at.x + dx, at.y, at.z + dz)).is_some())
        });
        match next {
            Some(next) => {
                mob.move_to_position(next, 1.0, cx);
            }
            None => self.stuck = true,
        }
    }

    fn can_continue(&mut self, mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        !(mob.navigation.is_done()
            || self.stuck
            || mob.homeward
            || mob.love.as_ref().is_some_and(|l| l.is_in_love())
            || has_egg(mob))
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        self.target = None;
        mob.navigation.water_destinations = false;
    }
}

fn nest_spot(_mob: &MobCore, pos: BlockPos, cx: &TickContext) -> bool {
    (cx.block)(BlockPos::new(pos.x, pos.y + 1, pos.z)).is_some_and(tuff_world_model::chunk::is_air)
        && sand(cx, pos)
}

/// Back home with eggs, digs into the sand and lays them.
#[derive(Debug)]
struct LayEgg {
    seek: MoveToBlock,
    counter: i32,
}

impl LayEgg {
    fn new() -> Self {
        Self {
            seek: MoveToBlock::new(1.0, 16, 1, nest_spot),
            counter: 0,
        }
    }

    fn lay(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let pos = BlockPos::new(self.seek.pos.x, self.seek.pos.y + 1, self.seek.pos.z);
        let pitch = 0.9 + cx.rng.r#gen::<f32>() * 0.2;
        cx.effects
            .push(Effect::ChunkPackets(packets([framed(&play::Sound {
                sound_event: SoundEvent::EntityTurtleLayEgg.id(),
                category: play::sound_category::BLOCKS,
                position: block_pos(mob.base.position).center(),
                volume: 0.3,
                pitch,
                seed: cx.rng.r#gen(),
            })])));
        let eggs = cx.rng.gen_range(0..4) + 1;
        if let Some(state) = Block::TurtleEgg.default_state().with_eggs(eggs) {
            cx.effects.push(Effect::SetBlock { pos, state });
        }
        set_signal(mob, SIGNAL_HAS_EGG, false);
        set_signal(mob, SIGNAL_LAYING, false);
        self.counter = 0;
        if let Some(love) = &mut mob.love {
            love.ticks = super::animal::LOVE_TICKS;
        }
    }
}

impl Goal for LayEgg {
    fn flags(&self) -> Flags {
        self.seek.flags()
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        has_egg(mob) && mob.near_home(9.0) && self.seek.can_use(mob, cx)
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        self.seek.can_continue(mob, cx) && has_egg(mob) && mob.near_home(9.0)
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        self.seek.start(mob, cx);
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        set_signal(mob, SIGNAL_LAYING, false);
        self.counter = 0;
    }

    fn every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        self.seek.tick(mob, cx);
        if mob.body.in_water || !self.seek.reached() {
            return;
        }
        if self.counter < 1 {
            set_signal(mob, SIGNAL_LAYING, true);
            self.counter = 1;
        } else if self.counter > LAY_TICKS {
            self.lay(mob, cx);
        }
        if mob.signal & SIGNAL_LAYING != 0 {
            self.counter += 1;
            let here = block_pos(mob.base.position);
            let below = BlockPos::new(here.x, here.y - 1, here.z);
            if self.counter % 5 == 0
                && sand(cx, below)
                && let Some(state) = (cx.block)(below)
            {
                let packet = framed(&play::LevelEvent {
                    event: play::level_event::BLOCK_BREAK,
                    position: here,
                    data: state.id() as i32,
                    global: false,
                });
                cx.effects.push(Effect::ChunkPackets(packets([packet])));
            }
        }
    }
}

/// Out of the water (unless heading home with eggs), makes for the
/// nearest water.
#[derive(Debug)]
struct GoToWater(MoveToBlock);

fn water_spot(_mob: &MobCore, pos: BlockPos, cx: &TickContext) -> bool {
    water_block(cx, pos)
}

impl GoToWater {
    fn new() -> Self {
        let mut seek = MoveToBlock::new(1.0, 24, 1, water_spot);
        seek.vertical_start = -1;
        seek.recalculate_every = 160;
        Self(seek)
    }
}

impl Goal for GoToWater {
    fn flags(&self) -> Flags {
        self.0.flags()
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        if mob.body.in_water || !mob.is_baby() && (mob.homeward || has_egg(mob)) {
            return false;
        }
        self.0.can_use(mob, cx)
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        !mob.body.in_water
            && self.0.try_ticks <= MoveToBlock::GIVE_UP_TICKS
            && self.0.is_valid(mob, cx)
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        self.0.start(mob, cx);
    }

    fn every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        self.0.tick(mob, cx);
    }
}

fn idle_on_land(mob: &MobCore, _cx: &mut TickContext) -> bool {
    mob.body.in_water || mob.homeward || has_egg(mob)
}

/// Water (unless heading home) and sand please a wandering turtle most.
fn walk_target_value(mob: &MobCore, pos: BlockPos, cx: &TickContext) -> f32 {
    let water = (cx.block)(pos)
        .is_some_and(|s| tuff_datapack::tags::is(tag::fluid::key::WATER, s.fluid_state().fluid()));
    if (!mob.homeward && water) || sand(cx, BlockPos::new(pos.x, pos.y - 1, pos.z)) {
        10.0
    } else {
        mob.brightness_at(pos, cx) - 0.5
    }
}

#[derive(Debug, Default)]
pub struct Turtle {
    was_baby: bool,
}

impl Species for Turtle {
    fn pushed_by_fluid(&self) -> bool {
        false
    }

    /// Lightning kills a turtle outright.
    fn damage_override(&self, _core: &MobCore, damage: &super::Damage) -> Option<f32> {
        (damage.kind.name == tuff_vanilla_data::damage_type::LIGHTNING_BOLT.name)
            .then_some(f32::MAX)
    }

    fn create(_rng: &mut StdRng) -> Self {
        Self::default()
    }

    fn configure(core: &mut MobCore) {
        core.make_animal();
        core.set_path_malus(PathType::Water, 0.0);
        core.set_path_malus(PathType::DoorIronClosed, -1.0);
        core.set_path_malus(PathType::DoorWoodClosed, -1.0);
        core.set_path_malus(PathType::DoorOpen, -1.0);
        core.navigation.kind = NavKind::Amphibious;
        core.move_style = MoveStyle::Turtle;
        core.water_travel = WaterTravel::Turtle;
        core.walk_value = Some(walk_target_value);
        if core.attribute_base(Attribute::StepHeight) < 1.0 {
            core.set_attribute_base(Attribute::StepHeight, 1.0);
        }
        let mut goals = GoalSelector::default();
        goals.add(0, Panic::to_water(1.2, 7));
        goals.add(1, Breed::new(1.0).unless(has_egg));
        goals.add(1, LayEgg::new());
        goals.add(2, Tempt::new(1.1, turtle_food));
        goals.add(3, GoToWater::new());
        goals.add(4, GoHome::default());
        goals.add(7, Travel::default());
        goals.add(8, LookAtPlayer::new(8.0));
        goals.add(
            9,
            RandomStroll::with_interval(1.0, 100).unless(idle_on_land),
        );
        core.goals = goals;
    }

    fn finalize_spawn(&mut self, core: &mut MobCore, _cx: &mut TickContext) {
        core.bound_origin = Some(block_pos(core.base.position));
    }

    fn load(&mut self, core: &mut MobCore, nbt: &mut Compound) {
        let home = take(nbt, "home_pos", |tag| match tag {
            Tag::IntArray(v) if v.len() == 3 => Some(BlockPos::new(v[0], v[1], v[2])),
            _ => None,
        });
        core.bound_origin = Some(home.unwrap_or_else(|| block_pos(core.base.position)));
        if take_bool(nbt, "has_egg").unwrap_or(false) {
            core.signal |= SIGNAL_HAS_EGG;
        }
        self.was_baby = core.is_baby();
    }

    fn save(&self, core: &MobCore, out: &mut Compound) {
        let home = home(core);
        out.insert("home_pos", Tag::IntArray(vec![home.x, home.y, home.z]));
        out.insert("has_egg", has_egg(core));
    }

    fn metadata(&self, core: &MobCore) -> Vec<(u8, MetadataValue)> {
        vec![
            (HAS_EGG, MetadataValue::Boolean(has_egg(core))),
            (
                LAYING_EGG,
                MetadataValue::Boolean(core.signal & SIGNAL_LAYING != 0),
            ),
        ]
    }

    fn ambient_sound_interval(&self) -> i32 {
        200
    }

    fn sounds(&self, core: &MobCore) -> MobSounds {
        let baby = core.is_baby();
        MobSounds {
            ambient: (!core.body.in_water && core.base.on_ground && !baby)
                .then_some(SoundEvent::EntityTurtleAmbientLand),
            hurt: Some(if baby {
                SoundEvent::EntityTurtleHurtBaby
            } else {
                SoundEvent::EntityTurtleHurt
            }),
            death: Some(if baby {
                SoundEvent::EntityTurtleDeathBaby
            } else {
                SoundEvent::EntityTurtleDeath
            }),
            step: Some(if baby {
                SoundEvent::EntityTurtleShambleBaby
            } else {
                SoundEvent::EntityTurtleShamble
            }),
            volume: 1.0,
        }
    }

    /// A baby growing up sheds a scute.
    fn ai_step(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        let baby = core.is_baby();
        if self.was_baby && !baby && cx.shared.game_rules.borrow().bool("mob_drops") {
            let facts = tuff_core::world::drops::EntityFacts {
                kind: Some(core.base.kind),
                tables: Some(cx.shared.loot_tables()),
                ..Default::default()
            };
            for stack in cx
                .shared
                .block_loot()
                .entity_drops(GROW_TABLE, &facts, cx.rng)
            {
                core.drop_item(stack, 0.0, cx);
            }
        }
        self.was_baby = baby;
    }

    fn is_food<M: tuff_item::item::Mode>(&self, stack: &ItemStack<M>) -> bool {
        turtle_food(stack.item())
    }

    fn can_fall_in_love(&self, core: &MobCore) -> bool {
        !has_egg(core)
    }

    /// Bred turtles get eggs to lay at home rather than a baby.
    fn breed(
        mob: &mut Mob<Self>,
        partner: &mut Mob<Self>,
        cx: &mut TickContext,
    ) -> Option<Box<dyn RegionEntity>> {
        let in_love = |m: &Mob<Self>| m.core.love.as_ref().is_some_and(|l| l.is_in_love());
        if !in_love(mob) || !in_love(partner) || !mob.core.is_alive() || !partner.core.is_alive() {
            return None;
        }
        let cause = [&mob.core, &partner.core]
            .into_iter()
            .find_map(|parent| parent.love.as_ref().and_then(|love| love.cause));
        if let Some(player) = cause {
            cx.effects.push(Effect::Advancement {
                player,
                event: Box::new(tuff_core::world::messages::AdvancementEvent::BredAnimals {
                    parent: mob.core.facts(),
                    partner: partner.core.facts(),
                    child: None,
                }),
            });
        }
        set_signal(&mut mob.core, SIGNAL_HAS_EGG, true);
        for parent in [&mut mob.core, &mut partner.core] {
            parent.set_age(super::animal::PARENT_AGE_AFTER_BREEDING);
            if let Some(love) = &mut parent.love {
                love.reset();
            }
        }
        if cx.shared.game_rules.borrow().bool("mob_drops") {
            cx.effects.push(Effect::Experience {
                position: mob.core.base.position,
                amount: cx.rng.gen_range(1..=7),
            });
        }
        None
    }

    fn experience(&self, _core: &MobCore, rng: &mut StdRng) -> i32 {
        1 + rng.gen_range(0..3)
    }

    fn remove_when_far_away(&self) -> bool {
        false
    }
}

/// Registers the turtle's loader.
pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    registry.register(EntityType::Turtle, super::load::<Turtle>);
    registry.register_spawn_rule(EntityType::Turtle, super::spawning::turtle_rule);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn turtle_data_indices_match_the_dumped_layout() {
        let layout = EntityType::Turtle.data_layout();
        assert_eq!(layout.len(), LAYING_EGG as usize + 1);
        let boolean = MetadataValue::Boolean(false).type_id() as u8;
        assert_eq!(layout[HAS_EGG as usize], boolean);
        assert_eq!(layout[LAYING_EGG as usize], boolean);
    }
}
