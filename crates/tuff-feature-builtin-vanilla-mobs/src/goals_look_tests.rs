use super::*;
use tuff_core::world::entity::NearbyPlayer;

fn player(position: [f64; 3], vehicle: Option<i32>) -> NearbyPlayer<'static> {
    NearbyPlayer {
        id: 1,
        uuid: 1,
        name: "",
        position,
        eye_height: 1.62,
        yaw: 0.0,
        pitch: 0.0,
        held: [None, None],
        creative: false,
        height: 1.8,
        sneaking: false,
        invisible: false,
        invulnerable: false,
        armor: [None, None, None, None],
        effects: 0,
        movement: [0.0; 3],
        vehicle,
        sleeping: None,
        last_hurt_by: None,
        last_hurt_mob: None,
        perchable: false,
        sprinting: false,
    }
}

fn flat(pos: tuff_world_model::BlockPos) -> Option<tuff_vanilla_data::BlockState> {
    Some(if pos.y < 64 {
        tuff_vanilla_data::Block::GrassBlock.default_state()
    } else {
        tuff_vanilla_data::Block::Air.default_state()
    })
}

fn looks(vehicle: Option<i32>) -> bool {
    let mut looked = false;
    crate::tests::with_players(
        &flat,
        &[],
        &[player([0.0, 64.0, 0.0], vehicle)],
        |core, cx| {
            core.base.position = [0.0, 64.0, 0.0];
            core.base.id = 7;
            let mut goal = LookAtPlayer::new(10.0);
            looked = (0..2000).any(|_| goal.can_use(core, cx));
        },
    );
    looked
}

fn follows(goal: &mut FollowOwner, distance: f64) -> bool {
    let mut used = false;
    crate::tests::with_players(
        &flat,
        &[],
        &[player([distance, 64.0, 0.0], None)],
        |core, cx| {
            core.base.position = [0.0, 64.0, 0.0];
            core.tame = Some(crate::animal::Tame {
                owner: Some(1),
                ..Default::default()
            });
            used = goal.can_use(core, cx);
        },
    );
    used
}

#[tuff_core::watchdog]
#[test]
fn follow_owner_starts_at_the_species_distance() {
    assert!(!follows(&mut FollowOwner::new(1.0), 9.0));
    assert!(follows(&mut FollowOwner::new(1.0), 10.0));
    assert!(!follows(&mut FollowOwner::flying(1.0), 4.0));
    assert!(follows(&mut FollowOwner::flying(1.0), 5.0));
    assert!(!follows(
        &mut FollowOwner::with_distances(1.0, 10.0, 5.0),
        9.0
    ));
}

#[tuff_core::watchdog]
#[test]
fn a_mob_looks_at_a_nearby_player_but_not_one_riding_it() {
    assert!(looks(None));
    assert!(looks(Some(99)));
    assert!(!looks(Some(7)));
}
