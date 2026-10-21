//! Command execution against a real region and session, driven through
//! the same channels the actors use.

use super::*;
use crate::auth::GameProfile;
use crate::protocol::configuration::ClientInformation;
use crate::world::messages::{
    BlockOp, BlockOpResult, Directive, DirectiveSource, EntitySnapshot, Teleport,
};
use crate::world::session::PlayerSession;
use tuff_item::item::Enchantments;
use tuff_storage::admin::PermissionLevel;
use tuff_vanilla_data::BlockState;
use tuff_vanilla_data::registry::EntityType;

async fn blocks(region: &RegionHandle, op: BlockOp) -> BlockOpResult {
    let (reply, answer) = oneshot::channel();
    region.request(RegionRequest::Blocks { op, reply }).await;
    answer.await.unwrap()
}

async fn entities(region: &RegionHandle) -> Vec<EntitySnapshot> {
    let (reply, answer) = oneshot::channel();
    region.request(RegionRequest::Entities { reply }).await;
    answer.await.unwrap()
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn filling_a_volume_sends_one_packet_per_section() {
    let dir = TestDir::new();
    let (viewer, mut events) = PlayerHandle::new(100);
    let pos = ChunkPos::new(0, 0);
    let world = TestWorld::start(dir.path());
    let region = world.region(pos.region()).await;
    subscribe(&region, &viewer, &mut events, &[pos]).await;
    let glass = Block::Glass.default_state();
    let mut changes = Vec::new();
    for y in 64..96 {
        for x in 0..16 {
            for z in 0..16 {
                changes.push((BlockPos::new(x, y, z), glass));
            }
        }
    }
    let op = BlockOp::Set {
        changes,
        destroy: false,
        keep: false,
        strict: true,
        quiet: false,
        data: None,
    };
    assert!(matches!(
        blocks(&region, op).await,
        BlockOpResult::Set(8192)
    ));

    let single = tuff_vanilla_data::packet::play::Clientbound::BlockUpdate.id();
    let section = tuff_vanilla_data::packet::play::Clientbound::SectionBlocksUpdate.id();
    let mut messages = 0;
    let mut sections = Vec::new();
    while let Ok(event) = events.try_recv() {
        let PlayerEvent::ChunkPackets { packets, .. } = event else {
            continue;
        };
        let mut carried = false;
        for frame in packets.iter() {
            let mut r = framed_payload(frame);
            let id = r.read_var_int().unwrap();
            assert_ne!(id, single, "no block is sent on its own");
            if id != section {
                continue;
            }
            carried = true;
            let section_y = r.read_long().unwrap() << 44 >> 44;
            let count = r.read_var_int().unwrap();
            let first = r.read_var_long().unwrap();
            assert_eq!(first >> 12, i64::from(glass.id()));
            sections.push((section_y, count));
        }
        messages += usize::from(carried);
    }
    assert_eq!(messages, 1, "one message per viewer and chunk");
    sections.sort_unstable();
    assert_eq!(sections, vec![(4, 4096), (5, 4096)]);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mined_ore_drops_its_experience_unless_silk_touched() {
    let dir = TestDir::new();
    let (viewer, mut events) = PlayerHandle::new(100);
    let pos = ChunkPos::new(0, 0);
    let height = Generator::new(1).height(1, 1);
    let world = TestWorld::start(dir.path());
    let region = world.region(pos.region()).await;
    subscribe(&region, &viewer, &mut events, &[pos]).await;
    let ore = BlockPos::new(1, height, 1);
    let lapis = BlockPos::new(2, height, 1);
    let changes = vec![
        (ore, Block::DiamondOre.default_state()),
        (lapis, Block::LapisOre.default_state()),
    ];
    let result = blocks(
        &region,
        BlockOp::Set {
            changes,
            destroy: false,
            keep: false,
            strict: false,
            quiet: false,
            data: None,
        },
    )
    .await;
    assert!(matches!(result, BlockOpResult::Set(2)));

    let mut silk = ItemStack::mint(Item::IronPickaxe, 1, MintReason::Test);
    silk.components.enchantments =
        Some(Enchantments::new([("minecraft:silk_touch".to_owned(), 1)]));
    region
        .request(RegionRequest::Break {
            player: viewer.clone(),
            pos: lapis,
            tool: Some(silk),
            mode: BreakMode::Finish,
            sequence: 1,
        })
        .await;
    expect(&mut events, |event| match event {
        PlayerEvent::ActionResult { sequence: 1, .. } => Some(()),
        _ => None,
    })
    .await;
    assert!(
        entities(&region)
            .await
            .iter()
            .all(|e| e.kind != EntityType::ExperienceOrb),
        "silk touch drops no experience"
    );

    region
        .request(RegionRequest::Break {
            player: viewer.clone(),
            pos: ore,
            tool: Some(ItemStack::mint(Item::IronPickaxe, 1, MintReason::Test)),
            mode: BreakMode::Finish,
            sequence: 2,
        })
        .await;
    expect(&mut events, |event| match event {
        PlayerEvent::ActionResult { sequence: 2, .. } => Some(()),
        _ => None,
    })
    .await;
    let orbs: i32 = entities(&region)
        .await
        .iter()
        .filter(|e| e.kind == EntityType::ExperienceOrb)
        .map(|e| e.experience)
        .sum();
    assert!(
        (3..=7).contains(&orbs),
        "diamond ore drops 3..7, got {orbs}"
    );
    let ids: Vec<i32> = entities(&region).await.iter().map(|e| e.id).collect();
    let answer = region
        .ask(move |world| ids.into_iter().filter(|id| world.kill_entity(*id)).count())
        .await
        .unwrap();
    assert!(answer >= 1);
    assert!(entities(&region).await.is_empty());
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn block_operations_load_chunks_and_report_counts() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let region = world.region(RegionPos { x: 0, z: 0 }).await;
    let corner = BlockPos::new(40, 200, 40);
    let unloaded = blocks(
        &region,
        BlockOp::Get {
            positions: vec![corner],
            load: false,
        },
    )
    .await;
    assert!(matches!(unloaded, BlockOpResult::States(states) if states == [None]));
    let changes: Vec<(BlockPos, BlockState)> = (0..3)
        .flat_map(|dx| (0..3).map(move |dz| (dx, dz)))
        .map(|(dx, dz)| {
            (
                BlockPos::new(40 + dx, 200, 40 + dz),
                Block::Stone.default_state(),
            )
        })
        .collect();
    let set = blocks(
        &region,
        BlockOp::Set {
            changes: changes.clone(),
            destroy: false,
            keep: false,
            strict: false,
            quiet: false,
            data: None,
        },
    )
    .await;
    assert!(matches!(set, BlockOpResult::Set(9)));
    let kept = blocks(
        &region,
        BlockOp::Set {
            changes: changes.clone(),
            destroy: false,
            keep: true,
            strict: false,
            quiet: false,
            data: None,
        },
    )
    .await;
    assert!(matches!(kept, BlockOpResult::Set(0)), "keep fills only air");
    let read = blocks(
        &region,
        BlockOp::Get {
            positions: vec![corner],
            load: false,
        },
    )
    .await;
    assert!(
        matches!(read, BlockOpResult::States(states) if states == [Some(Block::Stone.default_state())])
    );
    world.shutdown().await;
}

fn session(world: &TestWorld, id: i32) -> (PlayerSession, mpsc::UnboundedReceiver<bytes::Bytes>) {
    let (outbound, outbound_rx) = mpsc::unbounded_channel();
    let (handle, events) = PlayerHandle::new(id);
    let (_packets, packets_rx) = mpsc::channel(8);
    let profile = GameProfile {
        id: id as u128,
        name: format!("Player{id}"),
        properties: Vec::new(),
    };
    let session = PlayerSession::new(
        world.shared.clone(),
        id,
        profile,
        ClientInformation::default(),
        outbound,
        handle,
        events,
        packets_rx,
        PermissionLevel::Owners,
    );
    (session, outbound_rx)
}

fn source() -> DirectiveSource {
    DirectiveSource {
        name: TextComponent::new("Server"),
        player: None,
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn directives_change_the_players_inventory_and_experience() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut session, _outbound) = session(&world, 7);

    let stone = ItemStack::mint(Item::Stone, 1, MintReason::Test);
    let reply = session
        .apply_directive(
            Directive::Give {
                stack: stone.dup_unchecked(),
                count: 70,
            },
            source(),
        )
        .await;
    assert!(reply.affected);
    let stacks: Vec<u8> = session
        .inventory()
        .slots()
        .iter()
        .flatten()
        .filter(|s| s.item == Item::Stone)
        .map(|s| s.count())
        .collect();
    assert_eq!(stacks, [64, 6], "the game splits gifts into full stacks");

    let counted = session
        .apply_directive(
            Directive::Clear {
                predicate: None,
                max_count: 0,
            },
            source(),
        )
        .await;
    assert_eq!(counted.value, 70);
    assert_eq!(
        session.inventory().slots().iter().flatten().count(),
        2,
        "counting removes nothing"
    );
    let some = session
        .apply_directive(
            Directive::Clear {
                predicate: Some(
                    crate::command::item::parse_item_predicate(
                        &mut crate::command::reader::Reader::new("stone"),
                    )
                    .unwrap(),
                ),
                max_count: 10,
            },
            source(),
        )
        .await;
    assert_eq!(some.value, 10);
    let left: u8 = session
        .inventory()
        .slots()
        .iter()
        .flatten()
        .map(|s| s.count())
        .sum();
    assert_eq!(left, 60);
    let rest = session
        .apply_directive(
            Directive::Clear {
                predicate: None,
                max_count: -1,
            },
            source(),
        )
        .await;
    assert_eq!(rest.value, 60);
    assert!(session.inventory().slots().iter().all(Option::is_none));
    let nothing = session
        .apply_directive(
            Directive::Clear {
                predicate: None,
                max_count: -1,
            },
            source(),
        )
        .await;
    assert!(!nothing.affected);

    let levels = session
        .apply_directive(
            Directive::Experience {
                amount: 5,
                levels: true,
                set: true,
            },
            source(),
        )
        .await;
    assert!(levels.affected);
    assert_eq!(session.experience().level, 5);
    let points = session
        .apply_directive(
            Directive::Experience {
                amount: 3,
                levels: false,
                set: true,
            },
            source(),
        )
        .await;
    assert!(points.affected);
    let query = session
        .apply_directive(Directive::ExperienceQuery { levels: false }, source())
        .await;
    assert_eq!(query.value, 3);
    let invalid = session
        .apply_directive(
            Directive::Experience {
                amount: 1000,
                levels: false,
                set: true,
            },
            source(),
        )
        .await;
    assert!(!invalid.affected);
    assert_eq!(
        invalid.error.unwrap().translate.as_deref(),
        Some("commands.experience.set.points.invalid")
    );
    let added = session
        .apply_directive(
            Directive::Experience {
                amount: 2,
                levels: true,
                set: false,
            },
            source(),
        )
        .await;
    assert!(added.affected);
    assert_eq!(session.experience().level, 7);
    let queried = session
        .apply_directive(Directive::ExperienceQuery { levels: true }, source())
        .await;
    assert_eq!(queried.value, 7);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn enchant_checks_the_held_item_like_the_game() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut session, _outbound) = session(&world, 8);
    let itemless = session
        .apply_directive(
            Directive::Enchant {
                enchantment: "minecraft:sharpness".into(),
                level: 2,
            },
            source(),
        )
        .await;
    assert_eq!(
        itemless.error.unwrap().translate.as_deref(),
        Some("commands.enchant.failed.itemless")
    );
    let hand = crate::world::inventory::Inventory::hotbar_slot(0);
    session.inventory_mut().set_slot(
        hand,
        Some(ItemStack::mint(Item::DiamondSword, 1, MintReason::Test)),
    );
    let sharp = session
        .apply_directive(
            Directive::Enchant {
                enchantment: "minecraft:sharpness".into(),
                level: 2,
            },
            source(),
        )
        .await;
    assert!(sharp.affected);
    assert_eq!(
        session.inventory().slots()[hand]
            .as_ref()
            .unwrap()
            .enchantment_level("minecraft:sharpness"),
        2
    );
    let smite = session
        .apply_directive(
            Directive::Enchant {
                enchantment: "minecraft:smite".into(),
                level: 1,
            },
            source(),
        )
        .await;
    assert_eq!(
        smite.error.unwrap().translate.as_deref(),
        Some("commands.enchant.failed.incompatible"),
        "smite conflicts with sharpness"
    );
    let unbreaking = session
        .apply_directive(
            Directive::Enchant {
                enchantment: "minecraft:unbreaking".into(),
                level: 3,
            },
            source(),
        )
        .await;
    assert!(unbreaking.affected);
    session.inventory_mut().set_slot(
        hand,
        Some(ItemStack::mint(Item::Stone, 1, MintReason::Test)),
    );
    let stone = session
        .apply_directive(
            Directive::Enchant {
                enchantment: "minecraft:unbreaking".into(),
                level: 1,
            },
            source(),
        )
        .await;
    assert_eq!(
        stone.error.unwrap().translate.as_deref(),
        Some("commands.enchant.failed.incompatible")
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mending_spends_orb_experience_on_a_damaged_item_before_granting_xp() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut session, _outbound) = session(&world, 10);
    let hand = crate::world::inventory::Inventory::hotbar_slot(0);
    let mut pick = ItemStack::mint(Item::DiamondPickaxe, 1, MintReason::Test);
    pick.set_damage(10);
    pick.components.enchantments = Some(Enchantments::new([("minecraft:mending".to_owned(), 1)]));
    session.inventory_mut().set_slot(hand, Some(pick));

    session
        .handle_event(PlayerEvent::Experience { amount: 3 })
        .await;
    let worn = session.inventory().slots()[hand].as_ref().unwrap();
    assert_eq!(worn.damage(), 4, "6 durability repaired for 3 xp");
    assert_eq!(session.experience().total, 0, "all the xp went to the tool");

    session
        .handle_event(PlayerEvent::Experience { amount: 10 })
        .await;
    let worn = session.inventory().slots()[hand].as_ref().unwrap();
    assert_eq!(worn.damage(), 0, "the last 4 damage is fully repaired");
    assert_eq!(
        session.experience().total,
        8,
        "2 xp finished the repair, 8 left over as normal experience"
    );

    session
        .handle_event(PlayerEvent::Experience { amount: 5 })
        .await;
    assert_eq!(
        session.experience().total,
        13,
        "nothing left to mend, so it all becomes experience"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn teleport_and_spawnpoint_move_the_player() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut session, _outbound) = session(&world, 9);
    let overworld = session.dimension_id();
    let moved = session
        .apply_directive(
            Directive::Teleport(Teleport {
                dimension: overworld,
                position: [100.5, 90.0, -20.5],
                relative: [false; 3],
                rotation: Some([45.0, 10.0]),
                relative_rotation: [false; 2],
                look_at: None,
            }),
            source(),
        )
        .await;
    assert!(moved.affected);
    assert_eq!(session.position(), [100.5, 90.0, -20.5]);
    let nether = world.dimension(THE_NETHER);
    session
        .apply_directive(
            Directive::Teleport(Teleport {
                dimension: nether,
                position: [8.5, 70.0, 8.5],
                relative: [false; 3],
                rotation: None,
                relative_rotation: [false; 2],
                look_at: None,
            }),
            source(),
        )
        .await;
    assert_eq!(session.dimension_id(), nether);
    assert_eq!(session.position(), [8.5, 70.0, 8.5]);
    session
        .apply_directive(
            Directive::SpawnPoint {
                dimension: nether,
                pos: BlockPos::new(1, 2, 3),
                yaw: 90.0,
                pitch: 0.0,
            },
            source(),
        )
        .await;
    let respawn = session.respawn_point_save().unwrap();
    assert_eq!(respawn.dimension, THE_NETHER);
    assert_eq!(respawn.pos, [1, 2, 3]);
    world.shutdown().await;
}

/// `/tp` into a chunk far enough out that it has never been generated
/// must not release the player into it before the terrain exists, or the
/// client free-falls through the unrendered column and takes fall/void
/// damage while the real ground is still loading.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn teleport_into_unloaded_terrain_waits_for_the_chunk() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut session, _outbound) = session(&world, 10);
    let overworld = session.dimension_id();
    let target = [20_048.5, 200.0, 20_048.5];
    let target_block = BlockPos::new(20_048, 200, 20_048);

    let moved = session
        .apply_directive(
            Directive::Teleport(Teleport {
                dimension: overworld,
                position: target,
                relative: [false; 3],
                rotation: None,
                relative_rotation: [false; 2],
                look_at: None,
            }),
            source(),
        )
        .await;
    assert!(moved.affected);
    assert_eq!(session.position(), target);
    assert_eq!(
        session.health(),
        crate::world::player::MAX_HEALTH,
        "arriving must not damage the player"
    );

    let region = world
        .region_in(OVERWORLD, ChunkPos::of_position(target).region())
        .await;
    let states = blocks(
        &region,
        BlockOp::Get {
            positions: vec![target_block],
            load: false,
        },
    )
    .await;
    assert!(
        matches!(&states, BlockOpResult::States(s) if s[0].is_some()),
        "destination chunk should already be loaded, not {states:?}"
    );

    session.run_tick().await;
    assert_eq!(
        session.health(),
        crate::world::player::MAX_HEALTH,
        "no void/fall damage right after arrival"
    );
    world.shutdown().await;
}

