//! The identity a plugin-defined item carries in its `custom_data`.

use tuff_nbt::{Compound, Tag};

pub const IDENTITY_KEY: &str = "tuff";
pub const PRIVATE_KEY: &str = "private";
pub const ITEM_KEY: &str = "item";

/// The `custom_data` a client is sent for a private custom item: only the
/// identity. `None` for any other value, which is sent as stored.
pub fn client_custom_data(value: &Tag) -> Option<Compound> {
    let identity = value.as_compound()?.get(IDENTITY_KEY)?.as_compound()?;
    if !identity.get_bool(PRIVATE_KEY).unwrap_or(false) {
        return None;
    }
    let id = identity.get_str(ITEM_KEY)?;
    Some(Compound::new().with(IDENTITY_KEY, Compound::new().with(ITEM_KEY, id)))
}
