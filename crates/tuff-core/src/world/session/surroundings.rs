use super::*;

/// What the home region last reported about the blocks around the player.
#[derive(Default)]
pub struct Surroundings {
    /// The biome and block under the player, as the home region last
    /// reported them.
    pub biome: Option<&'static str>,
    pub stepping_on: Option<tuff_vanilla_data::BlockState>,
    /// The home region's last report: on a ladder, vine or the like.
    pub climbable: bool,
    /// What was last climbed, kept until the player stands on the ground.
    pub fall_location: Option<crate::world::death::FallLocation>,
    /// Inside lava as of the last surroundings report.
    pub in_lava: bool,
    /// The harmful blocks under and around the player as of the last
    /// surroundings report.
    pub contact: block_contact::Contact,
    /// The attribute modifiers active `location_changed` enchantment
    /// effects give (Soul Speed on soul sand): attribute, id, amount and
    /// operation. Not saved; recomputed when the player changes block.
    pub enchantment_modifiers: Vec<(
        tuff_vanilla_data::registry::Attribute,
        String,
        f64,
        tuff_vanilla_data::item::ModifierOperation,
    )>,
    /// The damage of the fire the player stands in, as of the last
    /// surroundings report.
    pub in_fire: Option<f32>,
    /// Water at the eyes and feet as of the last surroundings report.
    pub submersion: player::Submersion,
}
