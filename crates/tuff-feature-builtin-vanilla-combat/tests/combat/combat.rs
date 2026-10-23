//! Shields, armor and beds on a real session, driven by the events
//! regions and other players send it.

use super::*;
use crate::auth::GameProfile;
use crate::protocol::configuration::ClientInformation;
use crate::protocol::play::Serverbound;
use crate::world::inventory::Inventory;
use crate::world::messages::{Attack, AttackKind, BedUse, Blast, SessionTask};
use crate::world::session::PlayerSession;
use std::collections::HashMap;
use tuff_item::item::Enchantments;
use tuff_storage::admin::PermissionLevel;

use tuff_vanilla_data::damage_type;
use tuff_vanilla_data::entity::Pose;
use tuff_vanilla_data::item::EquipmentSlot;
use tuff_vanilla_data::registry::Attribute;

type EntityData = Option<(i32, Vec<(u8, i32, Entry)>)>;

fn session(world: &TestWorld, id: i32) -> PlayerSession {
    let (session, outbound) = session_with_outbound(world, id);
    std::mem::forget(outbound);
    session
}

/// Sessions and their handler futures live on the stack wherever they
/// are driven (a debug-build test thread has 2 MiB), so a bulky field
/// such as an inline inventory must stay behind a pointer.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn session_and_its_handler_futures_stay_small() {
    const SESSION_BOUND: usize = 16 * 1024;
    const FUTURE_BOUND: usize = 32 * 1024;
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut player = session(&world, 1);
    assert!(std::mem::size_of::<PlayerSession>() <= SESSION_BOUND);
    let event = std::mem::size_of_val(&player.handle_event(hit(1.0, FRONT, None)));
    let packet = std::mem::size_of_val(&player.handle_packet(Serverbound::Attack(2)));
    let tick = std::mem::size_of_val(&player.run_tick());
    for (name, size) in [("event", event), ("packet", packet), ("tick", tick)] {
        assert!(
            size <= FUTURE_BOUND,
            "{name} handler future is {size} bytes"
        );
    }
    world.shutdown().await;
}

fn hit(damage: f32, from: [f64; 3], weapon: Option<Item>) -> PlayerEvent {
    let (attacker, _) = PlayerHandle::new(999);
    PlayerEvent::Attacked(Attack {
        attacker,
        attacker_name: "Attacker".to_owned(),
        attacker_uuid: 0,
        weapon: weapon.map(|item| tuff_item::item::ItemSnapshot::new(item, 1)),
        attacker_position: from,
        attacker_yaw: 180.0,
        damage,
        knockback: 0.0,
        kind: AttackKind::Direct {
            critical: false,
            magic: false,
            full_strength: true,
            sweep: None,
            smash: None,
        },
    })
}

const FRONT: [f64; 3] = [0.0, 0.0, 3.0];
const BEHIND: [f64; 3] = [0.0, 0.0, -3.0];

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn raised_shields_block_hits_from_the_front_and_wear() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let shield = ItemStack::mint(Item::Shield, 1, MintReason::Test);

    let mut lowered = session(&world, 1);
    lowered
        .inventory_mut()
        .set_slot(Inventory::OFFHAND_SLOT, Some(shield.test_copy()));
    lowered.hold_up(true, 2);
    lowered.handle_event(hit(5.0, FRONT, None)).await;
    assert_eq!(lowered.health(), 15.0, "not raised long enough");

    let mut raised = session(&world, 2);
    raised
        .inventory_mut()
        .set_slot(Inventory::OFFHAND_SLOT, Some(shield.test_copy()));
    raised.hold_up(true, 10);
    raised.handle_event(hit(5.0, FRONT, None)).await;
    assert_eq!(raised.health(), 20.0);
    let worn = raised.inventory().hand(true).unwrap().damage();
    assert_eq!(worn, 6, "the shield wears by 1 + the damage blocked");

    let mut turned = session(&world, 3);
    turned
        .inventory_mut()
        .set_slot(Inventory::OFFHAND_SLOT, Some(shield.test_copy()));
    turned.hold_up(true, 10);
    turned.handle_event(hit(5.0, BEHIND, None)).await;
    assert_eq!(turned.health(), 15.0, "shields only cover the front");

    let mut disabled = session(&world, 4);
    disabled
        .inventory_mut()
        .set_slot(Inventory::OFFHAND_SLOT, Some(shield));
    disabled.hold_up(true, 10);
    disabled
        .handle_event(hit(5.0, FRONT, Some(Item::IronAxe)))
        .await;
    assert_eq!(disabled.health(), 20.0);
    assert!(!disabled.is_using(), "an axe knocks the shield down");
    assert!(disabled.item_on_cooldown(Item::Shield));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn armor_and_protection_reduce_damage_and_wear() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chest_slot = Inventory::armor_slot(EquipmentSlot::Chest).unwrap();

    let mut armored = session(&world, 1);
    armored.inventory_mut().set_slot(
        chest_slot,
        Some(ItemStack::mint(
            Item::DiamondChestplate,
            1,
            MintReason::Test,
        )),
    );
    armored.handle_event(hit(10.0, FRONT, None)).await;
    assert!((armored.health() - (20.0 - 8.4)).abs() < 1e-4);
    let worn = armored.inventory().slots()[chest_slot].as_ref().unwrap();
    assert_eq!(worn.damage(), 2, "armor wears by a quarter of the damage");

    let blast = |kind| PlayerEvent::Exploded {
        damage: 10.0,
        blast: Blast {
            center: FRONT,
            radius: 4.0,
            damage_type: kind,
            igniter: None,
            culprit: None,
            wind: None,
        },
        push: [0.0; 3],
        push_sent: true,
    };
    let mut plain = session(&world, 2);
    plain.handle_event(blast(&damage_type::EXPLOSION)).await;
    assert_eq!(plain.health(), 10.0);
    let mut protected = session(&world, 3);
    let mut chest = ItemStack::mint(Item::LeatherChestplate, 1, MintReason::Test);
    chest.components.enchantments = Some(Enchantments::new([(
        "minecraft:blast_protection".to_owned(),
        4,
    )]));
    protected.inventory_mut().set_slot(chest_slot, Some(chest));
    protected.handle_event(blast(&damage_type::EXPLOSION)).await;
    let context = tuff_vanilla_common::combat::DamageContext::melee(
        tuff_vanilla_data::registry::EntityType::Player,
    );
    let mut rng = <rand::rngs::StdRng as rand::SeedableRng>::seed_from_u64(0);
    let expected = 20.0
        - tuff_vanilla_common::combat::after_protection(
            tuff_vanilla_common::combat::after_armor(10.0, 3.0, 0.0, None, &context, &mut rng),
            8.0,
        );
    assert!((protected.health() - expected).abs() < 1e-4);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn beds_set_the_spawn_and_sleepers_wake_up() {
    use crate::world::stats::Stat;
    use tuff_vanilla_data::registry::CustomStat;

    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut sleeper = session(&world, 1);
    let bed = BlockPos::new(5, 70, 5);
    let overworld = sleeper.dimension_id();
    sleeper
        .handle_event(PlayerEvent::Run(SessionTask::new(move |s| {
            Box::pin(s.use_bed(BedUse {
                dimension: overworld,
                pos: bed,
                rule: tuff_vanilla_data::block::BedRuleAttribute::Bed,
                set_spawn: true,
                sleep: Ok(70.6875),
                monsters_near: false,
            }))
        })))
        .await;
    assert!(sleeper.is_sleeping());
    assert_eq!(sleeper.position(), [5.5, 70.6875, 5.5]);
    let respawn = sleeper.respawn_point_save().unwrap();
    assert_eq!(respawn.pos, [5, 70, 5]);
    assert!(!respawn.forced);
    assert_eq!(sleeper.stat(Stat::custom(CustomStat::SleepInBed)), 1);
    assert_eq!(sleeper.stat(Stat::custom(CustomStat::SleepInStrawBed)), 0);
    sleeper.handle_event(PlayerEvent::WakeUp).await;
    assert!(!sleeper.is_sleeping());

    let mut straw_sleeper = session(&world, 2);
    straw_sleeper
        .handle_event(PlayerEvent::Run(SessionTask::new(move |s| {
            Box::pin(s.use_bed(BedUse {
                dimension: overworld,
                pos: bed,
                rule: tuff_vanilla_data::block::BedRuleAttribute::StrawBed,
                set_spawn: false,
                sleep: Ok(70.6875),
                monsters_near: false,
            }))
        })))
        .await;
    assert_eq!(
        straw_sleeper.stat(Stat::custom(CustomStat::SleepInStrawBed)),
        1
    );
    assert_eq!(straw_sleeper.stat(Stat::custom(CustomStat::SleepInBed)), 0);
    world.shutdown().await;
}

fn enchanted(item: Item, enchantments: &[(&str, i32)]) -> ItemStack {
    let mut stack = ItemStack::mint(item, 1, MintReason::Test);
    stack.components.enchantments = Some(Enchantments::new(
        enchantments
            .iter()
            .map(|(id, level)| (format!("minecraft:{id}"), *level)),
    ));
    stack
}

fn attacked(events: &mut mpsc::UnboundedReceiver<PlayerEvent>) -> Option<Attack> {
    std::iter::from_fn(|| events.try_recv().ok()).find_map(|event| match event {
        PlayerEvent::Attacked(attack) => Some(attack),
        _ => None,
    })
}

fn direct(attacker: &PlayerHandle, damage: f32, weapon: Option<ItemStack>) -> PlayerEvent {
    PlayerEvent::Attacked(Attack {
        attacker: attacker.clone(),
        attacker_name: "Attacker".to_owned(),
        attacker_uuid: 0,
        weapon: weapon.as_ref().map(tuff_item::item::ItemSnapshot::of),
        attacker_position: FRONT,
        attacker_yaw: 180.0,
        damage,
        knockback: 0.0,
        kind: AttackKind::Direct {
            critical: false,
            magic: false,
            full_strength: true,
            sweep: None,
            smash: None,
        },
    })
}

