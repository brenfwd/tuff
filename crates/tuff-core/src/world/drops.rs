//! What blocks drop. Loot tables are feature content, so core asks a
//! [`BlockLoot`] a feature registers whenever a block breaks, is destroyed
//! or is blown up; with none, nothing drops.

use rand::rngs::StdRng;
use tuff_vanilla_data::BlockState;

use tuff_item::item::ItemStack;
use tuff_nbt::Compound;

/// The component marking a rolled explorer map still waiting for its map.
pub const EXPLORATION_PENDING: &str = "tuff:exploration_map";

/// What entity loot tables can see of the entity the loot comes from and
/// of whatever killed or sheared it.
#[derive(tuff_item::item::DupClone, Debug, Default)]
pub struct EntityFacts {
    pub kind: Option<tuff_vanilla_data::registry::EntityType>,
    pub on_fire: bool,
    /// Component values the entity exposes, like `minecraft:sheep/color`.
    pub components: Vec<(&'static str, String)>,
    /// `minecraft:type_specific/sheep`'s `sheared`.
    pub sheared: Option<bool>,
    /// `minecraft:type_specific/cube_mob`'s `size` (slimes, magma cubes).
    pub cube_size: Option<i32>,
    /// `minecraft:type_specific/raider`'s `has_raid` and `is_captain`.
    pub raider: Option<(bool, bool)>,
    /// The weapon of the attacker that dealt the killing blow.
    pub attacker_weapon: Option<ItemStack>,
    /// The type of what dealt the killing blow.
    pub attacker_kind: Option<tuff_vanilla_data::registry::EntityType>,
    pub killed_by_player: bool,
    /// The scoreboard holder name (player name or entity uuid), for
    /// `entity_scores`.
    pub score_holder: Option<String>,
    /// The name of the player the loot comes from, for `fill_player_head`.
    pub player_name: Option<String>,
    /// The damage that dealt the killing blow, for `damage_source_properties`.
    pub damage_kind: Option<&'static tuff_vanilla_data::damage_type::DamageType>,
    /// The killer dealt the killing blow itself, not through a projectile.
    pub damage_direct: bool,
    /// `minecraft:type_specific/fishing_hook`'s `in_open_water`.
    pub open_water: Option<bool>,
    /// Luck shifting entries by their `quality`.
    pub luck: f32,
    /// The server's recipes, for `furnace_smelt`; the data pack's without.
    pub recipes: Option<std::sync::Arc<crate::world::crafting::RecipeSet>>,
    /// The loot tables in effect; the built-in ones without.
    pub tables: Option<std::sync::Arc<tuff_datapack::loot::LootTables>>,
    /// The loot context's origin, for `location_check`.
    pub origin: Option<crate::world::predicate::LocationFacts>,
}

pub trait BlockLoot: Send + Sync {
    /// These rules reading `tables`, the loot tables data packs left in
    /// effect; `None` when they do not read loot tables.
    fn with_tables(
        &self,
        _tables: std::sync::Arc<tuff_datapack::loot::LootTables>,
    ) -> Option<std::sync::Arc<dyn BlockLoot>> {
        None
    }

    /// Whether `held` opens a container locked with `lock` (an item
    /// predicate). Without loot rules locks are ignored.
    fn lock_admits(&self, _lock: &Compound, _held: Option<&ItemStack>) -> bool {
        true
    }

    /// The loot table `name` rolled into a container of `slots` slots.
    fn container_loot(
        &self,
        _name: &str,
        _slots: usize,
        _rng: &mut rand::rngs::StdRng,
    ) -> Option<Vec<Option<ItemStack>>> {
        None
    }

    /// Starts finishing a rolled stack that needs a search (an explorer
    /// map) from `origin`; the answer is the finished stack, or `None`
    /// when there is nothing to find. `None` here when the stack needs no
    /// finishing.
    fn explore(
        &self,
        _shared: &std::sync::Arc<crate::world::Shared>,
        _dimension: crate::world::dimension::DimensionId,
        _origin: tuff_world_model::BlockPos,
        _stack: &ItemStack,
    ) -> Option<tokio::sync::oneshot::Receiver<Option<ItemStack>>> {
        None
    }

