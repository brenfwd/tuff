//! The vanilla entity registry as the plugin set fills it: every
//! registered type's data layout, air rules and common-field round trips.

use tuff_core::protocol::play::MetadataValue;
use tuff_core::world::entity::{Registry, save};
use tuff_nbt::{Compound, Tag};
use tuff_vanilla_data::registry::{EntityType, Registry as _};

fn entity_types() -> Registry {
    tuff_test_support::vanilla_registries().entity_types.clone()
}

/// Saved data turning on every flag and optional field any registered
/// type reads; each type ignores what it does not know.
fn every_field(kind: EntityType) -> Compound {
    let item = Compound::new()
        .with("id", "minecraft:stone")
        .with("count", 1);
    Compound::new()
        .with("id", kind.name())
        .with(
            "Pos",
            vec![Tag::Double(0.5), Tag::Double(64.0), Tag::Double(0.5)],
        )
        .with("Fire", 100i16)
        .with("HasVisualFire", true)
        .with("Glowing", true)
        .with("CustomName", "name")
        .with("CustomNameVisible", true)
        .with("Silent", true)
        .with("NoGravity", true)
        .with("Item", item)
        .with("Value", 7i16)
        .with("fuse", 40i16)
        .with("Health", 5.0f32)
        .with("IsBaby", true)
        .with("Age", -24000)
        .with("AgeLocked", true)
        .with("LeftHanded", true)
        .with("NoAI", true)
        .with("powered", true)
        .with("ignited", true)
        .with("IsImmuneToZombification", true)
        .with("Size", 3)
        .with("Sheared", true)
        .with("carriedBlockState", "minecraft:grass_block")
        .with("ConversionTime", 100)
        .with("PierceLevel", 2i8)
        .with("crit", true)
        .with("inGround", true)
        .with("beam_target", Tag::IntArray(vec![0, 70, 0]))
        .with("ShowBottom", false)
}

fn check_layout(
    kind: EntityType,
    what: &str,
    values: &[(u8, MetadataValue)],
    failures: &mut Vec<String>,
) {
    let mut seen = Vec::new();
    for (index, value) in values {
        if seen.contains(index) {
            failures.push(format!("{}: {what} sends index {index} twice", kind.name()));
        }
        seen.push(*index);
        let expected = kind.data_serializer(*index);
        if expected.map(i32::from) != Some(value.type_id()) {
            failures.push(format!(
                "{}: {what} sends index {index} as serializer {} ({value:?}), layout has {expected:?}",
                kind.name(),
                value.type_id(),
            ));
        }
    }
}

