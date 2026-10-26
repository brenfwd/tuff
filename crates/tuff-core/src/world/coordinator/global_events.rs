/// Where a global level event in `dimension` at `pos` is sent to a player
/// at `from` in `player_dimension`: players elsewhere get it at their own
/// position, the others within 32 blocks of themselves.
pub(super) fn global_event_position(
    dimension: crate::world::dimension::DimensionId,
    pos: tuff_world_model::BlockPos,
    player_dimension: crate::world::dimension::DimensionId,
    from: [f64; 3],
) -> tuff_world_model::BlockPos {
    if player_dimension != dimension {
        return tuff_world_model::BlockPos::new(
            from[0].floor() as i32,
            from[1].floor() as i32,
            from[2].floor() as i32,
        );
    }
    let center = pos.center();
    let delta = [
        center[0] - from[0],
        center[1] - from[1],
        center[2] - from[2],
    ];
    let distance = (delta[0] * delta[0] + delta[1] * delta[1] + delta[2] * delta[2]).sqrt();
    if distance <= 32.0 {
        return pos;
    }
    let scale = 32.0 / distance;
    tuff_world_model::BlockPos::new(
        (from[0] + delta[0] * scale).floor() as i32,
        (from[1] + delta[1] * scale).floor() as i32,
        (from[2] + delta[2] * scale).floor() as i32,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::dimension::DimensionId;
    use tuff_world_model::BlockPos;

    #[tuff_core::watchdog]
    #[test]
    fn other_dimensions_get_their_own_position_and_the_same_one_is_pulled_in_range() {
        let (overworld, nether) = (DimensionId::for_tests(0), DimensionId::for_tests(1));
        let pos = BlockPos::new(1000, 70, 0);
        assert_eq!(
            global_event_position(overworld, pos, nether, [5.5, 70.0, -3.5]),
            BlockPos::new(5, 70, -4)
        );
        assert_eq!(
            global_event_position(overworld, pos, overworld, [980.0, 70.0, 0.0]),
            pos
        );
        let pulled = global_event_position(overworld, pos, overworld, [0.0, 70.5, 0.5]);
        assert!((31..=32).contains(&pulled.x), "{pulled:?}");
    }
}
