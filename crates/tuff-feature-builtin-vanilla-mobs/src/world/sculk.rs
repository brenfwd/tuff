//! Sculk shrieker data: reading and writing a shrieker's warning level.
//!
//! The vibration pieces live in `tuff_core::world::game_event`; the
//! shriekers' region part is `world::region::shriekers`.

use tuff_nbt::{Compound, Tag};

pub const ID: &str = "minecraft:sculk_shrieker";

/// How far a shrieker hears a disturbance.
pub const WARNING_RANGE: f64 = 8.0;

pub fn is_shrieker(nbt: &Compound) -> bool {
    nbt.get_str("id") == Some(ID)
}

fn as_i32(tag: &Tag) -> Option<i32> {
    match *tag {
        Tag::Byte(v) => Some(i32::from(v)),
        Tag::Short(v) => Some(i32::from(v)),
        Tag::Int(v) => Some(v),
        _ => None,
    }
}

pub fn warning_level(nbt: &Compound) -> i32 {
    nbt.get("warning_level").and_then(as_i32).unwrap_or(0)
}

pub fn set_warning_level(nbt: &mut Compound, level: i32) {
    nbt.insert("warning_level", level);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn warning_level_round_trips() {
        let mut nbt = Compound::new().with("id", ID);
        assert!(is_shrieker(&nbt));
        assert_eq!(warning_level(&nbt), 0);
        set_warning_level(&mut nbt, 3);
        assert_eq!(warning_level(&nbt), 3);
    }
}
