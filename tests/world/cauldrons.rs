//! Cauldrons filled and emptied by buckets and bottles, washing banners,
//! shulker boxes and dyed armor, and lit candles put out by an empty hand.

use super::*;
use crate::world::messages::{BlockOp, HandEffect};
use tuff_nbt::Tag;
use tuff_vanilla_data::BlockState;
use tuff_vanilla_data::registry::Potion;

const Y: i32 = 200;
const X: i32 = 96;
const Z: i32 = 96;

type Events = mpsc::UnboundedReceiver<PlayerEvent>;

async fn start() -> (TestDir, TestWorld, RegionHandle, PlayerHandle, Events) {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (player, mut events) = PlayerHandle::new(100);
    let center = ChunkPos::new(6, 6);
    view_around(&world, &player, &mut events, center).await;
    let region = world.region(center.region()).await;
    (dir, world, region, player, events)
}

async fn set(region: &RegionHandle, pos: BlockPos, state: BlockState) {
    blocks(
        region,
        BlockOp::Set {
            changes: vec![(pos, state)],
            destroy: false,
            keep: false,
            strict: false,
            quiet: false,
            data: None,
        },
    )
    .await;
}

async fn click(region: &RegionHandle, events: &mut Events, mut place: Place) -> ActionOutcome {
    let sequence = place.sequence;
    place.item = None;
    region.request(RegionRequest::Place(place)).await;
    expect(events, |event| match event {
        PlayerEvent::ActionResult {
            sequence: seq,
            outcome,
        } if seq == sequence => Some(outcome),
        _ => None,
    })
    .await
}

fn pos() -> BlockPos {
    BlockPos::new(X, Y, Z)
}

