//! Placement, support and shape rules of blocks whose behaviour the dumped
//! block kinds do not describe: bells, chorus plants and flowers, big
//! dripleaf, hanging moss, frogspawn and potent sulfur.
//!
//! Chorus, dripleaf stems and hanging moss do not break the moment they
//! lose support: the shape update schedules a tick ([`shape_tick`]) and the
//! tick breaks them if they are still unsupported ([`breaks_on_tick`]).

use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::tag::block as tags;
use tuff_vanilla_data::{Block, BlockState, Support};

use crate::placement::{Context, Level, step};
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::is_air;

const AIR: BlockState = Block::Air.default_state();

/// Ticks between a supporting block changing and the unsupported block
/// breaking.
pub const UNSUPPORTED_TICK_DELAY: u32 = 1;

fn handles(block: Block) -> bool {
    matches!(
        block,
        Block::Bell
            | Block::ChorusPlant
            | Block::ChorusFlower
            | Block::BigDripleaf
            | Block::BigDripleafStem
            | Block::PaleHangingMoss
            | Block::Frogspawn
            | Block::PotentSulfur
            | Block::BambooSapling
    )
}

/// Blocks a scheduled tick breaks (with drops) when they cannot survive.
pub fn breaks_on_tick(block: Block) -> bool {
    matches!(
        block,
        Block::ChorusPlant
            | Block::ChorusFlower
            | Block::BigDripleafStem
            | Block::PaleHangingMoss
            | Block::Cactus
            | Block::SugarCane
            | Block::Bamboo
    ) || block.growing_plant().is_some()
}

/// Blocks that keep their state when a shape update finds them unsupported
/// and break on the tick it schedules instead.
pub fn defers_break(block: Block) -> bool {
    matches!(block, Block::Cactus | Block::SugarCane | Block::Bamboo)
        || block.growing_plant().is_some()
}

/// The state `context` places for `block` from its waterlogged default
/// `base`: `None` when the rules here do not cover the block, `Some(None)`
/// when nothing can be placed.
pub fn placement(
    block: Block,
    base: BlockState,
    context: &Context,
    level: Level,
) -> Option<Option<BlockState>> {
    let pos = context.pos;
    Some(match block {
        Block::Bell => bell_placement(base, context, level),
        Block::ChorusPlant => Some(chorus_plant_state(pos, level)),
        Block::BigDripleaf => {
            let below = level(step(pos, Facing::Down));
            let facing = if matches!(below.block(), Block::BigDripleaf | Block::BigDripleafStem) {
                below.facing()
            } else {
                Some(context.horizontal().opposite())
            };
            Some(facing.and_then(|f| base.with_facing(f)).unwrap_or(base))
        }
        Block::PotentSulfur => Some(potent_sulfur_state(base, pos, level)),
        Block::Bamboo => bamboo_placement(base, pos, level),
        _ => return None,
    })
}

fn bamboo_placement(base: BlockState, pos: BlockPos, level: Level) -> Option<BlockState> {
    if !level(pos).fluid_state().is_empty() {
        return None;
    }
    let below = level(step(pos, Facing::Down));
    if !tuff_datapack::tags::is(tags::key::SUPPORTS_BAMBOO, below.block()) {
        return None;
    }
    let with_age = |age: u8| base.with_age(age).or(Some(base));
    match below.block() {
        Block::BambooSapling => with_age(0),
        Block::Bamboo => with_age(u8::from(below.age().unwrap_or(0) > 0)),
        _ => {
            let above = level(step(pos, Facing::Up));
            if above.block() == Block::Bamboo {
                with_age(above.age().unwrap_or(0))
            } else {
                Some(Block::BambooSapling.default_state())
            }
        }
    }
}