    /// `stack` run through item modifier `functions` as a command does.
    fn modify_item(
        &self,
        _functions: &[tuff_vanilla_data::loot_table::LootFunction],
        stack: ItemStack,
        _rng: &mut StdRng,
    ) -> ItemStack {
        stack
    }

    /// The loot table `name` rolled with no block, entity or tool.
    fn roll(&self, _name: &str, _rng: &mut StdRng) -> Vec<ItemStack> {
        Vec::new()
    }

    /// The entity loot table `name` rolled for the entity `facts`
    /// describes.
    fn entity_drops(&self, _name: &str, _facts: &EntityFacts, _rng: &mut StdRng) -> Vec<ItemStack> {
        Vec::new()
    }

    /// What a bobber reels in, with the angler's `luck`.
    fn fishing(
        &self,
        _luck: f32,
        _open_water: bool,
        _rod: &ItemStack,
        _rng: &mut StdRng,
    ) -> Vec<ItemStack> {
        Vec::new()
    }

    /// Whether breaking `state` with `tool` (or bare hands) yields drops.
    fn can_harvest(&self, state: BlockState, tool: Option<&ItemStack>) -> bool;

    /// The drops of `state` broken with `tool`, or destroyed by the world
    /// (no tool). `block_entity` is its saved block entity, if any.
    fn drops(
        &self,
        state: BlockState,
        tool: Option<&ItemStack>,
        block_entity: Option<&Compound>,
        rng: &mut StdRng,
    ) -> Vec<ItemStack>;

    /// Like [`drops`](Self::drops), with the broken block's location for
    /// `location_check` conditions.
    fn drops_at(
        &self,
        state: BlockState,
        tool: Option<&ItemStack>,
        block_entity: Option<&Compound>,
        _at: &crate::world::predicate::LocationFacts,
        _world: Option<&dyn crate::world::predicate::PredicateWorld>,
        rng: &mut StdRng,
    ) -> Vec<ItemStack> {
        self.drops(state, tool, block_entity, rng)
    }

    /// The drops of `state` destroyed by the world or a command, which
    /// ignores whether a tool would have been needed.
    fn destroyed(
        &self,
        state: BlockState,
        block_entity: Option<&Compound>,
        rng: &mut StdRng,
    ) -> Vec<ItemStack> {
        self.drops(state, None, block_entity, rng)
    }

    /// A named loot table rolled for `state` (a harvest, a tool's
    /// transformation), with the item used if any.
    fn table(
        &self,
        name: &str,
        state: BlockState,
        tool: Option<tuff_vanilla_data::Item>,
        rng: &mut StdRng,
    ) -> Vec<ItemStack>;

    /// The drops of `state` blown up by an explosion; `decay` is its
    /// radius when drops thin out with it.
    fn exploded(
        &self,
        state: BlockState,
        decay: Option<f32>,
        block_entity: Option<&Compound>,
        rng: &mut StdRng,
    ) -> Vec<ItemStack>;
}

/// Nothing drops.
pub struct NoBlockLoot;

impl BlockLoot for NoBlockLoot {
    fn can_harvest(&self, _: BlockState, _: Option<&ItemStack>) -> bool {
        true
    }

    fn drops(
        &self,
        _: BlockState,
        _: Option<&ItemStack>,
        _: Option<&Compound>,
        _: &mut StdRng,
    ) -> Vec<ItemStack> {
        Vec::new()
    }

    fn table(
        &self,
        _: &str,
        _: BlockState,
        _: Option<tuff_vanilla_data::Item>,
        _: &mut StdRng,
    ) -> Vec<ItemStack> {
        Vec::new()
    }

    fn exploded(
        &self,
        _: BlockState,
        _: Option<f32>,
        _: Option<&Compound>,
        _: &mut StdRng,
    ) -> Vec<ItemStack> {
        Vec::new()
    }
}
