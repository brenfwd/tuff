use super::*;

/// Blocks that react to being covered with a scheduled tick instead of
/// breaking: dirt paths and farmland.
pub fn ticks_when_covered(block: Block) -> bool {
    matches!(block.kind(), BlockKind::Path | BlockKind::Farmland)
}

/// Ticks after placement or a support-losing neighbour change before a
/// falling block (sand, gravel, anvils, ...) checks whether it should fall.
pub fn falling_delay(block: Block) -> Option<u32> {
    block.is_falling().then(|| u32::from(block.falling_delay()))
}

/// The delay of the block tick placing `block` schedules, if any: falling
/// blocks check for a drop, scaffolding for its support.
pub fn place_tick(block: Block) -> Option<u32> {
    if scaffolding::is_scaffolding(block) {
        return Some(scaffolding::TICK_DELAY);
    }
    if crate::brushable::is_brushable(block) {
        return Some(crate::brushable::TICK_DELAY);
    }
    falling_delay(block)
}

/// The delay of the block tick `state` at `pos` schedules when its
/// neighbour in `direction` changes, if it schedules one. Falling blocks
/// schedule on any neighbour change, not just the one they land on; the
/// tick itself (see `world::region::redstone::placement_tick`) is what
/// decides whether the block below is actually free.
pub fn shape_tick(
    state: BlockState,
    pos: BlockPos,
    direction: Facing,
    level: Level,
) -> Option<u32> {
    if let Some(delay) = place_tick(state.block()) {
        return Some(delay);
    }
    if speleothem::is_speleothem(state.block()) {
        return speleothem::shape_tick(state, pos, direction, level);
    }
    if let Some(delay) = block_support::shape_tick(state, pos, direction, level) {
        return Some(delay);
    }
    let watches = match state.block().growing_plant() {
        Some(plant) => direction == plant.direction.opposite(),
        None => block_support::defers_break(state.block()),
    };
    if watches && !can_survive(state, pos, level) {
        return Some(block_support::UNSUPPORTED_TICK_DELAY);
    }
    (ticks_when_covered(state.block())
        && direction == Facing::Up
        && !can_survive(state, pos, level))
    .then_some(COVERED_TICK_DELAY)
}

/// What a scheduled block tick turns `state` at `pos` into, if anything: a
/// dirt path always reverts, farmland only while it is still covered.
pub fn tick_state(state: BlockState, pos: BlockPos, level: Level) -> Option<BlockState> {
    let base = state.block().base_block()?.default_state();
    match state.block().kind() {
        BlockKind::Path => Some(base),
        BlockKind::Farmland => (!can_survive(state, pos, level)).then_some(base),
        _ => None,
    }
}

/// Whether `old` becoming `new` is a dirt path or farmland reverting to
/// its full-height base block, which lifts whatever stands in it.
pub fn reverts_to_base(old: BlockState, new: BlockState) -> bool {
    matches!(old.block().kind(), BlockKind::Path | BlockKind::Farmland)
        && old.block().base_block() == Some(new.block())
}

