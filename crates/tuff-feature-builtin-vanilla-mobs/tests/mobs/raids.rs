//! Raids and patrols: Bad Omen in a village, the raid it starts, its
//! waves, captains, the boss bar and the heroes; patrols out in the open;
//! witches healing raiders; villagers thanking heroes.

use super::*;
use crate::world::entity::EntityAttack;
use crate::world::messages::{EntitySnapshot, PlayerLooks};
use crate::world::player::Difficulty;
use crate::world::region_api::RegionTask;
use tuff_vanilla_data::registry::{CustomStat, EntityType, MobEffect};

const PLAYER: u128 = 77;
const DAY: i64 = 3000;
const BED_HEAD: BlockPos = BlockPos::new(64, SKY + 1, 64);
const BED_FOOT: BlockPos = BlockPos::new(64, SKY + 1, 65);

const RAIDERS: [EntityType; 6] = [
    EntityType::Pillager,
    EntityType::Vindicator,
    EntityType::Evoker,
    EntityType::Witch,
    EntityType::Ravager,
    EntityType::Illusioner,
];

fn set_time(world: &TestWorld, ticks: i64) {
    let overworld = crate::world::clock::clock_id("minecraft:overworld").unwrap();
    world
        .clocks
        .send_modify(|clocks| clocks.get_mut(overworld).set(ticks));
}

fn bed() -> Vec<(BlockPos, tuff_vanilla_data::BlockState)> {
    let bed = Block::RedBed
        .default_state()
        .with("facing", "north")
        .unwrap();
    vec![
        (BED_HEAD, bed.with("part", "head").unwrap()),
        (BED_FOOT, bed.with("part", "foot").unwrap()),
    ]
}

fn villager_at_home(home: BlockPos, profession: &str) -> Compound {
    let place = Compound::new().with(
        "value",
        Compound::new()
            .with("dimension", "minecraft:overworld")
            .with("pos", Tag::IntArray(vec![home.x, home.y, home.z])),
    );
    Compound::new()
        .with(
            "VillagerData",
            Compound::new()
                .with("type", "minecraft:plains")
                .with("profession", profession)
                .with("level", 1),
        )
        .with("PersistenceRequired", true)
        .with(
            "Brain",
            Compound::new().with("memories", Compound::new().with("minecraft:home", place)),
        )
}

fn chunks(min: i32, max: i32) -> Vec<ChunkPos> {
    let mut found = Vec::new();
    for x in min..=max {
        for z in min..=max {
            found.push(ChunkPos::new(x, z));
        }
    }
    found
}

fn looks_with(effects: &[MobEffect]) -> PlayerLooks {
    PlayerLooks {
        invulnerable: true,
        effects: PlayerLooks::effect_bits(effects.iter().copied()),
        ..Default::default()
    }
}

async fn entity_data(region: &RegionHandle, id: i32) -> Option<Compound> {
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::EntityData { id, reply })
        .await;
    answer.await.unwrap()
}

async fn raiders(region: &RegionHandle) -> Vec<EntitySnapshot> {
    snapshots(region)
        .await
        .into_iter()
        .filter(|s| RAIDERS.contains(&s.kind))
        .collect()
}

struct Village {
    world: TestWorld,
    region: RegionHandle,
    player: PlayerHandle,
    events: mpsc::UnboundedReceiver<PlayerEvent>,
}

/// A walled field with one bed a villager calls home: a village.
async fn village(dir: &TestDir, difficulty: Difficulty) -> Village {
    let world = TestWorld::start(dir.path());
    world.difficulty.send_replace(difficulty);
    let region = world.region(ChunkPos::new(4, 4).region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &chunks(0, 8)).await;
    set_time(&world, DAY);
    let mut blocks = pen(20, 108);
    blocks.extend(bed());
    place_blocks(&world, &region, blocks).await;
    summon(
        &region,
        EntityType::Villager,
        villager_at_home(BED_HEAD, "minecraft:none"),
        [60.5, SKY as f64 + 1.0, 60.5],
    )
    .await;
    world.ticks(5).await;
    Village {
        world,
        region,
        player,
        events,
    }
}

