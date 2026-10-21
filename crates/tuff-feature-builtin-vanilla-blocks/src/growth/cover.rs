//! Bone meal on ground cover: dripleaves growing taller, glow lichen
//! spreading and moss carpets growing a layer above themselves.

use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::fluid::Fluid;
use tuff_vanilla_data::{Block, BlockState};

use super::{Behaviours, Bonemeal, Features, GrowthLevel, GrowthLevelExt, above, always, below};
use tuff_core::world::redstone::UPDATE_ALL;
use tuff_vanilla_common::placement::step;
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::is_air;

const UPDATE_NO_SHAPES: u32 = 18;
const HORIZONTAL: [Facing; 4] = [Facing::North, Facing::East, Facing::South, Facing::West];

pub fn register(behaviours: &mut Behaviours) {
    behaviours.register_bonemeal_kind(
        "big_dripleaf",
        Bonemeal {
            valid: |level, pos, _| can_grow_into(level, above(pos)),
            success: always,
            perform: big_dripleaf,
        },
    );
    behaviours.register_bonemeal_kind(
        "big_dripleaf_stem",
        Bonemeal {
            valid: |level, pos, state| {
                top_leaf(level, pos, state.block())
                    .is_some_and(|head| can_grow_into(level, above(head)))
            },
            success: always,
            perform: big_dripleaf_stem,
        },
    );
    behaviours.register_bonemeal_kind(
        "small_dripleaf",
        Bonemeal {
            valid: |_, _, _| true,
            success: always,
            perform: small_dripleaf,
        },
    );
    behaviours.register_bonemeal_kind(
        "glow_lichen",
        Bonemeal {
            valid: lichen_valid,
            success: always,
            perform: lichen_spread,
        },
    );
    behaviours.register_bonemeal_kind(
        "mossy_carpet",
        Bonemeal {
            valid: |level, pos, state| {
                state.bottom() == Some(true) && topper(level, pos, &mut || true).is_some()
            },
            success: always,
            perform: |level, pos, _| {
                if let Some(top) = topper(level, pos, &mut || true) {
                    level.set(above(pos), top, UPDATE_ALL);
                }
            },
        },
    );
    behaviours.register_placed(Block::PaleMossCarpet, carpet_placed);
}

fn carpet_placed(level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) {
    if state.bottom() != Some(true) {
        return;
    }
    if let Some(top) = topper(level, pos, &mut || true) {
        level.set(above(pos), top, UPDATE_ALL);
    }
}

fn water_source(level: &dyn GrowthLevel, pos: BlockPos) -> bool {
    let fluid = level.state(pos).fluid_state();
    fluid.fluid() == Fluid::Water && fluid.is_source()
}

fn can_grow_into(level: &dyn GrowthLevel, pos: BlockPos) -> bool {
    let state = level.state(pos);
    level.is_inside_build_height(pos.y)
        && (is_air(state) || state.block() == Block::Water || state.block() == Block::SmallDripleaf)
}

fn place_part(level: &mut dyn GrowthLevel, block: Block, pos: BlockPos, facing: Facing) {
    let water = water_source(level, pos);
    let state = block
        .default_state()
        .with_waterlogged(water)
        .and_then(|s| s.with_facing(facing));
    if let Some(state) = state {
        level.set(pos, state, UPDATE_ALL);
    }
}

fn big_dripleaf(level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) {
    let target = above(pos);
    if !can_grow_into(level, target) {
        return;
    }
    let facing = state.facing().unwrap_or(Facing::North);
    place_part(level, Block::BigDripleafStem, pos, facing);
    place_part(level, Block::BigDripleaf, target, facing);
}

fn top_leaf(level: &dyn GrowthLevel, pos: BlockPos, stem: Block) -> Option<BlockPos> {
    let mut at = pos;
    while level.state(at).block() == stem {
        at = above(at);
    }
    (level.state(at).block() == Block::BigDripleaf).then_some(at)
}

fn big_dripleaf_stem(level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) {
    let Some(head) = top_leaf(level, pos, state.block()) else {
        return;
    };
    let facing = state.facing().unwrap_or(Facing::North);
    place_part(level, Block::BigDripleafStem, head, facing);
    place_part(level, Block::BigDripleaf, above(head), facing);
}

