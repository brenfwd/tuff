use super::*;

fn holding_stone(position: [f64; 3]) -> NearbyPlayer<'static> {
    NearbyPlayer {
        id: 1,
        uuid: 1,
        name: "",
        position,
        eye_height: 1.62,
        yaw: 0.0,
        pitch: 0.0,
        held: [Some(Item::Stone), None],
        creative: false,
        height: 1.8,
        sneaking: false,
        invisible: false,
        invulnerable: false,
        armor: [None, None, None, None],
        effects: 0,
        movement: [0.0; 3],
        vehicle: None,
        sleeping: None,
        last_hurt_by: None,
        last_hurt_mob: None,
        perchable: false,
        sprinting: false,
    }
}

fn flat(pos: BlockPos) -> Option<tuff_vanilla_data::BlockState> {
    Some(if pos.y < 64 {
        Block::GrassBlock.default_state()
    } else {
        Block::Air.default_state()
    })
}

#[tuff_core::watchdog]
#[test]
fn a_flier_wants_a_point_on_the_line_to_the_eyes_and_waits_inside_the_stop_distance() {
    let far = holding_stone([6.0, 70.0, 0.0]);
    crate::tests::with_players(&flat, &[], &[far], |core, cx| {
        core.base.position = [0.0, 64.0, 0.0];
        let mut goal = Tempt::new(1.0, |item| item == Item::Stone).without_pathfinding(3.0);
        assert!(goal.can_use(core, cx));
        goal.start(core, cx);
        goal.tick(core, cx);
        assert_eq!(core.movement.operation, Operation::MoveTo);
        let w = core.movement.wanted;
        assert!((0.0..=6.0).contains(&w[0]));
        assert!((64.0..=71.62).contains(&w[1]));
        assert!(core.navigation.is_done());
    });
    let near = holding_stone([1.0, 64.0, 0.0]);
    crate::tests::with_players(&flat, &[], &[near], |core, cx| {
        core.base.position = [0.0, 64.0, 0.0];
        let mut goal = Tempt::new(1.0, |item| item == Item::Stone).without_pathfinding(3.0);
        assert!(goal.can_use(core, cx));
        core.movement.operation = Operation::MoveTo;
        goal.tick(core, cx);
        assert_eq!(core.movement.operation, Operation::Wait);
    });
}