const STANDING: [f64; 3] = [66.5, SKY as f64 + 1.0, 60.5];

/// Plays the player's session: Bad Omen in the village turns into Raid
/// Omen, which runs out and starts the raid.
async fn start_raid(v: &mut Village) {
    start_raid_at(v, STANDING).await;
}

async fn start_raid_at(v: &mut Village, standing: [f64; 3]) {
    v.region
        .request(player_at(
            &v.player,
            standing,
            looks_with(&[MobEffect::BadOmen]),
        ))
        .await;
    let taken = ticking_expect(&v.world, &mut v.events, 100, |event| match event {
        PlayerEvent::SwapEffect {
            from,
            to,
            duration,
            taken,
        } => Some((from, to, duration, taken)),
        _ => None,
    })
    .await;
    assert_eq!(
        (taken.0, taken.1, taken.2),
        (MobEffect::BadOmen, MobEffect::RaidOmen, 600),
        "Bad Omen turns into Raid Omen for half a minute"
    );
    taken.3.expect("the region hears back").send(0).unwrap();
    v.region
        .request(player_at(
            &v.player,
            standing,
            looks_with(&[MobEffect::RaidOmen]),
        ))
        .await;
    let stat = ticking_expect(&v.world, &mut v.events, 700, |event| match event {
        PlayerEvent::AwardStat(stat, _) => Some(stat),
        _ => None,
    })
    .await;
    assert_eq!(
        stat,
        CustomStat::RaidTrigger,
        "the omen ran out into a raid"
    );
    v.region
        .request(player_at(&v.player, standing, looks_with(&[])))
        .await;
}