fn water(level: u8) -> BlockState {
    Block::WaterCauldron
        .default_state()
        .with_level(level)
        .unwrap()
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn buckets_fill_and_empty_cauldrons() {
    let (_dir, world, region, player, mut events) = start().await;
    set(&region, pos(), Block::Cauldron.default_state()).await;

    let outcome = click(
        &region,
        &mut events,
        place(&player, pos(), Item::WaterBucket, 1),
    )
    .await;
    assert!(
        matches!(
            outcome,
            ActionOutcome::Used {
                item: Item::WaterBucket,
                effect: HandEffect::Emptied(Item::Bucket),
                ..
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(block_at(&region, pos()).await, Some(water(3)));

    let outcome = click(&region, &mut events, place(&player, pos(), Item::Bucket, 2)).await;
    assert!(
        matches!(
            outcome,
            ActionOutcome::Used {
                effect: HandEffect::Exchange(Item::WaterBucket),
                ..
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(
        block_at(&region, pos()).await,
        Some(Block::Cauldron.default_state())
    );

    set(&region, pos(), water(2)).await;
    click(&region, &mut events, place(&player, pos(), Item::Bucket, 3)).await;
    assert_eq!(block_at(&region, pos()).await, Some(water(2)));

    click(
        &region,
        &mut events,
        place(&player, pos(), Item::LavaBucket, 4),
    )
    .await;
    assert_eq!(
        block_at(&region, pos()).await,
        Some(Block::LavaCauldron.default_state())
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bottles_move_water_a_level_at_a_time() {
    let (_dir, world, region, player, mut events) = start().await;
    set(&region, pos(), water(2)).await;

    let outcome = click(
        &region,
        &mut events,
        place(&player, pos(), Item::GlassBottle, 1),
    )
    .await;
    let ActionOutcome::Used {
        effect: HandEffect::ExchangeStack(stack),
        ..
    } = outcome
    else {
        panic!("{outcome:?}");
    };
    assert_eq!(stack.item, Item::Potion);
    assert_eq!(
        stack.components.potion_contents.as_ref().unwrap().potion,
        Some(Potion::Water)
    );
    assert_eq!(block_at(&region, pos()).await, Some(water(1)));

    click(
        &region,
        &mut events,
        place(&player, pos(), Item::GlassBottle, 2),
    )
    .await;
    assert_eq!(
        block_at(&region, pos()).await,
        Some(Block::Cauldron.default_state())
    );

    let mut bottle = place(&player, pos(), Item::Potion, 3);
    bottle.components.potion_contents = Some(Box::new(tuff_item::item::PotionContents {
        potion: Some(Potion::Water),
        ..Default::default()
    }));
    let outcome = click(&region, &mut events, bottle.clone()).await;
    assert!(
        matches!(
            outcome,
            ActionOutcome::Used {
                effect: HandEffect::Exchange(Item::GlassBottle),
                ..
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(block_at(&region, pos()).await, Some(water(1)));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn water_washes_banners_shulker_boxes_and_dyed_armor() {
    let (_dir, world, region, player, mut events) = start().await;
    set(&region, pos(), water(3)).await;

    let mut banner = place(&player, pos(), Item::WhiteBanner, 1);
    banner.components = tuff_item::item::Components::from_nbt(&Compound::new().with(
        "minecraft:banner_patterns",
        Tag::List(vec![
            Tag::Compound(
                Compound::new()
                    .with("pattern", "minecraft:stripe_top")
                    .with("color", "red"),
            ),
            Tag::Compound(
                Compound::new()
                    .with("pattern", "minecraft:border")
                    .with("color", "blue"),
            ),
        ]),
    ));
    let outcome = click(&region, &mut events, banner).await;
    let ActionOutcome::Used {
        effect: HandEffect::ExchangeStack(stack),
        ..
    } = outcome
    else {
        panic!("{outcome:?}");
    };
    assert_eq!(stack.components.banner_patterns.as_ref().unwrap().len(), 1);
    assert_eq!(block_at(&region, pos()).await, Some(water(2)));

    let plain = place(&player, pos(), Item::WhiteBanner, 2);
    click(&region, &mut events, plain).await;
    assert_eq!(block_at(&region, pos()).await, Some(water(2)));

    let outcome = click(
        &region,
        &mut events,
        place(&player, pos(), Item::RedShulkerBox, 3),
    )
    .await;
    let ActionOutcome::Used {
        effect: HandEffect::ExchangeStack(stack),
        ..
    } = outcome
    else {
        panic!("{outcome:?}");
    };
    assert_eq!(stack.item, Item::ShulkerBox);
    assert_eq!(block_at(&region, pos()).await, Some(water(1)));

    set(&region, pos(), water(3)).await;
    let mut boots = place(&player, pos(), Item::LeatherBoots, 4);
    boots.components.dyed_color = Some(0xff0000);
    let outcome = click(&region, &mut events, boots).await;
    let ActionOutcome::Used {
        effect: HandEffect::Replace(Some(stack)),
        ..
    } = outcome
    else {
        panic!("{outcome:?}");
    };
    assert_eq!(stack.components.dyed_color, None);
    assert_eq!(block_at(&region, pos()).await, Some(water(2)));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_empty_hand_puts_out_lit_candles() {
    let (_dir, world, region, player, mut events) = start().await;
    let lit = Block::Candle.default_state().with_lit(true).unwrap();
    set(&region, pos(), lit).await;
    let mut hand = place(&player, pos(), Item::Stick, 1);
    hand.held = None;
    let outcome = click(&region, &mut events, hand).await;
    assert!(
        matches!(outcome, ActionOutcome::Interacted { .. }),
        "{outcome:?}"
    );
    assert_eq!(
        block_at(&region, pos()).await,
        Some(Block::Candle.default_state().with_lit(false).unwrap())
    );

    let cake = Block::CandleCake.default_state().with_lit(true).unwrap();
    set(&region, pos(), cake).await;
    let mut low = place(&player, pos(), Item::Stick, 2);
    low.held = None;
    low.cursor = [0.5, 0.25, 0.5];
    click(&region, &mut events, low).await;
    assert_eq!(
        block_at(&region, pos()).await.map(|s| s.block()),
        Some(Block::Cake)
    );
    set(&region, pos(), cake).await;
    let mut high = place(&player, pos(), Item::Stick, 3);
    high.held = None;
    high.cursor = [0.5, 0.75, 0.5];
    click(&region, &mut events, high).await;
    assert_eq!(
        block_at(&region, pos()).await,
        Some(Block::CandleCake.default_state().with_lit(false).unwrap())
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn clicking_a_dragon_egg_teleports_it_onto_nearby_ground() {
    let (_dir, world, region, player, mut events) = start().await;
    let floor: Vec<(BlockPos, BlockState)> = (-20..=20)
        .flat_map(|dx| (-20..=20).map(move |dz| BlockPos::new(X + dx, Y - 1, Z + dz)))
        .map(|at| (at, Block::Stone.default_state()))
        .collect();
    blocks(
        &region,
        BlockOp::Set {
            changes: floor,
            destroy: false,
            keep: false,
            strict: false,
            quiet: false,
            data: None,
        },
    )
    .await;
    set(&region, pos(), Block::DragonEgg.default_state()).await;

    let outcome = click(&region, &mut events, place(&player, pos(), Item::Stick, 1)).await;
    assert!(
        matches!(outcome, ActionOutcome::Interacted { .. }),
        "{outcome:?}"
    );
    assert_eq!(
        block_at(&region, pos()).await,
        Some(Block::Air.default_state())
    );
    let mut found = Vec::new();
    for dx in -15..=15 {
        for dz in -15..=15 {
            let at = BlockPos::new(X + dx, Y, Z + dz);
            if block_at(&region, at).await == Some(Block::DragonEgg.default_state()) {
                found.push(at);
            }
        }
    }
    assert_eq!(found.len(), 1, "{found:?}");
    drop(world);
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_candle_turns_an_uncut_cake_into_a_candle_cake_but_not_a_bitten_one() {
    let (_dir, world, region, player, mut events) = start().await;
    set(&region, pos(), Block::Cake.default_state()).await;
    let outcome = click(
        &region,
        &mut events,
        place(&player, pos(), Item::RedCandle, 1),
    )
    .await;
    assert!(
        matches!(outcome, ActionOutcome::Placed { .. }),
        "{outcome:?}"
    );
    assert_eq!(
        block_at(&region, pos()).await,
        Some(Block::RedCandleCake.default_state())
    );
    let bitten = Block::Cake.default_state().with("bites", "1").unwrap();
    set(&region, pos(), bitten).await;
    click(
        &region,
        &mut events,
        place(&player, pos(), Item::RedCandle, 2),
    )
    .await;
    assert_eq!(
        block_at(&region, pos()).await.map(|s| s.block()),
        Some(Block::Cake)
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_water_bottle_turns_dirt_to_mud_unless_clicked_from_below() {
    let (_dir, world, region, player, mut events) = start().await;
    set(&region, pos(), Block::Dirt.default_state()).await;
    let mut bottle = place(&player, pos(), Item::Potion, 1);
    bottle.components.potion_contents = Some(Box::new(tuff_item::item::PotionContents {
        potion: Some(Potion::Water),
        ..Default::default()
    }));

    bottle.face = tuff_world_model::Face::Down;
    click(&region, &mut events, bottle.clone()).await;
    assert_eq!(
        block_at(&region, pos()).await,
        Some(Block::Dirt.default_state())
    );

    bottle.face = tuff_world_model::Face::Up;
    bottle.sequence = 2;
    let outcome = click(&region, &mut events, bottle).await;
    assert!(
        matches!(
            outcome,
            ActionOutcome::Used {
                effect: HandEffect::Exchange(Item::GlassBottle),
                ..
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(
        block_at(&region, pos()).await,
        Some(Block::Mud.default_state())
    );
    world.shutdown().await;
}
