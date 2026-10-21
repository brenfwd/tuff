//! Allays: gathering dropped items for the player who gave them one or a
//! note block they heard, dancing to jukeboxes and duplicating, and their
//! memories surviving a save.

use super::*;
use crate::world::messages::{BlockOp, HandEffect, PlayerLooks};
use tuff_nbt::Tag;
use tuff_vanilla_data::registry::EntityType;

const Y: f64 = SKY as f64 + 1.0;

async fn start() -> (
    TestDir,
    TestWorld,
    RegionHandle,
    PlayerHandle,
    mpsc::UnboundedReceiver<PlayerEvent>,
) {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    (dir, world, region, player, events)
}

async fn set(
    region: &RegionHandle,
    pos: BlockPos,
    state: tuff_vanilla_data::BlockState,
    data: Option<Compound>,
) {
    blocks(
        region,
        BlockOp::Set {
            changes: vec![(pos, state)],
            destroy: false,
            keep: false,
            strict: false,
            quiet: false,
            data,
        },
    )
    .await;
}

async fn drop_item(region: &RegionHandle, item: Item, count: u8, at: [f64; 3]) -> i32 {
    let before: Vec<i32> = snapshots(region).await.iter().map(|s| s.id).collect();
    summon(
        region,
        EntityType::Item,
        Compound::new().with(
            "Item",
            ItemStack::mint(item, count, MintReason::Test).to_nbt(),
        ),
        at,
    )
    .await;
    mobs(region, EntityType::Item)
        .await
        .into_iter()
        .find(|s| !before.contains(&s.id))
        .expect("the item was summoned")
        .id
}

/// Dropped stacks of `item` and where they lie.
async fn items_of(region: &RegionHandle, item: Item) -> Vec<(i32, [f64; 3], u8)> {
    let mut found = Vec::new();
    for snapshot in mobs(region, EntityType::Item).await {
        let Some(nbt) = entity_nbt(region, snapshot.id).await else {
            continue;
        };
        let Some(stack) = nbt
            .get_compound("Item")
            .and_then(|nbt| ItemStack::mint_from_nbt(nbt, MintReason::Test))
        else {
            continue;
        };
        if stack.item == item {
            found.push((snapshot.id, snapshot.position, stack.count()));
        }
    }
    found
}

/// Right-clicks `target` holding `item`; what the click did to the hand,
/// or `None` when the allay ignored it.
async fn click(
    world: &TestWorld,
    region: &RegionHandle,
    player: &PlayerHandle,
    events: &mut mpsc::UnboundedReceiver<PlayerEvent>,
    target: i32,
    item: Item,
) -> Option<HandEffect> {
    while events.try_recv().is_ok() {}
    region
        .request(RegionRequest::InteractEntity {
            player: player.clone(),
            target,
            interaction: interaction(player, Some(item)),
        })
        .await;
    world.ticks(1).await;
    std::iter::from_fn(|| events.try_recv().ok()).find_map(|event| match event {
        PlayerEvent::EntityInteracted { effect, .. } => Some(effect),
        _ => None,
    })
}

fn holding(item: Item) -> Compound {
    Compound::new().with(
        "equipment",
        Compound::new().with(
            "mainhand",
            ItemStack::mint(item, 1, MintReason::Test).to_nbt(),
        ),
    )
}

fn memories(nbt: &Compound) -> Compound {
    nbt.get_compound("Brain")
        .and_then(|b| b.get_compound("memories"))
        .cloned()
        .unwrap_or_default()
}