fn boss_bar_added(event: &PlayerEvent) -> bool {
    let PlayerEvent::Packets(packets) = event else {
        return false;
    };
    let id = tuff_vanilla_data::packet::play::Clientbound::BossEvent.id();
    packets.iter().any(|p| {
        let mut r = framed_payload(p);
        r.read_var_int().ok() == Some(id)
            && r.read_uuid().is_ok()
            && r.read_var_int().ok() == Some(0)
    })
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bad_omen_in_a_village_starts_a_raid_whose_first_wave_has_a_captain() {
    let dir = TestDir::new();
    let mut v = village(&dir, Difficulty::Normal).await;
    start_raid(&mut v).await;
    let raid = {
        let (reply, answer) = oneshot::channel();
        v.region
            .request(RegionRequest::WhenLoaded {
                min: BED_HEAD,
                max: BED_HEAD,
                run: RegionTask::new(move |world| {
                    let raid = world
                        .state()
                        .get::<tuff_feature_builtin_vanilla_mobs::raid::Raids>()
                        .at(BED_HEAD);
                    let _ = reply.send(raid);
                }),
            })
            .await;
        answer
            .await
            .unwrap()
            .expect("a raid centred on the village")
    };
    assert_eq!(raid.center, BED_HEAD, "centred on the occupied bed");
    assert_eq!(raid.omen_level, 1);
    assert_eq!(
        raid.phase,
        tuff_feature_builtin_vanilla_mobs::raid::Phase::Preparing
    );
    ticking_expect(&v.world, &mut v.events, 400, |event| {
        boss_bar_added(&event).then_some(())
    })
    .await;
    let wave = wait_for(&v.world, &v.region, 400, |found| {
        found
            .iter()
            .filter(|s| s.kind == EntityType::Pillager)
            .count()
            >= 4
    })
    .await;
    let pillagers: Vec<&EntitySnapshot> = wave
        .iter()
        .filter(|s| s.kind == EntityType::Pillager)
        .collect();
    let mut captains = 0;
    for pillager in &pillagers {
        let nbt = entity_data(&v.region, pillager.id).await.unwrap();
        assert_eq!(
            nbt.get("RaidId"),
            Some(&Tag::Int(raid.id as i32)),
            "{nbt:?}"
        );
        assert_eq!(nbt.get("Wave"), Some(&Tag::Int(1)));
        let head = nbt
            .get_compound("equipment")
            .and_then(|e| e.get_compound("head"))
            .and_then(|h| h.get_str("id"));
        if nbt.get("PatrolLeader") == Some(&Tag::Byte(1)) {
            assert_eq!(head, Some("minecraft:white_banner"), "{nbt:?}");
            captains += 1;
        }
    }
    let vindicators = wave
        .iter()
        .filter(|s| s.kind == EntityType::Vindicator)
        .count();
    assert_eq!(
        captains,
        usize::from(vindicators == 0),
        "the first raider that can lead carries the banner"
    );
    v.world.shutdown().await;
}

async fn raid_at_bed(
    region: &RegionHandle,
) -> Option<tuff_feature_builtin_vanilla_mobs::raid::Raid> {
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::WhenLoaded {
            min: BED_HEAD,
            max: BED_HEAD,
            run: RegionTask::new(move |world| {
                let raid = world
                    .state()
                    .get::<tuff_feature_builtin_vanilla_mobs::raid::Raids>()
                    .at(BED_HEAD);
                let _ = reply.send(raid);
            }),
        })
        .await;
    answer.await.unwrap()
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_raid_and_its_raiders_survive_a_restart() {
    let dir = TestDir::new();
    let mut v = village(&dir, Difficulty::Normal).await;
    start_raid(&mut v).await;
    wait_for(&v.world, &v.region, 400, |found| {
        found.iter().any(|s| s.kind == EntityType::Pillager)
    })
    .await;
    v.world.ticks(40).await;
    let before = raid_at_bed(&v.region).await.expect("a raid");
    assert!(before.groups_spawned >= 1);
    let mut members = Vec::new();
    for raider in raiders(&v.region).await {
        let nbt = entity_data(&v.region, raider.id).await.unwrap();
        if nbt.get("RaidId") == Some(&Tag::Int(before.id as i32)) {
            members.push(nbt.get_uuid("UUID").unwrap());
        }
    }
    assert!(!members.is_empty());
    v.world.shared.services.flush().await;
    v.world.shutdown().await;
    assert!(
        dir.path()
            .join("dimensions/minecraft/overworld/data/minecraft/raids.dat")
            .exists()
    );

    let world = TestWorld::start(dir.path());
    world.difficulty.send_replace(Difficulty::Normal);
    let region = world.region(ChunkPos::new(4, 4).region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &viewer, &mut events, &chunks(0, 8)).await;
    set_time(&world, DAY);
    for _ in 0..60 {
        world.ticks(1).await;
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }
    let after = raid_at_bed(&region).await.expect("the raid came back");
    assert_eq!(after.id, before.id);
    assert_eq!(after.groups_spawned, before.groups_spawned);
    assert_eq!(after.omen_level, before.omen_level);
    assert_eq!(
        after.phase,
        tuff_feature_builtin_vanilla_mobs::raid::Phase::Active,
        "its raiders rejoined, so the wave is still on"
    );
    let mut kept = 0;
    for raider in raiders(&region).await {
        let nbt = entity_data(&region, raider.id).await.unwrap();
        if !members.contains(&nbt.get_uuid("UUID").unwrap()) {
            continue;
        }
        assert_eq!(
            nbt.get("RaidId"),
            Some(&Tag::Int(before.id as i32)),
            "{nbt:?}"
        );
        kept += 1;
    }
    assert_eq!(kept, members.len(), "every member is back in the raid");
    world.shutdown().await;
}

async fn kill_all_raiders(v: &mut Village) {
    for raider in raiders(&v.region).await {
        kill_raider(v, raider.id).await;
    }
}

async fn kill_raider(v: &mut Village, id: i32) {
    {
        v.region
            .request(RegionRequest::AttackEntity {
                attacker: v.player.clone(),
                target: id,
                attack: EntityAttack {
                    attacker: v.player.id,
                    attacker_uuid: PLAYER,
                    attacker_position: STANDING,
                    attacker_yaw: 0.0,
                    attacker_pitch: 0.0,
                    damage: 1000.0,
                    knockback: 0.0,
                    weapon: None,
                    critical: false,
                    magic: false,
                    sweep: false,
                    part: None,
                },
                sweep: None,
            })
            .await;
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn beating_every_wave_makes_the_player_a_hero_of_the_village() {
    let dir = TestDir::new();
    let mut v = village(&dir, Difficulty::Easy).await;
    start_raid(&mut v).await;
    let mut waves = 0;
    let mut hero = None;
    for _ in 0..3000 {
        while let Ok(event) = v.events.try_recv() {
            if let PlayerEvent::AddEffect(effect) = event
                && effect.effect == MobEffect::HeroOfTheVillage
            {
                hero = Some(effect);
            }
        }
        if hero.is_some() {
            break;
        }
        let found = raiders(&v.region).await;
        if !found.is_empty() && found.iter().all(|s| s.kind != EntityType::Vex) {
            waves += 1;
            v.world.ticks(2).await;
            kill_all_raiders(&mut v).await;
        }
        v.world.ticks(1).await;
    }
    let hero = hero.expect("the raid was won");
    assert!(waves >= 3, "three waves on easy, saw {waves}");
    assert_eq!(hero.amplifier, 0, "a level one omen gives level one");
    assert_eq!(hero.duration, 48_000);
    assert!(!hero.show_particles);
    v.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_raid_starts_outside_a_village_or_in_peaceful() {
    let dir = TestDir::new();
    let mut v = village(&dir, Difficulty::Normal).await;
    v.region
        .request(player_at(
            &v.player,
            [24.5, SKY as f64 + 1.0, 104.5],
            looks_with(&[MobEffect::BadOmen]),
        ))
        .await;
    v.world.ticks(60).await;
    while let Ok(event) = v.events.try_recv() {
        assert!(
            !matches!(event, PlayerEvent::SwapEffect { .. }),
            "too far from the village"
        );
    }
    v.world.difficulty.send_replace(Difficulty::Peaceful);
    v.region
        .request(player_at(
            &v.player,
            STANDING,
            looks_with(&[MobEffect::BadOmen]),
        ))
        .await;
    v.world.ticks(60).await;
    while let Ok(event) = v.events.try_recv() {
        assert!(
            !matches!(event, PlayerEvent::SwapEffect { .. }),
            "no raids in peaceful"
        );
    }
    v.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_patrol_spawns_led_by_a_banner_carrier_that_its_members_follow() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let region = world.region(ChunkPos::new(12, 12).region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    let mut near = Vec::new();
    for x in 8..=16 {
        for z in 8..=16 {
            near.push(ChunkPos::new(x, z));
        }
    }
    subscribe(&region, &viewer, &mut events, &near).await;
    set_time(&world, DAY);
    place_blocks(&world, &region, pen(140, 260)).await;
    world.ticks(20).await;
    let player = BlockPos::new(200, SKY + 1, 200);
    let (reply, spawned) = oneshot::channel();
    region
        .request(RegionRequest::WhenLoaded {
            min: player,
            max: player,
            run: RegionTask::new(move |world| {
                let spawned = (0..20).any(|_| {
                    tuff_feature_builtin_vanilla_mobs::world::region::patrols::spawn_patrol_near(
                        world, player,
                    )
                });
                let _ = reply.send(spawned);
            }),
        })
        .await;
    assert!(spawned.await.unwrap(), "a patrol spawned");
    let pillagers = mobs(&region, EntityType::Pillager).await;
    assert_eq!(pillagers.len(), 3, "group of difficulty plus one");
    let mut leader = None;
    let mut followers = Vec::new();
    for pillager in &pillagers {
        let nbt = entity_data(&region, pillager.id).await.unwrap();
        assert_eq!(nbt.get("Patrolling"), Some(&Tag::Byte(1)), "{nbt:?}");
        if nbt.get("PatrolLeader") == Some(&Tag::Byte(1)) {
            assert!(nbt.get("patrol_target").is_some());
            leader = Some(pillager.id);
        } else {
            followers.push(pillager.id);
        }
    }
    let leader = leader.expect("one leads");
    assert_eq!(followers.len(), 2);
    let start = mobs(&region, EntityType::Pillager)
        .await
        .into_iter()
        .find(|p| p.id == leader)
        .unwrap()
        .position;
    let mut followed = false;
    for _ in 0..400 {
        world.ticks(1).await;
        let nbt = entity_data(&region, followers[0]).await;
        if nbt.is_some_and(|n| n.get("patrol_target").is_some()) {
            followed = true;
            break;
        }
    }
    assert!(followed, "the leader shared where it heads");
    let now = mobs(&region, EntityType::Pillager)
        .await
        .into_iter()
        .find(|p| p.id == leader)
        .map(|p| p.position)
        .unwrap();
    assert!(
        crate::world::distance_sq(start, now) > 1.0,
        "the leader set off: {start:?} -> {now:?}"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_raid_witch_throws_healing_at_a_hurt_raider() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let region = world.region(ChunkPos::new(4, 4).region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &viewer, &mut events, &chunks(2, 6)).await;
    set_time(&world, DAY);
    let mut blocks = pen(60, 70);
    for x in 59..=71 {
        for z in 59..=71 {
            blocks.push((BlockPos::new(x, SKY + 4, z), Block::Stone.default_state()));
        }
    }
    place_blocks(&world, &region, blocks).await;
    let center = BlockPos::new(65, SKY + 1, 65);
    let (reply, id) = oneshot::channel();
    region
        .request(RegionRequest::WhenLoaded {
            min: center,
            max: center,
            run: RegionTask::new(move |world| {
                let mut raid = tuff_feature_builtin_vanilla_mobs::raid::Raid::new(
                    4242,
                    center,
                    tuff_feature_builtin_vanilla_mobs::raid::Phase::Active,
                );
                raid.groups_spawned = 1;
                let raids = world
                    .state()
                    .get::<tuff_feature_builtin_vanilla_mobs::raid::Raids>();
                raids.mirror(raid, u64::MAX / 2);
                let _ = reply.send(raid.id);
            }),
        })
        .await;
    let raid = id.await.unwrap();
    let member = |nbt: Compound| {
        nbt.with("RaidId", raid as i32)
            .with("Wave", 1)
            .with("CanJoinRaid", true)
            .with("PersistenceRequired", true)
    };
    summon(
        &region,
        EntityType::Witch,
        member(Compound::new()),
        [62.5, SKY as f64 + 1.0, 62.5],
    )
    .await;
    summon(
        &region,
        EntityType::Pillager,
        member(Compound::new().with("Health", 3.0f32).with("NoAI", true)),
        [67.5, SKY as f64 + 1.0, 67.5],
    )
    .await;
    let found = wait_for(&world, &region, 6000, |found| {
        found.iter().any(|s| s.kind == EntityType::SplashPotion)
    })
    .await;
    let potion = found
        .iter()
        .find(|s| s.kind == EntityType::SplashPotion)
        .unwrap();
    let nbt = entity_data(&region, potion.id).await.unwrap();
    let contents = nbt
        .get_compound("Item")
        .and_then(|i| i.get_compound("components"))
        .and_then(|c| c.get_compound("minecraft:potion_contents"))
        .and_then(|p| p.get_str("potion"))
        .map(str::to_owned);
    assert_eq!(contents.as_deref(), Some("minecraft:healing"), "{nbt:?}");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn villagers_throw_gifts_to_a_hero_of_the_village() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let region = world.region(ChunkPos::new(4, 4).region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &chunks(2, 6)).await;
    set_time(&world, DAY);
    place_blocks(&world, &region, pen(60, 70)).await;
    summon(
        &region,
        EntityType::Villager,
        Compound::new()
            .with(
                "VillagerData",
                Compound::new()
                    .with("type", "minecraft:plains")
                    .with("profession", "minecraft:none")
                    .with("level", 1),
            )
            .with("PersistenceRequired", true),
        [64.5, SKY as f64 + 1.0, 64.5],
    )
    .await;
    region
        .request(player_at(
            &player,
            [66.5, SKY as f64 + 1.0, 64.5],
            looks_with(&[MobEffect::HeroOfTheVillage]),
        ))
        .await;
    wait_for(&world, &region, 1200, |found| {
        found.iter().any(|s| s.kind == EntityType::Item)
    })
    .await;
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_captain_killed_outside_a_raid_drops_an_ominous_bottle() {
    let dir = TestDir::new();
    let mut v = village(&dir, Difficulty::Normal).await;
    let banner = tuff_feature_builtin_vanilla_mobs::raider::ominous_banner().to_nbt();
    summon(
        &v.region,
        EntityType::Pillager,
        Compound::new()
            .with("PatrolLeader", true)
            .with("NoAI", true)
            .with("equipment", Compound::new().with("head", banner)),
        [30.5, SKY as f64 + 1.0, 30.5],
    )
    .await;
    v.region
        .request(player_at(
            &v.player,
            [32.5, SKY as f64 + 1.0, 30.5],
            looks_with(&[]),
        ))
        .await;
    kill_all_raiders(&mut v).await;
    let found = wait_for(&v.world, &v.region, 40, |found| {
        found.iter().filter(|s| s.kind == EntityType::Item).count() >= 1
    })
    .await;
    let mut items = Vec::new();
    for item in found.iter().filter(|s| s.kind == EntityType::Item) {
        let nbt = entity_data(&v.region, item.id).await.unwrap();
        if let Some(id) = nbt.get_compound("Item").and_then(|i| i.get_str("id")) {
            items.push(id.to_owned());
        }
    }
    assert!(
        items.iter().any(|i| i == "minecraft:ominous_bottle"),
        "{items:?}"
    );
    v.world.shutdown().await;
}

async fn raid_phase_at(
    region: &RegionHandle,
    pos: BlockPos,
) -> Option<tuff_feature_builtin_vanilla_mobs::raid::Raid> {
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::WhenLoaded {
            min: pos,
            max: pos,
            run: RegionTask::new(move |world| {
                let raids = world
                    .state()
                    .get::<tuff_feature_builtin_vanilla_mobs::raid::Raids>();
                let _ = reply.send(raids.at(pos));
            }),
        })
        .await;
    answer.await.unwrap()
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_village_emptied_during_a_raid_loses_it() {
    let dir = TestDir::new();
    let mut v = village(&dir, Difficulty::Normal).await;
    start_raid(&mut v).await;
    wait_for(&v.world, &v.region, 600, |found| {
        found.iter().any(|s| s.kind == EntityType::Pillager)
    })
    .await;
    for villager in mobs(&v.region, EntityType::Villager).await {
        let (reply, done) = oneshot::channel();
        v.region
            .request(RegionRequest::WhenLoaded {
                min: BED_HEAD,
                max: BED_HEAD,
                run: RegionTask::new(move |world| {
                    world.remove_entity(villager.id);
                    let _ = reply.send(());
                }),
            })
            .await;
        done.await.unwrap();
    }
    let mut lost = false;
    for _ in 0..100 {
        v.world.ticks(1).await;
        if raid_phase_at(&v.region, BED_HEAD)
            .await
            .is_some_and(|r| r.phase == tuff_feature_builtin_vanilla_mobs::raid::Phase::Loss)
        {
            lost = true;
            break;
        }
    }
    assert!(lost, "no villagers, no village: the raid is lost");
    v.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_raider_picks_up_the_dropped_captain_banner_and_leads() {
    let dir = TestDir::new();
    let mut v = village(&dir, Difficulty::Normal).await;
    start_raid(&mut v).await;
    wait_for(&v.world, &v.region, 400, |found| {
        found
            .iter()
            .filter(|s| s.kind == EntityType::Pillager)
            .count()
            >= 4
    })
    .await;
    let mut captain = None;
    for raider in raiders(&v.region).await {
        let nbt = entity_data(&v.region, raider.id).await.unwrap();
        if nbt.get("PatrolLeader") == Some(&Tag::Byte(1)) {
            captain = Some(raider.id);
        }
    }
    kill_raider(&mut v, captain.expect("a captain")).await;
    let mut leader = false;
    for _ in 0..600 {
        v.world.ticks(1).await;
        for pillager in raiders(&v.region).await {
            let nbt = entity_data(&v.region, pillager.id).await.unwrap();
            if pillager.id != captain.unwrap() && nbt.get("PatrolLeader") == Some(&Tag::Byte(1)) {
                leader = true;
            }
        }
        if leader {
            break;
        }
    }
    assert!(leader, "another raider took up the banner");
    v.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_wave_spawns_in_the_neighbouring_region_that_owns_the_spawn_spot() {
    const SKY: i32 = 180;
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    world.difficulty.send_replace(Difficulty::Easy);
    let (player, mut events) = PlayerHandle::new(1000);
    let area = Area::subscribe(&world, &player, &mut events, &{
        let mut all = Vec::new();
        for x in 24..=40 {
            for z in 0..=10 {
                all.push(ChunkPos::new(x, z));
            }
        }
        all
    })
    .await;
    set_time(&world, DAY);
    let home = world.region(ChunkPos::new(24, 4).region()).await;
    let east = world.region(ChunkPos::new(40, 4).region()).await;
    let bed_head = BlockPos::new(500, SKY + 1, 64);
    let ground = |range: std::ops::RangeInclusive<i32>, block: Block| {
        let mut blocks = Vec::new();
        for x in range {
            for z in 0..=160 {
                blocks.push((BlockPos::new(x, SKY, z), block.default_state()));
            }
        }
        blocks
    };
    place_blocks(&world, &home, ground(495..=511, Block::Glass)).await;
    place_blocks(&world, &east, ground(512..=620, Block::GrassBlock)).await;
    let bed = Block::RedBed
        .default_state()
        .with("facing", "north")
        .unwrap();
    place_blocks(
        &world,
        &home,
        [
            (bed_head, bed.with("part", "head").unwrap()),
            (
                BlockPos::new(500, SKY + 1, 65),
                bed.with("part", "foot").unwrap(),
            ),
        ],
    )
    .await;
    summon(
        &home,
        EntityType::Villager,
        villager_at_home(bed_head, "minecraft:none"),
        [497.5, SKY as f64 + 1.0, 60.5],
    )
    .await;
    world.ticks(5).await;
    let mut v = Village {
        world,
        region: home,
        player,
        events,
    };
    start_raid_at(&mut v, [502.5, SKY as f64 + 1.0, 60.5]).await;
    let mut found = Vec::new();
    for _ in 0..1200 {
        found = raiders(&east).await;
        if !found.is_empty() {
            break;
        }
        v.world.ticks(1).await;
    }
    assert!(!found.is_empty(), "the wave spawned in the east region");
    assert!(found.iter().all(|s| s.position[0] >= 512.0));
    assert!(area.entities().await.len() >= found.len());
    v.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_last_normal_wave_brings_a_pillager_riding_its_ravager() {
    let dir = TestDir::new();
    let mut v = village(&dir, Difficulty::Normal).await;
    start_raid(&mut v).await;
    let mut ridden = None;
    for _ in 0..6000 {
        let found = raiders(&v.region).await;
        if found.iter().any(|s| s.kind == EntityType::Ravager) {
            v.world.ticks(3).await;
            let found = raiders(&v.region).await;
            let ravager = found.iter().find(|s| s.kind == EntityType::Ravager);
            let pillager = ravager.and_then(|r| {
                found.iter().find(|s| {
                    s.kind == EntityType::Pillager
                        && (s.position[0] - r.position[0]).abs() < 0.5
                        && (s.position[2] - r.position[2]).abs() < 0.5
                        && s.position[1] > r.position[1] + 1.5
                })
            });
            if let Some(pillager) = pillager {
                ridden = Some(pillager.id);
                break;
            }
        }
        if !found.is_empty() && found.iter().all(|s| s.kind != EntityType::Vex) {
            v.world.ticks(2).await;
            kill_all_raiders(&mut v).await;
        }
        v.world.ticks(1).await;
    }
    assert!(ridden.is_some(), "a pillager sat on the wave's ravager");
    v.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_zombie_siege_starts_at_night_in_a_village() {
    const ROLL: i64 = 18000;
    let dir = TestDir::new();
    let v = village(&dir, Difficulty::Hard).await;
    v.world.game_rules.send_modify(|rules| {
        rules
            .set(
                "spawn_mobs",
                crate::world::game_rules::RuleValue::Bool(true),
            )
            .unwrap();
    });
    v.region
        .request(player_at(&v.player, STANDING, looks_with(&[])))
        .await;
    let mut jump = 0;
    for _ in 0..60 {
        let before = mobs(&v.region, EntityType::Zombie).await.len();
        set_time(&v.world, ROLL - 1);
        v.world.ticks(80).await;
        jump = mobs(&v.region, EntityType::Zombie)
            .await
            .len()
            .saturating_sub(before);
        if jump >= 3 {
            break;
        }
    }
    assert!(jump >= 3, "a siege spawned {jump} zombies in one night");
    v.world.shutdown().await;
}

async fn debug_raid(
    region: &RegionHandle,
    action: tuff_feature_builtin_vanilla_mobs::world::region::raids::DebugAction,
) -> tuff_feature_builtin_vanilla_mobs::world::region::raids::DebugReply {
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::WhenLoaded {
            min: BED_HEAD,
            max: BED_HEAD,
            run: RegionTask::new(move |world| {
                tuff_feature_builtin_vanilla_mobs::world::region::raids::debug(
                    world, BED_HEAD, action, reply,
                );
            }),
        })
        .await;
    answer.await.unwrap()
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_raid_debug_command_starts_inspects_changes_and_stops_a_raid() {
    use tuff_feature_builtin_vanilla_mobs::world::region::raids::DebugAction;
    let dir = TestDir::new();
    let v = village(&dir, Difficulty::Normal).await;
    let none = debug_raid(&v.region, DebugAction::Check).await;
    assert_eq!(none.failure.as_deref(), Some("Found no started raids"));

    let started = debug_raid(&v.region, DebugAction::Start(3)).await;
    assert_eq!(started.messages, ["Created a raid in your local village"]);
    v.world.ticks(25).await;
    assert_eq!(raid_at_bed(&v.region).await.unwrap().omen_level, 3);
    let again = debug_raid(&v.region, DebugAction::Start(1)).await;
    assert_eq!(
        again.failure.as_deref(),
        Some("Raid already started close by")
    );

    let checked = debug_raid(&v.region, DebugAction::Check).await;
    assert_eq!(checked.messages[0], "Found a started raid!");
    assert!(checked.messages[1].contains("Raid omen level: 3"));

    let too_high = debug_raid(&v.region, DebugAction::SetOmen(9)).await;
    assert!(too_high.failure.unwrap().contains("max raid omen level"));
    let set = debug_raid(&v.region, DebugAction::SetOmen(5)).await;
    assert_eq!(
        set.messages,
        ["Changed village's raid omen level from 3 to 5"]
    );

    let stopped = debug_raid(&v.region, DebugAction::Stop).await;
    assert_eq!(stopped.messages, ["Stopped raid"]);
    v.world.ticks(25).await;
    assert!(raid_at_bed(&v.region).await.is_none());

    let leader = debug_raid(&v.region, DebugAction::SpawnLeader(STANDING)).await;
    assert_eq!(leader.messages, ["Spawned a raid captain"]);
    v.world.ticks(2).await;
    assert!(
        !mobs(&v.region, EntityType::Pillager).await.is_empty(),
        "the captain spawned"
    );
    v.world.shutdown().await;
}

async fn debug_path(
    region: &RegionHandle,
    id: i32,
    target: BlockPos,
) -> Option<crate::world::entity::DebugPath> {
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::Run(RegionTask::new(move |world| {
            let _ = reply.send(world.debug_path_entity(id, target));
        })))
        .await;
    answer.await.unwrap()
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_debug_path_command_reports_whether_a_mob_can_reach_a_block() {
    use crate::world::entity::DebugPath;
    let dir = TestDir::new();
    let v = village(&dir, Difficulty::Normal).await;
    let villager = mobs(&v.region, EntityType::Villager).await[0].id;
    let near = BlockPos::new(63, SKY + 1, 60);
    assert_eq!(
        debug_path(&v.region, villager, near).await,
        Some(DebugPath::Made)
    );
    assert_eq!(debug_path(&v.region, -5, near).await, None);
    v.world.shutdown().await;
}
