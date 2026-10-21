// SPDX-License-Identifier: MIT
//! Static item facts.

use crate::{host, types};

pub use types::ItemDetails;

/// Stack size, durability and consumability of an item id; `None` for an
/// unknown item or air.
pub fn info(item: &str) -> Option<ItemDetails> {
    host::item_info(item)
}

/// Member item names of an item tag (`#` optional); `None` for an unknown
/// or empty tag.
pub fn tag(tag: &str) -> Option<Vec<String>> {
    host::item_tag(tag)
}
