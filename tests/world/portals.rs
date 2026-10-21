//! Portals and dimension travel: lighting/breaking nether portals,
//! finding or building exit portals, and completing the end portal.

use super::*;
use tuff_feature_builtin_vanilla_dimensions::world::region::portals::PortalQuery;

/// An obsidian frame with a 2x3 opening whose bottom-left inner block is
/// `origin`, running east-west.
fn obsidian_frame(origin: BlockPos) -> Vec<(BlockPos, tuff_vanilla_data::BlockState)> {
    let mut blocks = Vec::new();
    for dx in -1..=2 {
        for dy in -1..=3 {
            let edge = dx == -1 || dx == 2 || dy == -1 || dy == 3;
            let corner = (dx == -1 || dx == 2) && (dy == -1 || dy == 3);
            if edge && !corner {
                blocks.push((
                    BlockPos::new(origin.x + dx, origin.y + dy, origin.z),
                    Block::Obsidian.default_state(),
                ));
            }
        }
    }
    blocks
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fire_appearing_in_an_empty_frame_lights_the_portal() {
    let dir = TestDir::new();
    let (player, mut events) = PlayerHandle::new(100);
    let pos = ChunkPos::new(0, 0);
    let height = Generator::new(1).height(8, 8);
    let origin = BlockPos::new(8, height + 1, 8);
    let world = TestWorld::start(dir.path());
    let region = world.region(pos.region()).await;
    subscribe(&region, &player, &mut events, &[pos]).await;
    place_blocks(&world, &region, obsidian_frame(origin)).await;
    place_blocks(&world, &region, [(origin, Block::Fire.default_state())]).await;
    for dx in 0..2 {
        for dy in 0..3 {
            let state = block_at(
                &region,
                BlockPos::new(origin.x + dx, origin.y + dy, origin.z),
            )
            .await
            .unwrap();
            assert_eq!(state.block(), Block::NetherPortal);
        }
    }
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flint_and_steel_lights_a_portal_that_breaks_with_its_frame() {
    use tuff_feature_builtin_vanilla_dimensions::world::portal::{PortalContact, PortalKind};

    let dir = TestDir::new();
    let (player, mut events) = PlayerHandle::new(100);
    let pos = ChunkPos::new(0, 0);
    let height = Generator::new(1).height(8, 8);
    let origin = BlockPos::new(8, height + 1, 8);
    let world = TestWorld::start(dir.path());
    let region = world.region(pos.region()).await;
    subscribe(&region, &player, &mut events, &[pos]).await;
    place_blocks(&world, &region, obsidian_frame(origin)).await;
    assert_eq!(
        block_at(&region, BlockPos::new(7, origin.y, 8))
            .await
            .unwrap()
            .block(),
        Block::Obsidian
    );

    let mut light = place(
        &player,
        BlockPos::new(origin.x, origin.y - 1, origin.z),
        Item::FlintAndSteel,
        1,
    );
    light.item = None;
    region.request(RegionRequest::Place(light)).await;
    let outcome = expect(&mut events, |event| match event {
        PlayerEvent::ActionResult {
            sequence: 1,
            outcome,
        } => Some(outcome),
        _ => None,
    })
    .await;
    assert!(
        matches!(
            outcome,
            ActionOutcome::Placed {
                item: Item::FlintAndSteel,
                ..
            }
        ),
        "{outcome:?}"
    );
    for dx in 0..2 {
        for dy in 0..3 {
            let state = block_at(
                &region,
                BlockPos::new(origin.x + dx, origin.y + dy, origin.z),
            )
            .await
            .unwrap();
            assert_eq!(state.block(), Block::NetherPortal);
            assert_eq!(state.get("axis"), Some("x"));
        }
    }

    let inside = [
        origin.x as f64 + 1.5,
        origin.y as f64,
        origin.z as f64 + 0.5,
    ];
    region.request(update(&player, 5, inside)).await;
    let contact = expect(&mut events, |event| match event {
        PlayerEvent::Surroundings { seq: 5, portal, .. } => {
            Some(portal.and_then(PortalContact::of))
        }
        _ => None,
    })
    .await;
    assert_eq!(
        contact,
        Some(PortalContact {
            kind: PortalKind::Nether,
            pos: BlockPos::new(origin.x + 1, origin.y, origin.z)
        })
    );
    let (reply, answer) = oneshot::channel();
    region
        .request(PortalQuery::Shape { pos: origin, reply }.request())
        .await;
    let rectangle = answer.await.unwrap().expect("portal rectangle");
    assert_eq!((rectangle.width, rectangle.height), (2, 3));
    assert_eq!(rectangle.min, origin);

    region
        .request(RegionRequest::Break {
            player: player.clone(),
            pos: BlockPos::new(origin.x - 1, origin.y + 1, origin.z),
            tool: None,
            mode: BreakMode::Creative,
            sequence: 2,
        })
        .await;
    expect(&mut events, |event| match event {
        PlayerEvent::ActionResult {
            sequence: 2,
            outcome: ActionOutcome::Broken { .. },
        } => Some(()),
        _ => None,
    })
    .await;
    assert_eq!(block_at(&region, origin).await.unwrap().block(), Block::Air);
    assert_eq!(
        block_at(&region, BlockPos::new(origin.x + 1, origin.y + 2, origin.z))
            .await
            .unwrap()
            .block(),
        Block::Air
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn nether_regions_find_and_build_exit_portals() {
    use tuff_feature_builtin_vanilla_dimensions::world::portal::Axis;

    let dir = TestDir::new();
    let (player, mut events) = PlayerHandle::new(100);
    let world = TestWorld::start(dir.path());
    let origin = BlockPos::new(40, 60, 40);
    let region = world
        .region_in(THE_NETHER, ChunkPos::of_block(origin).region())
        .await;
    subscribe(&region, &player, &mut events, &[ChunkPos::of_block(origin)]).await;

    let (reply, answer) = oneshot::channel();
    region
        .request(
            PortalQuery::Find {
                origin,
                radius: 16,
                reply,
            }
            .request(),
        )
        .await;
    assert!(answer.await.unwrap().is_empty());

    let (reply, answer) = oneshot::channel();
    region
        .request(
            PortalQuery::Create {
                origin,
                axis: Axis::Z,
                reply,
            }
            .request(),
        )
        .await;
    let rectangle = answer.await.unwrap().expect("a portal was built");
    assert_eq!(
        (rectangle.axis, rectangle.width, rectangle.height),
        (Axis::Z, 2, 3)
    );
    assert!((rectangle.min.x - origin.x).abs() <= 17 && (rectangle.min.z - origin.z).abs() <= 17);
    assert!(rectangle.min.y + 4 < 128, "{rectangle:?}");
    let portal = block_at(&region, rectangle.min).await.unwrap();
    assert_eq!(portal, Axis::Z.portal_state());
    assert_eq!(
        block_at(
            &region,
            BlockPos::new(rectangle.min.x, rectangle.min.y - 1, rectangle.min.z)
        )
        .await
        .unwrap()
        .block(),
        Block::Obsidian
    );

    let (reply, answer) = oneshot::channel();
    region
        .request(
            PortalQuery::Find {
                origin,
                radius: 16,
                reply,
            }
            .request(),
        )
        .await;
    let found = answer.await.unwrap();
    assert_eq!(found.len(), 6);
    assert!(found.contains(&rectangle.min));
    let (reply, answer) = oneshot::channel();
    region
        .request(
            PortalQuery::Shape {
                pos: rectangle.min,
                reply,
            }
            .request(),
        )
        .await;
    assert_eq!(answer.await.unwrap(), Some(rectangle));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn eyes_of_ender_complete_the_end_portal_and_the_end_has_a_platform() {
    let dir = TestDir::new();
    let (player, mut events) = PlayerHandle::new(100);
    let pos = ChunkPos::new(0, 0);
    let height = Generator::new(1).height(8, 8);
    let center = BlockPos::new(8, height + 1, 8);
    let world = TestWorld::start(dir.path());
    let region = world.region(pos.region()).await;
    subscribe(&region, &player, &mut events, &[pos]).await;

    let frame = |facing: &str| {
        Block::EndPortalFrame
            .default_state()
            .with("facing", facing)
            .unwrap()
    };
    let mut frames = Vec::new();
    for i in -1..=1 {
        frames.push((
            BlockPos::new(center.x + i, center.y, center.z - 2),
            frame("south"),
        ));
        frames.push((
            BlockPos::new(center.x + i, center.y, center.z + 2),
            frame("north"),
        ));
        frames.push((
            BlockPos::new(center.x - 2, center.y, center.z + i),
            frame("east"),
        ));
        frames.push((
            BlockPos::new(center.x + 2, center.y, center.z + i),
            frame("west"),
        ));
    }
    place_blocks(&world, &region, frames.clone()).await;
    for (sequence, (at, _)) in frames.iter().enumerate() {
        let sequence = sequence as i32 + 1;
        let mut eye = place(&player, *at, Item::EnderEye, sequence);
        eye.item = None;
        region.request(RegionRequest::Place(eye)).await;
        let outcome = expect(&mut events, |event| match event {
            PlayerEvent::ActionResult {
                sequence: at,
                outcome,
            } if at == sequence => Some(outcome),
            _ => None,
        })
        .await;
        assert!(
            matches!(
                outcome,
                ActionOutcome::Placed {
                    item: Item::EnderEye,
                    ..
                }
            ),
            "{outcome:?}"
        );
        let filled = block_at(&region, *at).await.unwrap();
        assert_eq!(filled.get("eye"), Some("true"));
        let hole = block_at(&region, center).await.unwrap().block();
        if sequence < 12 {
            assert_ne!(hole, Block::EndPortal);
        } else {
            assert_eq!(hole, Block::EndPortal);
        }
    }
    for dx in -1..=1 {
        for dz in -1..=1 {
            let at = BlockPos::new(center.x + dx, center.y, center.z + dz);
            assert_eq!(
                block_at(&region, at).await.unwrap().block(),
                Block::EndPortal
            );
        }
    }

    let platform = BlockPos::new(100, 49, 0);
    let end = world
        .region_in(
            crate::storage::level::THE_END,
            ChunkPos::of_block(platform).region(),
        )
        .await;
    subscribe(&end, &player, &mut events, &[ChunkPos::of_block(platform)]).await;
    place_blocks(&world, &end, [(platform, Block::Stone.default_state())]).await;
    assert_eq!(
        block_at(&end, platform).await.unwrap().block(),
        Block::Stone
    );
    let (reply, answer) = oneshot::channel();
    end.request(
        PortalQuery::EndPlatform {
            origin: platform,
            reply,
        }
        .request(),
    )
    .await;
    answer.await.unwrap();
    assert_eq!(block_at(&end, platform).await.unwrap().block(), Block::Air);
    assert_eq!(
        block_at(&end, BlockPos::new(100, 48, 0))
            .await
            .unwrap()
            .block(),
        Block::Obsidian
    );
    world.shutdown().await;
}

/// An exit portal built at a region border: the spot is chosen from both
/// regions' blocks and each region writes its own part, so the frame and
/// all six portal blocks stand whichever side they fall on.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_exit_portal_built_across_a_region_border_is_whole() {
    use tuff_feature_builtin_vanilla_dimensions::world::travel::{Borrowed, nether_exit};

    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let overworld = world.dimension(OVERWORLD);
    let nether = world.dimension(THE_NETHER);
    let mut regions = Borrowed::new(world.shared.clone());
    let position = [512.0 * 8.0 + 0.5, 70.0, 40.0 * 8.0 + 0.5];
    let entry = BlockPos::containing(position);
    let exit = nether_exit(
        &mut regions,
        &world.shared,
        overworld,
        entry,
        position,
        [0.6, 1.8],
        true,
    )
    .await
    .expect("an exit was built");
    assert_eq!(exit.dimension, nether);
    let mut portal_blocks = Vec::new();
    for x in [0, 1] {
        let region = world.region_in(THE_NETHER, RegionPos::new(x, 0)).await;
        let (reply, answer) = oneshot::channel();
        region
            .request(
                PortalQuery::Find {
                    origin: BlockPos::new(512, 64, 40),
                    radius: 24,
                    reply,
                }
                .request(),
            )
            .await;
        portal_blocks.extend(answer.await.unwrap());
    }
    assert_eq!(portal_blocks.len(), 6, "{portal_blocks:?}");
    let arrival = BlockPos::containing(exit.position);
    assert!(
        portal_blocks
            .iter()
            .any(|p| p.x == arrival.x && p.z == arrival.z),
        "{:?} arrives in {portal_blocks:?}",
        exit.position
    );
    world.ticks(5).await;
    let mut standing = 0;
    for pos in &portal_blocks {
        let region = world
            .region_in(THE_NETHER, ChunkPos::of_block(*pos).region())
            .await;
        if block_at(&region, *pos).await.map(|s| s.block()) == Some(Block::NetherPortal) {
            standing += 1;
        }
    }
    assert_eq!(standing, 6, "the portal survives its frame's updates");
    regions.release().await;
    world.shutdown().await;
}
