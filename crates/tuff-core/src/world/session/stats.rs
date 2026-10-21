//! Player statistics bookkeeping.

use crate::world::stats::Stat;
use tuff_vanilla_data::registry::{CustomStat, StatType};

use tuff_vanilla_data::Item;

use super::*;
use crate::protocol::play;
use crate::world::inventory::Inventory;
use tuff_item::item::{ItemStack, MintReason};

impl PlayerSession {
    /// How many of `item` an inventory holds outside its crafting grid,
    /// the cursor included.
    pub fn count_in(inventory: &Inventory, item: Item) -> i32 {
        inventory.slots()[5..]
            .iter()
            .flatten()
            .chain(inventory.carried())
            .filter(|stack| stack.item == item)
            .map(|stack| i32::from(stack.count()))
            .sum()
    }

    pub fn held_count(&self, item: Item) -> i32 {
        Self::count_in(&self.inventory, item)
    }

    /// Counts the `item`s a result slot gave: what the inventory gained
    /// since it held `before`, plus what fell out.
    pub fn count_crafted(&mut self, item: Item, before: i32, drops: &[ItemStack]) {
        let dropped: i32 = drops
            .iter()
            .filter(|stack| stack.item == item)
            .map(|stack| i32::from(stack.count()))
            .sum();
        let made = self.held_count(item) - before + dropped;
        if made > 0 {
            self.item_action_hooks(
                super::hooks::ItemAction::Crafted,
                ItemStack::mint(
                    item,
                    made.clamp(1, i32::from(i8::MAX)) as _,
                    MintReason::Template,
                )
                .view(),
            );
            self.progress
                .stats
                .award(Stat::item(StatType::Crafted, item), made);
        }
    }

    /// The counters every tick adds to.
    pub fn tick_stats(&mut self) {
        self.progress.stats.award_custom(CustomStat::PlayTime, 1);
        self.progress
            .stats
            .award_custom(CustomStat::TotalWorldTime, 1);
        if !self.is_dead() {
            self.progress
                .stats
                .award_custom(CustomStat::TimeSinceDeath, 1);
        }
        if self.pose.sneaking {
            self.progress.stats.award_custom(CustomStat::SneakTime, 1);
        }
        if self.sleep.sleeping.is_none() {
            self.progress
                .stats
                .award_custom(CustomStat::TimeSinceRest, 1);
        }
    }

    /// The distance counters for a move by `dx`, `dy`, `dz`.
    pub fn movement_stats(&mut self, dx: f64, dy: f64, dz: f64, on_ground: bool) {
        if self.ride.riding.is_some() {
            return;
        }
        let state = crate::world::stats::Moving {
            swimming: self.pose.swimming,
            under_water: self.surroundings.submersion.eyes,
            in_water: self.body.fall.in_water(),
            climbing: self.surroundings.climbable,
            on_ground,
            sprinting: self.pose.sprinting,
            crouching: self.pose.sneaking,
            fall_flying: self.motion.glide.active,
        };
        if let Some((stat, distance)) = crate::world::stats::movement(state, [dx, dy, dz]) {
            if state.swimming || state.under_water || state.in_water {
                self.exhaust(0.01 * distance as f32 * 0.01);
            }
            self.progress.stats.award_custom(stat, distance);
        }
    }

    /// The distance a rider travels with its vehicle.
    pub fn riding_stats(&mut self, delta: [f64; 3]) {
        let Some(riding) = &self.ride.riding else {
            return;
        };
        if let Some((stat, distance)) = crate::world::stats::riding(riding.kind, delta) {
            self.progress.stats.award_custom(stat, distance);
        }
    }

    /// Award Statistics with every statistic changed since the last one.
    pub fn send_stats(&mut self) {
        let changed: Vec<(u32, u32, i32)> = self
            .progress
            .stats
            .take_dirty()
            .into_iter()
            .map(|(stat, value)| (stat.kind.id(), stat.id, value))
            .collect();
        self.send(&play::AwardStats(&changed));
    }

    pub fn stat(&self, stat: crate::world::stats::Stat) -> i32 {
        self.progress.stats.get(stat)
    }
}
