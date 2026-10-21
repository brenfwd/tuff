//! The anvil menu's vocabulary: what a refresh produces and how long a
//! name may be. The repair, combine and rename arithmetic comes from the
//! feature that fills [`crate::registry::Registries::station_rules`].

use tuff_item::item::ItemStack;

pub const MAX_NAME_LENGTH: usize = 50;

#[derive(Debug, PartialEq)]
pub struct AnvilOutcome {
    pub result: Option<ItemStack>,
    pub cost: i32,
    pub repair_count: u8,
    pub only_renaming: bool,
}

impl AnvilOutcome {
    pub const NOTHING: Self = Self {
        result: None,
        cost: 0,
        repair_count: 0,
        only_renaming: false,
    };
}

pub fn valid_name(name: &str) -> bool {
    name.chars().count() <= MAX_NAME_LENGTH
}