fn memory<'a>(memories: &'a Compound, key: &str) -> Option<&'a Tag> {
    memories.get_compound(key)?.get("value")
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_allay_gathers_matching_items_and_brings_them_to_the_player_who_gave_it_one() {
    let (_dir, world, region, player, mut events) = start().await;
    summon(
        &region,
        EntityType::Allay,
        Compound::new(),
        [10.5, Y + 1.0, 10.5],
    )
    .await;
    let allay = mobs(&region, EntityType::Allay).await[0].id;
    let at = [12.5, Y, 12.5];
    region
        .request(player_at(&player, at, PlayerLooks::default()))
        .await;
    world.ticks(2).await;
    let given = click(
        &world,
        &region,
        &player,
        &mut events,
        allay,
        Item::Cobblestone,
    )
    .await;
    assert_eq!(given, Some(HandEffect::None));
    let nbt = entity_nbt(&region, allay).await.unwrap();
    assert_eq!(
        memory(&memories(&nbt), "minecraft:liked_player").and_then(Tag::as_uuid),
        Some(77),
        "it likes the player who gave it the item"
    );
    let dropped = drop_item(&region, Item::Cobblestone, 3, [4.5, Y, 4.5]).await;
    drop_item(&region, Item::Dirt, 1, [5.5, Y, 4.5]).await;
    let mut gathered = false;
    for _ in 0..600 {
        world.ticks(1).await;
        let nbt = entity_nbt(&region, allay).await.unwrap();
        if let Some(Tag::List(list)) = nbt.get("Inventory")
            && !list.is_empty()
        {
            gathered = true;
        }
        let delivered =
            std::iter::from_fn(|| events.try_recv().ok()).find_map(|event| match event {
                PlayerEvent::OfferItem { item, stack, .. } if item != dropped => Some(stack),
                _ => None,
            });
        if let Some(stack) = delivered {
            assert!(gathered, "it carried the items before handing them over");
            assert_eq!(stack.item, Item::Cobblestone);
            assert!(
                items_of(&region, Item::Cobblestone)
                    .await
                    .iter()
                    .all(|(id, ..)| *id != dropped),
                "the dropped stack was collected"
            );
            assert_eq!(
                items_of(&region, Item::Dirt).await.len(),
                1,
                "items unlike the one it holds are left alone"
            );
            world.shutdown().await;
            return;
        }
    }
    panic!("the allay never delivered anything (gathered: {gathered})");
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_allay_that_heard_a_note_block_drops_what_it_gathers_on_it() {
    let (_dir, world, region, player, _events) = start().await;
    region
        .request(player_at(&player, [7.5, Y, 13.5], PlayerLooks::default()))
        .await;
    let note = BlockPos::new(4, SKY + 1, 11);
    set(&region, note, Block::NoteBlock.default_state(), None).await;
    summon(
        &region,
        EntityType::Allay,
        holding(Item::Cobblestone),
        [8.5, Y + 1.0, 5.5],
    )
    .await;
    let allay = mobs(&region, EntityType::Allay).await[0].id;
    world.ticks(2).await;
    let mut tap = place(&player, note, Item::Stick, 1);
    tap.item = None;
    tap.held = None;
    region.request(RegionRequest::Place(tap)).await;
    world.ticks(1).await;
    let mut liked = None;
    for _ in 0..40 {
        world.ticks(1).await;
        let nbt = entity_nbt(&region, allay).await.unwrap();
        liked = memory(&memories(&nbt), "minecraft:liked_noteblock").cloned();
        if liked.is_some() {
            break;
        }
    }
    let liked = liked.expect("the vibration reached it");
    let liked = liked.as_compound().unwrap();
    assert_eq!(liked.get_str("dimension"), Some("minecraft:overworld"));
    assert_eq!(
        liked.get("pos").and_then(Tag::as_int_array),
        Some(&[note.x, note.y, note.z][..])
    );
    let dropped = drop_item(&region, Item::Cobblestone, 2, [11.5, Y, 6.5]).await;
    for _ in 0..600 {
        world.ticks(1).await;
        let on_top = items_of(&region, Item::Cobblestone)
            .await
            .into_iter()
            .any(|(id, at, _)| {
                id != dropped
                    && (at[0] - (note.x as f64 + 0.5)).abs() < 1.5
                    && (at[2] - (note.z as f64 + 0.5)).abs() < 1.5
                    && at[1] >= note.y as f64 + 1.0
            });
        if on_top {
            world.shutdown().await;
            return;
        }
    }
    panic!("nothing was dropped on the note block");
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dancing_allay_given_an_amethyst_shard_duplicates_once_per_cooldown() {
    let (_dir, world, region, player, mut events) = start().await;
    region
        .request(player_at(&player, [12.5, Y, 12.5], PlayerLooks::default()))
        .await;
    summon(
        &region,
        EntityType::Allay,
        holding(Item::Cobblestone),
        [7.5, Y + 1.0, 7.5],
    )
    .await;
    let allay = mobs(&region, EntityType::Allay).await[0].id;
    world.ticks(2).await;
    assert_eq!(
        click(
            &world,
            &region,
            &player,
            &mut events,
            allay,
            Item::AmethystShard
        )
        .await,
        None,
        "an allay that is not dancing does not duplicate"
    );
    assert_eq!(mobs(&region, EntityType::Allay).await.len(), 1);
    let disc = Compound::new()
        .with(
            "RecordItem",
            Compound::new()
                .with("id", "minecraft:music_disc_13")
                .with("count", 1i32),
        )
        .with("ticks_since_song_started", 0i64);
    let jukebox = BlockPos::new(5, SKY + 1, 5);
    set(
        &region,
        jukebox,
        Block::Jukebox
            .default_state()
            .with("has_record", "true")
            .unwrap(),
        Some(disc),
    )
    .await;
    let mut duplicated = false;
    for _ in 0..80 {
        world.ticks(5).await;
        if click(
            &world,
            &region,
            &player,
            &mut events,
            allay,
            Item::AmethystShard,
        )
        .await
            == Some(HandEffect::Consume(1))
        {
            duplicated = true;
            break;
        }
    }
    assert!(duplicated, "the dancing allay never took the shard");
    world.ticks(1).await;
    let allays = mobs(&region, EntityType::Allay).await;
    assert_eq!(allays.len(), 2, "a second allay appeared");
    for snapshot in &allays {
        let nbt = entity_nbt(&region, snapshot.id).await.unwrap();
        let cooldown = nbt.get_long("DuplicationCooldown").unwrap();
        assert!((5900..=6000).contains(&cooldown), "cooldown {cooldown}");
    }
    let twin = allays.iter().find(|s| s.id != allay).unwrap().id;
    let nbt = entity_nbt(&region, twin).await.unwrap();
    assert_eq!(nbt.get("PersistenceRequired"), Some(&Tag::Byte(1)));
    assert_eq!(
        click(
            &world,
            &region,
            &player,
            &mut events,
            allay,
            Item::AmethystShard
        )
        .await,
        None,
        "it cannot duplicate again until the cooldown ends"
    );
    assert_eq!(mobs(&region, EntityType::Allay).await.len(), 2);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn allay_memories_and_inventory_survive_saving_and_loading() {
    let (_dir, world, region, _player, _events) = start().await;
    let memories_in = Compound::new()
        .with(
            "minecraft:liked_player",
            Compound::new().with(
                "value",
                Tag::uuid(0x1234_5678_9abc_def0_1122_3344_5566_7788),
            ),
        )
        .with(
            "minecraft:liked_noteblock",
            Compound::new().with(
                "value",
                Compound::new()
                    .with("dimension", "minecraft:overworld")
                    .with("pos", Tag::IntArray(vec![3, SKY + 1, 4])),
            ),
        )
        .with(
            "minecraft:liked_noteblock_cooldown_ticks",
            Compound::new().with("value", 400i32),
        )
        .with(
            "minecraft:item_pickup_cooldown_ticks",
            Compound::new().with("value", 30i32),
        );
    let nbt = holding(Item::Cobblestone)
        .with("NoAI", true)
        .with("DuplicationCooldown", 1000i64)
        .with(
            "Inventory",
            Tag::List(vec![Tag::Compound(
                ItemStack::mint(Item::Cobblestone, 7, MintReason::Test).to_nbt(),
            )]),
        )
        .with("Brain", Compound::new().with("memories", memories_in));
    summon(&region, EntityType::Allay, nbt, [7.5, Y + 1.0, 7.5]).await;
    let allay = mobs(&region, EntityType::Allay).await[0].id;
    world.ticks(1).await;
    let saved = entity_nbt(&region, allay).await.unwrap();
    let memories = memories(&saved);
    assert_eq!(
        memory(&memories, "minecraft:liked_player").and_then(Tag::as_uuid),
        Some(0x1234_5678_9abc_def0_1122_3344_5566_7788)
    );
    let liked = memory(&memories, "minecraft:liked_noteblock")
        .and_then(Tag::as_compound)
        .unwrap();
    assert_eq!(liked.get_str("dimension"), Some("minecraft:overworld"));
    assert_eq!(
        liked.get("pos").and_then(Tag::as_int_array),
        Some(&[3, SKY + 1, 4][..])
    );
    assert_eq!(
        memory(&memories, "minecraft:liked_noteblock_cooldown_ticks"),
        Some(&Tag::Int(400)),
        "a mob without AI does not count its brain down"
    );
    assert_eq!(
        memory(&memories, "minecraft:item_pickup_cooldown_ticks"),
        Some(&Tag::Int(30))
    );
    let cooldown = saved.get_long("DuplicationCooldown").unwrap();
    assert!((990..1000).contains(&cooldown), "cooldown {cooldown}");
    let Some(Tag::List(inventory)) = saved.get("Inventory") else {
        panic!("no inventory saved: {saved:?}");
    };
    let kept =
        ItemStack::mint_from_nbt(inventory[0].as_compound().unwrap(), MintReason::Test).unwrap();
    assert_eq!((kept.item, kept.count()), (Item::Cobblestone, 7));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[test]
fn mansion_allays_load_as_live_allays() {
    let registry = tuff_test_support::vanilla_registries().entity_types.clone();
    let allay = tuff_feature_builtin_vanilla_worldgen::structure::structure_mob(
        "minecraft:allay",
        BlockPos::new(3, 70, -4),
        true,
    );
    let mut next = 0;
    let (mut live, raw) = registry.load_all(vec![allay], || {
        next += 1;
        next
    });
    assert!(raw.is_empty(), "an allay loads live, not raw");
    let allay = live[0]
        .downcast_mut::<tuff_feature_builtin_vanilla_mobs::Mob<tuff_feature_builtin_vanilla_mobs::allay::Allay>>()
        .expect("a live allay");
    assert_eq!(allay.core.base.position[1], 70.0);
}
