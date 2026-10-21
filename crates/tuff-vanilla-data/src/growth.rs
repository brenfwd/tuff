//! What random ticks, growth, bone meal and fire need that no report
//! carries: which states tick randomly, each block's random tick and bone
//! meal behaviour by kind name, flammability, tree growers, stems, growing
//! plants, copper weathering and coral death.

use crate::{Block, BlockState};

/// `block` in the state of `from`, as far as their properties match.
pub fn with_properties_of(block: Block, from: BlockState) -> BlockState {
    from.properties()
        .fold(block.default_state(), |state, (name, value)| {
            state.with(name, value).unwrap_or(state)
        })
}

/// The trees a sapling may grow into, as `(configured feature, weight)`.
#[derive(Debug)]
pub struct TreeGrower {
    pub name: &'static str,
    /// The tree whose trunk height sets the minimum growth height.
    pub shortest: Option<&'static str>,
    pub trees: &'static [(&'static str, u32)],
    /// Trees grown from a 2x2 of saplings.
    pub mega: &'static [(&'static str, u32)],
    /// Trees grown instead of `trees` with flowers nearby.
    pub flower: &'static [(&'static str, u32)],
}

impl TreeGrower {
    pub fn by_name(name: &str) -> Option<&'static TreeGrower> {
        GROWERS.iter().find(|g| g.name == name)
    }
}

#[derive(Debug)]
pub struct Stem {
    pub stem: Block,
    pub fruit: Block,
    pub attached: Block,
    /// Block tags the stem and the fruit it grows may stand on.
    pub stem_support: &'static str,
    pub fruit_support: &'static str,
}

/// A plant that grows block by block from its head.
#[derive(Debug)]
pub struct GrowingPlant {
    pub head: Block,
    pub body: Block,
    pub direction: crate::block::Facing,
    /// The chance a random tick grows it.
    pub chance: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct Weathering {
    /// Blocks only compare their age with blocks of the same group.
    pub group: u8,
    pub age: u8,
    pub chance_modifier: f32,
    pub next: Option<Block>,
}

include!(concat!(env!("OUT_DIR"), "/growth.rs"));

fn bit(set: &[u64], state: BlockState) -> bool {
    let id = state.id() as usize;
    set.get(id / 64)
        .is_some_and(|word| word >> (id % 64) & 1 == 1)
}

fn by_id<T: Copy>(table: &[(u32, T)], block: Block) -> Option<T> {
    table
        .binary_search_by_key(&block.id(), |(id, _)| *id)
        .ok()
        .map(|index| table[index].1)
}

impl BlockState {
    /// The state's block ticks randomly.
    pub fn is_randomly_ticking(self) -> bool {
        bit(&RANDOM_TICKING, self)
    }

    /// The state's fluid ticks randomly (lava).
    pub fn is_fluid_randomly_ticking(self) -> bool {
        bit(&FLUID_RANDOM_TICKING, self)
    }

    /// Lava next to the state sets fire to it.
    pub fn is_ignited_by_lava(self) -> bool {
        bit(&LAVA_IGNITES, self)
    }
}

impl Block {
    /// What the block's random tick does, by kind name.
    pub fn random_tick_kind(self) -> Option<&'static str> {
        let kind = *BLOCK_TICK.get(self.id() as usize)?;
        TICK_KINDS.get(kind as usize).copied()
    }

    /// What bone meal does to the block, by kind name.
    pub fn bonemeal_kind(self) -> Option<&'static str> {
        let kind = *BLOCK_BONEMEAL.get(self.id() as usize)?;
        BONEMEAL_KINDS.get(kind as usize).copied()
    }

    /// How readily fire next to the block spreads onto it (ignite odds) and
    /// how readily fire burns it away (burn odds).
    pub fn flammability(self) -> (u8, u8) {
        FLAMMABLE
            .get(self.id() as usize)
            .copied()
            .unwrap_or_default()
    }

    /// The trees a sapling grows into.
    pub fn tree_grower(self) -> Option<&'static TreeGrower> {
        let grower = *BLOCK_GROWER.get(self.id() as usize)?;
        GROWERS.get(grower as usize)
    }

    /// The configured feature the block grows into (huge mushrooms, fungi,
    /// moss patches).
    pub fn grown_feature(self) -> Option<&'static str> {
        by_id(FEATURES, self)
    }

    pub fn stem(self) -> Option<&'static Stem> {
        STEMS.iter().find(|stem| stem.stem == self)
    }

    /// For an attached stem, its plain stem and the fruit it faces.
    pub fn attached_stem(self) -> Option<(Block, Block)> {
        ATTACHED_STEMS
            .iter()
            .find(|(attached, _, _)| *attached == self)
            .map(|(_, stem, fruit)| (*stem, *fruit))
    }

    /// The plant the block is the head or body of.
    pub fn growing_plant(self) -> Option<&'static GrowingPlant> {
        GROWING_PLANTS
            .iter()
            .find(|plant| plant.head == self || plant.body == self)
    }

    pub fn weathering(self) -> Option<Weathering> {
        by_id(WEATHERING, self)
    }

    /// The block one weathering stage younger (scraped copper).
    pub fn unweathered(self) -> Option<Block> {
        WEATHERING
            .iter()
            .find(|(_, weathering)| weathering.next == Some(self))
            .and_then(|(id, _)| <Block as crate::registry::Registry>::from_id(*id))
    }

    /// For fire blocks, the damage they deal to what stands in them.
    pub fn fire_damage(self) -> Option<f32> {
        by_id(FIRE_DAMAGE, self)
    }

    /// The tall plant bone meal grows a short grass or fern into.
    pub fn grown_plant(self) -> Option<Block> {
        by_id(GROWN, self)
    }

    /// The dead form of a coral block.
    pub fn dead_coral(self) -> Option<Block> {
        by_id(DEAD, self)
    }
}

/// A block tag by resource name.
pub fn block_tag(name: &str) -> &'static [Block] {
    crate::lookup(crate::tag::block::ALL, name).unwrap_or(&[])
}
