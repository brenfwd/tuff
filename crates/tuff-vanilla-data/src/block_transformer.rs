//! The data pack's `minecraft:block_transformer` entries: what using an axe,
//! hoe or shovel does to a block (strip, scrape, remove wax, till, flatten).
//! This is a data-driven registry; items point at an entry through their
//! `minecraft:block_transformer` component ([`crate::Item::block_transformer`]).

use crate::block::{BlockState, Facing};
use crate::registry::{Block, Item, SoundEvent};

#[derive(Debug)]
pub struct BlockTransformer {
    pub name: &'static str,
    /// Tried in order; the first whose provider yields a state applies.
    pub transforms: &'static [Transform],
}

#[derive(Debug)]
pub struct Transform {
    pub provider: &'static [Rule],
    pub sound: Option<SoundEvent>,
    pub particle: TransformParticle,
    /// Clicking one of these faces does not transform.
    pub disallowed_faces: &'static [Facing],
    /// A loot table dropped from the block when it transforms.
    pub loot: Option<&'static str>,
    pub drop_strategy: DropStrategy,
    /// Whether the new state is fitted to its neighbours before it is set.
    pub update_from_neighbors: bool,
    pub transform_type: TransformType,
    /// For stackable items: whether one is used up.
    pub consume_on_use: bool,
    /// For damageable items: the durability each use costs.
    pub item_damage_per_use: u32,
}

/// One rule of a rule-based state provider.
#[derive(Debug)]
pub struct Rule {
    pub if_true: Predicate,
    pub then: StateSource,
}

#[derive(Debug)]
pub enum Predicate {
    /// The block at `offset` from the target is one of `blocks` (sorted).
    /// Tags are resolved into their blocks at build time.
    MatchingBlocks {
        offset: [i32; 3],
        blocks: &'static [Block],
    },
    AllOf(&'static [Predicate]),
    AnyOf(&'static [Predicate]),
    Not(&'static Predicate),
}

#[derive(Debug, Clone, Copy)]
pub enum StateSource {
    State(BlockState),
    /// This state with every property the target shares copied from it.
    CopyProperties(BlockState),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransformParticle {
    None,
    Scrape,
    WaxOn,
    WaxOff,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropStrategy {
    /// Pop the loot out of the clicked face.
    ClickedFace,
    FromMiddle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransformType {
    SingleBlock,
    /// A copper chest: the particles also show on a double chest's other
    /// half.
    CopperChest,
}

impl Predicate {
    /// Evaluates against `block_at(offset)`, the block at an offset from
    /// the target.
    pub fn test(&self, block_at: &dyn Fn([i32; 3]) -> BlockState) -> bool {
        match self {
            Self::MatchingBlocks { offset, blocks } => blocks
                .binary_search_by_key(&(block_at(*offset).block() as usize), |b| *b as usize)
                .is_ok(),
            Self::AllOf(all) => all.iter().all(|p| p.test(block_at)),
            Self::AnyOf(any) => any.iter().any(|p| p.test(block_at)),
            Self::Not(inner) => !inner.test(block_at),
        }
    }
}

impl StateSource {
    pub fn state(self, target: BlockState) -> BlockState {
        match self {
            Self::State(state) => state,
            Self::CopyProperties(state) => {
                target.properties().fold(state, |state, (name, value)| {
                    state.with(name, value).unwrap_or(state)
                })
            }
        }
    }
}

impl Transform {
    /// The state the target turns into, if a rule matches. `block_at`
    /// reads the world relative to the target.
    pub fn apply(&self, block_at: &dyn Fn([i32; 3]) -> BlockState) -> Option<BlockState> {
        let target = block_at([0, 0, 0]);
        self.provider
            .iter()
            .find(|rule| rule.if_true.test(block_at))
            .map(|rule| rule.then.state(target))
    }
}

impl BlockTransformer {
    pub fn get(name: &str) -> Option<&'static BlockTransformer> {
        let (namespace, path) = name.split_once(':').unwrap_or(("minecraft", name));
        ALL.iter()
            .find(|(key, _)| {
                key.strip_prefix(namespace)
                    .and_then(|k| k.strip_prefix(':'))
                    == Some(path)
            })
            .map(|(_, transformer)| transformer)
    }
}

pub(crate) fn of_item(item: Item) -> Option<&'static BlockTransformer> {
    ALL.get(ITEM_BLOCK_TRANSFORMER[item as usize] as usize)
        .map(|(_, transformer)| transformer)
}

include!(concat!(env!("OUT_DIR"), "/block_transformers.rs"));

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(item: Item, target: BlockState, above: BlockState) -> Option<BlockState> {
        let transformer = item.block_transformer()?;
        let block_at = |offset: [i32; 3]| {
            if offset == [0, 1, 0] { above } else { target }
        };
        transformer
            .transforms
            .iter()
            .find_map(|transform| transform.apply(&block_at))
    }

    #[test]
    fn tools_carry_their_transformers() {
        assert_eq!(
            Item::IronAxe.block_transformer().unwrap().name,
            "minecraft:axe"
        );
        assert_eq!(
            Item::StoneHoe.block_transformer().unwrap().name,
            "minecraft:hoe"
        );
        assert_eq!(
            Item::GoldenShovel.block_transformer().unwrap().name,
            "minecraft:shovel"
        );
        assert!(Item::Stick.block_transformer().is_none());
        assert!(BlockTransformer::get("minecraft:axe").is_some());
    }

    #[test]
    fn axes_strip_scrape_and_remove_wax_keeping_properties() {
        let air = Block::Air.default_state();
        let log = Block::OakLog.default_state().with("axis", "x").unwrap();
        let stripped = apply(Item::IronAxe, log, air).unwrap();
        assert_eq!(stripped.block(), Block::StrippedOakLog);
        assert_eq!(stripped.get("axis"), Some("x"));
        let exposed = Block::ExposedCopper.default_state();
        assert_eq!(
            apply(Item::IronAxe, exposed, air).unwrap().block(),
            Block::CopperBlock
        );
        let waxed = Block::WaxedCutCopperStairs
            .default_state()
            .with("facing", "east")
            .unwrap();
        let unwaxed = apply(Item::IronAxe, waxed, air).unwrap();
        assert_eq!(unwaxed.block(), Block::CutCopperStairs);
        assert_eq!(unwaxed.get("facing"), Some("east"));
        assert_eq!(
            apply(Item::IronAxe, Block::Stone.default_state(), air),
            None
        );
    }

    #[test]
    fn hoes_and_shovels_need_air_above() {
        let air = Block::Air.default_state();
        let dirt = Block::Dirt.default_state();
        assert_eq!(
            apply(Item::IronHoe, dirt, air).unwrap().block(),
            Block::Farmland
        );
        assert_eq!(apply(Item::IronHoe, dirt, dirt), None);
        assert_eq!(
            apply(Item::IronHoe, Block::CoarseDirt.default_state(), air).unwrap(),
            dirt
        );
        assert_eq!(
            apply(Item::IronHoe, Block::RootedDirt.default_state(), dirt).unwrap(),
            dirt
        );
        let grass = Block::GrassBlock.default_state();
        assert_eq!(
            apply(Item::IronShovel, grass, air).unwrap().block(),
            Block::DirtPath
        );
        assert_eq!(apply(Item::IronShovel, grass, dirt), None);
    }
}