/// Whether `state` can stay at `pos`, or `None` when the rules here do not
/// cover it.
pub fn can_survive(state: BlockState, pos: BlockPos, level: Level) -> Option<bool> {
    let block = state.block();
    if !handles(block) {
        return None;
    }
    let below = || level(step(pos, Facing::Down));
    let above = || level(step(pos, Facing::Up));
    Some(match block {
        Block::Bell => bell_survives(state, pos, level),
        Block::ChorusPlant => chorus_plant_survives(pos, level),
        Block::ChorusFlower => chorus_flower_survives(pos, level),
        Block::BigDripleaf => {
            let below = below().block();
            below == block
                || below == Block::BigDripleafStem
                || tuff_datapack::tags::is(tags::key::SUPPORTS_BIG_DRIPLEAF, below)
        }
        Block::BigDripleafStem => {
            let (below, above) = (below().block(), above().block());
            (below == block || tuff_datapack::tags::is(tags::key::SUPPORTS_BIG_DRIPLEAF, below))
                && (above == block || above == Block::BigDripleaf)
        }
        Block::PaleHangingMoss => hanging_moss_survives(block, pos, level),
        Block::BambooSapling => {
            tuff_datapack::tags::is(tags::key::SUPPORTS_BAMBOO, below().block())
        }
        Block::Frogspawn => {
            let below = below();
            let supports =
                tuff_datapack::tags::is(
                    tuff_vanilla_data::tag::fluid::key::SUPPORTS_FROGSPAWN,
                    below.fluid_state().fluid(),
                ) || tuff_datapack::tags::is(tags::key::SUPPORTS_FROGSPAWN, below.block());
            supports && level(pos).fluid_state().is_empty()
        }
        _ => true,
    })
}

/// How `state` reacts to its neighbour in `direction` becoming
/// `neighbour`, or `None` when the rules here do not cover it. `level`
/// must already read `neighbour` at that side.
pub fn update_shape(
    state: BlockState,
    pos: BlockPos,
    direction: Facing,
    neighbour: BlockState,
    level: Level,
) -> Option<BlockState> {
    let block = state.block();
    Some(match block {
        Block::Bell => bell_update(state, pos, direction, neighbour, level),
        Block::ChorusPlant => {
            if !chorus_plant_survives(pos, level) {
                return Some(state);
            }
            let connects = chorus_connects(direction, neighbour);
            state
                .with(direction.name(), if connects { "true" } else { "false" })
                .unwrap_or(state)
        }
        Block::ChorusFlower | Block::BigDripleafStem => state,
        Block::BigDripleaf => {
            if direction == Facing::Down && can_survive(state, pos, level) == Some(false) {
                return Some(AIR);
            }
            if direction == Facing::Up && neighbour.block() == block {
                return Some(dripleaf_stem(state));
            }
            state
        }
        Block::PaleHangingMoss => {
            let tip = level(step(pos, Facing::Down)).block() != block;
            state
                .with("tip", if tip { "true" } else { "false" })
                .unwrap_or(state)
        }
        Block::PotentSulfur => potent_sulfur_state(state, pos, level),
        Block::Bamboo => {
            if direction == Facing::Up
                && neighbour.block() == block
                && neighbour.age() > state.age()
            {
                state
                    .with_age(neighbour.age().unwrap_or(0))
                    .unwrap_or(state)
            } else {
                state
            }
        }
        Block::BambooSapling => {
            if can_survive(state, pos, level) == Some(false) {
                AIR
            } else if direction == Facing::Up && neighbour.block() == Block::Bamboo {
                Block::Bamboo.default_state()
            } else {
                state
            }
        }
        _ => return None,
    })
}

/// The delay of the tick `state` schedules when its neighbour in
/// `direction` changed, read through `level` after the change.
pub fn shape_tick(
    state: BlockState,
    pos: BlockPos,
    direction: Facing,
    level: Level,
) -> Option<u32> {
    let block = state.block();
    let checks = match block {
        Block::ChorusPlant | Block::PaleHangingMoss => true,
        Block::ChorusFlower => direction != Facing::Up,
        Block::BigDripleafStem => !direction.is_horizontal(),
        _ => false,
    };
    (checks && can_survive(state, pos, level) == Some(false)).then_some(UNSUPPORTED_TICK_DELAY)
}

