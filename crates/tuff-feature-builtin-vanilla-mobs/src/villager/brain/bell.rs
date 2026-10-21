//! Taking cover when a bell rings: a villager that hears one heads for a
//! bed nearby and stays there until the alarm times out.

use rand::Rng;
use tuff_vanilla_data::registry::PointOfInterestType;

use super::{Villager, center, distance_sq};
use crate::MobCore;
use crate::physics::block_pos;
use crate::world::poi::Occupancy;
use tuff_core::world::entity::TickContext;
use tuff_world_model::BlockPos;

const HIDE_SPEED: f64 = 0.5 * 1.25;
const CLOSE_ENOUGH: f64 = 2.0;
const HIDDEN_RANGE: f64 = 3.0;
const FIND_CLOSE: i32 = 3;
const FIND_FAR: i32 = 32;
const STAY_HIDDEN_TICKS: u32 = 15 * 20;
const HIDE_TIMEOUT: i64 = 300;

#[derive(Debug, Default)]
pub struct State {
    pending: bool,
    hiding: Option<BlockPos>,
    hidden_ticks: u32,
}

impl Villager {
    /// A bell rang within hearing.
    pub fn heard_bell(&mut self) {
        self.brain.bell.pending = true;
    }

    pub(super) fn note_bell(&mut self, cx: &TickContext) {
        if std::mem::take(&mut self.brain.bell.pending) {
            self.brain.memories.heard_bell = Some(cx.tick as i64);
        }
    }

    pub(super) fn is_hiding(&self) -> bool {
        self.brain.memories.heard_bell.is_some()
    }

    fn stop_hiding(&mut self) {
        self.brain.memories.heard_bell = None;
        self.brain.bell = State::default();
    }

    pub(super) fn hide(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        let Some(heard) = self.brain.memories.heard_bell else {
            return;
        };
        if self.brain.bell.hidden_ticks > STAY_HIDDEN_TICKS
            || heard + HIDE_TIMEOUT <= cx.tick as i64
        {
            self.stop_hiding();
            return;
        }
        let here = block_pos(core.base.position);
        if let Some(place) = self.brain.bell.hiding
            && distance_sq(
                [place.x as f64, place.y as f64, place.z as f64],
                [here.x as f64, here.y as f64, here.z as f64],
            ) < HIDDEN_RANGE * HIDDEN_RANGE
        {
            self.brain.bell.hidden_ticks += 1;
        }
        if !core.navigation.is_done() {
            return;
        }
        let Some(place) = self.hiding_place(core, cx, FIND_FAR) else {
            return;
        };
        self.end_interaction();
        self.brain.breeding = None;
        self.brain.bell.hiding = Some(place);
        if distance_sq(center(place), core.base.position) >= CLOSE_ENOUGH * CLOSE_ENOUGH {
            core.move_to_block(place, CLOSE_ENOUGH as i32, HIDE_SPEED, cx);
        }
    }

    /// A bed within two blocks, else a random one within reach, else its
    /// own.
    pub(super) fn hiding_place(
        &mut self,
        core: &MobCore,
        cx: &mut TickContext,
        far_radius: i32,
    ) -> Option<BlockPos> {
        let origin = block_pos(core.base.position);
        let homes = |radius| {
            cx.pois
                .closest_first(origin, radius, Occupancy::Any, |kind| {
                    kind == PointOfInterestType::Home
                })
        };
        let close = homes(FIND_CLOSE + 1)
            .into_iter()
            .map(|(pos, _)| pos)
            .find(|pos| {
                distance_sq(center(*pos), core.base.position) < CLOSE_ENOUGH * CLOSE_ENOUGH
            });
        close
            .or_else(|| {
                let far = homes(far_radius);
                (!far.is_empty()).then(|| far[cx.rng.gen_range(0..far.len())].0)
            })
            .or_else(|| self.brain.memories.home.as_ref().map(|h| h.pos))
    }
}
