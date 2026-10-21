//! Membership for the block sets world generation checks in its inner
//! loops: fixed lists from the data, or tags read from the tags in effect
//! (the snapshot the pipeline job runs under).

use tuff_vanilla_data::tag::Key;
use tuff_vanilla_data::tag::block::key;
use tuff_vanilla_data::{Block, BlockState};

pub enum BlockSet {
    Fixed(Box<[bool]>),
    Tag(Key<Block>),
}

impl BlockSet {
    pub fn new(blocks: &[Block]) -> Self {
        let mut set = vec![false; Block::ALL.len()].into_boxed_slice();
        for block in blocks {
            set[*block as usize] = true;
        }
        Self::Fixed(set)
    }

    /// The built-in block tag `name` (`minecraft:logs`).
    pub fn tag(name: &str) -> Option<Self> {
        tuff_datapack::tags::key(
            <Block as tuff_vanilla_data::Registry>::KEY,
            tuff_vanilla_data::tag::block::ALL,
            name,
        )
        .map(Self::Tag)
    }

    #[inline]
    pub fn contains(&self, block: Block) -> bool {
        match self {
            Self::Fixed(set) => set[block as usize],
            Self::Tag(key) => tuff_datapack::tags::is(*key, block),
        }
    }

    #[inline]
    pub fn contains_state(&self, state: BlockState) -> bool {
        self.contains(state.block())
    }
}

/// Biomes by id: a fixed list from the data, or a tag.
pub enum BiomeSet {
    Fixed(Vec<u16>),
    Tag(Key<&'static str>),
}

impl BiomeSet {
    /// The built-in biome tag `name` (`minecraft:is_ocean`).
    pub fn tag(name: &str) -> Option<Self> {
        tuff_datapack::tags::key(
            "minecraft:worldgen/biome",
            tuff_vanilla_data::tag::worldgen::biome::ALL,
            name,
        )
        .map(Self::Tag)
    }

    pub fn contains(&self, id: u16) -> bool {
        match self {
            Self::Fixed(ids) => ids.contains(&id),
            Self::Tag(key) => biome_is(*key, id),
        }
    }
}

/// Whether the built-in biome tag `key` holds biome `id` in the tags in
/// effect.
pub fn biome_is(key: Key<&'static str>, id: u16) -> bool {
    tuff_vanilla_data::data_registry::WORLDGEN_BIOME
        .entries
        .get(id as usize)
        .is_some_and(|name| tuff_datapack::tags::has(key, name))
}

macro_rules! block_tags {
    ($($name:ident => $tag:ident),* $(,)?) => {
        $(
            pub fn $name() -> &'static BlockSet {
                static SET: BlockSet = BlockSet::Tag(key::$tag);
                &SET
            }
        )*
    };
}

block_tags! {
    base_stone_overworld => BASE_STONE_OVERWORLD,
    blocks_motion_in_heightmap => BLOCKS_MOTION_IN_HEIGHTMAP,
    blocks_motion_in_heightmap_no_leaves => BLOCKS_MOTION_IN_HEIGHTMAP_NO_LEAVES,
    features_cannot_replace => FEATURES_CANNOT_REPLACE,
    leaves => LEAVES,
    logs => LOGS,
    prevents_nearby_leaf_decay => PREVENTS_NEARBY_LEAF_DECAY,
    replaceable_by_mushrooms => REPLACEABLE_BY_MUSHROOMS,
    replaceable_by_trees => REPLACEABLE_BY_TREES,
    sculk_growth_inhibitors => SCULK_GROWTH_INHIBITORS,
    sculk_replaceable => SCULK_REPLACEABLE,
    sculk_replaceable_world_gen => SCULK_REPLACEABLE_WORLD_GEN,
    uncarvable => UNCARVABLE,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_datapack::tags::{sync_scope, vanilla_with};

    #[tuff_core::watchdog]
    #[test]
    fn tag_sets_read_the_tags_in_effect() {
        let pack = vanilla_with(&[
            ("minecraft:block", "minecraft:uncarvable", "minecraft:stone"),
            (
                "minecraft:worldgen/biome",
                "minecraft:is_ocean",
                "minecraft:plains",
            ),
        ]);
        let plains = tuff_world_model::chunk::biome_id("minecraft:plains").unwrap();
        let named = crate::json::block_set(&serde_json::json!("#minecraft:uncarvable")).unwrap();
        let listed = crate::json::block_set(&serde_json::json!(["minecraft:dirt"])).unwrap();
        let ocean = BiomeSet::tag("minecraft:is_ocean").unwrap();
        let check = || {
            (
                crate::carver::carvable(Block::Stone.default_state()),
                named.contains(Block::Stone),
                listed.contains(Block::Dirt),
                ocean.contains(plains),
            )
        };
        assert_eq!(check(), (true, false, true, false));
        assert_eq!(sync_scope(pack, check), (false, true, true, true));
        assert!(BlockSet::tag("minecraft:no_such_tag").is_none());
    }

    #[tuff_core::watchdog]
    #[test]
    fn structure_biomes_follow_their_tag() {
        let structures = crate::structure::Structures::new(&[], 0);
        let village = structures
            .entries
            .iter()
            .find(|entry| entry.data.name == "minecraft:village_desert")
            .unwrap();
        assert_eq!(
            village.data.biome_tag,
            Some("minecraft:has_structure/village_desert")
        );
        let plains = tuff_world_model::chunk::biome_id("minecraft:plains").unwrap();
        let desert = tuff_world_model::chunk::biome_id("minecraft:desert").unwrap();
        assert!(village.allows(desert) && !village.allows(plains));
        let pack = vanilla_with(&[(
            "minecraft:worldgen/biome",
            "minecraft:has_structure/village_desert",
            "minecraft:plains",
        )]);
        assert!(sync_scope(pack, || village.allows(plains)));
    }
}