/// A chorus plant at `pos` connected to the chorus around it (and to its
/// ground below).
pub fn chorus_plant_state(pos: BlockPos, level: Level) -> BlockState {
    Facing::ALL
        .into_iter()
        .fold(Block::ChorusPlant.default_state(), |state, direction| {
            let connects = chorus_connects(direction, level(step(pos, direction)));
            state
                .with(direction.name(), if connects { "true" } else { "false" })
                .unwrap_or(state)
        })
}

fn chorus_connects(direction: Facing, neighbour: BlockState) -> bool {
    let block = neighbour.block();
    block == Block::ChorusPlant
        || block == Block::ChorusFlower
        || (direction == Facing::Down
            && tuff_datapack::tags::is(tags::key::SUPPORTS_CHORUS_PLANT, block))
}

fn is_chorus_ground(state: BlockState) -> bool {
    state.block() == Block::ChorusPlant
        || tuff_datapack::tags::is(tags::key::SUPPORTS_CHORUS_PLANT, state.block())
}

/// A chorus plant stands on its ground or more plant, or branches sideways
/// off a standing plant when nothing is above or below it.
fn chorus_plant_survives(pos: BlockPos, level: Level) -> bool {
    let below = level(step(pos, Facing::Down));
    let capped = !is_air(level(step(pos, Facing::Up))) && !is_air(below);
    for direction in Facing::HORIZONTAL {
        let side = step(pos, direction);
        if level(side).block() == Block::ChorusPlant {
            if capped {
                return false;
            }
            if is_chorus_ground(level(step(side, Facing::Down))) {
                return true;
            }
        }
    }
    is_chorus_ground(below)
}

/// A chorus flower sits on a plant or its ground, or in the air off exactly
/// one plant beside it with nothing else around.
fn chorus_flower_survives(pos: BlockPos, level: Level) -> bool {
    let below = level(step(pos, Facing::Down));
    if below.block() == Block::ChorusPlant
        || tuff_datapack::tags::is(tags::key::SUPPORTS_CHORUS_FLOWER, below.block())
    {
        return true;
    }
    if !is_air(below) {
        return false;
    }
    let mut plants = 0;
    for direction in Facing::HORIZONTAL {
        let side = level(step(pos, direction));
        if side.block() == Block::ChorusPlant {
            plants += 1;
        } else if !is_air(side) {
            return false;
        }
    }
    plants == 1
}

fn hanging_moss_survives(block: Block, pos: BlockPos, level: Level) -> bool {
    let above = level(step(pos, Facing::Up));
    above.block() == block
        || above.is_face_sturdy(Facing::Down, Support::Full)
        || above.is_collision_face_full(Facing::Down)
}

fn dripleaf_stem(leaf: BlockState) -> BlockState {
    let stem = Block::BigDripleafStem.default_state();
    let stem = leaf
        .facing()
        .and_then(|f| stem.with_facing(f))
        .unwrap_or(stem);
    leaf.waterlogged()
        .and_then(|w| stem.with_waterlogged(w))
        .unwrap_or(stem)
}

/// The side of a bell its support is on.
fn bell_support(state: BlockState) -> Option<Facing> {
    match state.get("attachment")? {
        "floor" => Some(Facing::Down),
        "ceiling" => Some(Facing::Up),
        _ => state.facing(),
    }
}

fn bell_survives(state: BlockState, pos: BlockPos, level: Level) -> bool {
    match bell_support(state) {
        Some(Facing::Up) => {
            level(step(pos, Facing::Up)).is_face_sturdy(Facing::Down, Support::Center)
        }
        Some(side) => level(step(pos, side)).is_face_sturdy(side.opposite(), Support::Full),
        None => false,
    }
}

