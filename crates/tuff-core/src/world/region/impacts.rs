//! Forwards projectile block hits to the region owning the block; the
//! features' region hooks decide the reaction.

use super::RegionActor;
use crate::world::entity::ProjectileImpact;
use crate::world::messages::RegionInternal;
use crate::world::raycast::Hit;
use crate::world::region_api::RegionTask;
use tuff_world_model::pos::ChunkPos;

impl RegionActor {
    /// A projectile struck the block at `hit.pos`.
    pub(super) fn projectile_hit_block(&mut self, hit: Hit, impact: &ProjectileImpact) {
        let pos = hit.pos;
        if !self.owns(pos) {
            if let Some(neighbour) = self.border.neighbors.get(&ChunkPos::of_block(pos).region()) {
                let impact = impact.clone();
                neighbour.internal(RegionInternal::Run(RegionTask::new(move |world| {
                    world.projectile_hit_block(hit, &impact);
                })));
            }
            return;
        }
        let Some(state) = self.block(pos) else {
            return;
        };
        let hooks = self.shared.region_hooks.clone();
        if hooks
            .iter()
            .any(|hook| hook.projectile_hit_block(self, hit, impact, state))
        {
            return;
        }
        if self.redstone_ready(pos) {
            self.block_behavior(state.block())
                .projectile_hit(self, pos, state);
        }
    }
}