fn small_dripleaf(level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) {
    if state.half() != Some(tuff_vanilla_data::block::Half::Lower) {
        let lower = below(pos);
        let lower_state = level.state(lower);
        small_dripleaf(level, lower, lower_state);
        return;
    }
    let up = above(pos);
    let legacy = if water_source(level, up) {
        Block::Water.default_state()
    } else {
        Block::Air.default_state()
    };
    level.set(up, legacy, UPDATE_NO_SHAPES);
    let facing = state.facing().unwrap_or(Facing::North);
    let desired = 2 + level.random_int(4);
    let mut at = pos;
    let mut height = 0;
    while height < desired && can_grow_into(level, at) {
        height += 1;
        at = above(at);
    }
    let leaf_y = pos.y + height - 1;
    let mut at = pos;
    while at.y < leaf_y {
        place_part(level, Block::BigDripleafStem, at, facing);
        at = above(at);
    }
    place_part(level, Block::BigDripleaf, at, facing);
}

fn attaches(neighbour: BlockState, direction: Facing) -> bool {
    let face = direction.opposite();
    neighbour.is_face_sturdy(face, tuff_vanilla_data::Support::Full)
        || neighbour.is_collision_face_full(face)
}

fn lichen_valid(level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) -> bool {
    let mut valid = false;
    level.with_features(pos, &mut |features: &mut Features| {
        valid = [
            Facing::Down,
            Facing::Up,
            Facing::North,
            Facing::South,
            Facing::West,
            Facing::East,
        ]
        .into_iter()
        .any(|face| {
            features
                .region
                .multiface_can_spread(state, pos, face.opposite())
        });
        false
    });
    valid
}

fn lichen_spread(level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) {
    level.with_features(pos, &mut |features: &mut Features| {
        features
            .region
            .multiface_spread(features.random, state, pos);
        true
    });
}

fn is_carpet(state: BlockState) -> bool {
    state.block() == Block::PaleMossCarpet
}

fn wall_side(state: BlockState, direction: Facing) -> &'static str {
    state.get(direction.name()).unwrap_or("none")
}

fn updated_state(
    level: &dyn GrowthLevel,
    mut state: BlockState,
    pos: BlockPos,
    create_sides: bool,
) -> BlockState {
    let create_sides = create_sides || state.bottom() == Some(true);
    for direction in HORIZONTAL {
        let supported =
            direction != Facing::Up && attaches(level.state(step(pos, direction)), direction);
        let mut side = if supported {
            if create_sides {
                "low"
            } else {
                wall_side(state, direction)
            }
        } else {
            "none"
        };
        if side == "low" {
            let above_state = level.state(above(pos));
            if is_carpet(above_state)
                && wall_side(above_state, direction) != "none"
                && above_state.bottom() != Some(true)
            {
                side = "tall";
            }
            if state.bottom() != Some(true) {
                let below_state = level.state(below(pos));
                if is_carpet(below_state) && wall_side(below_state, direction) == "none" {
                    side = "none";
                }
            }
        }
        if let Some(next) = state.with(direction.name(), side) {
            state = next;
        }
    }
    state
}

fn has_faces(state: BlockState) -> bool {
    state.bottom() == Some(true)
        || HORIZONTAL
            .into_iter()
            .any(|d| wall_side(state, d) != "none")
}

fn topper(
    level: &dyn GrowthLevel,
    pos: BlockPos,
    side_survives: &mut dyn FnMut() -> bool,
) -> Option<BlockState> {
    let up = above(pos);
    let previous = level.state(up);
    let carpet_above = is_carpet(previous);
    if (carpet_above && previous.bottom() == Some(true))
        || (!carpet_above && !previous.is_replaceable())
    {
        return None;
    }
    let base = Block::PaleMossCarpet.default_state().with_bottom(false)?;
    let mut state = updated_state(level, base, up, true);
    for direction in HORIZONTAL {
        if wall_side(state, direction) != "none"
            && !side_survives()
            && let Some(next) = state.with(direction.name(), "none")
        {
            state = next;
        }
    }
    (has_faces(state) && state != previous).then_some(state)
}

#[cfg(test)]
mod tests {
    use super::super::testing::TestLevel;
    use super::*;
    use crate::growth::bonemeal;

