//! Region entity files round-tripping through the vanilla entity registry.

use rand::SeedableRng;
use rand::rngs::StdRng;
use tuff_vanilla_data::Item;

use tuff_core::world::entity::{self, RegionEntity};
use tuff_core::world::messages::Igniter;
use tuff_feature_builtin_vanilla_combat::world::explosion::PrimedTnt;
use tuff_feature_builtin_vanilla_items::world::items::ItemEntity;
use tuff_feature_builtin_vanilla_survival::world::experience_orb::ExperienceOrb;
use tuff_item::item::ItemStack;
use tuff_nbt::{Compound, Tag};
use tuff_storage::entities::{decode, encode};
use tuff_world_model::BlockPos;
use tuff_world_model::pos::ChunkPos;

fn item() -> ItemEntity {
    let mut item = ItemEntity::new(
        7,
        0x1234,
        ItemStack::new(Item::Dirt, 3),
        [1.5, 70.0, -2.5],
        [0.0, -0.1, 0.0],
        4,
    );
    item.base.on_ground = true;
    item.age = 100;
    item.base.extra = Compound::new().with("Thrower", Tag::uuid(9));
    item
}

fn round_trip(pos: ChunkPos, saved: Vec<Compound>) -> (Vec<Box<dyn RegionEntity>>, Vec<Compound>) {
    let root = encode(pos, &saved).unwrap();
    assert_eq!(root.get_int_array("Position"), Some(&[pos.x, pos.z][..]));
    let mut next = 40;
    tuff_test_support::vanilla_registries()
        .entity_types
        .load_all(decode(&root, pos).unwrap(), || {
            next += 1;
            next
        })
}

#[tuff_core::watchdog]
#[test]
fn items_orbs_and_tnt_round_trip_and_other_entities_are_preserved() {
    let pig = Compound::new()
        .with("id", "minecraft:not_an_entity")
        .with("Health", 10.0f32);
    let mut rng = StdRng::seed_from_u64(1);
    let mut orb = ExperienceOrb::new(3, [1.0, 70.0, 1.0], 7, &mut rng);
    orb.count = 2;
    orb.age = 12;
    let tnt = PrimedTnt::lit(4, 0x77, BlockPos::new(1, 70, 1), 55, &mut rng).with_igniter(Some(
        Igniter {
            id: 9,
            uuid: 0x5151,
            name: "Alex".into(),
        },
    ));
    let entities: [&dyn RegionEntity; 3] = [&item(), &orb, &tnt];
    let mut saved: Vec<Compound> = entities.into_iter().map(entity::save).collect();
    saved.insert(0, pig.clone());
    let (live, raw) = round_trip(ChunkPos::new(0, -1), saved);
    assert_eq!(raw, vec![pig]);
    let loaded = live[0].downcast_ref::<ItemEntity>().unwrap();
    assert_eq!(loaded.base.id, 41);
    assert_eq!(loaded.base.uuid, 0x1234);
    assert_eq!(loaded.stack, ItemStack::new(Item::Dirt, 3));
    assert_eq!(loaded.base.position, [1.5, 70.0, -2.5]);
    assert!(loaded.base.on_ground);
    assert_eq!(
        (loaded.age, loaded.pickup_delay, loaded.health),
        (100, 4, 5)
    );
    assert_eq!(loaded.base.extra.get_uuid("Thrower"), Some(9));
    let loaded = live[1].downcast_ref::<ExperienceOrb>().unwrap();
    assert_eq!(
        (loaded.base.uuid, loaded.value, loaded.count, loaded.age),
        (orb.base.uuid, 7, 2, 12)
    );
    assert_eq!(loaded.base.velocity, orb.base.velocity);
    let loaded = live[2].downcast_ref::<PrimedTnt>().unwrap();
    assert_eq!(
        (loaded.base.uuid, loaded.fuse, loaded.owner),
        (0x77, 55, Some(0x5151))
    );
    assert_eq!(loaded.block, tuff_vanilla_data::Block::Tnt.default_state());
    assert_eq!(loaded.base.velocity, tnt.base.velocity);
    assert!(loaded.igniter.is_none());
}

#[tuff_core::watchdog]
#[test]
fn saved_items_and_tnt_use_the_vanilla_keys() {
    let saved = entity::save(&item());
    for key in [
        "id",
        "Pos",
        "Motion",
        "Rotation",
        "fall_distance",
        "Fire",
        "Air",
        "OnGround",
        "Invulnerable",
        "PortalCooldown",
        "UUID",
        "Health",
        "Age",
        "PickupDelay",
        "Item",
        "Thrower",
    ] {
        assert!(saved.contains_key(key), "{key}");
    }
    assert_eq!(saved.get_short("Health"), Some(5));
    let mut rng = StdRng::seed_from_u64(1);
    let mut tnt = PrimedTnt::lit(4, 1, BlockPos::new(0, 0, 0), 80, &mut rng);
    tnt.power = 8.0;
    let saved = entity::save(&tnt);
    assert_eq!(saved.get_short("fuse"), Some(80));
    assert_eq!(saved.get_str("block_state"), Some("minecraft:tnt"));
    assert_eq!(saved.get_float("explosion_power"), Some(8.0));
    assert!(!saved.contains_key("owner"));
}

#[tuff_core::watchdog]
#[test]
fn empty_chunks_encode_to_none_and_bad_positions_fail() {
    assert!(encode(ChunkPos::new(0, 0), &[]).is_none());
    let root = Compound::new().with("Position", vec![3, 3]);
    assert!(decode(&root, ChunkPos::new(0, 0)).is_err());
    let modded = Compound::new()
        .with("id", "minecraft:item")
        .with(
            "Item",
            Compound::new().with("id", "modded:gem").with("count", 1),
        )
        .with("Pos", Tag::List(vec![Tag::Double(0.0); 3]));
    let (live, raw) = round_trip(ChunkPos::new(0, 0), vec![modded.clone()]);
    assert!(live.is_empty());
    assert_eq!(raw, vec![modded]);
}

#[tuff_core::watchdog]
#[test]
fn item_components_survive_the_entity_round_trip() {
    let mut entity = item();
    entity.stack = ItemStack::new(Item::DiamondPickaxe, 1);
    entity.stack.set_damage(12);
    entity
        .stack
        .components
        .other
        .insert("minecraft:custom_data", Compound::new().with("k", 1));
    let (live, _) = round_trip(ChunkPos::new(0, 0), vec![entity::save(&entity)]);
    assert_eq!(
        live[0].downcast_ref::<ItemEntity>().unwrap().stack,
        entity.stack
    );
}