fn bell_placement(base: BlockState, context: &Context, level: Level) -> Option<BlockState> {
    let pos = context.pos;
    let face = context.face;
    let survives = |state: BlockState| bell_survives(state, pos, level).then_some(state);
    if !face.is_horizontal() {
        let attachment = if face == Facing::Down {
            "ceiling"
        } else {
            "floor"
        };
        return survives(
            base.with("attachment", attachment)?
                .with_facing(context.horizontal())?,
        );
    }
    let sturdy =
        |side: Facing| level(step(pos, side)).is_face_sturdy(side.opposite(), Support::Full);
    let sides = match face {
        Facing::East | Facing::West => [Facing::West, Facing::East],
        _ => [Facing::North, Facing::South],
    };
    let double = sides.into_iter().all(sturdy);
    let wall = base.with_facing(face.opposite())?.with(
        "attachment",
        if double { "double_wall" } else { "single_wall" },
    )?;
    if let Some(state) = survives(wall) {
        return Some(state);
    }
    let attachment = if level(step(pos, Facing::Down)).is_face_sturdy(Facing::Up, Support::Full) {
        "floor"
    } else {
        "ceiling"
    };
    survives(wall.with("attachment", attachment)?)
}

fn bell_update(
    state: BlockState,
    pos: BlockPos,
    direction: Facing,
    neighbour: BlockState,
    level: Level,
) -> BlockState {
    let attachment = state.get("attachment");
    let double = attachment == Some("double_wall");
    let support = bell_support(state);
    if support == Some(direction) && !double && !bell_survives(state, pos, level) {
        return AIR;
    }
    let Some(facing) = state.facing() else {
        return state;
    };
    if direction.axis() == facing.axis() {
        if double && !neighbour.is_face_sturdy(direction, Support::Full) {
            return state
                .with("attachment", "single_wall")
                .and_then(|s| s.with_facing(direction.opposite()))
                .unwrap_or(state);
        }
        if attachment == Some("single_wall")
            && support.map(Facing::opposite) == Some(direction)
            && neighbour.is_face_sturdy(facing, Support::Full)
        {
            return state.with("attachment", "double_wall").unwrap_or(state);
        }
    }
    state
}

