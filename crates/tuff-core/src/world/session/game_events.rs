//! Posting game events on the player's behalf.

use super::*;
use crate::world::game_event::{Context, GameEvent, Source, floor_pos};
use crate::world::region_api::RegionTask;

impl PlayerSession {
    /// Announces `event` at `pos` from this player, as the region that owns
    /// `pos` posts it.
    pub async fn emit_game_event(&self, event: GameEvent, pos: [f64; 3]) {
        let Some(region) = self.region_of(floor_pos(pos)) else {
            return;
        };
        let source = Source::player(self.id)
            .sneaking(self.pose.sneaking)
            .spectating(self.mode.game_mode == GameMode::Spectator);
        region
            .request(RegionRequest::Run(RegionTask::new(move |world| {
                world.game_event(event, pos, Context::from(source));
            })))
            .await;
    }
}
