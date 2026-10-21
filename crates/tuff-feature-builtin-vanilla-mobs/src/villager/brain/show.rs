//! Showing off wares: a villager that has a player close by turns to them,
//! and while the player holds an item one of its offers asks for, holds up
//! what that offer sells, cycling when several match.

use tuff_core::world::entity::TickContext;
use tuff_item::item::ItemStack;
use tuff_vanilla_data::Item;
use tuff_vanilla_data::item::EquipmentSlot;
use tuff_vanilla_data::registry::Activity;

use super::{Villager, distance_sq};
use crate::MobCore;

const INTERACT_RANGE_SQ: f64 = 16.0;
const KEEP_RANGE_SQ: f64 = 17.0;
const START_LOOK_TICKS: i32 = 40;
const MAX_LOOK_TICKS: i32 = 900;
const CYCLE_TICKS: i32 = 40;
const HELD_DROP_CHANCE: f32 = 0.085;

#[derive(Debug)]
pub struct State {
    player: i32,
    seen_item: Option<Item>,
    display: Vec<ItemStack>,
    cycle: i32,
    index: usize,
    look_ticks: i32,
    holding: bool,
}

fn clear_held(core: &mut MobCore) {
    core.equipment.set(EquipmentSlot::Mainhand, None);
    core.equipment
        .set_drop_chance(EquipmentSlot::Mainhand, HELD_DROP_CHANCE);
}

fn hold(core: &mut MobCore, stack: &ItemStack) {
    core.equipment
        .set(EquipmentSlot::Mainhand, Some(stack.dup_unchecked()));
    core.equipment.set_drop_chance(EquipmentSlot::Mainhand, 0.0);
}

impl Villager {
    fn shows_wares(&self) -> bool {
        matches!(
            self.brain.activity,
            None | Some(Activity::Idle)
                | Some(Activity::Work)
                | Some(Activity::Meet)
                | Some(Activity::Play)
        )
    }

    fn stop_showing(&mut self, core: &mut MobCore) {
        if let Some(state) = self.brain.show.take()
            && state.holding
        {
            clear_held(core);
        }
    }

    fn wanted_by_offers(&self, held: Item) -> Vec<ItemStack> {
        self.offers
            .iter()
            .flatten()
            .filter(|o| !o.is_out_of_stock())
            .filter(|o| o.buy.item == held || o.buy_b.as_ref().is_some_and(|b| b.item == held))
            .map(|o| o.sell.dup_unchecked())
            .collect()
    }

    /// One step of showing trades; the villager only looks, it keeps its
    /// feet.
    pub(super) fn show_trades_step(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if !self.shows_wares() || core.is_baby() || !core.is_alive() {
            self.stop_showing(core);
            return;
        }
        let near = |p: &tuff_core::world::entity::NearbyPlayer, range: f64| {
            distance_sq(p.position, core.base.position) <= range
        };
        let target = match &self.brain.show {
            Some(state) => cx
                .players
                .iter()
                .find(|p| p.id == state.player)
                .filter(|p| near(p, KEEP_RANGE_SQ)),
            None => cx
                .players
                .iter()
                .filter(|p| near(p, INTERACT_RANGE_SQ))
                .min_by(|a, b| {
                    distance_sq(a.position, core.base.position)
                        .total_cmp(&distance_sq(b.position, core.base.position))
                })
                .filter(|p| {
                    core.can_see_point(
                        [p.position[0], p.position[1] + p.eye_height, p.position[2]],
                        cx,
                    )
                }),
        };
        let Some(player) = target else {
            self.stop_showing(core);
            return;
        };
        let (id, eye, held) = (
            player.id,
            [
                player.position[0],
                player.position[1] + player.eye_height,
                player.position[2],
            ],
            player.held[0],
        );
        if self.brain.show.is_none() {
            self.brain.show = Some(State {
                player: id,
                seen_item: None,
                display: Vec::new(),
                cycle: 0,
                index: 0,
                look_ticks: START_LOOK_TICKS,
                holding: false,
            });
        }
        core.look.set_look_at(eye, 30.0, 30.0);
        let mut state = self.brain.show.take().expect("just set");
        if state.seen_item != held {
            state.seen_item = held;
            state.display.clear();
            if let Some(item) = held {
                state.display = self.wanted_by_offers(item);
                if let Some(first) = state.display.first() {
                    state.look_ticks = MAX_LOOK_TICKS;
                    state.index = 0;
                    state.cycle = 0;
                    hold(core, first);
                    state.holding = true;
                }
            }
        }
        if state.display.is_empty() {
            if state.holding {
                clear_held(core);
                state.holding = false;
            }
            state.look_ticks = state.look_ticks.min(START_LOOK_TICKS);
        } else if state.display.len() >= 2 {
            state.cycle += 1;
            if state.cycle >= CYCLE_TICKS {
                state.cycle = 0;
                state.index = (state.index + 1) % state.display.len();
                hold(core, &state.display[state.index]);
            }
        }
        state.look_ticks -= 1;
        if state.look_ticks > 0 {
            self.brain.show = Some(state);
        } else if state.holding {
            clear_held(core);
        }
    }
}
