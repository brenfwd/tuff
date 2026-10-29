//! Which mobs the despawn check removes when every player is far away:
//! animals, villagers and golems stay, hostile and wild water mobs go,
//! and name tags, buckets, taming and leads keep the ones that would go.

use super::*;
use crate::world::messages::PlayerLooks;
use tuff_nbt::Tag;
use tuff_vanilla_data::registry::EntityType as E;

struct Case {
    kind: E,
    nbt: Compound,
    stays: bool,
    why: &'static str,
}

fn case(kind: E, stays: bool, why: &'static str) -> Case {
    Case {
        kind,
        nbt: Compound::new(),
        stays,
        why,
    }
}

fn flagged(kind: E, nbt: Compound, stays: bool, why: &'static str) -> Case {
    Case {
        kind,
        nbt,
        stays,
        why,
    }
}

fn cases() -> Vec<Case> {
    let mut cases = Vec::new();
    for kind in [
        E::Cow,
        E::Mooshroom,
        E::Pig,
        E::Sheep,
        E::Chicken,
        E::Rabbit,
        E::Horse,
        E::Donkey,
        E::Mule,
        E::Llama,
        E::TraderLlama,
        E::SkeletonHorse,
        E::Camel,
        E::Wolf,
        E::Cat,
        E::Ocelot,
        E::Fox,
        E::Panda,
        E::PolarBear,
        E::Goat,
        E::Turtle,
        E::Frog,
        E::Sniffer,
        E::Armadillo,
        E::Bee,
        E::Parrot,
        E::Strider,
        E::HappyGhast,
        E::Allay,
        E::Villager,
        E::WanderingTrader,
        E::IronGolem,
        E::SnowGolem,
        E::CopperGolem,
        E::Shulker,
        E::Warden,
    ] {
        cases.push(case(kind, true, "animals and friendly mobs never despawn"));
    }
    cases.push(case(
        E::Tadpole,
        true,
        "tadpoles are always bucket-flagged and never despawn",
    ));
    for kind in [
        E::Zombie,
        E::Skeleton,
        E::Creeper,
        E::Spider,
        E::Enderman,
        E::Witch,
        E::Slime,
        E::Hoglin,
        E::Zoglin,
        E::Piglin,
        E::Phantom,
        E::Creaking,
        E::ZombieHorse,
        E::CamelHusk,
        E::Nautilus,
        E::ZombieNautilus,
        E::Cod,
        E::Salmon,
        E::TropicalFish,
        E::Pufferfish,
        E::Squid,
        E::GlowSquid,
        E::Dolphin,
        E::Bat,
        E::Axolotl,
        E::Guardian,
    ] {
        cases.push(case(kind, false, "wild hostile and water mobs despawn"));
    }
    let persistent = || Compound::new().with("PersistenceRequired", true);
    let named = || Compound::new().with("CustomName", "\"Bob\"");
    let bucket = || Compound::new().with("FromBucket", true);
    for kind in [E::Zombie, E::Skeleton, E::Cod, E::Squid, E::Horse] {
        cases.push(flagged(
            kind,
            persistent(),
            true,
            "persistence required stays",
        ));
    }
    for kind in [
        E::Cod,
        E::Salmon,
        E::TropicalFish,
        E::Pufferfish,
        E::Axolotl,
    ] {
        cases.push(flagged(kind, bucket(), true, "bucketed mobs stay"));
        cases.push(flagged(kind, named(), true, "named fish and axolotls stay"));
    }
    cases.push(flagged(
        E::Zombie,
        named(),
        false,
        "a name alone does not keep a monster",
    ));
    cases.push(flagged(
        E::Cat,
        Compound::new()
            .with("Owner", Tag::uuid(77))
            .with("Tame", true),
        true,
        "tamed cats stay",
    ));
    cases.push(flagged(
        E::Nautilus,
        Compound::new()
            .with("Owner", Tag::uuid(77))
            .with("Tame", true),
        true,
        "tamed nautilus stays",
    ));
    cases
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn far_from_every_player_only_the_mobs_vanilla_removes_despawn() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let center = ChunkPos::new(4, 4);
    let (player, mut events) = PlayerHandle::new(300);
    let area = Area::subscribe(&world, &player, &mut events, &around(center, 4)).await;
    let far = [8.5, SKY as f64 + 1.0, 64.0 + 200.5];
    world.publish_players(OVERWORLD, &[(&player, far)]);
    for region in area.regions() {
        region
            .request(player_at(&player, far, PlayerLooks::default()))
            .await;
    }
    let cases = cases();
    for (i, case) in cases.iter().enumerate() {
        let at = [1.5 + i as f64 * 2.0, SKY as f64 + 1.0, 104.5];
        summon(
            area.owner(at),
            case.kind,
            case.nbt.clone().with("NoAI", true),
            at,
        )
        .await;
    }
    world.ticks(5).await;
    let left: HashSet<usize> = area
        .entities()
        .await
        .into_iter()
        .filter(|s| (s.position[2] - 104.5).abs() < 1.0)
        .map(|s| ((s.position[0] - 1.5) / 2.0).round() as usize)
        .collect();
    let wrong: Vec<String> = cases
        .iter()
        .enumerate()
        .filter(|(i, case)| left.contains(i) != case.stays)
        .map(|(_, case)| {
            format!(
                "{:?}{}: {}",
                case.kind,
                if case.nbt.is_empty() {
                    ""
                } else {
                    " (flagged)"
                },
                case.why
            )
        })
        .collect();
    assert!(wrong.is_empty(), "{wrong:#?}");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wild_cats_and_ocelots_only_despawn_once_two_minutes_old() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let center = ChunkPos::new(4, 4);
    let (player, mut events) = PlayerHandle::new(300);
    let area = Area::subscribe(&world, &player, &mut events, &around(center, 2)).await;
    let far = [8.5, SKY as f64 + 1.0, 64.0 + 200.5];
    world.publish_players(OVERWORLD, &[(&player, far)]);
    for region in area.regions() {
        region
            .request(player_at(&player, far, PlayerLooks::default()))
            .await;
    }
    let at = [68.5, SKY as f64 + 1.0, 72.5];
    for kind in [E::Cat, E::Ocelot] {
        summon(area.owner(at), kind, Compound::new().with("NoAI", true), at).await;
    }
    world.ticks(100).await;
    let young = area.entities().await.len();
    assert_eq!(young, 2, "young wild cats and ocelots stay");
    world.ticks(2400).await;
    assert!(
        area.entities().await.is_empty(),
        "old wild cats and ocelots despawn"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn persistence_flags_survive_saving_a_mob() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(300);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    for (i, (kind, key)) in [
        (E::Zombie, "PersistenceRequired"),
        (E::Cod, "FromBucket"),
        (E::Axolotl, "FromBucket"),
    ]
    .into_iter()
    .enumerate()
    {
        let at = [2.5 + i as f64 * 3.0, SKY as f64 + 1.0, 4.5];
        summon(
            &region,
            kind,
            Compound::new().with("NoAI", true).with(key, true),
            at,
        )
        .await;
        let found = mobs(&region, kind).await;
        let saved = entity_data(&region, found[0].id).await;
        assert_eq!(
            saved.get(key).and_then(|t| t.as_bool()),
            Some(true),
            "{kind:?} keeps {key}"
        );
    }
    world.shutdown().await;
}

async fn entity_data(region: &RegionHandle, id: i32) -> Compound {
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::EntityData { id, reply })
        .await;
    answer.await.unwrap().expect("the entity exists here")
}
