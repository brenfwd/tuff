//! Entity markers: while a page holds a viewport lease, each running
//! region inside it lists the entities within the viewed area. Regions
//! outside every viewport are never asked, and nothing is loaded.

use std::sync::Arc;

use serde_json::Value;
use tokio::sync::oneshot;
use tuff_core::world::Shared;
use tuff_core::world::messages::RegionRequest;
use tuff_core::world::region_api::RegionTask;
use tuff_item::text::TextComponent;
use tuff_vanilla_data::entity::MobCategory;
use tuff_vanilla_data::registry::EntityType;
use tuff_world_model::BlockPos;
use tuff_world_model::pos::{REGION_CHUNKS, RegionPos};

/// Markers in one event.
pub const MAX_MARKERS: usize = 512;
/// Regions asked per dimension and poll.
pub const MAX_REGIONS: usize = 16;

const REGION_BLOCKS: i32 = REGION_CHUNKS * 16;

/// A viewed block area (inclusive-exclusive corners).
#[derive(Clone, Copy, Debug)]
pub struct Area {
    pub min: [i32; 2],
    pub max: [i32; 2],
}

impl Area {
    fn contains(&self, x: f64, z: f64) -> bool {
        x >= f64::from(self.min[0])
            && x < f64::from(self.max[0])
            && z >= f64::from(self.min[1])
            && z < f64::from(self.max[1])
    }

    fn regions(&self) -> impl Iterator<Item = RegionPos> {
        let (x0, x1) = (
            self.min[0].div_euclid(REGION_BLOCKS),
            (self.max[0] - 1).div_euclid(REGION_BLOCKS),
        );
        let (z0, z1) = (
            self.min[1].div_euclid(REGION_BLOCKS),
            (self.max[1] - 1).div_euclid(REGION_BLOCKS),
        );
        (x0..=x1).flat_map(move |x| (z0..=z1).map(move |z| RegionPos::new(x, z)))
    }
}

fn group(kind: EntityType) -> &'static str {
    let name = kind.name().trim_start_matches("minecraft:");
    if kind == EntityType::Item {
        "item"
    } else if name.contains("boat") || name.contains("raft") || name.contains("minecart") {
        "vehicle"
    } else if kind.category() != MobCategory::Misc {
        "mob"
    } else {
        "other"
    }
}

/// Markers for the entities inside `areas` of dimension `dim` (the index
/// the map uses), as a JSON array, sorted for a stable comparison.
pub async fn collect(shared: &Arc<Shared>, dim: u16, areas: Vec<Area>) -> Vec<Value> {
    let Some(dimension) = shared.dimensions.iter().nth(usize::from(dim)).map(|d| d.id) else {
        return Vec::new();
    };
    let mut wanted: Vec<RegionPos> = areas.iter().flat_map(Area::regions).collect();
    wanted.sort_unstable_by_key(|r| (r.x, r.z));
    wanted.dedup();
    wanted.truncate(MAX_REGIONS);
    let handles = crate::gather::running(shared, dimension).await;
    let mut out = Vec::new();
    for handle in handles.iter().filter(|h| wanted.contains(&h.pos)) {
        let (reply, answer) = oneshot::channel();
        let areas = areas.clone();
        let task = RegionTask::new(move |world| {
            let mut rows = Vec::new();
            for id in world.entity_ids() {
                let Some(tracked) = world.entity(id) else {
                    continue;
                };
                let base = tracked.entity.base();
                let [x, y, z] = base.position;
                if !areas.iter().any(|a| a.contains(x, z)) {
                    continue;
                }
                let round = |v: f64| (v * 10.0).round() / 10.0;
                let name = base
                    .custom_name
                    .as_ref()
                    .and_then(TextComponent::from_nbt)
                    .map(|t| t.plain());
                rows.push(serde_json::json!({
                    "id": id,
                    "type": base.kind.name(),
                    "group": group(base.kind),
                    "x": round(x),
                    "y": round(y),
                    "z": round(z),
                    "name": name,
                }));
            }
            let _ = reply.send(rows);
        });
        let sent = handle
            .request(RegionRequest::WhenLoaded {
                min: BlockPos::new(16, 0, 16),
                max: BlockPos::new(0, 0, 0),
                run: task,
            })
            .await;
        if sent && let Ok(rows) = answer.await {
            out.extend(rows);
        }
    }
    out.sort_by_key(|v| v["id"].as_i64());
    out.truncate(MAX_MARKERS);
    out
}