/// The entity and velocity of a framed Set Entity Motion packet.
fn motion(frame: &bytes::Bytes) -> Option<(i32, [f64; 3])> {
    let mut r = framed_payload(frame);
    if r.read_var_int().ok()? != tuff_vanilla_data::packet::play::Clientbound::SetEntityMotion.id()
    {
        return None;
    }
    Some((r.read_var_int().unwrap(), r.read_lp_vec3().unwrap()))
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn players_kill_and_milk_mobs_through_their_regions() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    let floor: Vec<_> = (0..16)
        .flat_map(|x| (0..16).map(move |z| BlockPos::new(x, 150, z)))
        .map(|pos| (pos, Block::Stone.default_state()))
        .collect();
    place_blocks(&world, &region, floor).await;
    let mut tracks = Vec::new();
    for x in [4.5, 11.5] {
        let (reply, summoned) = oneshot::channel();
        region
            .request(RegionRequest::Summon {
                nbt: Compound::new()
                    .with("id", "minecraft:cow")
                    .with("NoAI", true),
                position: [x, 151.0, 8.5],
                fresh: false,
                reply,
            })
            .await;
        assert!(summoned.await.unwrap());
        tracks.push(
            expect(&mut events, |event| match event {
                PlayerEvent::Track(track)
                    if track.kind == tuff_vanilla_data::registry::EntityType::Cow =>
                {
                    Some(track)
                }
                _ => None,
            })
            .await,
        );
    }
    let (target, calf) = (tracks[0].entity, tracks[1].entity);
    let mut player = session(&world, 1000);
    player.stand_at([4.5, 151.0, 6.5], true);
    for track in tracks {
        player.see_entity(track);
    }
    player.inventory_mut().set_slot(
        Inventory::hotbar_slot(0),
        Some(ItemStack::mint(Item::DiamondSword, 1, MintReason::Test)),
    );
    player.run_tick().await;
    for _ in 0..2 {
        player.skip_ticks(20);
        player.handle_packet(Serverbound::Attack(target)).await;
        assert!(
            player
                .handle_events_until(|e| matches!(e, PlayerEvent::AttackLanded { .. }))
                .await,
            "the hit landed"
        );
        world.ticks(21).await;
    }
    assert_eq!(player.inventory().hand(false).unwrap().damage(), 2);
    let (reply, answer) = oneshot::channel();
    region.request(RegionRequest::Entities { reply }).await;
    let left = answer.await.unwrap();
    assert!(!left.iter().any(|s| s.id == target), "the cow died");
    assert!(left.iter().any(|s| s.name == "minecraft:beef"));

    player.stand_at([11.5, 151.0, 6.5], true);
    player.inventory_mut().set_slot(
        Inventory::hotbar_slot(0),
        Some(ItemStack::mint(Item::Bucket, 1, MintReason::Test)),
    );
    player
        .handle_packet(Serverbound::Interact {
            entity: calf,
            off_hand: false,
            location: [0.0; 3],
            sneaking: false,
        })
        .await;
    assert!(
        player
            .handle_events_until(|e| matches!(e, PlayerEvent::EntityInteracted { .. }))
            .await
    );
    assert_eq!(
        player.inventory().hand(false).unwrap().item(),
        Item::MilkBucket
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn weapon_attacks_follow_the_cooldown_which_an_item_change_resets() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut attacker = session(&world, 1);
    attacker.inventory_mut().set_slot(
        Inventory::hotbar_slot(0),
        Some(ItemStack::mint(Item::DiamondSword, 1, MintReason::Test)),
    );
    let (victim, mut victim_events) = PlayerHandle::new(2);
    attacker.stand_at([0.5, 64.0, 0.5], true);
    attacker.see_player(&victim, [0.5, 64.0, 2.5]);
    attacker.run_tick().await;
    attacker.skip_ticks(20);
    attacker.handle_packet(Serverbound::Attack(2)).await;
    let strong = attacked(&mut victim_events).expect("the target is attacked");
    assert_eq!(strong.damage, 7.0);
    assert!(matches!(
        strong.kind,
        AttackKind::Direct {
            critical: false,
            full_strength: true,
            sweep: Some(_),
            ..
        }
    ));
    assert_eq!(strong.weapon.unwrap().item(), Item::DiamondSword);

    attacker.handle_packet(Serverbound::Attack(2)).await;
    let spam = attacked(&mut victim_events).unwrap();
    assert!(spam.damage < 2.0, "{}", spam.damage);
    assert!(matches!(
        spam.kind,
        AttackKind::Direct {
            full_strength: false,
            sweep: None,
            ..
        }
    ));

    attacker.skip_ticks(20);
    attacker.handle_packet(Serverbound::SetCarriedItem(1)).await;
    attacker.run_tick().await;
    attacker.handle_packet(Serverbound::Attack(2)).await;
    let fist = attacked(&mut victim_events).unwrap();
    let strength = tuff_vanilla_common::combat::attack_strength(0, 4.0);
    assert!((fist.damage - (0.2 + strength * strength * 0.8)).abs() < 1e-6);

    attacker.skip_ticks(20);
    attacker.handle_packet(Serverbound::Punch).await;
    attacker.handle_packet(Serverbound::Attack(2)).await;
    let swung = attacked(&mut victim_events).unwrap();
    assert!(swung.damage < 0.5, "a swing restarts the cooldown");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn landed_sweeps_hit_players_beside_the_target_and_wear_the_sword() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut attacker = session(&world, 1);
    let sword = enchanted(Item::IronSword, &[("sweeping_edge", 3)]);
    attacker
        .inventory_mut()
        .set_slot(Inventory::hotbar_slot(0), Some(sword.test_copy()));
    attacker.stand_at([0.5, 64.0, 0.5], true);
    attacker.set_sprinting(true);
    let target = [0.5, 64.0, 2.5];
    let (beside, mut beside_events) = PlayerHandle::new(3);
    let (far, mut far_events) = PlayerHandle::new(4);
    attacker.see_player(&beside, [1.5, 64.0, 2.5]);
    attacker.see_player(&far, [3.5, 64.0, 2.5]);
    attacker
        .handle_event(PlayerEvent::AttackLanded {
            weapon: Some(tuff_item::item::ItemSnapshot::of(&sword)),
            target: 2,
            target_position: target,
            knockback: true,
            sweep: Some(tuff_vanilla_common::combat::Sweep {
                damage: 1.0 + 0.75 * 6.0,
                strength: 1.0,
            }),
            health_lost: 5.0,
            target_kind: tuff_vanilla_data::registry::EntityType::Player,
            dealt: 5.0,
        })
        .await;
    let swept = attacked(&mut beside_events).expect("the bystander is swept");
    assert_eq!(swept.kind, AttackKind::Sweep);
    assert_eq!(swept.damage, 5.5);
    assert_eq!(swept.knockback, 0.4);
    assert!(attacked(&mut far_events).is_none());
    let held = attacker.inventory().hand(false).unwrap();
    assert_eq!(held.damage(), 1, "a landed hit wears the sword");
    assert!(!attacker.is_sprinting(), "a knockback hit ends the sprint");
    assert_eq!(
        attacker.stat(crate::world::stats::Stat::custom(
            tuff_vanilla_data::registry::CustomStat::DamageDealt
        )),
        50,
        "damage dealt is tracked in tenths of a heart"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn killing_a_player_credits_the_killer_with_a_pvp_kill() {
    use crate::world::stats::Stat;
    use tuff_vanilla_data::registry::{CustomStat, EntityType, StatType};

    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (attacker, mut attacker_events) = PlayerHandle::new(999);
    let mut victim = session(&world, 1);
    victim.stand_at([0.5, 64.0, 0.5], true);
    victim.handle_event(direct(&attacker, 1000.0, None)).await;
    assert_eq!(victim.health(), 0.0, "the hit is lethal");
    let kind = expect(&mut attacker_events, |event| match event {
        PlayerEvent::KilledEntity(kill) => Some(kill),
        _ => None,
    })
    .await;
    assert_eq!(kind.kind, EntityType::Player);
    let mut killer = session(&world, 2);
    killer.handle_event(PlayerEvent::KilledEntity(kind)).await;
    assert_eq!(
        killer.stat(Stat::entity(StatType::Killed, EntityType::Player)),
        1
    );
    assert_eq!(killer.stat(Stat::custom(CustomStat::PlayerKills)), 1);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stacks_own_weapon_component_disables_shields_and_sets_its_wear() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (attacker, _attacker_events) = PlayerHandle::new(999);
    let mut club = ItemStack::mint(Item::Stick, 1, MintReason::Test);
    club.components.weapon = Some(tuff_vanilla_data::item::Weapon {
        item_damage_per_attack: 4,
        disable_blocking_for_seconds: 5.0,
    });
    let mut blocker = session(&world, 1);
    blocker.inventory_mut().set_slot(
        Inventory::OFFHAND_SLOT,
        Some(ItemStack::mint(Item::Shield, 1, MintReason::Test)),
    );
    blocker.hold_up(true, 10);
    blocker
        .handle_event(direct(&attacker, 5.0, Some(club.test_copy())))
        .await;
    assert!(!blocker.is_using(), "the club knocks the shield down");
    assert!(blocker.item_on_cooldown(Item::Shield));

    let mut hammer = ItemStack::mint(Item::IronPickaxe, 1, MintReason::Test);
    hammer.components.weapon = club.components.weapon;
    let mut wielder = session(&world, 2);
    wielder
        .inventory_mut()
        .set_slot(Inventory::hotbar_slot(0), Some(hammer.test_copy()));
    wielder
        .handle_event(PlayerEvent::AttackLanded {
            weapon: Some(tuff_item::item::ItemSnapshot::of(&hammer)),
            target: 1,
            target_position: FRONT,
            knockback: false,
            sweep: None,
            health_lost: 5.0,
            target_kind: tuff_vanilla_data::registry::EntityType::Player,
            dealt: 5.0,
        })
        .await;
    assert_eq!(wielder.inventory().hand(false).unwrap().damage(), 4);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hits_push_the_victim_further_with_knockback() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (attacker, mut attacker_events) = PlayerHandle::new(999);
    let (mut plain, mut plain_out) = session_with_outbound(&world, 1);
    plain.handle_event(direct(&attacker, 2.0, None)).await;
    let push = drain(&mut plain_out)
        .iter()
        .find_map(motion)
        .expect("the hit pushes");
    assert_eq!(push.0, 1);
    assert!(push.1[2] < -0.39 && push.1[2] > -0.41, "{:?}", push.1);
    let landed =
        std::iter::from_fn(|| attacker_events.try_recv().ok()).find_map(|event| match event {
            PlayerEvent::AttackLanded {
                target, knockback, ..
            } => Some((target, knockback)),
            _ => None,
        });
    assert_eq!(landed, Some((1, false)));

    let (mut far, mut far_out) = session_with_outbound(&world, 2);
    let PlayerEvent::Attacked(mut attack) = direct(&attacker, 2.0, None) else {
        unreachable!()
    };
    attack.knockback = 1.0;
    far.handle_event(PlayerEvent::Attacked(attack)).await;
    let push = drain(&mut far_out).iter().find_map(motion).unwrap();
    assert!(push.1[2] < -1.1, "{:?}", push.1);

    let (mut heavy, mut heavy_out) = session_with_outbound(&world, 3);
    for (slot, item) in [
        (EquipmentSlot::Head, Item::NetheriteHelmet),
        (EquipmentSlot::Chest, Item::NetheriteChestplate),
        (EquipmentSlot::Legs, Item::NetheriteLeggings),
        (EquipmentSlot::Feet, Item::NetheriteBoots),
    ] {
        heavy.inventory_mut().set_slot(
            Inventory::armor_slot(slot).unwrap(),
            Some(ItemStack::mint(item, 1, MintReason::Test)),
        );
    }
    heavy.handle_event(direct(&attacker, 2.0, None)).await;
    let push = drain(&mut heavy_out).iter().find_map(motion).unwrap();
    assert!(push.1[2] < -0.23 && push.1[2] > -0.25, "{:?}", push.1);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fire_aspect_burns_the_victim_until_water_puts_it_out() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (attacker, _attacker_events) = PlayerHandle::new(999);
    let fiery = enchanted(Item::IronSword, &[("fire_aspect", 2)]);
    let mut victim = session(&world, 1);
    victim.make_hungry();
    victim
        .handle_event(direct(&attacker, 1.0, Some(fiery.test_copy())))
        .await;
    assert_eq!(victim.fire_ticks(), 160);
    assert_eq!(victim.health(), 19.0);
    for _ in 0..21 {
        victim.run_tick().await;
    }
    assert_eq!(
        victim.health(),
        18.0,
        "burning hurts once the hit's window ends"
    );
    victim
        .handle_event(PlayerEvent::Surroundings {
            dimension: victim.dimension_id(),
            seq: 0,
            in_water: true,
            submersion: Default::default(),
            in_lava: false,
            in_fire: None,
            climbable: false,
            climb_location: None,
            resets_fall: false,
            landed_on: None,
            portal: None,
            biome: None,
            stepping_on: None,
            contact: Default::default(),
        })
        .await;
    assert!(victim.fire_ticks() <= 0);

    let mut protected = session(&world, 2);
    let mut chest = ItemStack::mint(Item::IronChestplate, 1, MintReason::Test);
    chest.components.enchantments = Some(Enchantments::new([(
        "minecraft:fire_protection".to_owned(),
        4,
    )]));
    protected.inventory_mut().set_slot(
        Inventory::armor_slot(EquipmentSlot::Chest).unwrap(),
        Some(chest),
    );
    protected
        .handle_event(direct(&attacker, 1.0, Some(fiery)))
        .await;
    assert_eq!(
        protected.fire_ticks(),
        64,
        "Fire Protection shortens burning"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fire_resistance_potions_keep_lava_off_and_milk_clears_them() {
    use tuff_item::item::{ComponentPatch, PotionContents};
    use tuff_vanilla_data::registry::{MobEffect, Potion};

    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut player = session(&world, 1000);
    player.make_hungry();
    let potion =
        ItemStack::mint(Item::Potion, 1, MintReason::Test).with_components(ComponentPatch {
            potion_contents: Some(Box::new(PotionContents {
                potion: Some(Potion::FireResistance),
                ..Default::default()
            })),
            ..Default::default()
        });
    player
        .inventory_mut()
        .set_slot(Inventory::hotbar_slot(0), Some(potion));
    let drink = Serverbound::UseItem {
        off_hand: false,
        sequence: 1,
    };
    player.handle_packet(drink.clone()).await;
    assert!(player.is_using());
    for _ in 0..32 {
        player.run_tick().await;
    }
    assert!(!player.is_using());
    assert_eq!(
        player.inventory().hand(false),
        Some(&ItemStack::mint(Item::GlassBottle, 1, MintReason::Test))
    );
    assert_eq!(
        player.effect_duration(MobEffect::FireResistance),
        Some(3600)
    );
    player
        .handle_event(PlayerEvent::Surroundings {
            dimension: player.dimension_id(),
            seq: 0,
            in_water: false,
            submersion: Default::default(),
            in_lava: true,
            in_fire: None,
            climbable: false,
            climb_location: None,
            resets_fall: false,
            landed_on: None,
            portal: None,
            biome: None,
            stepping_on: None,
            contact: Default::default(),
        })
        .await;
    for _ in 0..40 {
        player.run_tick().await;
    }
    assert_eq!(player.health(), 20.0, "fire resistance stops lava and fire");
    assert!(player.fire_ticks() > 0, "the player still burns visibly");

    player.inventory_mut().set_slot(
        Inventory::hotbar_slot(0),
        Some(ItemStack::mint(Item::MilkBucket, 1, MintReason::Test)),
    );
    player.handle_packet(drink).await;
    for _ in 0..32 {
        player.run_tick().await;
    }
    assert_eq!(player.effect_duration(MobEffect::FireResistance), None);
    assert_eq!(
        player.inventory().hand(false),
        Some(&ItemStack::mint(Item::Bucket, 1, MintReason::Test))
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn splashed_potions_heal_add_effects_and_water_puts_fire_out() {
    use crate::world::effects::ActiveEffect;
    use tuff_feature_builtin_vanilla_combat::projectile::potion::PotionHit;
    use tuff_vanilla_data::registry::MobEffect;

    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (attacker, _attacker_events) = PlayerHandle::new(999);
    let mut player = session(&world, 1000);
    player.make_hungry();
    let fiery = enchanted(Item::IronSword, &[("fire_aspect", 1)]);
    player
        .handle_event(direct(&attacker, 6.0, Some(fiery)))
        .await;
    assert!(player.fire_ticks() > 0);
    assert_eq!(player.health(), 14.0);
    player
        .handle_event(PlayerEvent::Potion(Box::new(PotionHit {
            effects: vec![ActiveEffect::timed(MobEffect::Speed, 300, 0)],
            instant: vec![(MobEffect::InstantHealth, 0, 0.5)],
            extinguish: true,
        })))
        .await;
    assert_eq!(player.fire_ticks(), 0);
    assert_eq!(player.health(), 16.0, "half of a healing splash");
    assert_eq!(player.effect_duration(MobEffect::Speed), Some(300));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn riders_follow_the_vehicle_they_drive_and_stand_where_they_get_off() {
    use crate::world::messages::Riding;
    use tuff_vanilla_data::registry::EntityType;

    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut player = session(&world, 1000);
    player.stand_at([0.5, 100.0, 0.5], true);
    player
        .handle_event(PlayerEvent::Riding(Box::new(Riding {
            vehicle: 5000,
            kind: EntityType::OakBoat,
            dimension: player.dimension_id(),
            position: [0.5, 100.0, 0.5],
            riders: vec![1000],
            seat: [0.0, 0.1875, 0.0],
            driver: true,
            steers: false,
            no_gravity: false,
            flying: false,
            ridden: None,
            given: Default::default(),
        })))
        .await;
    let seated = player.position();
    assert!(
        (seated[1] - (100.0 + 0.1875 - 0.6)).abs() < 1.0e-6,
        "{seated:?}"
    );
    player
        .handle_packet(Serverbound::MoveVehicle {
            position: [3.5, 100.0, 0.5],
            yaw: 0.0,
            pitch: 0.0,
            on_ground: false,
        })
        .await;
    assert_eq!(player.position()[0], 3.5, "the rider moves with its boat");
    player
        .handle_event(PlayerEvent::Dismounted {
            vehicle: 5000,
            position: [5.5, 101.0, 0.5],
        })
        .await;
    assert_eq!(player.position(), [5.5, 101.0, 0.5]);
    player
        .handle_packet(Serverbound::MoveVehicle {
            position: [9.5, 100.0, 0.5],
            yaw: 0.0,
            pitch: 0.0,
            on_ground: false,
        })
        .await;
    assert_eq!(
        player.position(),
        [5.5, 101.0, 0.5],
        "off the boat it stays"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn statistics_count_time_moves_jumps_and_crafts_and_are_saved() {
    use crate::protocol::play::Movement;
    use crate::world::stats::Stat;
    use tuff_vanilla_data::registry::{CustomStat, StatType};

    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, mut outbound) = session_with_outbound(&world, 1000);
    player.stand_at([0.5, 100.0, 0.5], true);
    for _ in 0..3 {
        player.run_tick().await;
    }
    assert_eq!(player.stat(Stat::custom(CustomStat::PlayTime)), 3);
    assert_eq!(player.stat(Stat::custom(CustomStat::TimeSinceDeath)), 3);
    let step = |x: f64, y: f64, on_ground: bool| {
        Serverbound::Move(Movement {
            horizontal_collision: false,
            position: Some([x, y, 0.5]),
            rotation: None,
            on_ground,
        })
    };
    player.handle_packet(step(0.8, 100.0, true)).await;
    player.handle_packet(Serverbound::ClientTickEnd).await;
    player.handle_packet(step(1.2, 100.0, true)).await;
    player.handle_packet(Serverbound::ClientTickEnd).await;
    assert_eq!(player.stat(Stat::custom(CustomStat::WalkOneCm)), 70);
    player.handle_packet(step(1.2, 100.42, false)).await;
    player.handle_packet(Serverbound::ClientTickEnd).await;
    assert_eq!(player.stat(Stat::custom(CustomStat::Jump)), 1);

    player
        .inventory_mut()
        .set_carried(Some(ItemStack::mint(Item::OakLog, 1, MintReason::Test)));
    let click = |slot: i16, mode: i32| Serverbound::ContainerClick {
        window_id: 0,
        state_id: 0,
        slot,
        button: 0,
        mode,
        changed: Vec::new(),
        carried: None,
    };
    player.handle_packet(click(1, 0)).await;
    player.handle_packet(click(0, 1)).await;
    assert_eq!(
        player.stat(Stat::item(StatType::Crafted, Item::OakPlanks)),
        4
    );

    while outbound.try_recv().is_ok() {}
    player.handle_packet(Serverbound::ClientCommand(1)).await;
    let mut awarded = None;
    while let Ok(frame) = outbound.try_recv() {
        let mut r = framed_payload(&frame);
        if r.read_var_int().unwrap()
            == tuff_vanilla_data::packet::play::Clientbound::AwardStats.id()
        {
            awarded = Some(r.read_var_int().unwrap());
        }
    }
    assert!(
        awarded.unwrap() >= 5,
        "every statistic goes out the first time"
    );

    player.save().await;
    let saved = world
        .shared
        .backend
        .documents()
        .read(tuff_storage::document::Document::Stats(1000))
        .unwrap()
        .expect("statistics are saved");
    assert_eq!(
        saved
            .get_compound("stats")
            .and_then(|s| s.get_compound("minecraft:crafted"))
            .and_then(|c| c.get_int("minecraft:oak_planks")),
        Some(4)
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lava_burns_and_hurts_until_the_player_leaves_it() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut player = session(&world, 1);
    player.make_hungry();
    let surroundings = |player: &PlayerSession, in_lava: bool| PlayerEvent::Surroundings {
        dimension: player.dimension_id(),
        seq: 0,
        in_water: false,
        submersion: Default::default(),
        in_lava,
        in_fire: None,
        climbable: false,
        climb_location: None,
        resets_fall: false,
        landed_on: None,
        portal: None,
        biome: None,
        stepping_on: None,
        contact: Default::default(),
    };
    player.handle_event(surroundings(&player, true)).await;
    player.run_tick().await;
    assert_eq!(player.health(), 16.0);
    assert_eq!(player.fire_ticks(), 300, "set alight for 15 seconds");
    for _ in 0..5 {
        player.run_tick().await;
    }
    assert_eq!(player.health(), 16.0, "the hurt window holds off lava");
    for _ in 0..20 {
        player.run_tick().await;
    }
    assert!(player.health() < 16.0);
    assert_eq!(player.fire_ticks(), 300, "lava keeps the fire topped up");

    player.handle_event(surroundings(&player, false)).await;
    for _ in 0..40 {
        player.run_tick().await;
    }
    assert_eq!(player.fire_ticks(), 260, "out of lava the fire burns down");
    world.shutdown().await;
}

fn touching(player: &PlayerSession, contact: crate::world::block_contact::Contact) -> PlayerEvent {
    PlayerEvent::Surroundings {
        dimension: player.dimension_id(),
        seq: 0,
        in_water: false,
        submersion: Default::default(),
        in_lava: false,
        in_fire: None,
        climbable: false,
        climb_location: None,
        resets_fall: false,
        landed_on: None,
        portal: None,
        contact,
        biome: None,
        stepping_on: None,
    }
}

/// Health after `ticks` ticks standing on what `contact` reports.
async fn health_after(
    player: &mut PlayerSession,
    contact: crate::world::block_contact::Contact,
    ticks: usize,
) -> f32 {
    player.make_hungry();
    player.handle_event(touching(player, contact)).await;
    for _ in 0..ticks {
        player.run_tick().await;
    }
    player.health()
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn magma_burns_feet_unless_sneaking_fire_resistant_or_frost_walking() {
    use crate::world::block_contact::Contact;
    use crate::world::effects::ActiveEffect;
    use tuff_feature_builtin_vanilla_combat::projectile::potion::PotionHit;
    use tuff_vanilla_data::registry::MobEffect;

    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let magma = Contact {
        hot_floor: true,
        ..Contact::default()
    };
    let feet = [0.5, 64.0, 0.5];

    let mut player = session(&world, 1000);
    player.stand_at(feet, true);
    assert_eq!(health_after(&mut player, magma, 1).await, 19.0);
    for _ in 0..9 {
        player.run_tick().await;
    }
    assert_eq!(player.health(), 19.0, "the hurt window holds off magma");
    player.run_tick().await;
    assert_eq!(player.health(), 18.0, "magma hurts again after the window");
    assert!(player.fire_ticks() <= 0, "magma does not set anyone alight");
    player
        .handle_event(touching(&player, Contact::default()))
        .await;
    for _ in 0..40 {
        player.run_tick().await;
    }
    assert_eq!(player.health(), 18.0, "stepping off ends it");

    let mut jumper = session(&world, 1001);
    jumper.stand_at(feet, false);
    assert_eq!(
        health_after(&mut jumper, magma, 40).await,
        20.0,
        "only feet on the ground burn"
    );

    let mut sneaker = session(&world, 1002);
    sneaker.stand_at(feet, true);
    sneaker.handle_packet(Serverbound::PlayerInput(0x20)).await;
    assert_eq!(
        health_after(&mut sneaker, magma, 40).await,
        20.0,
        "sneaking steps carefully"
    );
    sneaker.handle_packet(Serverbound::PlayerInput(0)).await;
    sneaker.run_tick().await;
    assert_eq!(sneaker.health(), 19.0, "standing up burns again");

    let mut resistant = session(&world, 1003);
    resistant.stand_at(feet, true);
    resistant
        .handle_event(PlayerEvent::Potion(Box::new(PotionHit {
            effects: vec![ActiveEffect::timed(MobEffect::FireResistance, 600, 0)],
            instant: Vec::new(),
            extinguish: false,
        })))
        .await;
    assert_eq!(
        health_after(&mut resistant, magma, 40).await,
        20.0,
        "fire resistance keeps magma off"
    );

    let mut frost_walker = session(&world, 1004);
    frost_walker.stand_at(feet, true);
    frost_walker.inventory_mut().set_slot(
        Inventory::armor_slot(EquipmentSlot::Feet).unwrap(),
        Some(enchanted(Item::LeatherBoots, &[("frost_walker", 1)])),
    );
    assert_eq!(
        health_after(&mut frost_walker, magma, 40).await,
        20.0,
        "Frost Walker boots are immune to burning floors"
    );
    let campfire = Contact {
        campfire: Some(1.0),
        ..Contact::default()
    };
    assert_eq!(
        health_after(&mut frost_walker, campfire, 1).await,
        20.0,
        "and to campfires"
    );
    world.shutdown().await;
}

fn submerged(player: &PlayerSession, water: bool, bubbles: bool) -> PlayerEvent {
    PlayerEvent::Surroundings {
        dimension: player.dimension_id(),
        seq: 0,
        in_water: water,
        submersion: crate::world::player::Submersion {
            eyes: water,
            bubble_column: bubbles,
            feet: water,
        },
        in_lava: false,
        in_fire: None,
        climbable: false,
        climb_location: None,
        resets_fall: false,
        landed_on: None,
        portal: None,
        contact: Default::default(),
        biome: None,
        stepping_on: None,
    }
}

/// The air values in Set Entity Data packets and whether the drowning
/// bubbles event went out, among `frames`.
fn air_packets(frames: impl Iterator<Item = bytes::Bytes>, entity: i32) -> (Vec<i32>, bool) {
    let mut air = Vec::new();
    let mut bubbles = false;
    for frame in frames {
        let mut r = framed_payload(&frame);
        let id = r.read_var_int().unwrap();
        if id == tuff_vanilla_data::packet::play::Clientbound::EntityEvent.id() {
            bubbles |= r.read_int().unwrap() == entity
                && r.read_byte().unwrap() == play::entity_event::DROWN_PARTICLES;
        } else if id == tuff_vanilla_data::packet::play::Clientbound::SetEntityData.id()
            && r.read_var_int().unwrap() == entity
            && r.read_unsigned_byte().unwrap() == play::metadata::AIR_SUPPLY
        {
            assert_eq!(r.read_var_int().unwrap(), 1, "a VarInt");
            air.push(r.read_var_int().unwrap());
        }
    }
    (air, bubbles)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn submerged_players_run_out_of_air_drown_and_breathe_again() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, mut outbound) = session_with_outbound(&world, 1);
    player.make_hungry();
    player.handle_event(submerged(&player, true, false)).await;
    while outbound.try_recv().is_ok() {}
    player.run_tick().await;
    let (air, _) = air_packets(std::iter::from_fn(|| outbound.try_recv().ok()), 1);
    assert_eq!(air, [299], "the player's own client sees its air");
    for _ in 1..300 {
        player.run_tick().await;
    }
    assert_eq!(player.air(), 0);
    for _ in 0..19 {
        player.run_tick().await;
    }
    assert_eq!((player.air(), player.health()), (-19, 20.0));
    while outbound.try_recv().is_ok() {}
    player.run_tick().await;
    let (air, bubbles) = air_packets(std::iter::from_fn(|| outbound.try_recv().ok()), 1);
    assert_eq!(
        (player.air(), player.health()),
        (0, 18.0),
        "at -20 air the player drowns a little and gets its breath back"
    );
    assert!(bubbles, "the bubbles show");
    assert_eq!(air.last(), Some(&0));
    for _ in 0..19 {
        player.run_tick().await;
    }
    assert_eq!(player.health(), 18.0);
    player.run_tick().await;
    assert_eq!(player.health(), 16.0, "every 20 ticks after");

    player.save().await;
    let saved = world
        .shared
        .backend
        .documents()
        .read(tuff_storage::document::Document::Player(1))
        .unwrap()
        .expect("the player is saved");
    assert_eq!(saved.get_short("Air"), Some(0));

    player.handle_event(submerged(&player, false, false)).await;
    player.run_tick().await;
    assert_eq!(player.air(), 4, "out of water the air comes back 4 a tick");
    for _ in 0..80 {
        player.run_tick().await;
    }
    assert_eq!(player.air(), 300);

    player.handle_event(submerged(&player, true, false)).await;
    for _ in 0..30 {
        player.run_tick().await;
    }
    assert_eq!(player.air(), 270);
    player.handle_event(submerged(&player, true, true)).await;
    player.run_tick().await;
    assert_eq!(player.air(), 274, "a bubble column gives air");
    player.handle_event(submerged(&player, true, false)).await;
    player
        .give_effect(effects::ActiveEffect::timed(
            tuff_vanilla_data::registry::MobEffect::WaterBreathing,
            600,
            0,
        ))
        .await;
    for _ in 0..10 {
        player.run_tick().await;
    }
    assert_eq!(player.air(), 300, "Water Breathing refills it under water");
    for _ in 0..100 {
        player.run_tick().await;
    }
    assert_eq!(player.air(), 300);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn creative_players_and_respiration_keep_their_breath() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut creative = session(&world, 1);
    creative.set_game_mode(play::GameMode::Creative).await;
    creative
        .handle_event(submerged(&creative, true, false))
        .await;
    for _ in 0..400 {
        creative.run_tick().await;
    }
    assert_eq!(creative.air(), 300, "creative players never lose air");

    let mut plain = session(&world, 2);
    let mut helmeted = session(&world, 3);
    helmeted.inventory_mut().set_slot(
        Inventory::armor_slot(EquipmentSlot::Head).unwrap(),
        Some(enchanted(Item::TurtleHelmet, &[("respiration", 3)])),
    );
    for player in [&mut plain, &mut helmeted] {
        player.handle_event(submerged(player, true, false)).await;
        for _ in 0..200 {
            player.run_tick().await;
        }
    }
    assert_eq!(plain.air(), 100);
    assert!(
        (235..=265).contains(&helmeted.air()),
        "Respiration III loses air one tick in four: {}",
        helmeted.air()
    );
    world.shutdown().await;
}

fn water_contact(player: &PlayerSession, water: bool) -> PlayerEvent {
    PlayerEvent::FluidContact {
        dimension: player.dimension_id(),
        in_water: water,
        submersion: crate::world::player::Submersion {
            eyes: water,
            bubble_column: false,
            feet: water,
        },
        in_lava: false,
        in_fire: None,
    }
}

fn lava_contact(player: &PlayerSession, lava: bool) -> PlayerEvent {
    PlayerEvent::FluidContact {
        dimension: player.dimension_id(),
        in_water: false,
        submersion: crate::world::player::Submersion {
            eyes: false,
            bubble_column: false,
            feet: false,
        },
        in_lava: lava,
        in_fire: None,
    }
}

/// Lava reaching a motionless player, reported by its region rather than
/// by the player's own movement, sets it alight and hurts it with no
/// movement at all.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_region_pushed_lava_report_burns_a_still_player() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut player = session(&world, 1);
    let full = player.health();
    player.handle_event(lava_contact(&player, true)).await;
    player.run_tick().await;
    assert!(player.fire_ticks() > 0, "lava set the player alight");
    assert!(player.health() < full, "lava hurt the player");

    player.handle_event(lava_contact(&player, false)).await;
    let before = player.health();
    for _ in 0..10 {
        player.run_tick().await;
    }
    assert!(
        player.health() >= before - 1.0,
        "out of lava only the fire burns: {before} -> {}",
        player.health()
    );
    world.shutdown().await;
}

/// A region can recheck a motionless player's water contact on its
/// own (a fluid tick spreading water over it, or another player placing
/// one) rather than only ever learning it from that player's own next
/// movement report. This drives the session's handler for that report the
/// way `submerged_players_run_out_of_air_drown_and_breathe_again` drives
/// the ordinary movement-triggered one, to prove air actually starts
/// running out from it with no movement at all.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_region_pushed_water_report_starts_the_player_drowning() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut player = session(&world, 1);
    player.ignite(10.0);
    assert!(player.fire_ticks() > 0);
    player.handle_event(water_contact(&player, true)).await;
    assert_eq!(player.air(), 300, "still full until a tick runs");
    assert_eq!(player.fire_ticks(), 0, "water put the fire out at once");
    for _ in 0..300 {
        player.run_tick().await;
    }
    assert_eq!(player.air(), 0, "air ran out with no movement at all");

    player.handle_event(water_contact(&player, false)).await;
    player.run_tick().await;
    assert_eq!(player.air(), 4, "leaving water refills it again");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn campfires_burn_and_berry_bushes_prick_moving_players() {
    use crate::world::block_contact::Contact;

    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut player = session(&world, 1000);
    let soul_campfire = Contact {
        campfire: Some(2.0),
        ..Contact::default()
    };
    assert_eq!(health_after(&mut player, soul_campfire, 1).await, 18.0);
    assert!(
        player.fire_ticks() <= 0,
        "campfires do not set anyone alight"
    );

    let bush = Contact {
        berry_bush: true,
        ..Contact::default()
    };
    let mut still = session(&world, 1001);
    assert_eq!(
        health_after(&mut still, bush, 40).await,
        20.0,
        "a bush only pricks what moves"
    );
    let mut walker = session(&world, 1002);
    walker.stand_at([0.0, 64.0, 0.0], true);
    walker.make_hungry();
    walker.handle_event(touching(&walker, bush)).await;
    walker
        .handle_packet(Serverbound::Move(crate::protocol::play::Movement {
            position: Some([0.1, 64.0, 0.0]),
            rotation: None,
            on_ground: true,
            horizontal_collision: false,
        }))
        .await;
    walker.run_tick().await;
    assert_eq!(walker.health(), 19.0);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cactus_pricks_players_every_tick_while_touching() {
    use crate::world::block_contact::Contact;

    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let cactus = Contact {
        cactus: true,
        ..Contact::default()
    };
    let mut player = session(&world, 1000);
    assert_eq!(health_after(&mut player, cactus, 1).await, 19.0);
    for _ in 0..9 {
        player.run_tick().await;
    }
    assert_eq!(player.health(), 19.0, "the hurt window holds off cactus");
    player.run_tick().await;
    assert_eq!(player.health(), 18.0, "cactus hurts again after the window");
    assert!(
        player.fire_ticks() <= 0,
        "cactus does not set anyone alight"
    );

    player
        .handle_event(touching(&player, Contact::default()))
        .await;
    for _ in 0..40 {
        player.run_tick().await;
    }
    assert_eq!(player.health(), 18.0, "moving away ends it");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn suffocation_hurts_a_player_whose_eyes_are_inside_a_block() {
    use crate::world::block_contact::Contact;

    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let trapped = Contact {
        suffocating: true,
        ..Contact::default()
    };
    let mut player = session(&world, 1000);
    assert_eq!(health_after(&mut player, trapped, 1).await, 19.0);
    for _ in 0..9 {
        player.run_tick().await;
    }
    assert_eq!(
        player.health(),
        19.0,
        "the hurt window holds off suffocation"
    );
    player.run_tick().await;
    assert_eq!(
        player.health(),
        18.0,
        "suffocation hurts again after the window"
    );
    assert!(
        player.fire_ticks() <= 0,
        "suffocation does not set anyone alight"
    );

    // `minecraft:in_wall` is in the `bypasses_armor` damage type tag
    // (unlike, say, `minecraft:cactus`), so bare armor points do
    // nothing against it, but it is not in `bypasses_enchantments`, so
    // Protection still reduces it like most other damage.
    let mut armored = session(&world, 1001);
    armored.inventory_mut().set_slot(
        Inventory::armor_slot(EquipmentSlot::Chest).unwrap(),
        Some(ItemStack::mint(
            Item::DiamondChestplate,
            1,
            MintReason::Test,
        )),
    );
    assert_eq!(
        health_after(&mut armored, trapped, 1).await,
        19.0,
        "bare armor points do not reduce suffocation damage"
    );

    let mut protected = session(&world, 1002);
    protected.inventory_mut().set_slot(
        Inventory::armor_slot(EquipmentSlot::Chest).unwrap(),
        Some(enchanted(Item::DiamondChestplate, &[("protection", 4)])),
    );
    let after_protection = health_after(&mut protected, trapped, 1).await;
    assert!(
        (19.0..20.0).contains(&after_protection),
        "Protection still reduces suffocation damage: {after_protection}"
    );

    let mut clear = session(&world, 1003);
    assert_eq!(
        health_after(&mut clear, Contact::default(), 40).await,
        20.0,
        "no contact means no suffocation damage"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sprinting_under_water_swims_with_the_eyes_low() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut player = session(&world, 1);
    player.set_sprinting(true);
    player.run_tick().await;
    assert_eq!(player.pose(), Pose::Standing, "no swimming on land");
    player.handle_event(submerged(&player, true, false)).await;
    player.run_tick().await;
    assert_eq!(player.pose(), Pose::Swimming);
    assert_eq!(player.looks().eye_height, f64::from(0.4f32));
    let wading = PlayerEvent::Surroundings {
        dimension: player.dimension_id(),
        seq: 0,
        in_water: true,
        submersion: Default::default(),
        in_lava: false,
        in_fire: None,
        climbable: false,
        climb_location: None,
        resets_fall: false,
        landed_on: None,
        portal: None,
        contact: Default::default(),
        biome: None,
        stepping_on: None,
    };
    player.handle_event(wading).await;
    player.run_tick().await;
    assert_eq!(player.pose(), Pose::Swimming, "swimming keeps up in water");
    player.set_sprinting(false);
    player.run_tick().await;
    assert_eq!(player.pose(), Pose::Standing);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn standing_in_fire_sets_the_player_alight_and_hurts() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut player = session(&world, 1);
    player.make_hungry();
    let surroundings = |player: &PlayerSession, in_fire: Option<f32>| PlayerEvent::Surroundings {
        dimension: player.dimension_id(),
        seq: 0,
        in_water: false,
        submersion: Default::default(),
        in_lava: false,
        in_fire,
        climbable: false,
        climb_location: None,
        resets_fall: false,
        landed_on: None,
        portal: None,
        contact: crate::world::block_contact::Contact::default(),
        biome: None,
        stepping_on: None,
    };
    player.handle_event(surroundings(&player, Some(1.0))).await;
    player.run_tick().await;
    assert_eq!(player.health(), 19.0);
    assert!(player.fire_ticks() >= 160, "set alight for 8 seconds");
    player.handle_event(surroundings(&player, None)).await;
    for _ in 0..45 {
        player.run_tick().await;
    }
    assert!(player.health() < 19.0, "the fire keeps burning");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn thorns_hurt_the_attacker_and_wear_the_armor() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (attacker, mut attacker_events) = PlayerHandle::new(999);
    let chest_slot = Inventory::armor_slot(EquipmentSlot::Chest).unwrap();
    let mut victim = session(&world, 1);
    victim.inventory_mut().set_slot(
        chest_slot,
        Some(enchanted(Item::IronChestplate, &[("thorns", 10)])),
    );
    victim.handle_event(direct(&attacker, 4.0, None)).await;
    let thorns = std::iter::from_fn(|| attacker_events.try_recv().ok())
        .find_map(|event| match event {
            PlayerEvent::Hurt {
                amount,
                kind,
                attacker,
                position,
            } => Some((amount, kind.name, attacker, position)),
            _ => None,
        })
        .expect("thorns strike back");
    assert!((1.0..=5.0).contains(&thorns.0));
    assert_eq!(thorns.1, "minecraft:thorns");
    assert_eq!(thorns.2.map(|(id, _)| id), Some(1));
    assert!(thorns.3.is_some());
    let worn = victim.inventory().slots()[chest_slot].as_ref().unwrap();
    assert_eq!(worn.damage(), 1 + 2, "the hit's wear plus Thorns' own");

    let (mut struck, mut struck_out) = session_with_outbound(&world, 2);
    struck
        .handle_event(PlayerEvent::Hurt {
            amount: 3.0,
            kind: &damage_type::THORNS,
            attacker: Some((1, "Victim".to_owned())),
            position: Some(FRONT),
        })
        .await;
    assert_eq!(struck.health(), 17.0);
    let push = drain(&mut struck_out).iter().find_map(motion).unwrap();
    assert!(
        push.1[2] < -0.39,
        "pushed away from the thorns: {:?}",
        push.1
    );
    world.shutdown().await;
}

fn has_sound(frames: &[bytes::Bytes]) -> bool {
    let id = tuff_vanilla_data::packet::play::Clientbound::Sound.id();
    frames.iter().any(|frame| framed_packet_id(frame) == id)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn login_syncs_worn_armor_attributes_to_the_wearer() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, mut out) = session_with_outbound(&world, 1);
    for (slot, item) in [
        (EquipmentSlot::Head, Item::NetheriteHelmet),
        (EquipmentSlot::Chest, Item::NetheriteChestplate),
        (EquipmentSlot::Legs, Item::NetheriteLeggings),
        (EquipmentSlot::Feet, Item::NetheriteBoots),
    ] {
        player.inventory_mut().set_slot(
            Inventory::armor_slot(slot).unwrap(),
            Some(ItemStack::mint(item, 1, MintReason::Test)),
        );
    }
    player.run_tick().await;
    let values = synced_attribute_values(&drain(&mut out));
    assert_eq!(values.get(&Attribute::Armor), Some(&20.0));
    assert_eq!(values.get(&Attribute::ArmorToughness), Some(&12.0));
    assert_eq!(values.get(&Attribute::MaxHealth), Some(&20.0));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn equipping_and_removing_armor_resyncs_its_attributes() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, mut out) = session_with_outbound(&world, 1);
    player.run_tick().await;
    drain(&mut out);

    let chest_slot = Inventory::armor_slot(EquipmentSlot::Chest).unwrap();
    player.inventory_mut().set_slot(
        chest_slot,
        Some(ItemStack::mint(
            Item::NetheriteChestplate,
            1,
            MintReason::Test,
        )),
    );
    player.run_tick().await;
    let frames = drain(&mut out);
    assert!(has_sound(&frames), "equipping plays the equip sound");
    let values = synced_attribute_values(&frames);
    assert_eq!(values.get(&Attribute::Armor), Some(&8.0));
    assert_eq!(values.get(&Attribute::ArmorToughness), Some(&3.0));

    player.inventory_mut().set_slot(
        chest_slot,
        Some(ItemStack::mint(Item::IronChestplate, 1, MintReason::Test)),
    );
    player.run_tick().await;
    let frames = drain(&mut out);
    assert!(has_sound(&frames), "swapping to a different item plays it");
    let values = synced_attribute_values(&frames);
    assert_eq!(values.get(&Attribute::Armor), Some(&6.0));
    assert_eq!(values.get(&Attribute::ArmorToughness), Some(&0.0));

    player.inventory_mut().set_slot(chest_slot, None);
    player.run_tick().await;
    let frames = drain(&mut out);
    let values = synced_attribute_values(&frames);
    assert_eq!(values.get(&Attribute::Armor), Some(&0.0));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn armor_damaged_by_a_hit_resyncs_attributes_without_the_equip_sound() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (attacker, _attacker_events) = PlayerHandle::new(999);
    let (mut victim, mut out) = session_with_outbound(&world, 1);
    let chest_slot = Inventory::armor_slot(EquipmentSlot::Chest).unwrap();
    victim.inventory_mut().set_slot(
        chest_slot,
        Some(ItemStack::mint(Item::IronChestplate, 1, MintReason::Test)),
    );
    victim.run_tick().await;
    let frames = drain(&mut out);
    assert!(has_sound(&frames), "first equip still plays the sound");

    victim.handle_event(direct(&attacker, 4.0, None)).await;
    victim.run_tick().await;
    let worn = victim.inventory().slots()[chest_slot].as_ref().unwrap();
    assert!(worn.damage() > 0, "the hit damaged the chestplate");
    let frames = drain(&mut out);
    assert!(
        !has_sound(&frames),
        "durability damage alone must not replay the equip sound"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_effect_that_changes_a_synced_attribute_resyncs_it() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, mut out) = session_with_outbound(&world, 1);
    player.run_tick().await;
    drain(&mut out);

    player
        .give_effect(effects::ActiveEffect::timed(
            tuff_vanilla_data::registry::MobEffect::HealthBoost,
            600,
            0,
        ))
        .await;
    player.run_tick().await;
    let values = synced_attribute_values(&drain(&mut out));
    assert_eq!(
        values.get(&Attribute::MaxHealth),
        Some(&24.0),
        "Health Boost's own attribute modifier reaches the client"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn drawn_bows_shoot_arrows_that_infinity_makes_free() {
    use crate::protocol::play::PlayerActionStatus;
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let region = world.region(ChunkPos::new(0, 0).region()).await;
    let release = Serverbound::PlayerAction {
        status: PlayerActionStatus::ReleaseUseItem,
        position: BlockPos::new(0, 0, 0),
        face: 0,
        sequence: 2,
    };
    let arrows_in = |player: &PlayerSession| {
        player.inventory().slots()[Inventory::hotbar_slot(1)]
            .as_ref()
            .map_or(0, |s| s.count())
    };
    for (bow, spent) in [
        (ItemStack::mint(Item::Bow, 1, MintReason::Test), 1),
        (enchanted(Item::Bow, &[("infinity", 1), ("power", 5)]), 0),
    ] {
        let mut player = session(&world, 1000);
        player.stand_at([4.5, 151.0, 6.5], true);
        player
            .inventory_mut()
            .set_slot(Inventory::hotbar_slot(0), Some(bow));
        player.inventory_mut().set_slot(
            Inventory::hotbar_slot(1),
            Some(ItemStack::mint(Item::Arrow, 2, MintReason::Test)),
        );
        player
            .handle_packet(Serverbound::UseItem {
                off_hand: false,
                sequence: 1,
            })
            .await;
        assert!(player.is_using(), "the bow is drawn");
        for _ in 0..25 {
            player.run_tick().await;
        }
        player.handle_packet(release.clone()).await;
        assert!(!player.is_using());
        assert_eq!(arrows_in(&player), 2 - spent);
        assert_eq!(player.inventory().hand(false).unwrap().damage(), 1);
    }
    wait_for(&world, &region, 100, |found| {
        found
            .iter()
            .filter(|s| s.kind == tuff_vanilla_data::registry::EntityType::Arrow)
            .count()
            == 2
    })
    .await;
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_wind_burst_pushes_a_player_without_hurting() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut player = session(&world, 3);
    player
        .handle_event(PlayerEvent::Exploded {
            damage: 6.0,
            blast: Blast {
                center: FRONT,
                radius: 1.2,
                damage_type: &damage_type::WIND_CHARGE,
                igniter: None,
                culprit: None,
                wind: Some(crate::world::explosion::WindBurst {
                    knockback: 1.22,
                    sound: tuff_vanilla_data::registry::SoundEvent::EntityWindChargeWindBurst,
                    source: tuff_vanilla_data::registry::EntityType::WindCharge,
                    spares_fall: true,
                }),
            },
            push: [0.0, 0.8, 0.0],
            push_sent: true,
        })
        .await;
    assert_eq!(player.health(), 20.0);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn thrown_wind_charges_fly_from_the_eyes_and_cool_down() {
    use crate::world::stats::Stat;
    use tuff_vanilla_data::registry::{EntityType, StatType};

    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let region = world.region(ChunkPos::new(0, 0).region()).await;
    let mut player = session(&world, 1000);
    player.stand_at([4.5, 151.0, 6.5], true);
    player.inventory_mut().set_slot(
        Inventory::hotbar_slot(0),
        Some(ItemStack::mint(Item::WindCharge, 2, MintReason::Test)),
    );
    for sequence in 1..=2 {
        player
            .handle_packet(Serverbound::UseItem {
                off_hand: false,
                sequence,
            })
            .await;
    }
    assert_eq!(
        player.inventory().hand(false).map(|s| s.count()),
        Some(1),
        "the second throw waits out the cooldown"
    );
    assert_eq!(player.stat(Stat::item(StatType::Used, Item::WindCharge)), 1);
    let found = wait_for(&world, &region, 20, |found| {
        found.iter().any(|s| s.kind == EntityType::WindCharge)
    })
    .await;
    assert!(
        found.iter().any(|s| s.kind == EntityType::WindCharge),
        "a wind charge flies: {found:?}"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_fresh_wind_charge_resists_deflection_and_a_gone_thrower_ends_it() {
    use tuff_vanilla_data::registry::EntityType;
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(1, 1);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    view_around(&world, &viewer, &mut events, chunk).await;
    let (reply, summoned) = oneshot::channel();
    region
        .request(RegionRequest::Summon {
            nbt: Compound::new()
                .with("id", "minecraft:wind_charge")
                .with("Motion", Tag::List(vec![Tag::Double(0.0); 3])),
            position: [24.5, 160.0, 24.5],
            fresh: false,
            reply,
        })
        .await;
    assert!(summoned.await.unwrap());
    let charge = mobs(&region, EntityType::WindCharge).await[0].id;
    let motion = async || {
        let (reply, data) = oneshot::channel();
        region
            .request(RegionRequest::EntityData { id: charge, reply })
            .await;
        let data = data.await.unwrap().expect("the charge");
        let motion: Vec<f64> = data
            .get_list("Motion")
            .unwrap()
            .iter()
            .map(|t| match t {
                Tag::Double(v) => *v,
                _ => 0.0,
            })
            .collect();
        motion
    };
    let swing = || RegionRequest::AttackEntity {
        attacker: viewer.clone(),
        target: charge,
        attack: crate::world::entity::EntityAttack {
            attacker: viewer.id,
            attacker_uuid: 100,
            attacker_position: [24.5, 159.0, 22.5],
            attacker_yaw: 0.0,
            attacker_pitch: 0.0,
            damage: 0.0,
            knockback: 0.0,
            weapon: None,
            critical: false,
            magic: false,
            sweep: false,
            part: None,
        },
        sweep: None,
    };
    world.ticks(1).await;
    region.request(swing()).await;
    world.ticks(1).await;
    assert_eq!(motion().await, vec![0.0; 3], "too fresh to turn");
    world.ticks(5).await;
    region.request(swing()).await;
    world.ticks(1).await;
    let turned = motion().await;
    assert!(turned[2] > 0.9, "sent along the swing's look: {turned:?}");

    let mut thrower = session(&world, 1000);
    thrower.stand_at([20.5, 151.0, 20.5], true);
    thrower.inventory_mut().set_slot(
        Inventory::hotbar_slot(0),
        Some(ItemStack::mint(Item::WindCharge, 1, MintReason::Test)),
    );
    thrower
        .handle_packet(Serverbound::UseItem {
            off_hand: false,
            sequence: 1,
        })
        .await;
    world.ticks(1).await;
    let thrown = || async {
        mobs(&region, EntityType::WindCharge)
            .await
            .into_iter()
            .filter(|s| s.id != charge)
            .count()
    };
    assert_eq!(thrown().await, 1, "the thrown charge flies");
    drop(thrower);
    world.ticks(2).await;
    assert_eq!(thrown().await, 0, "its thrower left, so it is gone");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_blast_hands_a_wind_charge_to_its_culprit_and_a_dead_mob_owner_ends_it() {
    use tuff_vanilla_data::registry::EntityType;
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(1, 1);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    view_around(&world, &viewer, &mut events, chunk).await;
    let summon = async |nbt: Compound, at: [f64; 3]| {
        let (reply, summoned) = oneshot::channel();
        region
            .request(RegionRequest::Summon {
                nbt,
                position: at,
                fresh: false,
                reply,
            })
            .await;
        assert!(summoned.await.unwrap());
    };
    summon(
        Compound::new()
            .with("id", "minecraft:wind_charge")
            .with("Owner", Tag::uuid(5))
            .with("Motion", Tag::List(vec![Tag::Double(0.0); 3])),
        [24.5, 160.0, 24.5],
    )
    .await;
    let charge = mobs(&region, EntityType::WindCharge).await[0].id;
    let owner = async || {
        let (reply, data) = oneshot::channel();
        region
            .request(RegionRequest::EntityData { id: charge, reply })
            .await;
        data.await.unwrap().and_then(|d| d.get("Owner").cloned())
    };
    assert_eq!(owner().await, Some(Tag::uuid(5)));
    region
        .request(RegionRequest::Run(
            crate::world::region_api::RegionTask::new(|world| {
                world.explode(crate::world::explosion::Explosion {
                    center: [24.5, 160.0, 25.5],
                    radius: 1.2,
                    fire: false,
                    blocks: crate::world::explosion::BlockInteraction::Trigger(
                        crate::world::explosion::WindBurst {
                            knockback: 1.22,
                            sound:
                                tuff_vanilla_data::registry::SoundEvent::EntityWindChargeWindBurst,
                            source: EntityType::WindCharge,
                            spares_fall: true,
                        },
                    ),
                    damage_type: &tuff_vanilla_data::damage_type::WIND_CHARGE,
                    igniter: Some(crate::world::messages::Igniter {
                        id: 100,
                        uuid: 77,
                        name: "Steve".to_owned(),
                    }),
                    culprit: None,
                    dangerous_wither_skull: false,
                    spares_nether_portals: false,
                });
            }),
        ))
        .await;
    world.ticks(1).await;
    assert_eq!(owner().await, Some(Tag::uuid(77)), "the blast's player");

    summon(
        Compound::new()
            .with("id", "minecraft:pig")
            .with("UUID", Tag::uuid(9)),
        [28.5, 160.0, 28.5],
    )
    .await;
    summon(
        Compound::new()
            .with("id", "minecraft:wind_charge")
            .with("Owner", Tag::uuid(9))
            .with("Motion", Tag::List(vec![Tag::Double(0.0); 3])),
        [20.5, 160.0, 20.5],
    )
    .await;
    world.ticks(2).await;
    let pigs_charge: Vec<i32> = mobs(&region, EntityType::WindCharge)
        .await
        .into_iter()
        .map(|s| s.id)
        .filter(|id| *id != charge)
        .collect();
    assert_eq!(pigs_charge.len(), 1);
    region
        .request(RegionRequest::Run(
            crate::world::region_api::RegionTask::new(|world| {
                world.explode(crate::world::explosion::Explosion {
                    center: [28.5, 160.5, 28.5],
                    radius: 4.0,
                    fire: false,
                    blocks: crate::world::explosion::BlockInteraction::Keep,
                    damage_type: &tuff_vanilla_data::damage_type::EXPLOSION,
                    igniter: None,
                    culprit: None,
                    dangerous_wither_skull: false,
                    spares_nether_portals: false,
                });
            }),
        ))
        .await;
    world.ticks(30).await;
    assert!(
        mobs(&region, EntityType::Pig).await.is_empty(),
        "the pig died"
    );
    assert!(
        mobs(&region, EntityType::WindCharge)
            .await
            .iter()
            .all(|s| s.id != pigs_charge[0]),
        "the dead pig's charge is gone"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_arrow_hitting_a_wind_charge_sends_it_on_as_the_archers() {
    use tuff_vanilla_data::registry::EntityType;
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(1, 1);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    view_around(&world, &viewer, &mut events, chunk).await;
    for (nbt, at) in [
        (
            Compound::new()
                .with("id", "minecraft:pig")
                .with("UUID", Tag::uuid(8)),
            [30.5, 160.0, 30.5],
        ),
        (
            Compound::new()
                .with("id", "minecraft:wind_charge")
                .with("Owner", Tag::uuid(5))
                .with("Motion", Tag::List(vec![Tag::Double(0.0); 3])),
            [24.5, 160.0, 28.5],
        ),
        (
            Compound::new()
                .with("id", "minecraft:arrow")
                .with("Owner", Tag::uuid(8))
                .with(
                    "Motion",
                    Tag::List(vec![Tag::Double(0.0), Tag::Double(0.0), Tag::Double(1.5)]),
                ),
            [24.5, 160.0, 24.5],
        ),
    ] {
        let (reply, summoned) = oneshot::channel();
        region
            .request(RegionRequest::Summon {
                nbt,
                position: at,
                fresh: false,
                reply,
            })
            .await;
        assert!(summoned.await.unwrap());
    }
    let charge = mobs(&region, EntityType::WindCharge).await[0].id;
    world.ticks(6).await;
    let (reply, data) = oneshot::channel();
    region
        .request(RegionRequest::EntityData { id: charge, reply })
        .await;
    let data = data.await.unwrap().expect("the charge");
    assert_eq!(data.get("Owner"), Some(&Tag::uuid(8)));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mob_hits_scale_with_difficulty_push_and_burn() {
    use crate::world::entity::{Culprit, EntityBase, EntityHit};
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let zombie = Culprit::entity(&EntityBase::new(
        tuff_vanilla_data::registry::EntityType::Zombie,
        5,
        5,
        FRONT,
        [0.0; 3],
    ));
    let hit = |amount| {
        let mut hit = EntityHit::new(amount, &damage_type::MOB_ATTACK);
        hit.attacker = Some(zombie.clone());
        hit.position = Some(FRONT);
        hit.ignite = 2.0;
        PlayerEvent::EntityHurt(Box::new(hit))
    };
    let (mut player, mut outbound) = session_with_outbound(&world, 1);
    player.handle_event(hit(3.0)).await;
    assert_eq!(player.health(), 17.0);
    assert_eq!(player.fire_ticks(), 40);
    let push = drain(&mut outbound).iter().find_map(motion).unwrap();
    assert!(
        push.1[2] < -0.39,
        "pushed away from the zombie: {:?}",
        push.1
    );

    world.difficulty.send_replace(Difficulty::Hard);
    let mut player = session(&world, 2);
    player.handle_event(hit(3.0)).await;
    assert_eq!(player.health(), 20.0 - 4.5, "hard hits half again as hard");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mob_hits_that_land_give_their_effects() {
    use crate::world::effects::ActiveEffect;
    use crate::world::entity::EntityHit;
    use tuff_vanilla_data::registry::MobEffect;
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let hit = |amount| {
        let mut hit = EntityHit::new(amount, &damage_type::MOB_ATTACK);
        hit.position = Some(FRONT);
        hit.effects = vec![ActiveEffect::timed(MobEffect::Wither, 200, 0)];
        PlayerEvent::EntityHurt(Box::new(hit))
    };
    let mut player = session(&world, 1);
    player.handle_event(hit(2.0)).await;
    assert_eq!(
        player.effect(MobEffect::Wither).map(|e| e.duration),
        Some(200)
    );
    let mut player = session(&world, 2);
    player.handle_event(hit(0.0)).await;
    assert!(
        player.effect(MobEffect::Wither).is_none(),
        "a hit that does nothing gives nothing"
    );
    world.shutdown().await;
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Entry {
    Byte(i8),
    Float(f32),
    OptionalPosition(Option<BlockPos>),
    Pose(i32),
    Arm(i32),
    Boolean(bool),
    /// The number of particles in a list.
    Particles(i32),
}

/// The entity and `(index, type id, value)` entries of a framed Set
/// Entity Data packet.
fn entity_data(frame: &bytes::Bytes) -> EntityData {
    use crate::protocol::codec::ReadExt as _;
    let mut r = framed_payload(frame);
    if r.read_var_int().ok()? != tuff_vanilla_data::packet::play::Clientbound::SetEntityData.id() {
        return None;
    }
    let entity = r.read_var_int().unwrap();
    let mut entries = Vec::new();
    loop {
        let index = r.read_unsigned_byte().unwrap();
        if index == 0xFF {
            break;
        }
        let kind = r.read_var_int().unwrap();
        let value = match kind {
            0 => Entry::Byte(r.read_byte().unwrap()),
            3 => Entry::Float(r.read_float().unwrap()),
            11 => {
                Entry::OptionalPosition(r.read_bool().unwrap().then(|| r.read_position().unwrap()))
            }
            20 => Entry::Pose(r.read_var_int().unwrap()),
            kind if kind == play::MetadataValue::HumanoidArm(0).type_id() => {
                Entry::Arm(r.read_var_int().unwrap())
            }
            8 => Entry::Boolean(r.read_bool().unwrap()),
            17 => {
                let count = r.read_var_int().unwrap();
                assert_eq!(count, 0, "no effect particles in these tests");
                Entry::Particles(count)
            }
            other => panic!("unexpected metadata type {other}"),
        };
        entries.push((index, kind, value));
    }
    assert_eq!(r.remaining(), 0);
    Some((entity, entries))
}

fn animation(frame: &bytes::Bytes) -> Option<(i32, u8)> {
    let mut r = framed_payload(frame);
    if r.read_var_int().ok()? != tuff_vanilla_data::packet::play::Clientbound::Animate.id() {
        return None;
    }
    Some((r.read_var_int().unwrap(), r.read_unsigned_byte().unwrap()))
}

/// A still player's flags, pose (sleeping 2, standing 0), hand state,
/// absorption and bed.
fn bed_entries(bed: Option<BlockPos>) -> Vec<(u8, i32, Entry)> {
    vec![
        (0, 0, Entry::Byte(0)),
        (6, 20, Entry::Pose(if bed.is_some() { 2 } else { 0 })),
        (8, 0, Entry::Byte(0)),
        (17, 3, Entry::Float(0.0)),
        (14, 11, Entry::OptionalPosition(bed)),
    ]
}

fn drain(outbound: &mut mpsc::UnboundedReceiver<bytes::Bytes>) -> Vec<bytes::Bytes> {
    std::iter::from_fn(|| outbound.try_recv().ok()).collect()
}

/// The sleeper's own client lies down only once it learns its bed from
/// its own entity data, so that must reach it as well as its viewers.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sleepers_and_their_viewers_get_the_bed_pose_and_lose_it_on_waking() {
    use tuff_vanilla_data::block::{Facing, Part};
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    let head = BlockPos::new(5, 150, 5);
    let foot = Block::RedBed
        .default_state()
        .with_facing(Facing::South)
        .unwrap();
    place_blocks(
        &world,
        &region,
        [
            (BlockPos::new(5, 150, 4), foot),
            (head, foot.with_part(Part::Head).unwrap()),
        ],
    )
    .await;

    let (mut sleeper, mut own) = session_with_outbound(&world, 1);
    let dimension = sleeper.dimension_id();
    // Standing beside the bed first makes the session hold its region.
    sleeper
        .handle_event(PlayerEvent::Run(SessionTask::new(move |s| {
            Box::pin(s.left_bed(dimension, [5.5, 151.0, 3.5], 0.0))
        })))
        .await;
    expect(&mut events, |event| match event {
        PlayerEvent::Track(track) if track.entity == 1 => Some(()),
        _ => None,
    })
    .await;
    drain(&mut own);

    sleeper
        .handle_event(PlayerEvent::Run(SessionTask::new(move |s| {
            Box::pin(s.use_bed(BedUse {
                dimension,
                pos: head,
                rule: tuff_vanilla_data::block::BedRuleAttribute::Bed,
                set_spawn: false,
                sleep: Ok(150.6875),
                monsters_near: false,
            }))
        })))
        .await;
    assert!(sleeper.is_sleeping());
    world.ticks(2).await;
    let sent = drain(&mut own);
    let position_id = tuff_vanilla_data::packet::play::Clientbound::PlayerPosition.id();
    let teleport = sent
        .iter()
        .position(|frame| framed_packet_id(frame) == position_id)
        .expect("teleported onto the bed");
    let data: Vec<_> = sent
        .iter()
        .enumerate()
        .filter_map(|(i, frame)| entity_data(frame).map(|data| (i, data)))
        .collect();
    assert_eq!(
        data,
        vec![(teleport + 1, (1, bed_entries(Some(head))))],
        "the sleeper gets its own pose and bed right after the teleport"
    );

    let (mut seen, mut spawned, mut occupied) = (None, None, false);
    expect(&mut events, |event| {
        match event {
            PlayerEvent::Track(track) if track.entity == 1 => {
                if seen.is_none() {
                    seen = track.update.iter().find_map(entity_data);
                }
                if spawned.is_none() {
                    spawned = track.spawn.iter().find_map(entity_data);
                }
            }
            PlayerEvent::ChunkPackets { packets, .. } => {
                occupied |= packets
                    .iter()
                    .filter_map(decode_block_update)
                    .any(|(pos, state)| pos == head && state.occupied() == Some(true));
            }
            _ => {}
        }
        (seen.is_some() && occupied).then_some(())
    })
    .await;
    assert_eq!(seen, Some((1, bed_entries(Some(head)))));
    let (_, spawn_entries) = spawned.expect("spawn data");
    assert!(
        spawn_entries.starts_with(&bed_entries(Some(head))),
        "players who see the sleeper later spawn it lying down: {spawn_entries:?}"
    );

    sleeper.handle_event(PlayerEvent::WakeUp).await;
    assert!(!sleeper.is_sleeping());
    world.ticks(2).await;
    let sent = drain(&mut own);
    let woke = sent
        .iter()
        .position(|frame| animation(frame) == Some((1, 0)))
        .expect("the wake-up animation reaches the sleeper");
    let data: Vec<_> = sent
        .iter()
        .enumerate()
        .filter_map(|(i, frame)| entity_data(frame).map(|data| (i, data)))
        .collect();
    assert_eq!(data.len(), 1, "{data:?}");
    assert!(data[0].0 > woke);
    assert_eq!(data[0].1, (1, bed_entries(None)));

    let (mut seen, mut animated, mut freed) = (None, false, false);
    expect(&mut events, |event| {
        match event {
            PlayerEvent::Track(track) if track.entity == 1 => {
                if seen.is_none() {
                    seen = track.update.iter().find_map(entity_data);
                }
            }
            PlayerEvent::EntityPackets { entity: 1, packets } => {
                animated |= packets.iter().any(|p| animation(p) == Some((1, 0)));
            }
            PlayerEvent::ChunkPackets { packets, .. } => {
                freed |= packets
                    .iter()
                    .filter_map(decode_block_update)
                    .any(|(pos, state)| pos == head && state.occupied() == Some(false));
            }
            _ => {}
        }
        (seen.is_some() && animated && freed).then_some(())
    })
    .await;
    assert_eq!(seen, Some((1, bed_entries(None))));
    world.shutdown().await;
}

fn move_to(position: [f64; 3], on_ground: bool) -> Serverbound {
    Serverbound::Move(crate::protocol::play::Movement {
        horizontal_collision: false,
        position: Some(position),
        rotation: None,
        on_ground,
    })
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_wind_jump_lands_back_on_its_launch_block_unhurt() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let land = |seq: u64, dimension| PlayerEvent::Surroundings {
        dimension,
        seq,
        in_water: false,
        submersion: Default::default(),
        in_lava: false,
        in_fire: None,
        climbable: false,
        climb_location: None,
        resets_fall: false,
        landed_on: None,
        portal: None,
        biome: None,
        stepping_on: None,
        contact: Default::default(),
    };
    for (id, wind) in [(5, true), (6, false)] {
        let mut jumper = session(&world, id);
        jumper.stand_at([0.5, 100.0, 0.5], true);
        if wind {
            let blast = crate::world::messages::Blast {
                center: [0.5, 99.6, 0.5],
                radius: 1.2,
                damage_type: &damage_type::WIND_CHARGE,
                igniter: None,
                culprit: None,
                wind: Some(crate::world::explosion::WindBurst {
                    knockback: 1.22,
                    sound: tuff_vanilla_data::registry::SoundEvent::EntityWindChargeWindBurst,
                    spares_fall: true,
                    source: tuff_vanilla_data::registry::EntityType::WindCharge,
                }),
            };
            jumper.exploded(0.0, blast, [0.0, 1.5, 0.0], true).await;
        }
        jumper.stand_at([0.5, 110.0, 0.5], false);
        jumper.handle_packet(move_to([0.5, 100.0, 0.5], true)).await;
        let dimension = jumper.dimension_id();
        jumper.handle_event(land(1000, dimension)).await;
        if wind {
            assert_eq!(
                jumper.health(),
                20.0,
                "the burst's height counts, not the peak"
            );
        } else {
            assert!(
                jumper.health() < 20.0,
                "the same drop hurts without a burst"
            );
        }
    }
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn elytra_flight_starts_only_while_falling_and_wears_the_elytra_every_twenty_ticks() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chest_slot = Inventory::armor_slot(EquipmentSlot::Chest).unwrap();
    let elytra = || Some(ItemStack::mint(Item::Elytra, 1, MintReason::Test));

    let mut grounded = session(&world, 1);
    grounded.inventory_mut().set_slot(chest_slot, elytra());
    grounded.stand_at([0.5, 100.0, 0.5], true);
    grounded.handle_packet(Serverbound::PlayerCommand(6)).await;
    assert!(
        !grounded.is_gliding(),
        "flight cannot start while standing on the ground"
    );

    let mut bare = session(&world, 2);
    bare.stand_at([0.5, 100.0, 0.5], false);
    bare.handle_packet(Serverbound::PlayerCommand(6)).await;
    assert!(!bare.is_gliding(), "flight needs a worn elytra");

    let mut flier = session(&world, 3);
    flier.inventory_mut().set_slot(chest_slot, elytra());
    flier.stand_at([0.5, 100.0, 0.5], false);
    flier.handle_packet(Serverbound::PlayerCommand(6)).await;
    assert!(flier.is_gliding());
    flier.handle_packet(Serverbound::PlayerCommand(6)).await;
    assert!(flier.is_gliding(), "already gliding stays gliding");

    for tick in 1..=40 {
        flier.run_tick().await;
        let damage = flier.inventory().slots()[chest_slot]
            .as_ref()
            .unwrap()
            .damage();
        let expected = if tick < 20 {
            0
        } else if tick < 40 {
            1
        } else {
            2
        };
        assert_eq!(damage, expected, "tick {tick}");
    }
    assert!(flier.is_gliding(), "still flying after 40 ticks");

    flier.handle_packet(move_to([0.5, 60.0, 0.5], true)).await;
    assert!(!flier.is_gliding(), "landing stops the glide");
    assert_eq!(flier.health(), 20.0, "no fall damage while gliding");

    let mut faller = session(&world, 4);
    faller.stand_at([0.5, 100.0, 0.5], false);
    faller.handle_packet(move_to([0.5, 60.0, 0.5], true)).await;
    faller
        .handle_event(PlayerEvent::Surroundings {
            dimension: faller.dimension_id(),
            seq: 1,
            in_water: false,
            submersion: Default::default(),
            in_lava: false,
            in_fire: None,
            climbable: false,
            climb_location: None,
            resets_fall: false,
            landed_on: None,
            portal: None,
            biome: None,
            stepping_on: None,
            contact: Default::default(),
        })
        .await;
    assert!(
        faller.health() < 20.0,
        "the same drop hurts without an elytra"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn firework_rockets_are_consumed_and_boost_a_gliding_player() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chest_slot = Inventory::armor_slot(EquipmentSlot::Chest).unwrap();
    let (mut player, mut outbound) = session_with_outbound(&world, 1);
    player.inventory_mut().set_slot(
        chest_slot,
        Some(ItemStack::mint(Item::Elytra, 1, MintReason::Test)),
    );
    let mut rocket = ItemStack::mint(Item::FireworkRocket, 3, MintReason::Test);
    rocket.components.fireworks = Some(Box::new(tuff_item::item::decor::Fireworks {
        flight_duration: 1,
        explosions: Vec::new(),
    }));
    player
        .inventory_mut()
        .set_slot(Inventory::hotbar_slot(0), Some(rocket));
    player.stand_at([0.5, 100.0, 0.5], false);
    player.handle_packet(Serverbound::PlayerCommand(6)).await;
    assert!(player.is_gliding());

    player
        .handle_packet(Serverbound::UseItem {
            off_hand: false,
            sequence: 1,
        })
        .await;
    let held = player.inventory().hand(false).expect("rockets remain");
    assert_eq!(held.count(), 2, "using one consumes it");

    player.run_tick().await;
    let sent = drain(&mut outbound);
    assert!(
        sent.iter().all(|frame| motion(frame).is_none()),
        "the client drives the boost from the attached rocket"
    );

    let mut grounded = session(&world, 2);
    grounded.inventory_mut().set_slot(
        Inventory::hotbar_slot(0),
        Some(ItemStack::mint(Item::FireworkRocket, 1, MintReason::Test)),
    );
    grounded.stand_at([0.5, 100.0, 0.5], true);
    grounded
        .handle_packet(Serverbound::UseItem {
            off_hand: false,
            sequence: 1,
        })
        .await;
    assert_eq!(
        grounded.inventory().hand(false).unwrap().count(),
        1,
        "not gliding: the rocket is not a boost, just held"
    );
    world.shutdown().await;
}

/// The remaining-tick values at which a full eat of `item` plays its
/// per-tick chewing sound, computed the same way `UsingItem::emits_effects`
/// does, so the test does not hardcode a duration.
fn periodic_chew_count(item: Item, total: u32) -> usize {
    let mut probe = crate::world::player::UsingItem::new(false, item, total);
    let mut count = 0;
    loop {
        if probe.emits_effects() {
            count += 1;
        }
        if probe.tick() {
            return count;
        }
    }
}

/// A viewer gets the joining player's exact skin parts byte in the
/// spawn packet, and the exact new byte again after a later change --
/// using a bitmask other than the client's default so a bug that only
/// shows up for a non-default value would be caught.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn skin_parts_reach_viewers_at_spawn_and_after_a_change() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;

    const INITIAL: u8 = 0x2A;
    let info = ClientInformation {
        skin_parts: INITIAL,
        ..ClientInformation::default()
    };
    let (outbound, outbound_rx) = mpsc::unbounded_channel();
    let (handle, player_events) = PlayerHandle::new(1);
    let (_packets, packets_rx) = mpsc::channel(8);
    let profile = GameProfile {
        id: 1,
        name: "Alice".to_owned(),
        properties: Vec::new(),
    };
    let mut alice = PlayerSession::new(
        world.shared.clone(),
        1,
        profile,
        info,
        outbound,
        handle,
        player_events,
        packets_rx,
        PermissionLevel::Owners,
    );
    std::mem::forget(outbound_rx);

    alice.stand_at([5.5, 100.0, 5.5], true);
    alice.run_tick().await;

    fn skin_entry(entries: &[(u8, i32, Entry)]) -> Option<(u8, i32, Entry)> {
        entries
            .iter()
            .find(|(index, ..)| *index == play::metadata::SKIN_PARTS)
            .copied()
    }

    let spawned = expect(&mut events, |event| match event {
        PlayerEvent::Track(track) if track.entity == 1 => track
            .spawn
            .iter()
            .find_map(entity_data)
            .and_then(|(_, entries)| skin_entry(&entries)),
        _ => None,
    })
    .await;
    assert_eq!(
        spawned,
        (play::metadata::SKIN_PARTS, 0, Entry::Byte(INITIAL as i8)),
        "the joining player's skin parts reach a viewer in the spawn packet"
    );

    const CHANGED: u8 = 0x11;
    alice
        .handle_packet(Serverbound::ClientInformation(ClientInformation {
            skin_parts: CHANGED,
            ..info
        }))
        .await;

    let updated = expect(&mut events, |event| match event {
        PlayerEvent::EntityPackets { entity: 1, packets } => packets
            .iter()
            .find_map(entity_data)
            .and_then(|(_, entries)| skin_entry(&entries)),
        _ => None,
    })
    .await;
    assert_eq!(
        updated,
        (play::metadata::SKIN_PARTS, 0, Entry::Byte(CHANGED as i8)),
        "a later skin parts change reaches the viewer too"
    );
    world.shutdown().await;
}

/// Eating a food item from start to finish must play the per-tick
/// chewing sound only while the bite is in progress and exactly one
/// completion sequence (chew, food chew, burp) once, not a duplicate
/// chew stacked onto the finishing tick.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn eating_completes_with_exactly_one_completion_chew() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;

    let mut player = session(&world, 1);
    player.make_hungry();
    player.inventory_mut().set_slot(
        Inventory::hotbar_slot(0),
        Some(ItemStack::mint(Item::Apple, 1, MintReason::Test)),
    );

    let consumable = Item::Apple.consumable().unwrap();
    let total = consumable.consume_ticks();
    let expected_periodic = periodic_chew_count(Item::Apple, total);

    player
        .handle_packet(Serverbound::UseItem {
            off_hand: false,
            sequence: 1,
        })
        .await;
    for _ in 0..total {
        player.run_tick().await;
    }
    assert!(!player.is_using(), "the bite finished");

    let consumable_sound = consumable.sound.id();
    let burp_sound = tuff_vanilla_data::registry::SoundEvent::EntityPlayerBurp.id();
    let mut players_chews = 0usize;
    let mut neutral_chews = 0usize;
    let mut burps = 0usize;
    let expected_frames = expected_periodic + 3; // + completion chew, food chew, burp
    let mut seen = 0usize;
    while seen < expected_frames {
        let packets = expect(&mut events, |event| match event {
            PlayerEvent::ChunkPackets { packets, .. } => Some(packets),
            _ => None,
        })
        .await;
        for frame in packets.iter() {
            let Some(sound) = decode_sound(frame) else {
                continue;
            };
            seen += 1;
            assert!(sound.volume > 0.0 && sound.pitch > 0.0);
            match (sound.category, sound.sound_event) {
                (play::sound_category::PLAYERS, id) if id == consumable_sound => {
                    players_chews += 1;
                }
                (play::sound_category::NEUTRAL, id) if id == consumable_sound => {
                    neutral_chews += 1;
                }
                (play::sound_category::PLAYERS, id) if id == burp_sound => burps += 1,
                _ => {}
            }
        }
    }
    // Drain whatever else the completion publishes (e.g. a Track update
    // clearing the "using item" entity flag); only an extra decodable
    // Sound frame would indicate a duplicate.
    loop {
        match tokio::time::timeout(Duration::from_millis(200), events.recv()).await {
            Ok(Some(PlayerEvent::ChunkPackets { packets, .. })) => {
                for frame in packets.iter() {
                    if let Some(sound) = decode_sound(frame) {
                        panic!(
                            "unexpected extra sound after the completion sequence: {} category {}",
                            sound.sound_event, sound.category
                        );
                    }
                }
            }
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => break,
        }
    }
    assert_eq!(
        players_chews,
        expected_periodic + 1,
        "the per-tick chew plus exactly one completion chew, not a duplicate on the finishing tick"
    );
    assert_eq!(neutral_chews, 1, "one food completion sound");
    assert_eq!(burps, 1, "one burp");
    world.shutdown().await;
}

/// A suspicious stew carrying `minecraft:suspicious_stew_effects` applies
/// each listed effect when eaten.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn eating_suspicious_stew_applies_its_effects() {
    use tuff_item::item::{ComponentPatch, SuspiciousEffect};
    use tuff_vanilla_data::registry::MobEffect;

    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut player = session(&world, 1);
    player.make_hungry();
    let stew = ItemStack::mint(Item::SuspiciousStew, 1, MintReason::Test).with_components(
        ComponentPatch {
            suspicious_stew_effects: Some(vec![SuspiciousEffect {
                effect: MobEffect::NightVision,
                duration: 100,
            }]),
            ..Default::default()
        },
    );
    player
        .inventory_mut()
        .set_slot(Inventory::hotbar_slot(0), Some(stew));

    let consumable = Item::SuspiciousStew.consumable().unwrap();
    let total = consumable.consume_ticks();
    player
        .handle_packet(Serverbound::UseItem {
            off_hand: false,
            sequence: 1,
        })
        .await;
    for _ in 0..total {
        player.run_tick().await;
    }
    assert!(!player.is_using(), "the bite finished");
    assert_eq!(
        player.effect_duration(MobEffect::NightVision),
        Some(100),
        "the stew's effect was applied on eating"
    );
    world.shutdown().await;
}

/// A player already at full hunger who holds right-click on food (the
/// client resends Use Item every tick while the button stays down)
/// must not start a new eat: no `using` state, no sound, no animation.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn full_hunger_rejects_a_new_bite_while_holding_right_click() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut player = session(&world, 1);
    player.set_food_level(player::MAX_FOOD);
    player.inventory_mut().set_slot(
        Inventory::hotbar_slot(0),
        Some(ItemStack::mint(Item::Apple, 2, MintReason::Test)),
    );

    for sequence in 1..=5 {
        player
            .handle_packet(Serverbound::UseItem {
                off_hand: false,
                sequence,
            })
            .await;
        assert!(
            !player.is_using(),
            "a full hunger bar must reject every repeated bite attempt"
        );
    }
    assert_eq!(
        player.inventory().hand(false).unwrap().count(),
        2,
        "no food was actually consumed"
    );
    world.shutdown().await;
}

/// A bite already in progress when hunger reaches full must still
/// complete normally; only the *next* bite (a fresh Use Item packet)
/// is the one that gets rejected.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_bite_in_progress_finishes_even_as_hunger_fills_then_blocks_the_next_one() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut player = session(&world, 1);
    let apple = Item::Apple.food().unwrap();
    // One bite short of full, so this exact bite fills the bar.
    player.set_food_level(player::MAX_FOOD - apple.nutrition as i32);
    player.inventory_mut().set_slot(
        Inventory::hotbar_slot(0),
        Some(ItemStack::mint(Item::Apple, 2, MintReason::Test)),
    );

    player
        .handle_packet(Serverbound::UseItem {
            off_hand: false,
            sequence: 1,
        })
        .await;
    assert!(
        player.is_using(),
        "the bite in progress must start normally"
    );
    let total = Item::Apple.consumable().unwrap().consume_ticks();
    for _ in 0..total {
        player.run_tick().await;
    }
    assert!(!player.is_using(), "the in-progress bite completes");
    assert_eq!(player.food_level(), player::MAX_FOOD, "hunger is now full");
    assert_eq!(
        player.inventory().hand(false).unwrap().count(),
        1,
        "the in-progress bite consumed exactly one apple"
    );

    player
        .handle_packet(Serverbound::UseItem {
            off_hand: false,
            sequence: 2,
        })
        .await;
    assert!(
        !player.is_using(),
        "the next bite is rejected now that hunger is full"
    );
    assert_eq!(
        player.inventory().hand(false).unwrap().count(),
        1,
        "no second apple was consumed"
    );
    world.shutdown().await;
}

/// The home region reports what a player lands on, and the session scales
/// its fall damage by it: hay takes four fifths of it, while a cobweb the
/// body is caught in cancels the fall entirely.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn landing_on_hay_softens_the_fall_and_a_cobweb_cancels_it() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let pos = ChunkPos::new(0, 0);
    let region = world.region(pos.region()).await;
    let (probe, mut events) = PlayerHandle::new(100);
    subscribe(&region, &probe, &mut events, &[pos]).await;
    place_blocks(
        &world,
        &region,
        [
            (BlockPos::new(2, 149, 2), Block::HayBlock.default_state()),
            (BlockPos::new(6, 149, 2), Block::Stone.default_state()),
            (BlockPos::new(10, 149, 2), Block::Stone.default_state()),
            (BlockPos::new(10, 150, 2), Block::Cobweb.default_state()),
        ],
    )
    .await;
    let mut report_at = async |seq: u64, position: [f64; 3]| {
        region.request(update(&probe, seq, position)).await;
        expect(&mut events, |event| match event {
            PlayerEvent::Surroundings { seq: at, .. } if at == seq => Some(event),
            _ => None,
        })
        .await
    };
    let hay = report_at(1, [2.5, 150.0, 2.5]).await;
    let stone = report_at(2, [6.5, 150.0, 2.5]).await;
    let cobweb = report_at(3, [10.5, 150.0, 2.5]).await;

    let health_after_landing = async |id: i32, event: PlayerEvent| {
        let mut faller = session(&world, id);
        faller.stand_at([0.5, 163.0, 0.5], false);
        faller.handle_packet(move_to([0.5, 150.0, 0.5], true)).await;
        let PlayerEvent::Surroundings {
            dimension,
            in_water,
            submersion,
            in_lava,
            in_fire,
            climbable,
            climb_location,
            resets_fall,
            landed_on,
            portal,
            biome,
            stepping_on,
            contact,
            ..
        } = event
        else {
            unreachable!()
        };
        faller
            .handle_event(PlayerEvent::Surroundings {
                dimension,
                seq: 1,
                in_water,
                submersion,
                in_lava,
                in_fire,
                climbable,
                climb_location,
                resets_fall,
                landed_on,
                portal,
                biome,
                stepping_on,
                contact,
            })
            .await;
        faller.health()
    };
    assert_eq!(health_after_landing(1, stone).await, 10.0);
    assert_eq!(health_after_landing(2, hay).await, 18.0);
    assert_eq!(health_after_landing(3, cobweb).await, 20.0);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shields_resistance_and_absorption_count_towards_damage_statistics() {
    use crate::world::effects::ActiveEffect;
    use crate::world::stats::Stat;
    use tuff_feature_builtin_vanilla_combat::projectile::potion::PotionHit;
    use tuff_vanilla_data::registry::{CustomStat, MobEffect};

    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let custom = |player: &PlayerSession, stat| player.stat(Stat::custom(stat));

    let mut blocker = session(&world, 1);
    blocker.inventory_mut().set_slot(
        Inventory::OFFHAND_SLOT,
        Some(ItemStack::mint(Item::Shield, 1, MintReason::Test)),
    );
    blocker.hold_up(true, 10);
    blocker.handle_event(hit(5.0, FRONT, None)).await;
    assert_eq!(custom(&blocker, CustomStat::DamageBlockedByShield), 50);
    assert_eq!(custom(&blocker, CustomStat::DamageTaken), 0);

    let mut tough = session(&world, 2);
    tough
        .handle_event(PlayerEvent::Potion(Box::new(PotionHit {
            effects: vec![
                ActiveEffect::timed(MobEffect::Resistance, 600, 0),
                ActiveEffect::timed(MobEffect::Absorption, 600, 0),
            ],
            instant: Vec::new(),
            extinguish: false,
        })))
        .await;
    tough
        .handle_event(PlayerEvent::Hurt {
            amount: 10.0,
            kind: &damage_type::GENERIC,
            attacker: None,
            position: None,
        })
        .await;
    assert_eq!(custom(&tough, CustomStat::DamageResisted), 20);
    assert_eq!(custom(&tough, CustomStat::DamageAbsorbed), 40);
    assert_eq!(custom(&tough, CustomStat::DamageTaken), 40);
    assert_eq!(tough.health(), 16.0);

    let (attacker, _) = PlayerHandle::new(999);
    let mut struck = session(&world, 3);
    struck
        .handle_event(PlayerEvent::Potion(Box::new(PotionHit {
            effects: vec![ActiveEffect::timed(MobEffect::Resistance, 600, 1)],
            instant: Vec::new(),
            extinguish: false,
        })))
        .await;
    struck.handle_event(direct(&attacker, 5.0, None)).await;
    assert_eq!(custom(&struck, CustomStat::DamageResisted), 20);
    assert_eq!(custom(&struck, CustomStat::DamageTaken), 30);
    world.shutdown().await;
}

/// A neighbouring region's blast pushes through an Explosion packet of the
/// player's own, which adds to its velocity, never a Set Entity Motion,
/// which would replace it.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_neighbouring_regions_blast_adds_its_push_instead_of_replacing_the_velocity() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, mut outbound) = session_with_outbound(&world, 7);
    drain(&mut outbound);
    let push = [0.25, 0.5, -0.125];
    player
        .handle_event(PlayerEvent::Exploded {
            damage: 1.0,
            blast: Blast {
                center: [3.0, 70.0, 3.0],
                radius: 4.0,
                damage_type: &damage_type::EXPLOSION,
                igniter: None,
                culprit: None,
                wind: None,
            },
            push,
            push_sent: false,
        })
        .await;
    let explode = tuff_vanilla_data::packet::play::Clientbound::Explode.id();
    let motion = tuff_vanilla_data::packet::play::Clientbound::SetEntityMotion.id();
    let mut pushes = Vec::new();
    for frame in drain(&mut outbound) {
        let mut r = framed_payload(&frame);
        let id = r.read_var_int().unwrap();
        assert_ne!(id, motion, "no velocity replacement");
        if id == explode {
            let center = [0; 3].map(|_| r.read_double().unwrap());
            assert_eq!(center, [3.0, 70.0, 3.0]);
            r.read_float().unwrap();
            assert_eq!(r.read_int().unwrap(), 0, "no blocks to break again");
            assert!(r.read_bool().unwrap());
            pushes.push([0; 3].map(|_| r.read_double().unwrap()));
        }
    }
    assert_eq!(pushes, vec![push]);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_eater_hears_no_server_chews_and_gets_the_use_complete_event() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, mut own) = session_with_outbound(&world, 1);
    player.make_hungry();
    player.inventory_mut().set_slot(
        Inventory::hotbar_slot(0),
        Some(ItemStack::mint(Item::Apple, 1, MintReason::Test)),
    );
    let total = Item::Apple.consumable().unwrap().consume_ticks();
    let consumable_sound = Item::Apple.consumable().unwrap().sound.id();
    drain(&mut own);
    player
        .handle_packet(Serverbound::UseItem {
            off_hand: false,
            sequence: 1,
        })
        .await;
    for _ in 0..total {
        player.run_tick().await;
    }
    assert!(!player.is_using());
    let frames = drain(&mut own);
    let chews = frames
        .iter()
        .filter_map(decode_sound)
        .filter(|s| {
            s.category == play::sound_category::PLAYERS && s.sound_event == consumable_sound
        })
        .count();
    assert_eq!(chews, 0, "the client plays its own chews");
    let event = tuff_vanilla_data::packet::play::Clientbound::EntityEvent.id();
    let completions = frames
        .iter()
        .filter(|frame| {
            let mut r = framed_payload(frame);
            r.read_var_int().unwrap() == event
                && r.read_int().unwrap() == 1
                && r.read_byte().unwrap() == play::entity_event::USE_ITEM_COMPLETE
        })
        .count();
    assert_eq!(completions, 1);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_player_gets_its_own_skin_layers_and_main_arm_on_entering() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let info = ClientInformation {
        skin_parts: 0x2A,
        main_hand: 0,
        ..ClientInformation::default()
    };
    let (mut player, mut own) = session_with_info(&world, 1, info);
    player.send_world().await;
    let entries: Vec<_> = drain(&mut own)
        .iter()
        .filter_map(entity_data)
        .filter(|(entity, _)| *entity == 1)
        .flat_map(|(_, entries)| entries)
        .collect();
    assert!(entries.contains(&(play::metadata::SKIN_PARTS, 0, Entry::Byte(0x2A))));
    assert!(
        entries
            .iter()
            .any(|(index, _, value)| *index == play::metadata::MAIN_ARM && *value == Entry::Arm(0))
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ominous_bottles_give_bad_omen_at_their_amplifier_without_particles() {
    use tuff_vanilla_data::registry::MobEffect;

    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut player = session(&world, 1000);
    let mut bottle = ItemStack::mint(Item::OminousBottle, 1, MintReason::Test);
    bottle.set_ominous_bottle_amplifier(3);
    player
        .inventory_mut()
        .set_slot(Inventory::hotbar_slot(0), Some(bottle));
    player
        .handle_packet(Serverbound::UseItem {
            off_hand: false,
            sequence: 1,
        })
        .await;
    assert!(player.is_using());
    for _ in 0..40 {
        player.run_tick().await;
    }
    assert!(!player.is_using());
    assert!(
        player
            .effect_duration(MobEffect::BadOmen)
            .is_some_and(|ticks| (119_900..=120_000).contains(&ticks))
    );
    assert_eq!(player.effect_amplifier(MobEffect::BadOmen), Some(3));
    assert_eq!(player.inventory().hand(false), None);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn attribute_updates_keep_the_sprint_modifier_while_sprinting() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, mut own) = session_with_outbound(&world, 1);
    player.run_tick().await;
    drain(&mut own);
    player.handle_packet(Serverbound::PlayerCommand(1)).await;
    player.run_tick().await;
    assert!(sprint_modifiers(&drain(&mut own)).contains(&(
        "minecraft:sprinting".to_owned(),
        0.30000001192092896,
        2
    )));
    player.handle_packet(Serverbound::PlayerCommand(2)).await;
    player.run_tick().await;
    let frames = drain(&mut own);
    assert!(sprint_modifiers(&frames).is_empty());
    assert!(synced_attribute_values(&frames).contains_key(&Attribute::MovementSpeed));
    world.shutdown().await;
}

fn sprint_modifiers(frames: &[bytes::Bytes]) -> Vec<(String, f64, u8)> {
    let id = tuff_vanilla_data::packet::play::Clientbound::UpdateAttributes.id();
    let mut found = Vec::new();
    for frame in frames {
        let mut r = framed_payload(frame);
        if r.read_var_int().unwrap() != id {
            continue;
        }
        r.read_var_int().unwrap();
        let count = r.read_var_int().unwrap();
        for _ in 0..count {
            let attribute = Attribute::from_id(r.read_var_int().unwrap() as u32).unwrap();
            r.read_double().unwrap();
            for _ in 0..r.read_var_int().unwrap() {
                let name = r.read_string().unwrap();
                let amount = r.read_double().unwrap();
                let op = r.read_unsigned_byte().unwrap();
                if attribute == Attribute::MovementSpeed {
                    found.push((name.to_string(), amount, op));
                }
            }
        }
    }
    found
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bad_omen_converts_into_trial_omen_by_level() {
    use crate::world::effects::ActiveEffect;
    use tuff_vanilla_data::registry::MobEffect;

    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut player = session(&world, 1000);
    player
        .handle_event(PlayerEvent::Potion(Box::new(
            tuff_feature_builtin_vanilla_combat::projectile::potion::PotionHit {
                effects: vec![ActiveEffect::timed(MobEffect::BadOmen, 5000, 2)],
                instant: Vec::new(),
                extinguish: false,
            },
        )))
        .await;
    assert_eq!(player.effect_amplifier(MobEffect::BadOmen), Some(2));
    player
        .handle_event(PlayerEvent::ConvertEffect {
            from: MobEffect::BadOmen,
            to: MobEffect::TrialOmen,
            ticks_per_level: 18_000,
        })
        .await;
    assert_eq!(player.effect_duration(MobEffect::BadOmen), None);
    assert_eq!(player.effect_duration(MobEffect::TrialOmen), Some(54_000));
    assert_eq!(player.effect_amplifier(MobEffect::TrialOmen), Some(0));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_turtle_helmet_gives_water_breathing_out_of_the_water_only() {
    use tuff_vanilla_data::registry::MobEffect;
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, _outbound) = session_with_outbound(&world, 1);
    player.handle_event(submerged(&player, false, false)).await;
    player.inventory_mut().set_slot(
        Inventory::armor_slot(EquipmentSlot::Head).unwrap(),
        Some(ItemStack::mint(Item::TurtleHelmet, 1, MintReason::Test)),
    );
    player.run_tick().await;
    assert_eq!(player.effect_duration(MobEffect::WaterBreathing), Some(199));
    player.handle_event(submerged(&player, true, false)).await;
    for _ in 0..189 {
        player.run_tick().await;
    }
    assert!(player.effect_duration(MobEffect::WaterBreathing).unwrap() <= 10);
    for _ in 0..20 {
        player.run_tick().await;
    }
    assert_eq!(
        player.effect_duration(MobEffect::WaterBreathing),
        None,
        "under water the helmet no longer renews it"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_player_in_a_one_block_gap_crawls_with_low_eyes() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, _outbound) = session_with_outbound(&world, 1);
    let cramped_in = |player: &PlayerSession, cramped| {
        let mut event = submerged(player, false, false);
        if let PlayerEvent::Surroundings { contact, .. } = &mut event {
            contact.cramped = cramped;
        }
        event
    };
    let standing_eye = player.eye()[1] - player.position()[1];
    let event = cramped_in(&player, 2);
    player.handle_event(event).await;
    assert_eq!(player.current_pose(), Pose::Swimming);
    assert!(player.eye()[1] - player.position()[1] < standing_eye - 1.0);
    let event = cramped_in(&player, 1);
    player.handle_event(event).await;
    assert_eq!(player.current_pose(), Pose::Crouching);
    let event = cramped_in(&player, 0);
    player.handle_event(event).await;
    assert_eq!(player.current_pose(), Pose::Standing);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_door_closed_on_a_standing_player_keeps_them_standing() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, _outbound) = session_with_outbound(&world, 1);
    let door = Block::OakDoor.default_state();
    let at = |pos: BlockPos| {
        Some(if pos.x == 0 && pos.z == 0 && (64..=65).contains(&pos.y) {
            door
        } else {
            Block::Air.default_state()
        })
    };
    let mut event = submerged(&player, false, false);
    if let PlayerEvent::Surroundings { contact, .. } = &mut event {
        contact.cramped = tuff_core::world::block_contact::cramped_pose([0.5, 64.0, 0.9], 0.6, &at);
    }
    player.handle_event(event).await;
    assert_eq!(player.current_pose(), Pose::Standing);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_totem_in_hand_cheats_a_fatal_hit() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut player = session(&world, 1);
    player.inventory_mut().set_slot(
        Inventory::OFFHAND_SLOT,
        Some(ItemStack::mint(Item::TotemOfUndying, 2, MintReason::Test)),
    );
    player.handle_event(hit(50.0, FRONT, None)).await;
    assert_eq!(player.health(), 1.0);
    assert_eq!(player.inventory().hand(true).unwrap().count(), 1);
    for _ in 0..25 {
        player.run_tick().await;
    }
    player.handle_event(hit(50.0, FRONT, None)).await;
    assert_eq!(player.health(), 1.0);
    assert!(player.inventory().hand(true).is_none());
    for _ in 0..25 {
        player.run_tick().await;
    }
    player.handle_event(hit(50.0, FRONT, None)).await;
    assert!(player.health() <= 0.0);
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_charging_spear_hits_what_it_runs_into_once_per_contact_cooldown() {
    use crate::world::player::UsingItem;
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut attacker = session(&world, 1);
    attacker.inventory_mut().set_slot(
        Inventory::hotbar_slot(0),
        Some(ItemStack::mint(Item::IronSpear, 1, MintReason::Test)),
    );
    attacker.stand_at([0.5, 64.0, 0.5], true);
    let (victim, mut victim_events) = PlayerHandle::new(2);
    attacker.see_player(&victim, [0.5, 65.6, 2.5]);
    attacker.sync.known_movement = [0.0, 0.0, 0.5];

    let mut using = UsingItem::new(false, Item::IronSpear, UsingItem::HELD);
    using.remaining -= 5;
    attacker.actions.using = Some(using);
    attacker.run_tick().await;
    assert!(
        attacked(&mut victim_events).is_none(),
        "nothing is hit before the spear's delay"
    );

    let mut using = UsingItem::new(false, Item::IronSpear, UsingItem::HELD);
    using.remaining -= 14;
    attacker.actions.using = Some(using);
    attacker.run_tick().await;
    let hit = attacked(&mut victim_events).expect("the charging spear hits");
    assert!(hit.damage >= 1.0 + (10.0_f32 * 0.95).floor());
    assert!(hit.knockback >= 0.0);

    attacker.run_tick().await;
    assert!(
        attacked(&mut victim_events).is_none(),
        "the contact cooldown keeps it from hitting again at once"
    );
    world.shutdown().await;
}

async fn spear_hit_damage(
    target_velocity_z: f64,
    wall: bool,
    mutate: impl FnOnce(&mut PlayerSession),
) -> Option<f32> {
    use crate::world::player::UsingItem;
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut attacker = session(&world, 1);
    attacker.inventory_mut().set_slot(
        Inventory::hotbar_slot(0),
        Some(ItemStack::mint(Item::IronSpear, 1, MintReason::Test)),
    );
    attacker.stand_at([0.5, 64.0, 0.5], true);
    if wall {
        let chunk = tuff_world_model::pos::ChunkPos::new(0, 0);
        attacker.client_world_mut().insert(
            chunk,
            std::sync::Arc::new(tuff_world_model::chunk::Chunk::empty(
                tuff_world_model::chunk::ChunkShape::OVERWORLD,
            )),
        );
        let stone = Block::Stone.default_state();
        let changes: Vec<_> = (64..67).map(|y| (BlockPos::new(0, y, 1), stone)).collect();
        attacker.client_world_mut().apply(chunk, &changes);
    }
    let (victim, mut victim_events) = PlayerHandle::new(2);
    attacker.see_player(&victim, [0.5, 65.6, 2.5 - target_velocity_z]);
    attacker.skip_ticks(1);
    attacker.see_player(&victim, [0.5, 65.6, 2.5]);
    attacker.sync.known_movement = [0.0, 0.0, 0.5];
    mutate(&mut attacker);
    let mut using = UsingItem::new(false, Item::IronSpear, UsingItem::HELD);
    using.remaining -= 14;
    attacker.actions.using = Some(using);
    attacker.run_tick().await;
    let damage = attacked(&mut victim_events).map(|hit| hit.damage);
    world.shutdown().await;
    damage
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_spear_charge_counts_the_targets_own_speed_against_the_damage() {
    let still = spear_hit_damage(0.0, false, |_| {}).await.expect("hits");
    let fleeing = spear_hit_damage(0.4, false, |_| {}).await;
    assert!(
        fleeing.is_none_or(|damage| damage < still),
        "a target running away takes less: {fleeing:?} vs {still}"
    );
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_block_between_the_spear_and_its_target_stops_the_charge() {
    assert!(spear_hit_damage(0.0, false, |_| {}).await.is_some());
    assert!(spear_hit_damage(0.0, true, |_| {}).await.is_none());
}

async fn lunge_stab(food: i32, creative: bool) -> (f32, Option<u32>) {
    use crate::protocol::play::PlayerActionStatus;
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut player = session(&world, 1);
    player.inventory_mut().set_slot(
        Inventory::hotbar_slot(0),
        Some(enchanted(Item::IronSpear, &[("lunge", 2)])),
    );
    player.stand_at([0.5, 64.0, 0.5], true);
    player.set_food_level(food);
    player.mode.abilities.instabuild = creative;
    player.mode.abilities.invulnerable = creative;
    player.tick = 1000;
    player
        .handle_packet(Serverbound::PlayerAction {
            status: PlayerActionStatus::Stab,
            position: BlockPos::new(0, 0, 0),
            face: 0,
            sequence: 1,
        })
        .await;
    let result = (
        player.body.food.exhaustion,
        player
            .inventory()
            .hand(false)
            .and_then(|stack| stack.components.damage),
    );
    world.shutdown().await;
    result
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lunge_stab_costs_exhaustion_and_wear_unless_too_hungry() {
    let (exhaustion, damage) = lunge_stab(20, false).await;
    assert!(exhaustion >= 8.0, "{exhaustion}");
    assert_eq!(damage, Some(1));
    let (exhaustion, damage) = lunge_stab(6, false).await;
    assert_eq!(exhaustion, 0.0);
    assert_eq!(damage, None);
    let (exhaustion, _) = lunge_stab(3, true).await;
    assert_eq!(exhaustion, 0.0, "creative players are not exhausted");
}

fn landing_on(seq: u64, dimension: DimensionId, landed_on: Block) -> PlayerEvent {
    PlayerEvent::Surroundings {
        dimension,
        seq,
        in_water: false,
        submersion: Default::default(),
        in_lava: false,
        in_fire: None,
        climbable: false,
        climb_location: None,
        resets_fall: false,
        landed_on: Some(landed_on.default_state()),
        portal: None,
        biome: None,
        stepping_on: None,
        contact: Default::default(),
    }
}

fn contains(frames: &[bytes::Bytes], needle: &[u8]) -> bool {
    frames.iter().any(|frame| {
        let mut payload = framed_payload(frame);
        let rest = payload.read_bytes(payload.remaining()).unwrap();
        rest.windows(needle.len()).any(|window| window == needle)
    })
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dying_of_a_long_fall_says_it_was_an_accident() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut faller, mut outbound) = session_with_outbound(&world, 7);
    faller.stand_at([0.5, 200.0, 0.5], false);
    faller.handle_packet(move_to([0.5, 150.0, 0.5], true)).await;
    let dimension = faller.dimension_id();
    faller
        .handle_event(landing_on(1000, dimension, Block::Stone))
        .await;
    assert!(faller.is_dead());
    let frames = drain(&mut outbound);
    assert!(contains(&frames, b"death.fell.accident.generic"));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn landing_on_powder_snow_from_a_height_plays_the_fall_sound() {
    use tuff_vanilla_data::registry::SoundEvent;
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let pos = ChunkPos::new(0, 0);
    let region = world.region(pos.region()).await;
    let (probe, mut events) = PlayerHandle::new(100);
    subscribe(&region, &probe, &mut events, &[pos]).await;
    for (id, from, sound) in [
        (8, 156.0, SoundEvent::EntityGenericSmallFall),
        (9, 160.0, SoundEvent::EntityGenericBigFall),
    ] {
        let mut faller = session(&world, id);
        faller.stand_at([0.5, from, 0.5], false);
        faller.handle_packet(move_to([0.5, 150.0, 0.5], true)).await;
        let dimension = faller.dimension_id();
        faller
            .handle_event(landing_on(1000, dimension, Block::PowderSnow))
            .await;
        expect(&mut events, |event| match event {
            PlayerEvent::ChunkPackets { packets, .. } => packets
                .iter()
                .filter_map(decode_sound)
                .any(|s| s.sound_event == sound.id())
                .then_some(()),
            _ => None,
        })
        .await;
    }
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn frost_walker_boots_freeze_the_water_a_player_steps_onto() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (watcher, mut watcher_events) = PlayerHandle::new(2000);
    subscribe(&region, &watcher, &mut watcher_events, &[chunk]).await;
    let mut water = Vec::new();
    for x in 0..9 {
        for z in 0..9 {
            water.push((BlockPos::new(x, SKY, z), Block::Water.default_state()));
        }
    }
    place_blocks(&world, &region, water).await;
    let mut player = session(&world, 1000);
    player.inventory_mut().set_slot(
        Inventory::armor_slot(EquipmentSlot::Feet).unwrap(),
        Some(enchanted(Item::LeatherBoots, &[("frost_walker", 1)])),
    );
    player.stand_at([4.5, f64::from(SKY) + 1.0, 4.5], false);
    player.run_tick().await;
    world.ticks(2).await;
    assert_eq!(
        block_at(&region, BlockPos::new(4, SKY, 4)).await,
        Some(Block::Water.default_state()),
        "nothing freezes while airborne"
    );
    player.stand_at([4.5, f64::from(SKY) + 1.0, 5.5], true);
    player.run_tick().await;
    world.ticks(2).await;
    let frozen = block_at(&region, BlockPos::new(4, SKY, 5)).await;
    assert_eq!(frozen.map(|s| s.block()), Some(Block::FrostedIce));
    assert_eq!(
        block_at(&region, BlockPos::new(4, SKY, 8))
            .await
            .map(|s| s.block()),
        Some(Block::Water),
        "outside the radius"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn soul_speed_boots_lift_movement_speed_on_soul_sand_only() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (watcher, mut watcher_events) = PlayerHandle::new(2000);
    subscribe(&region, &watcher, &mut watcher_events, &[chunk]).await;
    let mut floor = Vec::new();
    for x in 0..9 {
        for z in 0..9 {
            let block = if z < 4 { Block::SoulSand } else { Block::Stone };
            floor.push((BlockPos::new(x, SKY, z), block.default_state()));
        }
    }
    place_blocks(&world, &region, floor.clone()).await;
    let mut player = session(&world, 1000);
    player.client_world_mut().insert(
        chunk,
        std::sync::Arc::new(tuff_world_model::chunk::Chunk::empty(
            tuff_world_model::chunk::ChunkShape::OVERWORLD,
        )),
    );
    player.client_world_mut().apply(chunk, &floor);
    player.inventory_mut().set_slot(
        Inventory::armor_slot(EquipmentSlot::Feet).unwrap(),
        Some(enchanted(Item::LeatherBoots, &[("soul_speed", 1)])),
    );
    let speed = |player: &PlayerSession| {
        player
            .surroundings
            .enchantment_modifiers
            .iter()
            .filter(|(a, ..)| *a == Attribute::MovementSpeed)
            .map(|(_, _, amount, _)| *amount)
            .sum::<f64>()
    };
    player.stand_at([4.5, f64::from(SKY) + 1.0, 6.5], true);
    player.run_tick().await;
    assert_eq!(speed(&player), 0.0);
    player.stand_at([4.5, f64::from(SKY) + 1.0, 2.5], true);
    player.run_tick().await;
    assert!(speed(&player) > 0.04, "{}", speed(&player));
    player.stand_at([4.5, f64::from(SKY) + 1.0, 6.5], true);
    player.run_tick().await;
    assert_eq!(speed(&player), 0.0);
    world.shutdown().await;
}
