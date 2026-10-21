fn boxed(id: i32, x: f64, z: f64, half: f64) -> EntitySummary {
    EntitySummary {
        id,
        uuid: 0,
        kind: EntityType::Pig,
        position: [x, 0.0, z],
        velocity: [0.0; 3],
        yaw: 0.0,
        bounds: [x - half, 0.0, z - half, x + half, 1.0, z + half],
        living: None,
        pickable: false,
        item: None,
        vehicle: None,
        carries: false,
        ominous_banner: false,
        blue_sheep: false,
    }
}

#[tuff_core::watchdog]
#[test]
fn grid_candidates_cover_overlaps_and_skip_the_far_field() {
    let mut all: Vec<EntitySummary> = (0..400)
        .map(|i| boxed(i, f64::from(i % 20) * 6.0, f64::from(i / 20) * 6.0, 0.3))
        .collect();
    all.push(boxed(1000, 50.0, 50.0, 8.0));
    let mut grid = EntityGrid::build(&all);
    let probe = [11.8, 0.0, 5.8, 12.4, 1.0, 6.4];
    let near = grid.candidates(&probe);
    assert!(near.len() < 10, "{} candidates", near.len());
    assert!(near.contains(&(all.len() as u32 - 1)));
    let hit = all
        .iter()
        .position(|e| {
            e.bounds[0] <= probe[3]
                && e.bounds[3] >= probe[0]
                && e.bounds[2] <= probe[5]
                && e.bounds[5] >= probe[2]
                && e.id < 1000
        })
        .unwrap();
    assert!(near.contains(&(hit as u32)));
    all[0] = boxed(0, 12.1, 6.1, 0.3);
    grid.replace(0, &all[0]);
    assert!(grid.candidates(&probe).contains(&0));
    assert!(
        !grid
            .candidates(&[0.0, 0.0, 0.0, 0.5, 1.0, 0.5])
            .contains(&0)
    );
}
use super::*;

#[tuff_core::watchdog]
#[test]
fn delta_packets_skip_values_viewers_already_have() {
    let inert = crate::world::inert_entity::Inert::load(
        tuff_vanilla_data::registry::EntityType::Cow,
        1,
        &Compound::new().with("Pos", Tag::List([0.0f64; 3].map(Tag::Double).to_vec())),
    )
    .unwrap();
    let mut tracked = TrackedEntity::new(Box::new(inert), 0);
    assert!(tracked.flags_packets().is_none());
    let on_fire = [(metadata::ENTITY_FLAGS, MetadataValue::Byte(1))];
    assert!(tracked.delta_packets(&on_fire).is_some());
    assert!(tracked.delta_packets(&on_fire).is_none());
    assert!(tracked.flags_packets().is_some());
}

/// The player is not a `RegionEntity` (it is driven by a session, not
/// the registry above), so `every_registered_type_sends_data_in_its_own_layout`
/// never checks it. Pin its hand-derived metadata indices against the
/// dumped layout directly so they cannot silently drift from it.
#[tuff_core::watchdog]
#[test]
fn player_metadata_indices_match_the_dumped_layout() {
    let layout = tuff_vanilla_data::registry::EntityType::Player.data_layout();
    let checks: &[(&str, u8, i32)] = &[
        (
            "ENTITY_FLAGS",
            metadata::ENTITY_FLAGS,
            MetadataValue::Byte(0).type_id(),
        ),
        (
            "AIR_SUPPLY",
            metadata::AIR_SUPPLY,
            MetadataValue::VarInt(0).type_id(),
        ),
        ("POSE", metadata::POSE, MetadataValue::Pose(0).type_id()),
        (
            "LIVING_FLAGS",
            metadata::LIVING_FLAGS,
            MetadataValue::Byte(0).type_id(),
        ),
        (
            "HEALTH",
            metadata::HEALTH,
            MetadataValue::Float(0.0).type_id(),
        ),
        (
            "EFFECT_PARTICLES",
            metadata::EFFECT_PARTICLES,
            MetadataValue::Particles(Vec::new()).type_id(),
        ),
        (
            "EFFECT_AMBIENCE",
            metadata::EFFECT_AMBIENCE,
            MetadataValue::Boolean(false).type_id(),
        ),
        (
            "SLEEPING_POS",
            metadata::SLEEPING_POS,
            MetadataValue::OptionalPosition(None).type_id(),
        ),
        (
            "MAIN_ARM",
            metadata::MAIN_ARM,
            MetadataValue::HumanoidArm(1).type_id(),
        ),
        (
            "SKIN_PARTS",
            metadata::SKIN_PARTS,
            MetadataValue::Byte(0).type_id(),
        ),
        (
            "ABSORPTION",
            metadata::ABSORPTION,
            MetadataValue::Float(0.0).type_id(),
        ),
        ("SCORE", metadata::SCORE, MetadataValue::VarInt(0).type_id()),
        (
            "SHOULDER_LEFT",
            metadata::SHOULDER_LEFT,
            MetadataValue::OptionalVarInt(None).type_id(),
        ),
        (
            "SHOULDER_RIGHT",
            metadata::SHOULDER_RIGHT,
            MetadataValue::OptionalVarInt(None).type_id(),
        ),
    ];
    for (name, index, expected_type) in checks {
        assert_eq!(
            layout.get(usize::from(*index)).copied().map(i32::from),
            Some(*expected_type),
            "metadata::{name} (index {index}) does not match the player's dumped data layout"
        );
    }
}

