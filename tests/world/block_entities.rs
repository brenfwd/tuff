//! Block entities of every kind through a real region: placement with item
//! data, the chunk packet, saving, drops, signs and player heads.

use super::*;
use crate::storage::codec::WorldBackendExt as _;
use crate::world::messages::HandEffect;
use tuff_item::item::decor::BannerLayer;
use tuff_item::item::{ComponentPatch, Components};
use tuff_vanilla_data::item::DyeColor;
use tuff_vanilla_data::packet::play::Clientbound;
use tuff_vanilla_data::registry::BlockEntityType;

async fn outcome(events: &mut mpsc::UnboundedReceiver<PlayerEvent>, seq: i32) -> ActionOutcome {
    expect(events, |event| match event {
        PlayerEvent::ActionResult { sequence, outcome } if sequence == seq => Some(outcome),
        _ => None,
    })
    .await
}

/// Places `item` with `components` on top of the terrain at `x`/`z` of
/// chunk (0, 0) and returns where the block went.
async fn place_with(
    region: &RegionHandle,
    player: &PlayerHandle,
    events: &mut mpsc::UnboundedReceiver<PlayerEvent>,
    (x, z): (i32, i32),
    item: Item,
    components: Components,
    sequence: i32,
) -> BlockPos {
    let floor = BlockPos::new(x, Generator::new(1).height(x, z), z);
    let mut placing = place(player, floor, item, sequence);
    placing.components = components;
    region.request(RegionRequest::Place(placing)).await;
    let result = outcome(events, sequence).await;
    assert!(
        matches!(result, ActionOutcome::Placed { .. }),
        "{item:?}: {result:?}"
    );
    BlockPos::new(x, floor.y + 1, z)
}

fn patterned() -> Components {
    Components::from(ComponentPatch {
        banner_patterns: Some(vec![BannerLayer {
            inline: None,
            pattern: "minecraft:stripe_top".into(),
            color: DyeColor::from_name("red").unwrap(),
        }]),
        custom_name: Some(Box::new(TextComponent::new("Flag"))),
        ..Default::default()
    })
}

fn head(sound: &str) -> Components {
    Components::from(ComponentPatch {
        other: Compound::new()
            .with("minecraft:profile", Compound::new().with("name", "Steve"))
            .with("minecraft:note_block_sound", sound),
        ..Default::default()
    })
}

/// The data of the first Block Entity Data packet about `at`.
async fn block_entity_data(
    events: &mut mpsc::UnboundedReceiver<PlayerEvent>,
    at: BlockPos,
) -> (i32, Compound) {
    block_entity_data_where(events, at, |_| true).await
}

/// The data of the first Block Entity Data packet about `at` that
/// `wanted` accepts.
async fn block_entity_data_where(
    events: &mut mpsc::UnboundedReceiver<PlayerEvent>,
    at: BlockPos,
    wanted: impl Fn(&Compound) -> bool,
) -> (i32, Compound) {
    expect(events, |event| match event {
        PlayerEvent::ChunkPackets { packets, .. } => packets.iter().find_map(|frame| {
            let mut r = framed_payload(frame);
            if r.read_var_int().unwrap() != Clientbound::BlockEntityData.id() {
                return None;
            }
            (r.read_position().unwrap() == at).then_some(())?;
            let kind = r.read_var_int().unwrap();
            let nbt = r.read_nbt().unwrap()?.as_compound()?.clone();
            wanted(&nbt).then_some((kind, nbt))
        }),
        _ => None,
    })
    .await
}

fn listed_at(
    listed: &[(u8, i16, i32, Option<Tag>)],
    at: BlockPos,
) -> Option<(i32, Option<Compound>)> {
    listed
        .iter()
        .find(|(xz, y, ..)| *xz == ((at.x & 15) << 4 | (at.z & 15)) as u8 && *y == at.y as i16)
        .map(|(_, _, kind, nbt)| (*kind, nbt.as_ref().and_then(|n| n.as_compound().cloned())))
}

