use super::*;
use tuff_vanilla_data::growth::with_properties_of;

/// Whether a chest of `block` joins with `neighbour`: copper chests join
/// any copper chest, others only their own block.
pub(super) fn chest_connects(block: Block, neighbour: BlockState) -> bool {
    if tuff_datapack::tags::is(tags::key::COPPER_CHESTS, block) {
        tuff_datapack::tags::is(tags::key::COPPER_CHESTS, neighbour.block())
    } else {
        neighbour.block() == block
    }
}

pub(super) fn chest_candidate(
    block: Block,
    pos: BlockPos,
    side: Facing,
    level: Level,
) -> Option<Facing> {
    let state = level(step(pos, side));
    (chest_connects(block, state) && state.r#type() == Some(Type::Single))
        .then(|| state.facing())
        .flatten()
}

/// The merge type (single, or joined left/right into a double chest) a
/// chest placed at `pos` facing `facing` picks up from its two side
/// neighbours -- the non-sneaking default-placement rule, shared by manual
/// placement and copper golem assembly (see
/// [`chest_state_for_facing`]).
pub(super) fn chest_type_for(block: Block, pos: BlockPos, facing: Facing, level: Level) -> Type {
    if chest_candidate(block, pos, facing.clockwise(), level) == Some(facing) {
        Type::Left
    } else if chest_candidate(block, pos, facing.counter_clockwise(), level) == Some(facing) {
        Type::Right
    } else {
        Type::Single
    }
}

fn is_copper_chest(block: Block) -> bool {
    tuff_datapack::tags::is(tags::key::COPPER_CHESTS, block)
}

fn oxidation_stage(block: Block) -> u8 {
    match block.unwaxed().unwrap_or(block) {
        Block::CopperChest => 0,
        Block::ExposedCopperChest => 1,
        Block::WeatheredCopperChest => 2,
        _ => 3,
    }
}

fn unwaxed_state(state: BlockState) -> BlockState {
    state
        .block()
        .unwaxed()
        .map_or(state, |block| with_properties_of(block, state))
}

/// A joined copper chest takes the least oxidized block among the pair;
/// a waxed and an unwaxed chest are compared as unwaxed.
pub(super) fn least_oxidized_chest(state: BlockState, pos: BlockPos, level: Level) -> BlockState {
    let block = state.block();
    let Some(direction) = chest_connected_direction(state) else {
        return state;
    };
    let connected = level(step(pos, direction));
    if !is_copper_chest(block) || !is_copper_chest(connected.block()) {
        return state;
    }
    let (mut own, mut other) = (state, connected);
    if block.unwaxed().is_some() != connected.block().unwaxed().is_some() {
        own = unwaxed_state(own);
        other = unwaxed_state(other);
    }
    let least = if oxidation_stage(block) <= oxidation_stage(connected.block()) {
        own.block()
    } else {
        other.block()
    };
    with_properties_of(least, own)
}

pub(super) fn chest_state(base: BlockState, context: &Context, level: Level) -> Option<BlockState> {
    let block = base.block();
    let pos = context.pos;
    let mut facing = context.horizontal().opposite();
    let mut kind = Type::Single;
    if context.face.is_horizontal()
        && context.sneaking
        && let Some(partner) = chest_candidate(block, pos, context.face.opposite(), level)
        && partner.axis() != context.face.axis()
    {
        facing = partner;
        kind = if facing.counter_clockwise() == context.face.opposite() {
            Type::Right
        } else {
            Type::Left
        };
    }
    if kind == Type::Single && !context.sneaking {
        kind = chest_type_for(block, pos, facing, level);
    }
    let state = base.with_facing(facing)?.with_type(kind)?;
    Some(least_oxidized_chest(state, pos, level))
}

/// The state a copper golem's freshly assembled chest takes at `pos`,
/// facing `facing` (the pumpkin's own facing): the same default
/// (non-sneaking) merge rule a manually placed chest gets, since the wiki
/// describes the assembled chest as joining a neighbour "as it would if
/// the new copper chest had been placed manually".
pub fn chest_state_for_facing(
    block: Block,
    pos: BlockPos,
    facing: Facing,
    level: Level,
) -> Option<BlockState> {
    let kind = chest_type_for(block, pos, facing, level);
    let state = block.default_state().with_facing(facing)?.with_type(kind)?;
    Some(least_oxidized_chest(state, pos, level))
}

pub(super) fn chest_update(
    state: BlockState,
    direction: Facing,
    neighbour: BlockState,
) -> BlockState {
    let updated = chest_shape(state, direction, neighbour);
    if is_copper_chest(neighbour.block())
        && updated.r#type() != Some(Type::Single)
        && chest_connected_direction(updated) == Some(direction)
    {
        return with_properties_of(neighbour.block(), updated);
    }
    updated
}

fn chest_shape(state: BlockState, direction: Facing, neighbour: BlockState) -> BlockState {
    let block = state.block();
    if chest_connects(block, neighbour) && direction.is_horizontal() {
        let joined = state.r#type() == Some(Type::Single)
            && neighbour.r#type().is_some_and(|t| t != Type::Single)
            && state.facing() == neighbour.facing()
            && chest_connected_direction(neighbour) == Some(direction.opposite());
        if joined {
            let kind = match neighbour.r#type() {
                Some(Type::Left) => Type::Right,
                _ => Type::Left,
            };
            return or_keep(state, state.with_type(kind));
        }
    } else if chest_connected_direction(state) == Some(direction) {
        return or_keep(state, state.with_type(Type::Single));
    }
    state
}
