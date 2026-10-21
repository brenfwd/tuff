use super::*;
use crate::tests::with_context;

#[tuff_core::watchdog]
#[test]
fn walkers_in_powder_snow_with_room_above_jump_and_others_do_not() {
    with_context(|core, cx| {
        let mut goal = ClimbOnTopOfPowderSnow;
        core.base.position = [0.5, 70.0, 0.5];
        core.base.kind = EntityType::Rabbit;
        assert!(!goal.can_use(core, cx));
        core.body.in_powder_snow = true;
        assert!(goal.can_use(core, cx));
        goal.tick(core, cx);
        assert!(core.jump.jump);
        core.base.kind = EntityType::Zombie;
        assert!(!goal.can_use(core, cx));
    });
}
