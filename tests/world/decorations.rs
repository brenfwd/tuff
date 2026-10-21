//! Item frames and armor stands through a real region: placing them from
//! items, filling and turning them, breaking them, and saving them.

use super::*;
use crate::world::entity::{EntityAttack, EntityInteraction};
use crate::world::messages::{ActionOutcome, HandEffect};
use tuff_vanilla_data::registry::EntityType;

fn punch(attacker: i32) -> EntityAttack {
    EntityAttack {
        attacker,
        attacker_uuid: 77,
        attacker_position: [3.5, SKY as f64 + 1.0, 3.5],
        attacker_yaw: 0.0,
        attacker_pitch: 0.0,
        damage: 1.0,
        knockback: 0.0,
        weapon: None,
        critical: false,
        magic: false,
        sweep: false,
        part: None,
    }
}

async fn saved(region: &RegionHandle, id: i32) -> Compound {
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::EntityData { id, reply })
        .await;
    answer.await.unwrap().expect("the entity exists")
}

async fn dropped(region: &RegionHandle) -> Vec<String> {
    snapshots(region)
        .await
        .into_iter()
        .filter(|s| s.kind == EntityType::Item)
        .map(|s| s.name)
        .collect()
}

async fn interact_with(
    region: &RegionHandle,
    player: &PlayerHandle,
    events: &mut mpsc::UnboundedReceiver<PlayerEvent>,
    target: i32,
    interaction: EntityInteraction,
) -> (Option<Item>, HandEffect) {
    region
        .request(RegionRequest::InteractEntity {
            player: player.clone(),
            target,
            interaction,
        })
        .await;
    expect(events, |event| match event {
        PlayerEvent::EntityInteracted { item, effect, .. } => Some((item, effect)),
        _ => None,
    })
    .await
}

struct Scene {
    world: TestWorld,
    region: RegionHandle,
    player: PlayerHandle,
    events: mpsc::UnboundedReceiver<PlayerEvent>,
    _dir: TestDir,
}

async fn scene(player_id: i32) -> Scene {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(player_id);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    Scene {
        world,
        region,
        player,
        events,
        _dir: dir,
    }
}

