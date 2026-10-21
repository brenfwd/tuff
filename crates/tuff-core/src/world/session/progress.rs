use super::*;

/// Advancements, statistics, the recipe book and the trigger bookkeeping
/// that feeds them.
pub struct Progress {
    pub recipe_book: RecipeBook,
    /// The data pack load this client was last sent tags and recipes of.
    pub datapack_generation: u64,
    /// The tree `advancements` is bound to; replaced on the first tick
    /// after a data pack reload.
    pub advancement_tree: Arc<dyn crate::world::advancement::Advancements>,
    /// Where this player left the overworld for the nether, until it
    /// leaves the nether.
    pub entered_nether: Option<[f64; 3]>,
    /// The inventory as of the last unlock scan.
    pub last_inventory: Vec<Option<ItemStack>>,
    pub stats: Box<crate::world::stats::Stats>,
    /// Where the current fall and levitation (with its first tick)
    /// began, for advancements.
    pub fall_start: Option<[f64; 3]>,
    pub levitation_start: Option<([f64; 3], u64)>,
    /// Where the last explosion to hit it found it and what set it off,
    /// until it next starts to fall.
    pub explosion_impact: Option<([f64; 3], Option<crate::world::predicate::EntityFacts>)>,
    /// Where it was when its vehicle entered lava, while it stays there.
    pub lava_ride_start: Option<[f64; 3]>,
    /// Arrow kills credited since the last tick, with the firing weapon.
    pub arrow_kills: Vec<(
        crate::world::predicate::EntityFacts,
        Option<ItemStack>,
        Option<i32>,
    )>,
    /// The credits were shown once; later exits from the end skip them.
    pub seen_credits: bool,
    pub warden_tracker: crate::storage::player_data::WardenTracker,
}
