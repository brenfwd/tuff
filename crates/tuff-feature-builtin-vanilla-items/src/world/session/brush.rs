//! Brushing: a brush used on a block strokes whatever the player looks at
//! every ten ticks, uncovering suspicious sand and gravel.

use tokio::sync::oneshot;
use tuff_core::protocol::play;
use tuff_core::world::messages::{PlayerHandle, RegionInternal, RegionRequest};
use tuff_core::world::player::UsingItem;
use tuff_core::world::raycast::{self, FluidMode};
use tuff_core::world::region_api::{RegionTask, RegionWorld};
use tuff_core::world::session::PlayerSession;
use tuff_vanilla_common::brushable;
use tuff_vanilla_data::Block;
use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::registry::Attribute;
use tuff_world_model::BlockPos;
use tuff_world_model::pos::ChunkPos;

/// What one brush stroke found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stroke {
    Brushed,
    /// The stroke finished uncovering a block, which wears the brush.
    Completed,
    /// The player no longer looks at a block, so brushing stops.
    Missed,
}

#[derive(Default)]
struct PendingStroke(Option<(bool, oneshot::Receiver<Stroke>)>);

/// A stroke from `eye` along `look` within `reach`, run on the region
/// around the eye and forwarded to the owner of the block it hits.
pub fn stroke_request(
    eye: [f64; 3],
    look: [f64; 3],
    reach: f64,
    player: PlayerHandle,
    reply: oneshot::Sender<Stroke>,
) -> RegionRequest {
    let at = BlockPos::containing(eye);
    RegionRequest::WhenLoaded {
        min: at,
        max: at,
        run: RegionTask::new(move |world| stroke(world, eye, look, reach, player, reply)),
    }
}

fn stroke(
    world: &mut dyn RegionWorld,
    eye: [f64; 3],
    look: [f64; 3],
    reach: f64,
    player: PlayerHandle,
    reply: oneshot::Sender<Stroke>,
) {
    let to = [0, 1, 2].map(|i| eye[i] + look[i] * reach);
    let hit = {
        let level = |pos: BlockPos| world.block(pos).unwrap_or(Block::Air.default_state());
        raycast::cast(eye, to, FluidMode::None, &level)
    };
    let Some(hit) = hit else {
        let _ = reply.send(Stroke::Missed);
        return;
    };
    if world.owns(hit.pos) {
        let _ = reply.send(brush_at(world, hit.pos, hit.face, &player));
        return;
    }
    let Some(owner) = world.neighbor(ChunkPos::of_block(hit.pos).region()) else {
        let _ = reply.send(Stroke::Brushed);
        return;
    };
    owner.internal(RegionInternal::Run(RegionTask::new(move |world| {
        let _ = reply.send(brush_at(world, hit.pos, hit.face, &player));
    })));
}

fn brush_at(
    world: &mut dyn RegionWorld,
    pos: BlockPos,
    face: Facing,
    player: &PlayerHandle,
) -> Stroke {
    if brushable::brush(world, pos, face, player) {
        Stroke::Completed
    } else {
        Stroke::Brushed
    }
}

#[expect(
    async_fn_in_trait,
    reason = "crate-internal trait whose futures are awaited in place"
)]
pub trait BrushExt {
    /// One tick of brushing: applies the last stroke's answer, strokes on
    /// the fifth tick of every ten, and ends with the use duration.
    async fn tick_brush(&mut self, using: UsingItem);
}

impl BrushExt for PlayerSession {
    async fn tick_brush(&mut self, mut using: UsingItem) {
        let answer = match &mut self.ext.entry::<PendingStroke>().0 {
            Some((off_hand, rx)) => match rx.try_recv() {
                Ok(stroke) => Some((*off_hand, Some(stroke))),
                Err(oneshot::error::TryRecvError::Closed) => Some((*off_hand, None)),
                Err(oneshot::error::TryRecvError::Empty) => None,
            },
            None => None,
        };
        if let Some((off_hand, stroke)) = answer {
            self.ext.entry::<PendingStroke>().0 = None;
            match stroke {
                Some(Stroke::Completed) => {
                    self.hurt_hand(off_hand, 1).await;
                    self.send_inventory();
                }
                Some(Stroke::Missed) => {
                    self.stop_using();
                    self.publish(false).await;
                    return;
                }
                _ => {}
            }
        }
        if brushable::is_stroke(using.total, using.remaining) {
            let (reply, rx) = oneshot::channel();
            let eye = self.eye();
            let look = raycast::view_vector(self.pose.yaw, self.pose.pitch);
            let reach = self.attribute(Attribute::BlockInteractionRange);
            if let Some(region) = self.region_of(BlockPos::containing(eye)) {
                self.ext.entry::<PendingStroke>().0 = Some((using.off_hand, rx));
                region
                    .request(stroke_request(
                        eye,
                        look,
                        reach,
                        self.conn.handle.clone(),
                        reply,
                    ))
                    .await;
            }
        }
        let finished = using.tick();
        self.actions.using = Some(using);
        if finished {
            self.send(&play::EntityEvent {
                entity_id: self.id,
                status: play::entity_event::USE_ITEM_COMPLETE,
            });
            self.actions.using = None;
            self.publish(false).await;
        }
    }
}
