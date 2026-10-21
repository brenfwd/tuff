//! Switching vanilla dimensions off and picking the spawn dimension:
//! a world with only a plugin dimension spawns players there, and portals
//! into a switched-off dimension do nothing.

use super::*;
use crate::feature::{Feature, LoadContext};
use crate::world::custom_dimension::{DimensionDefinition, DimensionProvider};
use crate::world::dimension::THE_END;
use crate::world::generator::ChunkGenerator;
use tuff_item::load::{LoadError, Phase};
use tuff_vanilla_data::registry::EntityType;
use tuff_world_model::biomes::Biomes;
use tuff_world_model::chunk::Chunk;

pub(crate) const LOBBY: &str = "test:lobby";
pub(crate) const FLOOR: i32 = 40;

const ALL_OFF: [(&str, &str); 3] = [
    ("tuff.world.dimensions.overworld.enabled", "false"),
    ("tuff.world.dimensions.nether.enabled", "false"),
    ("tuff.world.dimensions.end.enabled", "false"),
];

/// A stone floor; dirt with a seed given, sand in a forced desert.
struct Floor(Block);

impl ChunkGenerator for Floor {
    fn variant(&self, seed: Option<i64>, biome: Option<&str>) -> Option<Arc<dyn ChunkGenerator>> {
        let block = match (biome, seed) {
            (Some("minecraft:desert"), _) => Block::Sand,
            (Some(_), _) => return None,
            (None, Some(_)) => Block::Dirt,
            (None, None) => self.0,
        };
        Some(Arc::new(Floor(block)))
    }

    fn generate(&self, pos: ChunkPos, shape: ChunkShape) -> Chunk {
        placeholder::Layered {
            layers: vec![(FLOOR, self.0.default_state())],
            biome: "minecraft:plains",
            sea_level: 0,
        }
        .generate(pos, shape)
    }

    fn sea_level(&self) -> i32 {
        0
    }

    fn spawn(&self) -> BlockPos {
        BlockPos::new(3, FLOOR + 1, -5)
    }
}

/// Adds [`LOBBY`], claiming the spawn when `.0` says so.
pub(crate) struct Lobby(pub(crate) bool);

impl DimensionProvider for Lobby {
    fn dimensions(&self) -> Vec<DimensionDefinition> {
        vec![DimensionDefinition {
            key: LOBBY.into(),
            dimension_type: OVERWORLD.into(),
            type_json: String::new(),
            biomes: Vec::new(),
            natural_spawning: false,
            claims_spawn: self.0,
        }]
    }

    fn generator(&self, _: &str, _: i64, _: &Arc<Biomes>) -> Arc<dyn ChunkGenerator> {
        Arc::new(Floor(Block::Stone))
    }
}

impl Feature for Lobby {
    fn id(&self) -> &'static str {
        "test-lobby"
    }

    fn load(&self, cx: &mut LoadContext) -> Result<(), LoadError> {
        if cx.phase() == Phase::Content {
            cx.registries()
                .dimension_providers
                .push(Arc::new(Lobby(self.0)));
        }
        Ok(())
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_world_of_one_plugin_dimension_spawns_players_there() {
    let dir = TestDir::new();
    let world = TestWorld::start_with_settings(dir.path(), vec![Box::new(Lobby(true))], &ALL_OFF);
    assert_eq!(world.shared.dimensions.keys(), [LOBBY]);
    let lobby = world.dimension(LOBBY);
    assert_eq!(world.shared.spawn_dimension(), lobby);
    assert_eq!(world.shared.dimensions.default_id(), lobby);
    assert_eq!(world.shared.spawn().pos, BlockPos::new(3, FLOOR + 1, -5));

    let (mut session, _outbound) = session_with_outbound(&world, 1000);
    session.load().await.unwrap();
    assert_eq!(session.dimension, lobby);
    assert_eq!(session.pose.position[1], f64::from(FLOOR + 1));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_operator_spawn_dimension_beats_a_plugin_claim() {
    let dir = TestDir::new();
    let world = TestWorld::start_with_settings(
        dir.path(),
        vec![Box::new(Lobby(true))],
        &[("tuff.world.spawnDimension", "minecraft:the_nether")],
    );
    assert_eq!(world.shared.dimensions.iter().len(), 4);
    assert_eq!(world.shared.spawn().dimension, THE_NETHER);
    assert_eq!(world.shared.spawn_dimension(), world.dimension(THE_NETHER));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fire_in_a_frame_lights_no_portal_without_the_nether() {
    let dir = TestDir::new();
    let world = TestWorld::start_with_settings(
        dir.path(),
        Vec::new(),
        &[("tuff.world.dimensions.nether.enabled", "false")],
    );
    assert!(world.shared.dimensions.by_key(THE_NETHER).is_none());
    let (player, mut events) = PlayerHandle::new(100);
    let pos = ChunkPos::new(0, 0);
    let origin = BlockPos::new(8, 100, 8);
    let region = world.region(pos.region()).await;
    subscribe(&region, &player, &mut events, &[pos]).await;
    let mut frame = Vec::new();
    for dx in -1..=2 {
        for dy in -1..=3 {
            let edge = dx == -1 || dx == 2 || dy == -1 || dy == 3;
            let corner = (dx == -1 || dx == 2) && (dy == -1 || dy == 3);
            if edge && !corner {
                frame.push((
                    BlockPos::new(origin.x + dx, origin.y + dy, origin.z),
                    Block::Obsidian.default_state(),
                ));
            }
        }
    }
    place_blocks(&world, &region, frame).await;
    place_blocks(&world, &region, [(origin, Block::Fire.default_state())]).await;
    world.ticks(2).await;
    let above = block_at(&region, BlockPos::new(origin.x, origin.y + 1, origin.z))
        .await
        .unwrap();
    assert_eq!(above.block(), Block::Air);
    assert_ne!(
        block_at(&region, origin).await.unwrap().block(),
        Block::NetherPortal
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_item_in_an_end_portal_stays_without_the_end() {
    let dir = TestDir::new();
    let world = TestWorld::start_with_settings(
        dir.path(),
        Vec::new(),
        &[("tuff.world.dimensions.end.enabled", "false")],
    );
    assert!(world.shared.dimensions.by_key(THE_END).is_none());
    let pos = ChunkPos::new(0, 0);
    let region = world.region(pos.region()).await;
    let (player, mut events) = PlayerHandle::new(101);
    subscribe(&region, &player, &mut events, &[pos]).await;
    let portal = BlockPos::new(8, 200, 8);
    place_blocks(
        &world,
        &region,
        [(portal, Block::EndPortal.default_state())],
    )
    .await;
    let item = tuff_nbt::Compound::new()
        .with("id", "minecraft:item")
        .with(
            "Item",
            tuff_nbt::Compound::new()
                .with("id", "minecraft:stone")
                .with("count", 1),
        )
        .with("NoGravity", true);
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::Summon {
            nbt: item,
            position: [8.5, 200.5, 8.5],
            fresh: false,
            reply,
        })
        .await;
    assert!(answer.await.unwrap());
    world.ticks(40).await;
    let (reply, answer) = oneshot::channel();
    region.request(RegionRequest::Entities { reply }).await;
    let items: Vec<_> = answer
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == EntityType::Item)
        .collect();
    assert_eq!(items.len(), 1, "the item is still in the overworld");
    world.shutdown().await;
}
