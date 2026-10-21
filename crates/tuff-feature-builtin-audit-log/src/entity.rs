//! Kill rows: the victim's saved form, so a rollback summons it again and
//! a restore takes it out by UUID.

use tuff_nbt::{Compound, Tag};
use tuff_world_model::BlockPos;

/// What a kill row's text holds for a mob.
pub fn encode_text(uuid: u128) -> String {
    format!("uuid={uuid:032x}")
}

/// What a kill row's text holds for a player victim.
pub fn encode_player(name: &str) -> String {
    format!("player={name}")
}

/// The victim's UUID, `None` for a player or text this version did not write.
pub fn parse_text(text: &str) -> Option<u128> {
    let hex = text.strip_prefix("uuid=")?;
    if hex.len() != 32 {
        return None;
    }
    u128::from_str_radix(hex, 16).ok()
}

/// One kill row's undo: summon the victim again, or remove it.
#[derive(Clone, Debug)]
pub struct Undo {
    pub id: i64,
    pub uuid: u128,
    pub pos: BlockPos,
    /// The saved victim to summon; `None` removes `uuid` instead.
    pub summon: Option<(Compound, [f64; 3])>,
}

/// Where the saved entity stood, or the middle of the row's block.
pub fn position_of(nbt: &Compound, pos: BlockPos) -> [f64; 3] {
    if let Some(Tag::List(list)) = nbt.get("Pos")
        && let [Tag::Double(x), Tag::Double(y), Tag::Double(z)] = list.as_slice()
    {
        return [*x, *y, *z];
    }
    [
        f64::from(pos.x) + 0.5,
        f64::from(pos.y),
        f64::from(pos.z) + 0.5,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn uuids_round_trip_and_players_have_none() {
        let uuid = 0x1234_5678_9abc_def0_0fed_cba9_8765_4321;
        assert_eq!(parse_text(&encode_text(uuid)), Some(uuid));
        assert_eq!(parse_text(&encode_player("Steve")), None);
        assert_eq!(parse_text("uuid=zz"), None);
    }

    #[tuff_core::watchdog]
    #[test]
    fn position_comes_from_the_saved_pos_or_the_block() {
        let saved = Compound::new().with(
            "Pos",
            Tag::List(vec![
                Tag::Double(1.5),
                Tag::Double(64.0),
                Tag::Double(-2.25),
            ]),
        );
        let pos = BlockPos::new(1, 64, -3);
        assert_eq!(position_of(&saved, pos), [1.5, 64.0, -2.25]);
        assert_eq!(position_of(&Compound::new(), pos), [1.5, 64.0, -2.5]);
    }
}