fn opens_sign_editor(event: &PlayerEvent) -> bool {
    matches!(event, PlayerEvent::ChunkPackets { packets, .. }
        if packets.iter().any(|p| framed_packet_id(p) == Clientbound::OpenSignEditor.id()))
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn banners_signs_heads_and_bells_are_listed_and_survive_a_restart() {
    let dir = TestDir::new();
    let pos = ChunkPos::new(0, 0);
    let world = TestWorld::start(dir.path());
    let region = world.region(pos.region()).await;
    let (alice, mut alice_events) = PlayerHandle::new(100);
    subscribe(&region, &alice, &mut alice_events, &[pos]).await;

    let banner = place_with(
        &region,
        &alice,
        &mut alice_events,
        (1, 1),
        Item::WhiteBanner,
        patterned(),
        1,
    )
    .await;
    let (kind, data) = block_entity_data(&mut alice_events, banner).await;
    assert_eq!(kind, BlockEntityType::Banner.id() as i32);
    assert_eq!(data.get_list("patterns").unwrap().len(), 1);
    assert_eq!(data.get_str("CustomName"), Some("Flag"));

    let skull = place_with(
        &region,
        &alice,
        &mut alice_events,
        (4, 1),
        Item::PlayerHead,
        head("minecraft:entity.pig.ambient"),
        2,
    )
    .await;
    let sign = place_with(
        &region,
        &alice,
        &mut alice_events,
        (7, 1),
        Item::OakSign,
        Components::default(),
        3,
    )
    .await;
    let bell = place_with(
        &region,
        &alice,
        &mut alice_events,
        (10, 1),
        Item::Bell,
        Components::default(),
        4,
    )
    .await;

    let check = |listed: &[(u8, i16, i32, Option<Tag>)]| {
        let (kind, data) = listed_at(listed, banner).expect("banner listed");
        assert_eq!(kind, BlockEntityType::Banner.id() as i32);
        assert_eq!(data.unwrap().get_list("patterns").unwrap().len(), 1);
        let (kind, data) = listed_at(listed, skull).expect("skull listed");
        assert_eq!(kind, BlockEntityType::Skull.id() as i32);
        let data = data.unwrap();
        assert_eq!(
            data.get_compound("profile").unwrap().get_str("name"),
            Some("Steve")
        );
        let (kind, data) = listed_at(listed, sign).expect("sign listed");
        assert_eq!(kind, BlockEntityType::Sign.id() as i32);
        assert!(data.unwrap().contains_key("front_text"));
        assert_eq!(
            listed_at(listed, bell),
            Some((BlockEntityType::Bell.id() as i32, None))
        );
    };
    let (bob, mut bob_events) = PlayerHandle::new(101);
    check(&chunk_block_entities(
        &chunk_packet_for(&region, &bob, &mut bob_events, pos).await,
    ));
    world.shutdown().await;

    let root = stored_chunk(dir.path(), pos, false).expect("chunk saved");
    let stored: Vec<&str> = root
        .get_list("block_entities")
        .unwrap()
        .iter()
        .filter_map(|tag| tag.as_compound()?.get_str("id"))
        .collect();
    for id in [
        "minecraft:banner",
        "minecraft:skull",
        "minecraft:sign",
        "minecraft:bell",
    ] {
        assert!(stored.contains(&id), "{id} in {stored:?}");
    }

    let world = TestWorld::start(dir.path());
    let region = world.region(pos.region()).await;
    let (carol, mut carol_events) = PlayerHandle::new(102);
    check(&chunk_block_entities(
        &chunk_packet_for(&region, &carol, &mut carol_events, pos).await,
    ));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn broken_banners_and_named_chests_drop_their_data() {
    let dir = TestDir::new();
    let pos = ChunkPos::new(0, 0);
    let world = TestWorld::start(dir.path());
    let region = world.region(pos.region()).await;
    let (alice, mut alice_events) = PlayerHandle::new(100);
    subscribe(&region, &alice, &mut alice_events, &[pos]).await;
    let banner = place_with(
        &region,
        &alice,
        &mut alice_events,
        (3, 3),
        Item::WhiteBanner,
        patterned(),
        1,
    )
    .await;
    let named = Components::from(ComponentPatch {
        custom_name: Some(Box::new(TextComponent::new("Loot"))),
        ..Default::default()
    });
    let chest = place_with(
        &region,
        &alice,
        &mut alice_events,
        (8, 3),
        Item::Chest,
        named.clone(),
        2,
    )
    .await;

    for (sequence, at) in [(3, banner), (4, chest)] {
        region
            .request(RegionRequest::Break {
                player: alice.clone(),
                pos: at,
                tool: None,
                mode: BreakMode::Finish,
                sequence,
            })
            .await;
        outcome(&mut alice_events, sequence).await;
    }
    let mut dropped = Vec::new();
    for (seq, at) in [(1, banner), (2, chest)] {
        region
            .request(update(
                &alice,
                seq,
                [at.x as f64 + 0.5, at.y as f64, at.z as f64 + 0.5],
            ))
            .await;
        world.ticks(items::BLOCK_DROP_PICKUP_DELAY as u64 + 3).await;
        let stack = expect(&mut alice_events, |event| match event {
            PlayerEvent::OfferItem { stack, .. } => Some(stack),
            _ => None,
        })
        .await;
        dropped.push(stack);
    }
    let banner_drop = dropped
        .iter()
        .find(|s| s.item == Item::WhiteBanner)
        .expect("banner dropped");
    assert_eq!(banner_drop.components, patterned());
    let chest_drop = dropped
        .iter()
        .find(|s| s.item == Item::Chest)
        .expect("chest dropped");
    assert_eq!(chest_drop.components, named);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn signs_are_edited_by_their_placer_then_dyed_and_waxed() {
    let dir = TestDir::new();
    let pos = ChunkPos::new(0, 0);
    let world = TestWorld::start(dir.path());
    let region = world.region(pos.region()).await;
    let (alice, mut alice_events) = PlayerHandle::new(100);
    let (bob, mut bob_events) = PlayerHandle::new(101);
    subscribe(&region, &alice, &mut alice_events, &[pos]).await;
    subscribe(&region, &bob, &mut bob_events, &[pos]).await;
    let x = 5;
    let floor = BlockPos::new(x, Generator::new(1).height(x, 5), 5);
    region
        .request(RegionRequest::Place(place(&alice, floor, Item::OakSign, 1)))
        .await;
    expect(&mut alice_events, |event| {
        opens_sign_editor(&event).then_some(())
    })
    .await;
    outcome(&mut alice_events, 1).await;
    let sign = BlockPos::new(x, floor.y + 1, 5);
    let eye = [
        sign.x as f64 + 0.5,
        sign.y as f64 + 1.6,
        sign.z as f64 + 2.5,
    ];
    let lines = |text: &str| vec![text.to_owned(), String::new(), String::new(), String::new()];

    region
        .request(RegionRequest::UpdateSign {
            player: bob.clone(),
            pos: sign,
            front: true,
            lines: lines("bob was here"),
            eye,
            filtered: None,
        })
        .await;
    region
        .request(RegionRequest::UpdateSign {
            player: alice.clone(),
            pos: sign,
            front: true,
            lines: lines("\u{a7}cHello"),
            eye,
            filtered: None,
        })
        .await;
    let first_line = |nbt: &Compound| {
        nbt.get_compound("front_text")
            .and_then(|text| text.get_list("messages"))
            .and_then(|lines| lines.first().cloned())
    };
    block_entity_data_where(&mut bob_events, sign, |nbt| {
        first_line(nbt) == Some(Tag::from("Hello"))
    })
    .await;
    region
        .request(RegionRequest::UpdateSign {
            player: alice.clone(),
            pos: sign,
            front: true,
            lines: lines("again"),
            eye,
            filtered: None,
        })
        .await;

    let mut click = place(&bob, sign, Item::RedDye, 2);
    click.item = None;
    click.position = [sign.x as f64 + 0.5, sign.y as f64, sign.z as f64 - 2.5];
    region.request(RegionRequest::Place(click.clone())).await;
    let (_, data) = block_entity_data_where(&mut bob_events, sign, |nbt| {
        nbt.get_compound("front_text")
            .and_then(|text| text.get_str("color"))
            == Some("red")
    })
    .await;
    assert!(matches!(
        outcome(&mut bob_events, 2).await,
        ActionOutcome::Used {
            effect: HandEffect::Consume(1),
            ..
        }
    ));
    let front = data.get_compound("front_text").unwrap();
    assert_eq!(
        front.get_list("messages").unwrap()[0],
        Tag::from("Hello"),
        "the second edit came after the editor closed"
    );

    click.held = Some(Item::Honeycomb);
    click.sequence = 3;
    region.request(RegionRequest::Place(click.clone())).await;
    block_entity_data_where(&mut bob_events, sign, |nbt| {
        nbt.get_bool("is_waxed") == Some(true)
    })
    .await;
    assert!(matches!(
        outcome(&mut bob_events, 3).await,
        ActionOutcome::Used {
            effect: HandEffect::Consume(1),
            ..
        }
    ));

    click.held = None;
    click.sequence = 4;
    region.request(RegionRequest::Place(click)).await;
    assert!(matches!(
        outcome(&mut bob_events, 4).await,
        ActionOutcome::Interacted { .. }
    ));
    while let Ok(event) = bob_events.try_recv() {
        assert!(!opens_sign_editor(&event), "a waxed sign cannot be edited");
    }
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn waxing_a_sign_reports_the_item_used_on_the_block() {
    let dir = TestDir::new();
    let pos = ChunkPos::new(0, 0);
    let world = TestWorld::start(dir.path());
    let region = world.region(pos.region()).await;
    let (alice, mut events) = PlayerHandle::new(100);
    subscribe(&region, &alice, &mut events, &[pos]).await;
    let floor = BlockPos::new(5, Generator::new(1).height(5, 5), 5);
    region
        .request(RegionRequest::Place(place(&alice, floor, Item::OakSign, 1)))
        .await;
    outcome(&mut events, 1).await;
    let sign = BlockPos::new(5, floor.y + 1, 5);
    let mut click = place(&alice, sign, Item::Honeycomb, 2);
    click.item = None;
    click.position = [sign.x as f64 + 0.5, sign.y as f64, sign.z as f64 - 2.5];
    region.request(RegionRequest::Place(click)).await;
    expect(&mut events, |event| match event {
        PlayerEvent::ItemUsedOnBlock { item, .. } => (item == Item::Honeycomb).then_some(()),
        _ => None,
    })
    .await;
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn player_heads_give_note_blocks_their_sound() {
    let dir = TestDir::new();
    let pos = ChunkPos::new(0, 0);
    let world = TestWorld::start(dir.path());
    let region = world.region(pos.region()).await;
    let (alice, mut alice_events) = PlayerHandle::new(100);
    subscribe(&region, &alice, &mut alice_events, &[pos]).await;
    let note = place_with(
        &region,
        &alice,
        &mut alice_events,
        (6, 6),
        Item::NoteBlock,
        Components::default(),
        1,
    )
    .await;
    region
        .request(RegionRequest::Place(Place {
            components: head("minecraft:entity.pig.ambient"),
            ..place(&alice, note, Item::PlayerHead, 2)
        }))
        .await;
    assert!(matches!(
        outcome(&mut alice_events, 2).await,
        ActionOutcome::Placed { .. }
    ));
    assert_eq!(
        block_at(&region, note)
            .await
            .and_then(|s| s.get("instrument")),
        Some("custom_head")
    );
    let mut attack = place(&alice, note, Item::Stick, 3);
    attack.face = tuff_world_model::Face::North;
    region
        .request(RegionRequest::Break {
            player: alice.clone(),
            pos: note,
            tool: None,
            mode: BreakMode::Start {
                eye: attack.eye,
                miner: player::Miner::plain(true),
            },
            sequence: 3,
        })
        .await;
    let pig = tuff_vanilla_data::registry::SoundEvent::EntityPigAmbient;
    expect(&mut alice_events, |event| match event {
        PlayerEvent::ChunkPackets { packets, .. } => packets.iter().find_map(|frame| {
            let mut r = framed_payload(frame);
            if r.read_var_int().unwrap() != Clientbound::Sound.id() {
                return None;
            }
            (r.read_var_int().unwrap() == pig.id() as i32 + 1).then_some(())
        }),
        _ => None,
    })
    .await;
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn block_entity_data_applies_only_for_operators_where_the_game_requires_it() {
    let dir = TestDir::new();
    let pos = ChunkPos::new(0, 0);
    let world = TestWorld::start(dir.path());
    let region = world.region(pos.region()).await;
    let (alice, mut alice_events) = PlayerHandle::new(100);
    subscribe(&region, &alice, &mut alice_events, &[pos]).await;
    let with_data = |id: &str, key: &str, value: Tag| {
        Components::from(ComponentPatch {
            other: Compound::new().with(
                "minecraft:block_entity_data",
                Compound::new().with("id", id).with(key, value),
            ),
            ..Default::default()
        })
    };
    let spawn_data =
        Tag::Compound(Compound::new().with("entity", Compound::new().with("id", "minecraft:pig")));
    let refused = place_with(
        &region,
        &alice,
        &mut alice_events,
        (2, 9),
        Item::Spawner,
        with_data("minecraft:mob_spawner", "SpawnData", spawn_data.clone()),
        1,
    )
    .await;
    let mut op = place(
        &alice,
        BlockPos::new(5, Generator::new(1).height(5, 9), 9),
        Item::Spawner,
        2,
    );
    op.components = with_data("minecraft:mob_spawner", "SpawnData", spawn_data.clone());
    op.game_master = true;
    region.request(RegionRequest::Place(op)).await;
    outcome(&mut alice_events, 2).await;
    let allowed = BlockPos::new(5, Generator::new(1).height(5, 9) + 1, 9);
    let beacon = place_with(
        &region,
        &alice,
        &mut alice_events,
        (8, 9),
        Item::Beacon,
        with_data(
            "minecraft:beacon",
            "primary_effect",
            "minecraft:speed".into(),
        ),
        3,
    )
    .await;
    let wrong_type = place_with(
        &region,
        &alice,
        &mut alice_events,
        (11, 9),
        Item::Beacon,
        with_data("minecraft:bell", "primary_effect", "minecraft:speed".into()),
        4,
    )
    .await;

    let (bob, mut bob_events) = PlayerHandle::new(101);
    let listed = chunk_block_entities(&chunk_packet_for(&region, &bob, &mut bob_events, pos).await);
    let data = |at| {
        listed_at(&listed, at)
            .and_then(|(_, data)| data)
            .unwrap_or_default()
    };
    assert!(!data(refused).contains_key("SpawnData"));
    assert_eq!(data(allowed).get("SpawnData"), Some(&spawn_data));
    assert_eq!(
        data(beacon).get_str("primary_effect"),
        Some("minecraft:speed")
    );
    assert!(!data(wrong_type).contains_key("primary_effect"));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn breaking_an_open_ender_chest_closes_its_menu() {
    let dir = TestDir::new();
    let pos = ChunkPos::new(0, 0);
    let world = TestWorld::start(dir.path());
    let region = world.region(pos.region()).await;
    let (alice, mut alice_events) = PlayerHandle::new(100);
    subscribe(&region, &alice, &mut alice_events, &[pos]).await;
    let ender = BlockPos::new(3, Generator::new(1).height(3, 12) + 1, 12);
    place_blocks(
        &world,
        &region,
        [(ender, Block::EnderChest.default_state())],
    )
    .await;
    region
        .request(RegionRequest::Place(place(&alice, ender, Item::Torch, 1)))
        .await;
    expect(&mut alice_events, container_menu).await;
    place_blocks(&world, &region, [(ender, Block::Air.default_state())]).await;
    expect(&mut alice_events, |event| match event {
        PlayerEvent::ContainerClosed { pos, .. } if pos == ender => Some(()),
        _ => None,
    })
    .await;
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn brewing_stands_brew_with_blaze_powder_and_show_their_bottles() {
    use crate::world::block_entity::brewing;
    use tuff_item::item::PotionContents;
    use tuff_vanilla_data::registry::Potion;

    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let pos = ChunkPos::new(0, 0);
    let region = world.region(pos.region()).await;
    let (alice, mut alice_events) = PlayerHandle::new(100);
    subscribe(&region, &alice, &mut alice_events, &[pos]).await;
    let generator = Generator::new(1);
    let block = BlockPos::new(1, generator.height(1, 1) + 1, 1);
    place_blocks(
        &world,
        &region,
        [(block, Block::BrewingStand.default_state())],
    )
    .await;
    let feet = [block.x as f64 + 0.5, block.y as f64, block.z as f64 + 1.5];
    region.request(update(&alice, 1, feet)).await;
    region
        .request(RegionRequest::Place(place(&alice, block, Item::Torch, 1)))
        .await;
    let open = expect(&mut alice_events, container_menu).await;
    assert_eq!(open.kind, MenuKind::BrewingStand);
    assert_eq!(
        open.title,
        TextComponent::translate("container.brewing", [])
    );
    let MenuSource::Blocks { contents, data, .. } = open.source else {
        panic!("a brewing stand is a block container");
    };
    assert_eq!(
        contents,
        (0..5).map(|_| None).collect::<Vec<Option<ItemStack>>>()
    );
    assert_eq!(data, vec![0; 4]);

    let potion = |item: Item, potion: Potion| {
        Some(
            ItemStack::mint(item, 1, MintReason::Test).with_components(ComponentPatch {
                potion_contents: Some(Box::new(PotionContents {
                    potion: Some(potion),
                    ..Default::default()
                })),
                ..Default::default()
            }),
        )
    };
    let change = |slot: usize, new: Option<ItemStack>| ContainerChange {
        pos: block,
        slot,
        expected: None,
        new,
    };
    region
        .request(RegionRequest::ContainerTransaction {
            player: alice.clone(),
            window_id: 1,
            changes: vec![
                change(1, potion(Item::Potion, Potion::Water)),
                change(
                    brewing::INGREDIENT,
                    Some(ItemStack::mint(Item::NetherWart, 1, MintReason::Test)),
                ),
                change(
                    brewing::FUEL,
                    Some(ItemStack::mint(Item::BlazePowder, 1, MintReason::Test)),
                ),
            ],
            took_result: false,
        })
        .await;
    assert!(expect(&mut alice_events, transaction_answer).await.0);
    world.ticks(2).await;
    let state = block_at(&region, block).await.unwrap();
    assert_eq!(state.get("has_bottle_1"), Some("true"));
    assert_eq!(state.get("has_bottle_0"), Some("false"));
    expect(&mut alice_events, |event| match event {
        PlayerEvent::ContainerData { pos, data, .. }
            if pos == block && data.contains(&(brewing::DATA_FUEL, 19)) =>
        {
            Some(())
        }
        _ => None,
    })
    .await;
    world.ticks(400).await;
    let brewed = expect(&mut alice_events, |event| match event {
        PlayerEvent::ContainerSlots { pos, slots, .. }
            if pos == block && slots.iter().any(|(slot, _)| *slot == 1) =>
        {
            Some(slots)
        }
        _ => None,
    })
    .await;
    assert!(brewed.contains(&(1, potion(Item::Potion, Potion::Awkward))));
    assert!(brewed.contains(&(brewing::INGREDIENT, None)));
    world.shutdown().await;
}

/// Front text whose first line runs `command` when clicked.
fn clickable_text(command: &str) -> Compound {
    let line = Compound::new().with("text", "click me").with(
        "click_event",
        Compound::new()
            .with("action", "run_command")
            .with("command", command),
    );
    let empty = || Tag::from("");
    Compound::new().with(
        "messages",
        Tag::List(vec![Tag::Compound(line), empty(), empty(), empty()]),
    )
}

/// Placing a sign resets its text to the item's, so the click events are
/// written into the saved chunk between two runs of the world.
async fn signs_with_command(
    backend: &Arc<dyn WorldBackend>,
    command: &str,
    allow_op_features: [bool; 2],
) -> Vec<BlockPos> {
    let pos = ChunkPos::new(0, 0);
    let world = TestWorld::start_with(backend.clone());
    let region = world.region(pos.region()).await;
    let (alice, mut alice_events) = PlayerHandle::new(100);
    subscribe(&region, &alice, &mut alice_events, &[pos]).await;
    let mut placed = Vec::new();
    for (sequence, x) in [3, 7].into_iter().enumerate() {
        let floor = BlockPos::new(x, Generator::new(1).height(x, 5), 5);
        let sequence = sequence as i32 + 1;
        region
            .request(RegionRequest::Place(place(
                &alice,
                floor,
                Item::OakSign,
                sequence,
            )))
            .await;
        outcome(&mut alice_events, sequence).await;
        placed.push(BlockPos::new(x, floor.y + 1, 5));
    }
    world.shutdown().await;

    let codec = backend.chunk_codec();
    let mut store = backend.open_region(OVERWORLD, pos.file_region()).unwrap();
    let blob = store.read_chunk(pos).unwrap().expect("chunk saved");
    let Decoded::Chunk(chunk, mut meta) = codec
        .decode_chunk(&blob, pos, ChunkShape::OVERWORLD)
        .unwrap()
    else {
        panic!("saved chunk has terrain");
    };
    for (sign, allowed) in placed.iter().zip(allow_op_features) {
        let nbt = meta
            .raw_block_entities
            .get_mut(sign)
            .expect("the sign was saved");
        nbt.insert("front_text", clickable_text(command));
        nbt.insert("allow_op_features", allowed);
    }
    let blob = codec.encode_chunk(&chunk, &meta, pos, 0).unwrap();
    store.write_chunk(pos, &blob).unwrap();
    placed
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn clicked_sign_commands_run_only_where_op_features_are_allowed() {
    let backend: Arc<dyn WorldBackend> = Arc::new(tuff_storage::memory::MemoryBackend::default());
    let pos = ChunkPos::new(0, 0);
    let target = BlockPos::new(9, Generator::new(1).height(9, 9) + 3, 9);
    let command = format!(
        "/setblock {} {} {} minecraft:gold_block",
        target.x, target.y, target.z
    );
    let placed = signs_with_command(&backend, &command, [true, false]).await;

    let world = TestWorld::start_with(backend.clone());
    let region = world.region(pos.region()).await;
    let (alice, mut alice_events) = PlayerHandle::new(100);
    subscribe(&region, &alice, &mut alice_events, &[pos]).await;
    let click = |sign: BlockPos, sequence: i32| {
        let mut click = place(&alice, sign, Item::OakSign, sequence);
        click.item = None;
        click.held = None;
        click.position = [sign.x as f64 + 0.5, sign.y as f64, sign.z as f64 - 2.5];
        click
    };
    region
        .request(RegionRequest::Place(click(placed[0], 10)))
        .await;
    let (dimension, position, commands) = expect(&mut alice_events, |event| match event {
        PlayerEvent::SignCommands {
            dimension,
            position,
            commands,
        } => Some((dimension, position, commands)),
        PlayerEvent::Overlay(_) => panic!("op features are allowed on this sign"),
        _ => None,
    })
    .await;
    assert_eq!(commands, std::slice::from_ref(&command));
    let sign = placed[0];
    assert_eq!(
        position,
        [
            sign.x as f64 + 0.5,
            sign.y as f64 + 0.5,
            sign.z as f64 + 0.5
        ]
    );
    assert!(matches!(
        outcome(&mut alice_events, 10).await,
        ActionOutcome::Interacted { .. }
    ));

    region
        .request(RegionRequest::Place(click(placed[1], 11)))
        .await;
    let overlay = expect(&mut alice_events, |event| match event {
        PlayerEvent::Overlay(text) => Some(text),
        PlayerEvent::SignCommands { .. } => panic!("op features are off on this sign"),
        _ => None,
    })
    .await;
    assert_eq!(overlay.plain(), "Click actions are disabled for this sign");
    assert!(matches!(
        outcome(&mut alice_events, 11).await,
        ActionOutcome::Interacted { .. }
    ));

    let (outbound, _outbound_rx) = mpsc::unbounded_channel();
    let (handle, events) = PlayerHandle::new(200);
    let (_packets, packets_rx) = mpsc::channel(8);
    let profile = crate::auth::GameProfile {
        id: 200,
        name: "Clicker".to_owned(),
        properties: Vec::new(),
    };
    let mut session = crate::world::session::PlayerSession::new(
        world.shared.clone(),
        200,
        profile,
        crate::protocol::configuration::ClientInformation::default(),
        outbound,
        handle,
        events,
        packets_rx,
        tuff_storage::admin::PermissionLevel::All,
    );
    session
        .on_event(PlayerEvent::SignCommands {
            dimension,
            position,
            commands,
        })
        .await;
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::Blocks {
            op: crate::world::messages::BlockOp::Get {
                positions: vec![target],
                load: false,
            },
            reply,
        })
        .await;
    let crate::world::messages::BlockOpResult::States(states) = answer.await.unwrap() else {
        panic!("a read answers with states");
    };
    assert_eq!(
        states,
        [Some(Block::GoldBlock.default_state())],
        "a sign runs its command with gamemaster permission"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_beacon_over_a_pyramid_gives_its_effect_to_players_in_range() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let pos = ChunkPos::new(0, 0);
    let region = world.region(pos.region()).await;
    let (alice, mut events) = PlayerHandle::new(100);
    subscribe(&region, &alice, &mut events, &[pos]).await;
    let beacon = place_with(
        &region,
        &alice,
        &mut events,
        (8, 9),
        Item::Beacon,
        Components::from(ComponentPatch {
            other: Compound::new().with(
                "minecraft:block_entity_data",
                Compound::new()
                    .with("id", "minecraft:beacon")
                    .with("primary_effect", "minecraft:speed"),
            ),
            ..Default::default()
        }),
        1,
    )
    .await;
    let base = (7..=9).flat_map(|x| {
        (8..=10).map(move |z| {
            (
                BlockPos::new(x, beacon.y - 1, z),
                Block::IronBlock.default_state(),
            )
        })
    });
    place_blocks(&world, &region, base).await;
    world.publish_players(
        "minecraft:overworld",
        &[(
            &alice,
            [
                f64::from(beacon.x),
                f64::from(beacon.y),
                f64::from(beacon.z),
            ],
        )],
    );
    world.ticks(200).await;
    let effect = expect(&mut events, |event| match event {
        PlayerEvent::GiveEffect { effect, .. } => Some(effect),
        _ => None,
    })
    .await;
    assert_eq!(effect, tuff_vanilla_data::registry::MobEffect::Speed);
    world.shutdown().await;
}