#[tuff_core::watchdog]
#[test]
fn passengers_list_in_boarding_order_across_players_and_entities() {
    let mut base = EntityBase::new(EntityType::Pig, 1, 1, [0.0; 3], [0.0; 3]);
    base.entity_riders.push(30);
    base.note_boarded(30);
    let (handle, _events) = crate::world::messages::PlayerHandle::new(9);
    base.riders.push(Rider {
        id: 9,
        uuid: 0x99,
        handle,
    });
    base.note_boarded(9);
    assert_eq!(all_passengers(&base), vec![30, 9]);
    assert_eq!(base.seat_index(9), (1, 2));
    base.entity_riders.clear();
    base.note_boarded(9);
    assert_eq!(all_passengers(&base), vec![9]);
}

#[tuff_core::watchdog]
#[test]
fn live_riders_are_saved_as_offline_passenger_markers() {
    let mut base = EntityBase::new(EntityType::Pig, 1, 1, [0.0; 3], [0.0; 3]);
    let (handle, _events) = crate::world::messages::PlayerHandle::new(9);
    base.riders.push(Rider {
        id: 9,
        uuid: 0x99,
        handle,
    });
    base.offline_riders.push(0x77);
    let mut out = Compound::new();
    base.save(&mut out);
    // `EntityBase::save` itself does not write Passengers (the wrapper
    // `save()` does, since it also needs `base.riders`); check the
    // wrapper's merge logic directly through `saved_passengers`.
    let passengers = saved_passengers(&base);
    assert_eq!(passengers.len(), 2);
    let uuids: Vec<u128> = passengers
        .iter()
        .filter_map(|c| c.get("UUID").and_then(Tag::as_uuid))
        .collect();
    assert!(uuids.contains(&0x99), "{uuids:?}");
    assert!(uuids.contains(&0x77), "{uuids:?}");
}

#[tuff_core::watchdog]
#[test]
fn glowing_effect_sets_the_glowing_flag_without_saving_the_tag() {
    let mut base = EntityBase::new(EntityType::Pig, 1, 1, [0.0; 3], [0.0; 3]);
    base.glowing_effect = true;
    assert_eq!(base.flags(), metadata::FLAG_GLOWING);
    let mut saved = Compound::new();
    base.save(&mut saved);
    assert_ne!(saved.get_bool("Glowing"), Some(true));
}

#[tuff_core::watchdog]
#[test]
fn common_entity_data_is_sent_only_when_set() {
    let mut base = EntityBase::new(EntityType::Tnt, 1, 1, [0.0; 3], [0.0; 3]);
    assert!(base.metadata().is_empty());
    base.fire = 10;
    base.glowing = true;
    base.custom_name = Some(Tag::from("Boom"));
    base.no_gravity = true;
    let values = base.metadata();
    assert_eq!(
        values,
        vec![
            (
                metadata::ENTITY_FLAGS,
                MetadataValue::Byte(metadata::FLAG_ON_FIRE | metadata::FLAG_GLOWING)
            ),
            (
                metadata::CUSTOM_NAME,
                MetadataValue::OptionalText(Some(TextComponent::new("Boom")))
            ),
            (metadata::NO_GRAVITY, MetadataValue::Boolean(true)),
        ]
    );
}