/// A same-dimension `/tp` into already-loaded terrain (the common case)
/// must not be delayed: the chunk wait resolves without ever touching
/// disk or the generator.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn teleport_into_loaded_terrain_is_immediate() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut session, _outbound) = session(&world, 12);
    let overworld = session.dimension_id();
    let target = [8.5, 90.0, 8.5];
    let region = world
        .region_in(OVERWORLD, ChunkPos::of_position(target).region())
        .await;
    let (viewer, mut viewer_events) = PlayerHandle::new(999);
    subscribe(
        &region,
        &viewer,
        &mut viewer_events,
        &[ChunkPos::of_position(target)],
    )
    .await;

    let moved = tokio::time::timeout(
        Duration::from_millis(200),
        session.apply_directive(
            Directive::Teleport(Teleport {
                dimension: overworld,
                position: target,
                relative: [false; 3],
                rotation: None,
                relative_rotation: [false; 2],
                look_at: None,
            }),
            source(),
        ),
    )
    .await
    .expect("teleporting into already-loaded terrain must not wait");
    assert!(moved.affected);
    assert_eq!(session.position(), target);
    world.shutdown().await;
}

/// Cross-dimension `/tp` (the respawn-flow `relocate` path) has the
/// same requirement: the target dimension's chunk must be installed
/// before the player arrives there.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cross_dimension_teleport_into_unloaded_terrain_waits_for_the_chunk() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut session, _outbound) = session(&world, 13);
    let nether = world.dimension(THE_NETHER);
    let target = [20_048.5, 70.0, 20_048.5];
    let target_block = BlockPos::new(20_048, 70, 20_048);

    let moved = session
        .apply_directive(
            Directive::Teleport(Teleport {
                dimension: nether,
                position: target,
                relative: [false; 3],
                rotation: None,
                relative_rotation: [false; 2],
                look_at: None,
            }),
            source(),
        )
        .await;
    assert!(moved.affected);
    assert_eq!(session.dimension_id(), nether);
    assert_eq!(session.position(), target);
    assert_eq!(
        session.health(),
        crate::world::player::MAX_HEALTH,
        "arriving must not damage the player"
    );

    let region = world
        .region_in(THE_NETHER, ChunkPos::of_position(target).region())
        .await;
    let states = blocks(
        &region,
        BlockOp::Get {
            positions: vec![target_block],
            load: false,
        },
    )
    .await;
    assert!(
        matches!(&states, BlockOpResult::States(s) if s[0].is_some()),
        "destination chunk should already be loaded, not {states:?}"
    );

    session.run_tick().await;
    assert_eq!(
        session.health(),
        crate::world::player::MAX_HEALTH,
        "no void/fall damage right after arrival"
    );
    world.shutdown().await;
}