/// Potent sulfur is dry unless a water source sits on it; then the block
/// below decides whether it erupts all the time, now and then, or never.
fn potent_sulfur_state(state: BlockState, pos: BlockPos, level: Level) -> BlockState {
    let set = |value: &str| state.with("potent_sulfur_state", value).unwrap_or(state);
    let above = level(step(pos, Facing::Up)).fluid_state();
    if !(above.is_source() && above.fluid() == tuff_vanilla_data::fluid::Fluid::Water) {
        return set("dry");
    }
    let below = level(step(pos, Facing::Down));
    let fluid = below.fluid_state();
    let source_if_fluid = fluid.is_empty() || fluid.is_source();
    if source_if_fluid
        && tuff_datapack::tags::is(tags::key::CAUSES_CONTINUOUS_GEYSER_ERUPTIONS, below.block())
    {
        return set("continuous");
    }
    if source_if_fluid
        && tuff_datapack::tags::is(tags::key::CAUSES_PERIODIC_GEYSER_ERUPTIONS, below.block())
    {
        if state.get("potent_sulfur_state") == Some("erupting") {
            return state;
        }
        return set("dormant");
    }
    set("wet")
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use tuff_vanilla_data::Item;

    use super::*;
    use crate::placement;
    use tuff_world_model::chunk::ChunkShape;

    #[derive(Default)]
    struct World(HashMap<BlockPos, BlockState>);

    impl World {
        fn get(&self, pos: BlockPos) -> BlockState {
            self.0.get(&pos).copied().unwrap_or(AIR)
        }

        fn set(&mut self, pos: BlockPos, state: impl Into<BlockState>) {
            self.0.insert(pos, state.into());
        }

        fn with(blocks: &[(BlockPos, Block)]) -> Self {
            let mut world = Self::default();
            for (pos, block) in blocks {
                world.set(*pos, block.default_state());
            }
            world
        }

        fn survives(&self, state: BlockState, pos: BlockPos) -> bool {
            placement::can_survive(state, pos, &|p| self.get(p))
        }

        fn place(
            &self,
            item: Item,
            clicked: BlockPos,
            face: Facing,
            yaw: f32,
        ) -> Option<BlockState> {
            let level = |p| self.get(p);
            let context = placement::context(
                clicked,
                face,
                [0.5, 0.5, 0.5],
                yaw,
                0.0,
                false,
                item,
                ChunkShape::OVERWORLD,
                &level,
            );
            placement::placement_state(&context, &level)
        }

        fn update(&self, pos: BlockPos, direction: Facing) -> BlockState {
            let level = |p| self.get(p);
            placement::update_shape(
                self.get(pos),
                pos,
                direction,
                self.get(step(pos, direction)),
                &level,
            )
        }
    }

    fn at(x: i32, y: i32, z: i32) -> BlockPos {
        BlockPos::new(x, y, z)
    }

    #[tuff_core::watchdog]
    #[test]
    fn chorus_plants_stand_on_their_ground_or_branch_off_a_standing_plant() {
        let ground = at(0, 0, 0);
        let mut world =
            World::with(&[(ground, Block::EndStone), (at(0, 1, 0), Block::ChorusPlant)]);
        let plant = Block::ChorusPlant.default_state();
        assert!(world.survives(plant, at(0, 1, 0)));
        assert!(
            world.survives(plant, at(1, 1, 0)),
            "a branch off a standing plant"
        );
        assert!(!world.survives(plant, at(2, 1, 0)), "nothing holds it");
        world.set(at(1, 0, 0), Block::Stone);
        assert!(
            world.survives(plant, at(1, 1, 0)),
            "a branch with something below but air above"
        );
        world.set(at(1, 0, 0), AIR);
        world.set(at(1, 2, 0), Block::Stone);
        assert!(
            world.survives(plant, at(1, 1, 0)),
            "only one of above and below is filled"
        );
        world.set(at(1, 0, 0), Block::Stone);
        assert!(!world.survives(plant, at(1, 1, 0)));
        assert!(!world.survives(plant, at(5, 1, 5)));
        world.set(at(5, 0, 5), Block::Stone);
        assert!(
            !world.survives(plant, at(5, 1, 5)),
            "stone is no chorus ground"
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn chorus_flowers_need_a_plant_below_or_exactly_one_beside_them() {
        let mut world = World::with(&[
            (at(0, 0, 0), Block::EndStone),
            (at(0, 1, 0), Block::ChorusPlant),
        ]);
        let flower = Block::ChorusFlower.default_state();
        assert!(world.survives(flower, at(0, 2, 0)));
        assert!(world.survives(flower, at(1, 1, 0)));
        assert!(!world.survives(flower, at(1, 2, 0)));
        world.set(at(2, 1, 0), Block::ChorusPlant);
        assert!(!world.survives(flower, at(1, 1, 0)), "two plants beside it");
        world.set(at(2, 1, 0), Block::Stone);
        assert!(
            !world.survives(flower, at(1, 1, 0)),
            "something else beside it"
        );
        assert!(!world.survives(flower, at(3, 2, 0)), "on stone");
    }

    #[tuff_core::watchdog]
    #[test]
    fn placing_chorus_checks_support_and_connects() {
        let world = World::with(&[(at(0, 0, 0), Block::EndStone), (at(1, 0, 0), Block::Stone)]);
        let plant = world
            .place(Item::ChorusPlant, at(0, 0, 0), Facing::Up, 0.0)
            .expect("plants on end stone");
        assert_eq!(plant.get("down"), Some("true"));
        assert_eq!(plant.get("up"), Some("false"));
        assert_eq!(
            world.place(Item::ChorusPlant, at(1, 0, 0), Facing::Up, 0.0),
            None
        );
        assert_eq!(
            world.place(Item::ChorusFlower, at(0, 0, 0), Facing::Up, 0.0),
            Some(Block::ChorusFlower.default_state())
        );
        assert_eq!(
            world.place(Item::ChorusFlower, at(1, 0, 0), Facing::Up, 0.0),
            None
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn unsupported_chorus_waits_for_a_tick_instead_of_breaking() {
        let world = World::with(&[
            (at(0, 1, 0), Block::ChorusPlant),
            (at(0, 2, 0), Block::ChorusFlower),
        ]);
        let level = |p| world.get(p);
        let plant = world.get(at(0, 1, 0));
        assert_eq!(
            world.update(at(0, 1, 0), Facing::Down),
            plant,
            "kept until its tick"
        );
        assert_eq!(
            placement::shape_tick(plant, at(0, 1, 0), Facing::Down, &level),
            Some(UNSUPPORTED_TICK_DELAY)
        );
        let flower = world.get(at(0, 2, 0));
        assert_eq!(world.update(at(0, 2, 0), Facing::Down), flower);
        assert_eq!(
            placement::shape_tick(flower, at(0, 2, 0), Facing::Up, &level),
            None
        );
        assert_eq!(
            placement::shape_tick(flower, at(0, 2, 0), Facing::North, &level),
            None
        );
        let lonely = World::with(&[(at(0, 2, 0), Block::ChorusFlower)]);
        let level = |p| lonely.get(p);
        assert_eq!(
            placement::shape_tick(flower, at(0, 2, 0), Facing::North, &level),
            Some(UNSUPPORTED_TICK_DELAY)
        );
        assert!(breaks_on_tick(Block::ChorusPlant) && breaks_on_tick(Block::ChorusFlower));
    }

    #[tuff_core::watchdog]
    #[test]
    fn supported_chorus_plants_keep_connecting_to_neighbours() {
        let mut world = World::with(&[
            (at(0, 0, 0), Block::EndStone),
            (at(0, 1, 0), Block::ChorusPlant),
        ]);
        world.set(at(0, 2, 0), Block::ChorusFlower);
        let updated = world.update(at(0, 1, 0), Facing::Up);
        assert_eq!(updated.get("up"), Some("true"));
    }

    #[tuff_core::watchdog]
    #[test]
    fn bells_hang_from_floors_ceilings_and_walls() {
        let world = World::with(&[(at(0, 0, 0), Block::Stone), (at(0, 3, 0), Block::Stone)]);
        let floor = world
            .place(Item::Bell, at(0, 0, 0), Facing::Up, 90.0)
            .unwrap();
        assert_eq!(floor.get("attachment"), Some("floor"));
        assert_eq!(floor.facing(), Some(Facing::West));
        let ceiling = world
            .place(Item::Bell, at(0, 3, 0), Facing::Down, 0.0)
            .unwrap();
        assert_eq!(ceiling.get("attachment"), Some("ceiling"));
        let wall = world
            .place(Item::Bell, at(0, 3, 0), Facing::North, 0.0)
            .unwrap();
        assert_eq!(wall.get("attachment"), Some("single_wall"));
        assert_eq!(wall.facing(), Some(Facing::South));
        let fence = World::with(&[(at(0, 3, 0), Block::OakFence)]);
        let hung = fence.place(Item::Bell, at(0, 3, 0), Facing::Down, 0.0);
        assert_eq!(hung.and_then(|s| s.get("attachment")), Some("ceiling"));
        let floating = World::with(&[(at(0, 3, 0), Block::Torch)]);
        assert_eq!(
            floating.place(Item::Bell, at(0, 3, 0), Facing::Down, 0.0),
            None
        );
        assert_eq!(
            floating.place(Item::Bell, at(0, 3, 0), Facing::North, 0.0),
            None
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_bell_between_two_walls_holds_on_to_whichever_stays() {
        let mut world = World::with(&[(at(0, 1, 0), Block::Stone), (at(0, 1, -2), Block::Stone)]);
        let bell = at(0, 1, -1);
        let double = world
            .place(Item::Bell, at(0, 1, 0), Facing::North, 0.0)
            .unwrap();
        assert_eq!(double.get("attachment"), Some("double_wall"));
        assert_eq!(double.facing(), Some(Facing::South));
        world.set(bell, double);
        world.set(at(0, 1, 0), AIR);
        let single = world.update(bell, Facing::South);
        assert_eq!(single.get("attachment"), Some("single_wall"));
        assert_eq!(single.facing(), Some(Facing::North));
        world.set(bell, single);
        world.set(at(0, 1, 0), Block::Stone);
        assert_eq!(
            world.update(bell, Facing::South).get("attachment"),
            Some("double_wall")
        );
        world.set(at(0, 1, 0), AIR);
        world.set(at(0, 1, -2), AIR);
        assert_eq!(world.update(bell, Facing::North), AIR);
        assert_eq!(
            world.update(bell, Facing::East),
            single,
            "only its support side breaks it"
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_side_placed_bell_without_a_wall_falls_back_to_floor_or_ceiling() {
        let world = World::with(&[(at(0, 2, 0), Block::OakFence), (at(0, 1, -1), Block::Stone)]);
        let bell = world
            .place(Item::Bell, at(0, 2, 0), Facing::North, 0.0)
            .unwrap();
        assert_eq!(bell.get("attachment"), Some("floor"));
    }

    #[tuff_core::watchdog]
    #[test]
    fn big_dripleaf_stacks_into_a_stem() {
        let mut world = World::with(&[(at(0, 0, 0), Block::Clay)]);
        let leaf = world
            .place(Item::BigDripleaf, at(0, 0, 0), Facing::Up, 90.0)
            .unwrap();
        assert_eq!(leaf.facing(), Some(Facing::East));
        assert_eq!(
            world.place(Item::BigDripleaf, at(5, 5, 5), Facing::Up, 0.0),
            None
        );
        world.set(at(0, 1, 0), leaf);
        let upper = world
            .place(Item::BigDripleaf, at(0, 1, 0), Facing::Up, 0.0)
            .unwrap();
        assert_eq!(upper.facing(), Some(Facing::East), "takes the facing below");
        world.set(at(0, 2, 0), upper);
        let stem = world.update(at(0, 1, 0), Facing::Up);
        assert_eq!(stem.block(), Block::BigDripleafStem);
        assert_eq!(stem.facing(), Some(Facing::East));
        world.set(at(0, 1, 0), stem);
        world.set(at(0, 0, 0), AIR);
        let level = |p| world.get(p);
        assert_eq!(world.update(at(0, 1, 0), Facing::Down), stem);
        assert_eq!(
            placement::shape_tick(stem, at(0, 1, 0), Facing::Down, &level),
            Some(UNSUPPORTED_TICK_DELAY)
        );
        assert_eq!(
            placement::shape_tick(stem, at(0, 1, 0), Facing::North, &level),
            None
        );
        world.set(at(0, 1, 0), AIR);
        assert_eq!(world.update(at(0, 2, 0), Facing::Down), AIR);
    }

    #[tuff_core::watchdog]
    #[test]
    fn hanging_moss_hangs_from_full_faces_and_marks_its_tip() {
        let mut world = World::with(&[(at(0, 5, 0), Block::Stone)]);
        let moss = Block::PaleHangingMoss.default_state();
        assert!(world.survives(moss, at(0, 4, 0)));
        assert!(!world.survives(moss, at(1, 4, 0)));
        assert!(
            world
                .place(Item::PaleHangingMoss, at(0, 5, 0), Facing::Down, 0.0)
                .is_some()
        );
        world.set(at(0, 4, 0), moss);
        world.set(at(0, 3, 0), moss);
        assert!(world.survives(moss, at(0, 3, 0)), "hangs from more moss");
        assert_eq!(
            world.update(at(0, 4, 0), Facing::Down).get("tip"),
            Some("false")
        );
        assert_eq!(
            world.update(at(0, 3, 0), Facing::Down).get("tip"),
            Some("true")
        );
        world.set(at(0, 5, 0), AIR);
        let level = |p| world.get(p);
        assert_eq!(
            world.update(at(0, 4, 0), Facing::Up).block(),
            Block::PaleHangingMoss
        );
        assert_eq!(
            placement::shape_tick(moss, at(0, 4, 0), Facing::Up, &level),
            Some(UNSUPPORTED_TICK_DELAY)
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn frogspawn_floats_only_on_open_water() {
        let mut world = World::with(&[(at(0, 0, 0), Block::Water)]);
        let spawn = Block::Frogspawn.default_state();
        assert!(world.survives(spawn, at(0, 1, 0)));
        assert!(!world.survives(spawn, at(1, 1, 0)));
        world.set(at(0, 1, 0), Block::Water);
        assert!(!world.survives(spawn, at(0, 1, 0)), "under water");
        world.set(at(0, 1, 0), spawn);
        world.set(at(0, 0, 0), Block::Stone);
        assert_eq!(world.update(at(0, 1, 0), Facing::Down), AIR);
    }

    #[tuff_core::watchdog]
    #[test]
    fn potent_sulfur_wakes_under_water_over_its_geyser_sources() {
        let continuous = Block::ALL
            .iter()
            .copied()
            .find(|b| tags::CAUSES_CONTINUOUS_GEYSER_ERUPTIONS.contains(b));
        let periodic = Block::ALL
            .iter()
            .copied()
            .find(|b| tags::CAUSES_PERIODIC_GEYSER_ERUPTIONS.contains(b) && *b != Block::Water);
        let sulfur = at(0, 1, 0);
        let state_with = |below: Option<Block>, water_above: bool| {
            let mut world = World::default();
            if let Some(below) = below {
                world.set(at(0, 0, 0), below);
            }
            if water_above {
                world.set(at(0, 2, 0), Block::Water);
            }
            let level = |p| world.get(p);
            Some(potent_sulfur_state(
                Block::PotentSulfur.default_state(),
                sulfur,
                &level,
            ))
        };
        let mut pool = World::with(&[(at(0, 0, 0), Block::Stone), (at(0, 2, 0), Block::Water)]);
        let placed = pool.place(Item::PotentSulfur, at(0, 0, 0), Facing::Up, 0.0);
        assert_eq!(
            placed.and_then(|s| s.get("potent_sulfur_state")),
            Some("wet")
        );
        pool.set(at(0, 2, 0), AIR);
        let placed = pool.place(Item::PotentSulfur, at(0, 0, 0), Facing::Up, 0.0);
        assert_eq!(
            placed.and_then(|s| s.get("potent_sulfur_state")),
            Some("dry")
        );
        let value = |state: Option<BlockState>| {
            state.and_then(|s| s.get("potent_sulfur_state").map(str::to_owned))
        };
        assert_eq!(
            value(state_with(Some(Block::Stone), false)).as_deref(),
            Some("dry")
        );
        assert_eq!(
            value(state_with(Some(Block::Stone), true)).as_deref(),
            Some("wet")
        );
        if let Some(block) = continuous {
            assert_eq!(
                value(state_with(Some(block), true)).as_deref(),
                Some("continuous")
            );
            assert_eq!(
                value(state_with(Some(block), false)).as_deref(),
                Some("dry")
            );
        }
        if let Some(block) = periodic {
            assert_eq!(
                value(state_with(Some(block), true)).as_deref(),
                Some("dormant")
            );
        }
        assert!(
            continuous.is_some() || periodic.is_some(),
            "the geyser tags are empty"
        );
    }
}
