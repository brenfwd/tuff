use super::*;

pub(super) fn play_horn(world: &mut dyn RegionWorld, raid: &Raid, from: BlockPos) {
    let seed = world.rng().r#gen::<i64>();
    let origin = [
        f64::from(from.x) + 0.5,
        f64::from(from.y) + 0.5,
        f64::from(from.z) + 0.5,
    ];
    for player in world.seen_player_details() {
        let [px, py, pz] = player.position;
        let distance = ((origin[0] - px).powi(2) + (origin[2] - pz).powi(2)).sqrt();
        let in_raid = raid.bar.viewers.contains_key(&player.handle.id);
        if distance > 64.0 && !in_raid {
            continue;
        }
        let (x, z) = if distance > 0.0 {
            (
                px + 13.0 / distance * (origin[0] - px),
                pz + 13.0 / distance * (origin[2] - pz),
            )
        } else {
            (px, pz)
        };
        player
            .handle
            .send(PlayerEvent::Packets(packets([framed(&play::Sound {
                sound_event: SoundEvent::EventRaidHorn.id(),
                category: play::sound_category::NEUTRAL,
                position: [x, py, z],
                volume: 64.0,
                pitch: 1.0,
                seed,
            })])));
    }
}
