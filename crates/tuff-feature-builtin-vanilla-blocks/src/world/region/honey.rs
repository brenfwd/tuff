//! Landing on a honey block: the slide sound and the landing particles
//! (entity event 54) for the one that landed.

use tuff_core::protocol::play;
use tuff_core::world::region_api::{Landing, PARTICLE_RANGE, RegionWorld, SOUND_RANGE};
use tuff_core::world::{framed, packets};
use tuff_vanilla_data::registry::{EntityType, SoundEvent};

const HONEY_LANDING_EVENT: i8 = 54;

pub fn landed(world: &mut dyn RegionWorld, landing: &Landing) {
    let player = landing.kind == EntityType::Player;
    let sound = framed(&play::Sound {
        sound_event: SoundEvent::BlockHoneyBlockSlide.id(),
        category: if player {
            play::sound_category::PLAYERS
        } else {
            play::sound_category::NEUTRAL
        },
        position: landing.position,
        volume: 1.0,
        pitch: 1.0,
        seed: rand::Rng::r#gen(world.rng()),
    });
    world.send_near(
        landing.position,
        SOUND_RANGE,
        player.then_some(landing.entity),
        packets([sound]),
    );
    let event = framed(&play::EntityEvent {
        entity_id: landing.entity,
        status: HONEY_LANDING_EVENT,
    });
    world.send_near(landing.position, PARTICLE_RANGE, None, packets([event]));
}
