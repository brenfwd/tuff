use super::*;

/// How readily fire next to a block in `state` spreads onto it; nothing
/// waterlogged burns.
pub fn ignite_odds(state: BlockState) -> u8 {
    if state.waterlogged() == Some(true) {
        0
    } else {
        state.block().flammability().0
    }
}

/// How readily fire burns a block in `state` away.
pub fn burn_odds(state: BlockState) -> u8 {
    if state.waterlogged() == Some(true) {
        0
    } else {
        state.block().flammability().1
    }
}

pub fn can_burn(state: BlockState) -> bool {
    ignite_odds(state) > 0
}

/// Fire may burn at `pos` without a floor: something next to it burns.
pub fn valid_fire_location(pos: BlockPos, level: Level) -> bool {
    Facing::ALL
        .into_iter()
        .any(|direction| can_burn(level(step(pos, direction))))
}

/// The fire that lights at `pos`: soul fire on soul fire bases, else fire
/// that faces whatever burns around it unless it stands on something.
pub fn fire_state(pos: BlockPos, level: Level) -> BlockState {
    let below = level(step(pos, Facing::Down));
    if tuff_datapack::tags::is(tags::key::SOUL_FIRE_BASE_BLOCKS, below.block()) {
        return Block::SoulFire.default_state();
    }
    let fire = Block::Fire.default_state();
    if can_burn(below) || below.is_face_sturdy(Facing::Up, Support::Full) {
        return fire;
    }
    Facing::ALL
        .into_iter()
        .filter(|direction| *direction != Facing::Down)
        .fold(fire, |state, direction| {
            let burns = can_burn(level(step(pos, direction)));
            or_keep(state, state.with(direction.name(), bool_name(burns)))
        })
}

/// [`fire_state`] with `age` if it is plain fire.
pub fn fire_state_with_age(pos: BlockPos, level: Level, age: u8) -> BlockState {
    let state = fire_state(pos, level);
    if state.block() == Block::Fire {
        or_keep(state, state.with_age(age))
    } else {
        state
    }
}
