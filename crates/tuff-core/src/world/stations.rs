//! What anvil, grindstone and loom menus produce. Core keeps the menus and their
//! slot vocabulary; the results come from the feature that fills
//! [`crate::registry::Registries::station_rules`]. With no feature the
//! stations offer nothing.

use rand::RngCore;

use tuff_item::item::ItemStack;

use crate::world::anvil::AnvilOutcome;

/// The loom's selection after its inputs changed: the new pattern list,
/// the selected index (-1 for none) and the banner to show.
pub struct LoomRefresh {
    pub patterns: &'static [&'static str],
    pub selected: i32,
    pub result: Option<ItemStack>,
}

impl LoomRefresh {
    pub const NOTHING: Self = Self {
        patterns: &[],
        selected: -1,
        result: None,
    };
}

pub trait StationRules: Send + Sync {
    /// What the anvil offers for its inputs and the typed name, and its
    /// level cost; `infinite` is the player's creative instabuild.
    fn anvil_result(
        &self,
        _input: Option<&ItemStack>,
        _addition: Option<&ItemStack>,
        _name: Option<&str>,
        _infinite: bool,
    ) -> AnvilOutcome {
        AnvilOutcome::NOTHING
    }

    /// What the grindstone offers for its two input slots.
    fn grindstone_result(
        &self,
        _input: Option<&ItemStack>,
        _additional: Option<&ItemStack>,
    ) -> Option<ItemStack> {
        None
    }

    /// The experience grinding `stack` is worth before the random award.
    fn grindstone_experience(&self, _stack: &ItemStack) -> i32 {
        0
    }

    /// The experience that drops for a grind worth `amount`.
    fn grindstone_award(&self, _amount: i32, _rng: &mut dyn RngCore) -> i32 {
        0
    }

    /// One banner with `pattern` added in the dye's color.
    fn loom_weave(
        &self,
        _banner: &ItemStack,
        _dye: &ItemStack,
        _pattern: &str,
    ) -> Option<ItemStack> {
        None
    }

    /// The loom's pattern list, selection and result after its inputs
    /// changed, given the previous list and selection.
    fn loom_refresh(
        &self,
        _previous: &[&'static str],
        _selected: i32,
        _banner: Option<&ItemStack>,
        _dye: Option<&ItemStack>,
        _pattern_item: Option<&ItemStack>,
    ) -> LoomRefresh {
        LoomRefresh::NOTHING
    }
}

/// The rules an inert core uses: stations offer nothing.
pub struct NoStationRules;

impl StationRules for NoStationRules {}