/// How `state` at `pos` reacts to its neighbour in `direction` becoming
/// `neighbour`: its new state, or air if it breaks (see [`remains`]).
pub fn update_shape(
    state: BlockState,
    pos: BlockPos,
    direction: Facing,
    neighbour: BlockState,
    level: Level,
) -> BlockState {
    if is_air(state) {
        return state;
    }
    let block = state.block();
    if is_chest(block) {
        return chest_update(state, direction, neighbour);
    }
    if ticks_when_covered(block) || scaffolding::is_scaffolding(block) {
        return state;
    }
    if speleothem::is_speleothem(block) {
        let level = |p: BlockPos| {
            if p == step(pos, direction) {
                neighbour
            } else {
                level(p)
            }
        };
        return speleothem::update_shape(state, pos, direction, &level);
    }
    {
        let level = |p: BlockPos| {
            if p == step(pos, direction) {
                neighbour
            } else {
                level(p)
            }
        };
        if let Some(updated) = block_support::update_shape(state, pos, direction, neighbour, &level)
        {
            return updated;
        }
    }
    if block == Block::Fire {
        let level = |p: BlockPos| {
            if p == step(pos, direction) {
                neighbour
            } else {
                level(p)
            }
        };
        return if can_survive(state, pos, &level) {
            fire_state_with_age(pos, &level, state.age().unwrap_or(0))
        } else {
            AIR
        };
    }
    if let Some(concrete) = concrete_from_powder(block) {
        let level = |p: BlockPos| {
            if p == step(pos, direction) {
                neighbour
            } else {
                level(p)
            }
        };
        return if touches_water(pos, &level) {
            concrete.default_state()
        } else {
            state
        };
    }
    if let Some((stem, fruit)) = block.attached_stem()
        && state.facing() == Some(direction)
        && neighbour.block() != fruit
    {
        return stem.default_state().with("age", "7").unwrap_or(AIR);
    }
    let vertical_partner =
        |lower: bool| !direction.is_horizontal() && lower == (direction == Facing::Up);
    let updated = match block.kind() {
        BlockKind::DoublePlant => {
            let half = state.half();
            if vertical_partner(half == Some(Half::Lower))
                && !(neighbour.block() == block && neighbour.half() != half)
            {
                return AIR;
            }
            state
        }
        BlockKind::Door => {
            let half = state.half();
            if vertical_partner(half == Some(Half::Lower)) {
                return match half {
                    Some(half)
                        if neighbour.block().kind() == BlockKind::Door
                            && neighbour.half() != Some(half) =>
                    {
                        neighbour.with_half(half).unwrap_or(AIR)
                    }
                    _ => AIR,
                };
            }
            state
        }
        BlockKind::Bed => {
            let (Some(part), Some(facing)) = (state.part(), state.facing()) else {
                return state;
            };
            let toward = if part == Part::Foot {
                facing
            } else {
                facing.opposite()
            };
            if direction == toward {
                if neighbour.block() != block || neighbour.part() == Some(part) {
                    return AIR;
                }
                or_keep(
                    state,
                    neighbour.occupied().and_then(|o| state.with_occupied(o)),
                )
            } else {
                state
            }
        }
        BlockKind::Stairs if direction.is_horizontal() => {
            or_keep(state, state.with_shape(stairs_shape(state, pos, level)))
        }
        BlockKind::Fence if direction.is_horizontal() => {
            let connects = fence_connects(block, neighbour, direction.opposite());
            or_keep(state, state.with(direction.name(), bool_name(connects)))
        }
        BlockKind::Bars if direction.is_horizontal() => {
            let connects = bars_connect(neighbour, direction.opposite());
            or_keep(state, state.with(direction.name(), bool_name(connects)))
        }
        BlockKind::Wall if direction == Facing::Up => {
            wall_state(state, wall_connections(state), neighbour)
        }
        BlockKind::Wall if direction.is_horizontal() => {
            let mut connections = wall_connections(state);
            if let Some(index) = Facing::HORIZONTAL.iter().position(|d| *d == direction) {
                connections[index] = wall_connects(neighbour, direction.opposite());
            }
            wall_state(state, connections, level(step(pos, Facing::Up)))
        }
        BlockKind::FenceGate
            if state
                .facing()
                .is_some_and(|f| f.clockwise().axis() == direction.axis()) =>
        {
            let in_wall = is_wall(neighbour) || is_wall(level(step(pos, direction.opposite())));
            or_keep(state, state.with_in_wall(in_wall))
        }
        BlockKind::Snowy if direction == Facing::Up => or_keep(
            state,
            state.with_snowy(tuff_datapack::tags::is(tags::key::SNOW, neighbour.block())),
        ),
        BlockKind::HugeMushroom if neighbour.block() == block => {
            or_keep(state, state.with(direction.name(), "false"))
        }
        BlockKind::NoteBlock if !direction.is_horizontal() => {
            let level = |p: BlockPos| {
                if p == step(pos, direction) {
                    neighbour
                } else {
                    level(p)
                }
            };
            or_keep(state, state.with_instrument(note_instrument(pos, &level)))
        }
        BlockKind::MossyCarpet => {
            let level = |p: BlockPos| {
                if p == step(pos, direction) {
                    neighbour
                } else {
                    level(p)
                }
            };
            if !can_survive(state, pos, &level) {
                return AIR;
            }
            let updated = mossy_carpet_state(state, pos, &level, false);
            if mossy_carpet_has_faces(updated) {
                updated
            } else {
                AIR
            }
        }
        BlockKind::Vine => {
            let mut state = state;
            for face in faces(state).collect::<Vec<_>>() {
                if !vine_supported(block, pos, face, level) {
                    state = or_keep(state, state.with(face.name(), "false"));
                }
            }
            state
        }
        BlockKind::Multiface => {
            let mut state = state;
            for face in faces(state).collect::<Vec<_>>() {
                if !attaches_to(pos, face, level) {
                    state = or_keep(state, state.with(face.name(), "false"));
                }
            }
            state
        }
        BlockKind::FacingPlayer
            if direction == Facing::Down
                && matches!(block, Block::Campfire | Block::SoulCampfire) =>
        {
            or_keep(
                state,
                state.with_signal_fire(neighbour.block() == Block::HayBlock),
            )
        }
        BlockKind::GrowingPlantUp | BlockKind::GrowingPlantDown => {
            growing_head_update(state, pos, direction, neighbour, level)
        }
        _ => state,
    };
    if can_survive(updated, pos, level) || block_support::defers_break(updated.block()) {
        updated
    } else {
        AIR
    }
}

/// `state` fitted to every neighbour of `pos`, as a block that changes in
/// place without being placed (a stripped log, tilled farmland) is.
pub fn update_from_neighbours(state: BlockState, pos: BlockPos, level: Level) -> BlockState {
    UPDATE_ORDER.into_iter().fold(state, |state, direction| {
        update_shape(state, pos, direction, level(step(pos, direction)), level)
    })
}

/// The instrument a note block at `pos` plays: a mob head on top wins,
/// otherwise the block below chooses (heads below count as harp).
pub fn note_instrument(pos: BlockPos, level: Level) -> Instrument {
    let above = level(step(pos, Facing::Up)).block().instrument();
    if above.works_above_note_block() {
        return above;
    }
    let below = level(step(pos, Facing::Down)).block().instrument();
    if below.works_above_note_block() {
        Instrument::Harp
    } else {
        below
    }
}