#[tuff_core::watchdog]
#[test]
fn every_registered_type_sends_data_in_its_own_layout() {
    let registry = entity_types();
    let mut failures = Vec::new();
    for kind in registry.kinds() {
        let mut entity = registry
            .load(&every_field(kind), 1)
            .unwrap_or_else(|| panic!("{} does not load", kind.name()));
        let mut spawn = entity.base().metadata();
        spawn.extend(entity.metadata());
        check_layout(kind, "spawn", &spawn, &mut failures);
        for index in 0..kind.data_layout().len() as u8 {
            entity.resend_data(index);
        }
        let update = entity.take_changed_data();
        if entity.living().is_some() && update.is_empty() {
            failures.push(format!("{}: resent no data", kind.name()));
        }
        check_layout(kind, "update", &update, &mut failures);
        for (index, value) in &update {
            if let Some((_, sent)) = spawn.iter().find(|(i, _)| i == index)
                && sent != value
            {
                failures.push(format!(
                    "{}: index {index} spawns as {sent:?} but updates as {value:?}",
                    kind.name()
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Mobs all breathe by the common rules (a tag lets a type breathe
/// under water); a type whose game code changes them (fish, dolphins,
/// iron golems, ...) needs its own rules (`Species::air_rules`) before
/// it is registered. `HANDLED` is the types that already have them;
/// any other registered type with the dumped flag fails the build.
const HANDLED_OWN_AIR_RULES: &[EntityType] = &[
    EntityType::Cod,
    EntityType::Salmon,
    EntityType::Pufferfish,
    EntityType::TropicalFish,
    EntityType::Axolotl,
    EntityType::Tadpole,
    EntityType::IronGolem,
    EntityType::Dolphin,
    EntityType::Squid,
    EntityType::GlowSquid,
    EntityType::HappyGhast,
    EntityType::Nautilus,
    EntityType::SulfurCube,
];

#[tuff_core::watchdog]
#[test]
fn registered_types_breathe_by_the_common_rules() {
    let registry = entity_types();
    let own: Vec<&str> = registry
        .kinds()
        .into_iter()
        .filter(|kind| kind.mob().own_air_rules && !HANDLED_OWN_AIR_RULES.contains(kind))
        .map(|kind| kind.name())
        .collect();
    assert!(own.is_empty(), "types with their own air rules: {own:?}");
}

/// A type is living when its data layout carries the living-entity
/// fields (flags, health, effects, arrows, stingers, sleeping spot)
/// right after the common entity fields.
fn is_living(kind: EntityType) -> bool {
    const LIVING_FIELDS: std::ops::RangeInclusive<usize> = 8..=14;
    let living = &EntityType::Pig.data_layout()[LIVING_FIELDS];
    kind.data_layout().get(LIVING_FIELDS) == Some(living)
}

#[tuff_core::watchdog]
#[test]
fn every_living_type_is_registered() {
    let registry = entity_types();
    let registered = registry.kinds();
    let missing: Vec<&str> = EntityType::ALL
        .iter()
        .copied()
        .filter(|kind| *kind != EntityType::Player && is_living(*kind))
        .filter(|kind| !registered.contains(kind))
        .map(EntityType::name)
        .collect();
    assert!(
        missing.is_empty(),
        "living types with no entity: {missing:?}"
    );
}

fn full() -> Compound {
    Compound::new()
        .with("id", "minecraft:tnt")
        .with(
            "Pos",
            vec![Tag::Double(1.5), Tag::Double(64.0), Tag::Double(-3.5)],
        )
        .with(
            "Motion",
            vec![Tag::Double(0.1), Tag::Double(20.0), Tag::Double(0.0)],
        )
        .with("Rotation", vec![Tag::Float(90.0), Tag::Float(-10.0)])
        .with("fall_distance", 1.5)
        .with("Fire", 40i16)
        .with("Air", 200i16)
        .with("OnGround", true)
        .with("Invulnerable", true)
        .with("PortalCooldown", 7)
        .with("UUID", Tag::uuid(0xabc))
        .with("CustomName", "Boom")
        .with("CustomNameVisible", true)
        .with("NoGravity", true)
        .with("Glowing", true)
        .with("Tags", vec![Tag::from("a"), Tag::from("b")])
        .with(
            "Passengers",
            vec![Compound::new().with("id", "minecraft:pig")],
        )
        .with("data", Compound::new().with("k", 1))
        .with("fuse", 30i16)
}

#[tuff_core::watchdog]
#[test]
fn common_fields_load_and_save_losslessly() {
    let registry = entity_types();
    let saved = full();
    let entity = registry.load(&saved, 5).unwrap();
    let base = entity.base();
    assert_eq!(base.id, 5);
    assert_eq!(base.uuid, 0xabc);
    assert_eq!(base.velocity, [0.1, 0.0, 0.0], "wild motion is dropped");
    assert_eq!((base.yaw, base.pitch), (90.0, -10.0));
    assert_eq!((base.fire, base.air, base.portal_cooldown), (40, 200, 7));
    assert!(base.invulnerable && base.no_gravity && base.glowing);
    assert_eq!(base.tags, ["a", "b"]);
    assert_eq!(base.passengers.len(), 1);
    assert_eq!(
        base.extra,
        Compound::new().with("data", Compound::new().with("k", 1))
    );
    let again = save(&*entity);
    for (key, value) in saved.iter().filter(|(key, _)| *key != "Motion") {
        assert_eq!(again.get(key), Some(value), "{key}");
    }
    let keys: Vec<&str> = again.keys().collect();
    assert_eq!(keys.first(), Some(&"id"));
    assert_eq!(keys.last(), Some(&"Passengers"));
}

#[tuff_core::watchdog]
#[test]
fn offline_riders_round_trip_through_passengers() {
    let registry = entity_types();
    let saved = full().with(
        "Passengers",
        vec![
            Compound::new().with("id", "minecraft:pig"),
            Compound::new()
                .with("id", "minecraft:player")
                .with("UUID", Tag::uuid(0x42)),
        ],
    );
    let entity = registry.load(&saved, 5).unwrap();
    let base = entity.base();
    assert_eq!(
        base.passengers,
        vec![Compound::new().with("id", "minecraft:pig")]
    );
    assert_eq!(base.offline_riders, vec![0x42]);

    let again = save(&*entity);
    let passengers = again.get_list("Passengers").unwrap();
    assert_eq!(passengers.len(), 2);
    assert!(
        passengers.contains(&Tag::Compound(
            Compound::new()
                .with("id", "minecraft:player")
                .with("UUID", Tag::uuid(0x42))
        ))
    );
}

#[tuff_core::watchdog]
#[test]
fn unexpected_value_types_are_saved_back_unchanged() {
    let registry = entity_types();
    let saved = full().with("PortalCooldown", "soon");
    let entity = registry.load(&saved, 1).unwrap();
    assert_eq!(entity.base().portal_cooldown, 0);
    assert_eq!(save(&*entity).get_str("PortalCooldown"), Some("soon"));
}

#[tuff_core::watchdog]
#[test]
fn unknown_types_and_bad_positions_stay_raw() {
    let registry = entity_types();
    let pig = Compound::new()
        .with("id", "minecraft:not_an_entity")
        .with("Pos", vec![Tag::Double(0.0); 3]);
    let lost = full().with("Pos", vec![Tag::Double(f64::INFINITY); 3]);
    let mut next = 0;
    let (live, raw) = registry.load_all(vec![pig.clone(), lost.clone(), full()], || {
        next += 1;
        next
    });
    assert_eq!(raw, vec![pig, lost]);
    assert_eq!(live.len(), 1);
}
