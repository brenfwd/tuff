//! Pipeline jobs run under the data pack tags in effect when handed out.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::watch;
use tuff_core::datapack::Contents;
use tuff_core::world::dimension::DimensionId;
use tuff_core::world::generator::{ChunkPipeline, Finished, PipelineContext};
use tuff_core::world::messages::{RegionHandle, RegionInternal};
use tuff_datapack::tags::{TagFeed, vanilla_with};
use tuff_vanilla_data::Block;
use tuff_vanilla_data::tag::block::key::LOGS;
use tuff_world_model::chunk::ChunkShape;
use tuff_world_model::pos::{ChunkPos, RegionPos};

use crate::chunk_generator::{StagedGenerator, StartsBlock};
use crate::pipeline::GenerationActor;
use crate::proto::ProtoChunk;

const SHAPE: ChunkShape = ChunkShape::new(0, 16);

/// Records, per step and chunk, whether stone counted as a log.
#[derive(Default)]
struct Probe(Mutex<HashMap<(&'static str, ChunkPos), bool>>);

impl Probe {
    fn note(&self, step: &'static str, pos: ChunkPos) {
        let seen = tuff_datapack::tags::is(LOGS, Block::Stone);
        self.0.lock().unwrap().insert((step, pos), seen);
    }

    fn seen(&self, step: &'static str, pos: ChunkPos) -> Option<bool> {
        self.0.lock().unwrap().get(&(step, pos)).copied()
    }
}

impl StagedGenerator for Probe {
    fn starts(&self, block: (i32, i32), _: ChunkShape) -> StartsBlock {
        self.note("starts", ChunkPos::new(block.0, block.1));
        StartsBlock {
            block,
            starts: Vec::new(),
        }
    }

    fn terrain(&self, pos: ChunkPos, shape: ChunkShape, _: &[Arc<StartsBlock>]) -> ProtoChunk {
        self.note("terrain", pos);
        let mut proto = ProtoChunk::new(pos, shape);
        proto.finish_terrain();
        proto
    }

    fn decorate(&self, center: ChunkPos, window: Vec<ProtoChunk>) -> Vec<ProtoChunk> {
        self.note("decorate", center);
        window
    }

    fn finish(&self, proto: ProtoChunk) -> Finished {
        self.note("finish", proto.pos);
        Finished::new(proto.into_chunk())
    }
}

async fn generated(
    inbox: &mut tokio::sync::mpsc::UnboundedReceiver<RegionInternal>,
    pos: ChunkPos,
) {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(20), inbox.recv())
            .await
            .expect("chunk arrives")
            .expect("pipeline alive");
        if matches!(message, RegionInternal::Generated { pos: p, .. } if p == pos) {
            return;
        }
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread")]
async fn jobs_see_the_newest_tags_when_handed_out() {
    let pack = vanilla_with(&[("minecraft:block", "minecraft:logs", "minecraft:stone")]);
    let (contents, feed) = watch::channel(Arc::new(Contents {
        tags: pack,
        ..Default::default()
    }));
    let probe = Arc::new(Probe::default());
    let (_clock_tx, clock) = watch::channel(0);
    let pipeline = GenerationActor::spawn_sharded(
        probe.clone(),
        PipelineContext {
            shape: SHAPE,
            clock,
            storage: None,
            tags: TagFeed::new(feed),
        },
        1,
    );
    let (region, _requests, mut inbox) =
        RegionHandle::new(DimensionId::default(), RegionPos::new(0, 0), 8);
    let first = ChunkPos::new(0, 0);
    assert!(pipeline.want(first, region.clone(), 0));
    generated(&mut inbox, first).await;
    for step in ["terrain", "decorate", "finish"] {
        assert_eq!(probe.seen(step, first), Some(true), "{step}");
    }
    assert_eq!(probe.seen("starts", first), Some(true));

    contents.send(Arc::new(Contents::default())).unwrap();
    let far = ChunkPos::new(64, 64);
    assert!(pipeline.want(far, region.clone(), 0));
    generated(&mut inbox, far).await;
    for step in ["terrain", "decorate", "finish"] {
        assert_eq!(probe.seen(step, far), Some(false), "{step}");
    }
}