async fn frame_on_a_wall(s: &mut Scene, item: Item) -> i32 {
    let wall = BlockPos::new(5, SKY + 1, 8);
    place_blocks(
        &s.world,
        &s.region,
        vec![(wall, Block::Stone.default_state())],
    )
    .await;
    let mut click = place(&s.player, wall, item, 1);
    click.face = tuff_world_model::Face::East;
    s.region.request(RegionRequest::Place(click)).await;
    let outcome = expect(&mut s.events, |event| match event {
        PlayerEvent::ActionResult {
            sequence: 1,
            outcome,
        } => Some(outcome),
        _ => None,
    })
    .await;
    assert!(
        matches!(outcome, ActionOutcome::Placed { .. }),
        "{outcome:?}"
    );
    let kind = if item == Item::GlowItemFrame {
        EntityType::GlowItemFrame
    } else {
        EntityType::ItemFrame
    };
    let found = mobs(&s.region, kind).await;
    assert_eq!(found.len(), 1);
    found[0].id
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_item_frame_goes_on_a_wall_and_holds_and_turns_an_item() {
    let mut s = scene(1000).await;
    let id = frame_on_a_wall(&mut s, Item::ItemFrame).await;
    let nbt = saved(&s.region, id).await;
    assert_eq!(nbt.get("Facing"), Some(&Tag::Byte(5)));
    assert_eq!(
        nbt.get("block_pos"),
        Some(&Tag::IntArray(vec![6, SKY + 1, 8]))
    );
    let frame = mobs(&s.region, EntityType::ItemFrame).await.remove(0);
    assert!((frame.position[0] - 6.03125).abs() < 1.0e-9, "{frame:?}");
    assert_eq!(frame.bounds[4] - frame.bounds[1], 0.75);
    assert_eq!(frame.bounds[3] - frame.bounds[0], 0.0625);

    let (item, effect) = interact_with(
        &s.region,
        &s.player,
        &mut s.events,
        id,
        interaction(&s.player, Some(Item::Diamond)),
    )
    .await;
    assert_eq!(item, Some(Item::Diamond));
    assert!(matches!(effect, HandEffect::Consume(1)));
    let nbt = saved(&s.region, id).await;
    let held = nbt.get("Item").and_then(Tag::as_compound).expect("an item");
    assert_eq!(held.get_str("id"), Some("minecraft:diamond"));
    assert_eq!(nbt.get("ItemRotation"), Some(&Tag::Byte(0)));

    let (item, effect) = interact_with(
        &s.region,
        &s.player,
        &mut s.events,
        id,
        interaction(&s.player, None),
    )
    .await;
    assert_eq!(item, None, "an empty hand still turns it");
    assert!(matches!(effect, HandEffect::None));
    assert_eq!(
        saved(&s.region, id).await.get("ItemRotation"),
        Some(&Tag::Byte(1))
    );
    s.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hitting_a_frame_pops_its_item_then_breaks_the_frame() {
    let mut s = scene(1001).await;
    let id = frame_on_a_wall(&mut s, Item::GlowItemFrame).await;
    interact_with(
        &s.region,
        &s.player,
        &mut s.events,
        id,
        interaction(&s.player, Some(Item::Diamond)),
    )
    .await;

    s.region
        .request(RegionRequest::AttackEntity {
            attacker: s.player.clone(),
            target: id,
            attack: punch(s.player.id),
            sweep: None,
        })
        .await;
    s.world.ticks(3).await;
    assert_eq!(mobs(&s.region, EntityType::GlowItemFrame).await.len(), 1);
    assert_eq!(dropped(&s.region).await, vec!["minecraft:diamond"]);
    assert!(saved(&s.region, id).await.get("Item").is_none());

    s.region
        .request(RegionRequest::AttackEntity {
            attacker: s.player.clone(),
            target: id,
            attack: punch(s.player.id),
            sweep: None,
        })
        .await;
    wait_for(&s.world, &s.region, 20, |found| {
        !found.iter().any(|e| e.kind == EntityType::GlowItemFrame)
    })
    .await;
    let drops = dropped(&s.region).await;
    assert!(
        drops.contains(&"minecraft:glow_item_frame".to_owned()),
        "{drops:?}"
    );
    s.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_frame_falls_off_when_its_wall_goes() {
    let mut s = scene(1002).await;
    let id = frame_on_a_wall(&mut s, Item::ItemFrame).await;
    let _ = id;
    place_blocks(
        &s.world,
        &s.region,
        vec![(BlockPos::new(5, SKY + 1, 8), Block::Air.default_state())],
    )
    .await;
    s.world.ticks(110).await;
    assert!(mobs(&s.region, EntityType::ItemFrame).await.is_empty());
    assert!(
        dropped(&s.region)
            .await
            .contains(&"minecraft:item_frame".to_owned())
    );
    s.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn summoned_frames_keep_their_saved_state() {
    let s = scene(1003).await;
    let item = ItemStack::mint(Item::Map, 1, MintReason::Test).to_nbt();
    let nbt = Compound::new()
        .with("Facing", 2i8)
        .with("Item", item)
        .with("ItemRotation", 3i8)
        .with("Fixed", true)
        .with("Invisible", true);
    summon(
        &s.region,
        EntityType::ItemFrame,
        nbt,
        [8.5, SKY as f64 + 1.5, 8.5],
    )
    .await;
    let frame = mobs(&s.region, EntityType::ItemFrame).await.remove(0);
    let nbt = saved(&s.region, frame.id).await;
    assert_eq!(nbt.get("Facing"), Some(&Tag::Byte(2)));
    assert_eq!(nbt.get("ItemRotation"), Some(&Tag::Byte(3)));
    assert_eq!(nbt.get("Fixed"), Some(&Tag::Byte(1)));
    assert_eq!(nbt.get("Invisible"), Some(&Tag::Byte(1)));
    assert!(nbt.get("Item").is_some());
    s.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_armor_stand_is_placed_dressed_and_broken() {
    let mut s = scene(1004).await;
    let floor = BlockPos::new(8, SKY, 8);
    let mut click = place(&s.player, floor, Item::ArmorStand, 1);
    click.yaw = 100.0;
    s.region.request(RegionRequest::Place(click)).await;
    let outcome = expect(&mut s.events, |event| match event {
        PlayerEvent::ActionResult {
            sequence: 1,
            outcome,
        } => Some(outcome),
        _ => None,
    })
    .await;
    assert!(
        matches!(outcome, ActionOutcome::Placed { .. }),
        "{outcome:?}"
    );
    let stand = mobs(&s.region, EntityType::ArmorStand).await.remove(0);
    assert_eq!(stand.position, [8.5, SKY as f64 + 1.0, 8.5]);
    let id = stand.id;
    let nbt = saved(&s.region, id).await;
    let yaw = match nbt.get("Rotation") {
        Some(Tag::List(r)) => match r[0] {
            Tag::Float(y) => y,
            _ => panic!("float"),
        },
        _ => panic!("rotation"),
    };
    assert_eq!(yaw, -90.0, "snapped to the player's opposite eighth");

    let mut helmet = interaction(&s.player, Some(Item::IronHelmet));
    helmet.location = [0.0, 1.8, 0.0];
    let (_, effect) = interact_with(&s.region, &s.player, &mut s.events, id, helmet).await;
    assert!(matches!(effect, HandEffect::Consume(1)));
    let nbt = saved(&s.region, id).await;
    let worn = nbt
        .get("equipment")
        .and_then(Tag::as_compound)
        .and_then(|e| e.get("head"))
        .and_then(Tag::as_compound)
        .expect("a helmet");
    assert_eq!(worn.get_str("id"), Some("minecraft:iron_helmet"));

    let mut stick = interaction(&s.player, Some(Item::Stick));
    stick.location = [0.0, 1.0, 0.0];
    s.region
        .request(RegionRequest::InteractEntity {
            player: s.player.clone(),
            target: id,
            interaction: stick,
        })
        .await;
    s.world.ticks(2).await;
    assert!(
        saved(&s.region, id)
            .await
            .get("equipment")
            .and_then(Tag::as_compound)
            .is_some_and(|e| e.get("mainhand").is_none()),
        "no arms, no hand items"
    );

    let mut take = interaction(&s.player, None);
    take.location = [0.0, 1.8, 0.0];
    let (item, effect) = interact_with(&s.region, &s.player, &mut s.events, id, take).await;
    assert_eq!(item, None);
    let HandEffect::Replace(Some(stack)) = effect else {
        panic!("the helmet goes to the hand: {effect:?}");
    };
    assert_eq!(stack.item, Item::IronHelmet);

    let mut helmet = interaction(&s.player, Some(Item::IronHelmet));
    helmet.location = [0.0, 1.8, 0.0];
    interact_with(&s.region, &s.player, &mut s.events, id, helmet).await;

    for _ in 0..2 {
        s.region
            .request(RegionRequest::AttackEntity {
                attacker: s.player.clone(),
                target: id,
                attack: punch(s.player.id),
                sweep: None,
            })
            .await;
    }
    wait_for(&s.world, &s.region, 20, |found| {
        !found.iter().any(|e| e.kind == EntityType::ArmorStand)
    })
    .await;
    let drops = dropped(&s.region).await;
    assert!(
        drops.contains(&"minecraft:armor_stand".to_owned()),
        "{drops:?}"
    );
    assert!(
        drops.contains(&"minecraft:iron_helmet".to_owned()),
        "{drops:?}"
    );
    s.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_hit_only_wobbles_an_armor_stand() {
    let s = scene(1005).await;
    summon(
        &s.region,
        EntityType::ArmorStand,
        Compound::new(),
        [8.5, SKY as f64 + 1.0, 8.5],
    )
    .await;
    let id = mobs(&s.region, EntityType::ArmorStand).await[0].id;
    s.region
        .request(RegionRequest::AttackEntity {
            attacker: s.player.clone(),
            target: id,
            attack: punch(s.player.id),
            sweep: None,
        })
        .await;
    s.world.ticks(10).await;
    assert_eq!(mobs(&s.region, EntityType::ArmorStand).await.len(), 1);
    s.world.ticks(10).await;
    s.region
        .request(RegionRequest::AttackEntity {
            attacker: s.player.clone(),
            target: id,
            attack: punch(s.player.id),
            sweep: None,
        })
        .await;
    s.world.ticks(10).await;
    assert_eq!(
        mobs(&s.region, EntityType::ArmorStand).await.len(),
        1,
        "the second hit came too late"
    );
    s.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_armor_stand_keeps_its_pose_and_flags() {
    let s = scene(1006).await;
    let pose = Compound::new().with(
        "Head",
        Tag::List(vec![Tag::Float(10.0), Tag::Float(20.0), Tag::Float(30.0)]),
    );
    let nbt = Compound::new()
        .with("Small", true)
        .with("ShowArms", true)
        .with("NoBasePlate", true)
        .with("DisabledSlots", 4i32)
        .with("Pose", pose);
    summon(
        &s.region,
        EntityType::ArmorStand,
        nbt,
        [8.5, SKY as f64 + 1.0, 8.5],
    )
    .await;
    let stand = mobs(&s.region, EntityType::ArmorStand).await.remove(0);
    assert_eq!(stand.bounds[3] - stand.bounds[0], 0.25);
    let saved = saved(&s.region, stand.id).await;
    assert_eq!(saved.get("Small"), Some(&Tag::Byte(1)));
    assert_eq!(saved.get("ShowArms"), Some(&Tag::Byte(1)));
    assert_eq!(saved.get("DisabledSlots"), Some(&Tag::Int(4)));
    let head = saved
        .get("Pose")
        .and_then(Tag::as_compound)
        .and_then(|p| p.get("Head"));
    assert_eq!(
        head,
        Some(&Tag::List(vec![
            Tag::Float(10.0),
            Tag::Float(20.0),
            Tag::Float(30.0)
        ]))
    );
    s.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_framed_map_marks_its_frame_and_reaches_players_nearby() {
    use tuff_feature_builtin_vanilla_maps::{MapFrame, MapService};
    let mut s = scene(1010).await;
    let shared = s.world.shared.clone();
    let maps = shared.services.get::<MapService>().unwrap();
    let map = maps
        .create("minecraft:overworld", 0.0, 0.0, 0, true, false)
        .await
        .unwrap();
    let id = frame_on_a_wall(&mut s, Item::ItemFrame).await;
    let mut put = interaction(&s.player, Some(Item::FilledMap));
    let mut stack =
        tuff_item::item::ItemStack::mint(Item::FilledMap, 1, tuff_item::item::MintReason::Test);
    stack.components.map_id = Some(map);
    put.stack = Some(tuff_item::item::ItemSnapshot::of(&stack));
    interact_with(&s.region, &s.player, &mut s.events, id, put).await;
    s.world
        .publish_players("minecraft:overworld", &[(&s.player, [6.0, 70.0, 8.0])]);
    s.world.ticks(12).await;
    maps.sync().await;
    let data = maps.data(map).await.unwrap();
    assert_eq!(
        data.frames,
        vec![MapFrame {
            pos: BlockPos::new(6, SKY + 1, 8),
            rotation: 270,
            entity_id: id,
        }]
    );
    let map_data = tuff_vanilla_data::packet::play::Clientbound::MapItemData.id();
    expect(&mut s.events, |event| match event {
        PlayerEvent::Packets(packets)
            if packets.iter().any(|p| framed_packet_id(p) == map_data) =>
        {
            Some(())
        }
        _ => None,
    })
    .await;

    s.region
        .request(RegionRequest::AttackEntity {
            attacker: s.player.clone(),
            target: id,
            attack: punch(s.player.id),
            sweep: None,
        })
        .await;
    s.world.ticks(3).await;
    maps.sync().await;
    assert!(maps.data(map).await.unwrap().frames.is_empty());
    s.world.shutdown().await;
}