async fn copy(region: &RegionHandle, pos: BlockPos) -> (BlockState, Option<Compound>) {
    match blocks(
        region,
        BlockOp::Copy {
            positions: vec![pos],
        },
    )
    .await
    {
        BlockOpResult::Copied(mut found) => found.remove(0).expect("owned and loaded"),
        other => panic!("{other:?}"),
    }
}

async fn paste(region: &RegionHandle, changes: Vec<(BlockPos, BlockState, Option<Compound>)>) {
    let result = blocks(
        region,
        BlockOp::Paste {
            changes,
            strict: false,
        },
    )
    .await;
    assert!(
        matches!(result, BlockOpResult::Set(n) if n > 0),
        "{result:?}"
    );
}

fn stored_item(nbt: &Compound, key: &str) -> Option<(String, i32)> {
    let item = nbt.get_compound(key)?;
    Some((item.get_str("id")?.to_owned(), item.get_int("count")?))
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pasted_chests_keep_their_items_across_a_region_border() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let source = BlockPos::new(500, 200, 40);
    let target = BlockPos::new(520, 200, 40);
    let west = world.region(ChunkPos::of_block(source).region()).await;
    let east = world.region(ChunkPos::of_block(target).region()).await;
    let chest = Block::Chest.default_state();
    let items = Compound::new().with("id", "minecraft:chest").with(
        "Items",
        Tag::List(vec![Tag::Compound(
            Compound::new()
                .with("Slot", Tag::Byte(3))
                .with("id", "minecraft:diamond")
                .with("count", Tag::Int(5)),
        )]),
    );
    paste(&west, vec![(source, chest, Some(items))]).await;
    let (state, data) = copy(&west, source).await;
    assert_eq!(state, chest);
    let data = data.expect("the chest has a block entity");
    assert_eq!(data.get_int("x"), Some(500));

    paste(&east, vec![(target, state, Some(data))]).await;
    let (state, data) = copy(&east, target).await;
    assert_eq!(state, chest);
    let data = data.expect("the pasted chest has a block entity");
    assert_eq!(data.get_int("x"), Some(520));
    let Some(Tag::List(slots)) = data.get("Items") else {
        panic!("no items in {data:?}");
    };
    let slot = slots[0].as_compound().unwrap();
    assert_eq!(slot.get_str("id"), Some("minecraft:diamond"));
    assert_eq!(slot.get_int("count"), Some(5));

    paste(&east, vec![(target, Block::Air.default_state(), None)]).await;
    world.ticks(2).await;
    assert!(
        entities(&east).await.is_empty(),
        "pasting over a container clears it without spilling"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn replaced_jukeboxes_and_lecterns_drop_their_record_and_book() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let region = world.region(RegionPos { x: 0, z: 0 }).await;
    let jukebox_at = BlockPos::new(40, 200, 40);
    let lectern_at = BlockPos::new(44, 200, 40);
    let jukebox = Block::Jukebox
        .default_state()
        .with("has_record", "true")
        .unwrap();
    let lectern = Block::Lectern
        .default_state()
        .with("has_book", "true")
        .unwrap()
        .with("facing", "east")
        .unwrap();
    let disc = Compound::new()
        .with("id", "minecraft:music_disc_cat")
        .with("count", Tag::Int(1));
    let book = Compound::new()
        .with("id", "minecraft:writable_book")
        .with("count", Tag::Int(1));
    paste(
        &region,
        vec![
            (
                jukebox_at,
                jukebox,
                Some(
                    Compound::new()
                        .with("id", "minecraft:jukebox")
                        .with("RecordItem", disc),
                ),
            ),
            (
                lectern_at,
                lectern,
                Some(
                    Compound::new()
                        .with("id", "minecraft:lectern")
                        .with("Book", book)
                        .with("Page", Tag::Int(0)),
                ),
            ),
        ],
    )
    .await;
    let (_, data) = copy(&region, jukebox_at).await;
    assert!(data.is_some_and(|nbt| stored_item(&nbt, "RecordItem").is_some()));

    let stone = Block::Stone.default_state();
    let replaced = blocks(
        &region,
        BlockOp::Set {
            changes: vec![(jukebox_at, stone), (lectern_at, stone)],
            destroy: false,
            keep: false,
            strict: false,
            quiet: false,
            data: None,
        },
    )
    .await;
    assert!(matches!(replaced, BlockOpResult::Set(2)));
    let mut dropped = Vec::new();
    for entity in entities(&region).await {
        let (reply, answer) = oneshot::channel();
        region
            .request(RegionRequest::EntityData {
                id: entity.id,
                reply,
            })
            .await;
        let nbt = answer.await.unwrap().unwrap();
        dropped.push((stored_item(&nbt, "Item").unwrap(), entity.position));
    }
    dropped.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(dropped.len(), 2, "{dropped:?}");
    let (disc, at) = &dropped[0];
    assert_eq!(disc, &("minecraft:music_disc_cat".to_owned(), 1));
    assert!((at[0] - 40.5).abs() <= 0.35 && (at[2] - 40.5).abs() <= 0.35);
    assert!((at[1] - 201.01).abs() < 1e-9);
    let (book, at) = &dropped[1];
    assert_eq!(book, &("minecraft:writable_book".to_owned(), 1));
    assert_eq!([at[0], at[1], at[2]], [44.75, 201.0, 40.5]);
    world.shutdown().await;
}

async fn command_set(
    region: &RegionHandle,
    changes: Vec<(BlockPos, BlockState)>,
    data: Option<Compound>,
) -> BlockOpResult {
    blocks(
        region,
        BlockOp::Set {
            changes,
            destroy: false,
            keep: false,
            strict: false,
            quiet: true,
            data,
        },
    )
    .await
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn command_sets_replace_block_entities_quietly_and_reload_their_data() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let region = world.region(RegionPos { x: 0, z: 0 }).await;
    let at = BlockPos::new(40, 200, 40);
    let chest = Block::Chest.default_state();
    let items = Compound::new().with(
        "Items",
        Tag::List(vec![Tag::Compound(
            Compound::new()
                .with("Slot", Tag::Byte(3))
                .with("id", "minecraft:diamond")
                .with("count", Tag::Int(5)),
        )]),
    );
    let placed = command_set(&region, vec![(at, chest)], Some(items.clone())).await;
    assert!(matches!(placed, BlockOpResult::Set(1)), "{placed:?}");
    let (_, data) = copy(&region, at).await;
    let data = data.expect("the chest has a block entity");
    assert!(data.get("Items").is_some(), "{data:?}");

    let again = command_set(&region, vec![(at, chest)], Some(items)).await;
    assert!(
        matches!(again, BlockOpResult::Set(0)),
        "same block, same data: {again:?}"
    );
    let named = Compound::new().with("CustomName", "Loot");
    let renamed = command_set(&region, vec![(at, chest)], Some(named)).await;
    assert!(matches!(renamed, BlockOpResult::Set(1)), "{renamed:?}");
    let (_, data) = copy(&region, at).await;
    let data = data.expect("the chest keeps its block entity");
    assert!(
        data.get_list("Items").is_none_or(|items| items.is_empty()),
        "the data reloads every field, so the items are gone: {data:?}"
    );
    assert!(data.get("CustomName").is_some(), "{data:?}");

    let cleared = command_set(&region, vec![(at, chest)], Some(Compound::new())).await;
    assert!(matches!(cleared, BlockOpResult::Set(1)), "{cleared:?}");
    let (_, data) = copy(&region, at).await;
    let data = data.expect("the chest keeps its block entity");
    assert!(
        data.get("CustomName").is_none(),
        "chest{{}} resets the name"
    );
    let unchanged = command_set(&region, vec![(at, chest)], Some(Compound::new())).await;
    assert!(
        matches!(unchanged, BlockOpResult::Set(0)),
        "reloading defaults over defaults changes nothing: {unchanged:?}"
    );

    let replaced = command_set(&region, vec![(at, Block::Stone.default_state())], None).await;
    assert!(matches!(replaced, BlockOpResult::Set(1)));
    world.ticks(2).await;
    assert!(
        entities(&region).await.is_empty(),
        "a replaced chest does not spill"
    );
    world.shutdown().await;
}