    fn apply(level: &mut TestLevel, pos: BlockPos) -> Option<BlockPos> {
        let behaviours = crate::growth::Behaviours::builtin();
        bonemeal::apply(&behaviours, level, pos, Facing::Up)
    }

    #[tuff_core::watchdog]
    #[test]
    fn bone_meal_lengthens_a_big_dripleaf() {
        let mut level = TestLevel::new();
        let base = BlockPos::new(0, 64, 0);
        level.put(BlockPos::new(0, 63, 0), Block::Clay.default_state());
        level.put(base, Block::BigDripleaf.default_state());
        assert!(apply(&mut level, base).is_some());
        assert_eq!(level.state(base).block(), Block::BigDripleafStem);
        assert_eq!(level.state(above(base)).block(), Block::BigDripleaf);
        assert!(apply(&mut level, base).is_some());
        assert_eq!(level.state(above(above(base))).block(), Block::BigDripleaf);
        level.put(above(above(above(base))), Block::Stone.default_state());
        assert!(apply(&mut level, above(base)).is_none());
    }

    #[tuff_core::watchdog]
    #[test]
    fn bone_meal_turns_a_small_dripleaf_into_a_big_one() {
        let mut level = TestLevel::new();
        let base = BlockPos::new(0, 64, 0);
        level.put(BlockPos::new(0, 63, 0), Block::Clay.default_state());
        let lower = Block::SmallDripleaf.default_state();
        level.put(base, lower);
        level.put(
            above(base),
            lower
                .with_half(tuff_vanilla_data::block::Half::Upper)
                .unwrap(),
        );
        assert!(apply(&mut level, above(base)).is_some());
        assert_eq!(level.state(base).block(), Block::BigDripleafStem);
        assert!(level.count(Block::BigDripleaf) == 1);
    }

    #[tuff_core::watchdog]
    #[test]
    fn bone_meal_spreads_glow_lichen() {
        let mut level = TestLevel::new();
        level.fill(
            BlockPos::new(-3, 63, -3),
            BlockPos::new(3, 63, 3),
            Block::Stone.default_state(),
        );
        let pos = BlockPos::new(0, 64, 0);
        level.put(
            pos,
            Block::GlowLichen.default_state().with_down(true).unwrap(),
        );
        for _ in 0..4 {
            assert!(apply(&mut level, pos).is_some());
        }
        assert_eq!(level.count(Block::GlowLichen), 5);
        assert!(apply(&mut level, pos).is_none());
    }

    #[tuff_core::watchdog]
    #[test]
    fn bone_meal_grows_a_moss_carpet_layer_on_top() {
        let mut level = TestLevel::new();
        level.fill(
            BlockPos::new(-3, 63, -3),
            BlockPos::new(3, 63, 3),
            Block::Stone.default_state(),
        );
        let pos = BlockPos::new(0, 64, 0);
        level.put(BlockPos::new(1, 64, 0), Block::Stone.default_state());
        level.put(BlockPos::new(1, 65, 0), Block::Stone.default_state());
        let base = updated_state(&level, Block::PaleMossCarpet.default_state(), pos, true);
        level.put(pos, base);
        assert!(apply(&mut level, pos).is_some());
        let top = level.state(above(pos));
        assert_eq!(top.block(), Block::PaleMossCarpet);
        assert_eq!(top.bottom(), Some(false));
        assert_eq!(top.get("east"), Some("low"));
        assert!(apply(&mut level, pos).is_none());
    }

    #[tuff_core::watchdog]
    #[test]
    fn placing_a_moss_carpet_adds_a_layer_on_top() {
        let mut level = TestLevel::new();
        let pos = BlockPos::new(0, 64, 0);
        level.put(BlockPos::new(1, 64, 0), Block::Stone.default_state());
        level.put(BlockPos::new(1, 65, 0), Block::Stone.default_state());
        let base = updated_state(&level, Block::PaleMossCarpet.default_state(), pos, true);
        level.put(pos, base);
        carpet_placed(&mut level, pos, base);
        let top = level.state(above(pos));
        assert_eq!(top.block(), Block::PaleMossCarpet);
        assert_eq!(top.bottom(), Some(false));
        carpet_placed(&mut level, above(pos), top);
        assert_eq!(level.state(above(above(pos))), Block::Air.default_state());
    }
}
