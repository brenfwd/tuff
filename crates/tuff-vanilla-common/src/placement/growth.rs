use super::*;

/// A stand-in for the random starting age (0-24) of a head grown from a
/// cut body, derived from its position so the update stays pure.
pub(super) fn position_age(pos: BlockPos) -> u8 {
    let mut h = (pos.x as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (pos.y as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
        ^ (pos.z as u64).wrapping_mul(0x1656_67B1_9E37_79F9);
    h ^= h >> 29;
    h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^= h >> 32;
    (h % 25) as u8
}

/// A growing plant's head turns into its body once the plant continues past
/// it.
pub(super) fn growing_head_update(
    state: BlockState,
    pos: BlockPos,
    direction: Facing,
    neighbour: BlockState,
    level: Level,
) -> BlockState {
    let block = state.block();
    let Some(plant) = block.growing_plant() else {
        return state;
    };
    let part_of = |s: BlockState| s.block() == plant.head || s.block() == plant.body;
    if plant.body == block {
        if direction == plant.direction && !part_of(neighbour) {
            let head = plant.head.default_state();
            let head = head.with_age(position_age(pos)).unwrap_or(head);
            return state
                .berries()
                .and_then(|berries| head.with_berries(berries))
                .unwrap_or(head);
        }
        return state;
    }
    if plant.head != block {
        return state;
    }
    let body = || {
        let body = plant.body.default_state();
        state
            .berries()
            .and_then(|berries| body.with_berries(berries))
            .unwrap_or(body)
    };
    if direction == plant.direction && part_of(neighbour) {
        return body();
    }
    if direction == plant.direction.opposite()
        && can_survive(state, pos, level)
        && part_of(level(step(pos, plant.direction)))
    {
        return body();
    }
    state
}
