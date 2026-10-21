//! Block entities from game code: which blocks have one, and per type what
//! a new block entity holds, how many item slots it has, whether clients
//! are sent its data on block changes, and which item components it takes
//! over from the item that places it.

use crate::registry::{Block, BlockEntityType, DataComponentType, Item};

struct TypeData {
    blocks: &'static [Block],
    default: Option<&'static str>,
    client: Option<&'static str>,
    size: Option<u16>,
    updates: bool,
    op_only: bool,
    implicit: &'static [DataComponentType],
    discard: &'static [&'static str],
}

impl Block {
    /// The type of block entity this block has, if any.
    pub fn block_entity(self) -> Option<BlockEntityType> {
        BLOCK_ENTITY_TYPES[self as usize].map(|(kind, _)| kind)
    }

    /// Whether this block replacing `old` keeps `old`'s block entity (as
    /// copper chests do while they oxidize).
    pub fn keeps_block_entity_of(self, old: Block) -> bool {
        KEEPS[self as usize].contains(&old)
    }

    /// Whether placing this block creates its block entity (moving pistons
    /// get theirs another way).
    pub fn creates_block_entity(self) -> bool {
        BLOCK_ENTITY_TYPES[self as usize].is_some_and(|(_, created)| created)
    }
}

impl Item {
    /// Whether using the item on a sign changes its text (dyes, ink sacs)
    /// or waxes it (honeycomb).
    pub fn is_sign_applicator(self) -> bool {
        SIGN_APPLICATORS.contains(&self)
    }
}

impl BlockEntityType {
    fn data(self) -> &'static TypeData {
        &TYPE_DATA[self.id() as usize]
    }

    /// The blocks whose block entity has this type.
    pub fn blocks(self) -> &'static [Block] {
        self.data().blocks
    }

    /// The saved data of a new block entity, as SNBT without its id and
    /// position; `None` for types no block creates.
    pub fn default_snbt(self) -> Option<&'static str> {
        self.data().default
    }

    /// The data clients receive for a new block entity, as SNBT.
    pub fn default_client_snbt(self) -> Option<&'static str> {
        self.data().client
    }

    /// The number of item slots, for block entities that hold items.
    pub fn container_size(self) -> Option<usize> {
        self.data().size.map(usize::from)
    }

    /// Whether clients are sent the block entity's data whenever its block
    /// changes.
    pub fn sends_updates(self) -> bool {
        self.data().updates
    }

    /// Whether only operators in creative mode may place the block with
    /// `block_entity_data`.
    pub fn only_op_can_set_data(self) -> bool {
        self.data().op_only
    }

    /// The item components the block entity takes from the item that
    /// places it (and gives back to its drops), in the order it reads them.
    pub fn implicit_components(self) -> &'static [DataComponentType] {
        self.data().implicit
    }

    /// The saved keys holding those components (and other state that is
    /// left out when the data is copied into an item).
    pub fn component_keys(self) -> &'static [&'static str] {
        self.data().discard
    }
}

include!(concat!(env!("OUT_DIR"), "/block_entities.rs"));

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_type_has_blocks_and_blocks_point_back() {
        for kind in BlockEntityType::ALL {
            assert!(!kind.blocks().is_empty(), "{kind:?}");
            for block in kind.blocks() {
                assert_eq!(block.block_entity(), Some(kind));
            }
        }
        assert_eq!(Block::OakSign.block_entity(), Some(BlockEntityType::Sign));
        assert_eq!(
            Block::WhiteWallBanner.block_entity(),
            Some(BlockEntityType::Banner)
        );
        assert_eq!(Block::Stone.block_entity(), None);
        assert!(Block::Bell.creates_block_entity());
        assert!(Item::Honeycomb.is_sign_applicator());
        assert!(Item::GlowInkSac.is_sign_applicator());
        assert!(Item::RedDye.is_sign_applicator());
        assert!(!Item::Stick.is_sign_applicator());
        assert!(Block::ExposedCopperChest.keeps_block_entity_of(Block::CopperChest));
        assert!(!Block::CopperChest.keeps_block_entity_of(Block::Chest));
        assert!(!Block::MovingPiston.creates_block_entity());
        assert_eq!(BlockEntityType::Piston.default_snbt(), None);
    }

    #[test]
    fn new_block_entities_hold_the_games_defaults() {
        assert_eq!(
            BlockEntityType::Chest.default_snbt(),
            Some("{Items:[],components:{}}")
        );
        assert_eq!(BlockEntityType::Chest.container_size(), Some(27));
        assert!(BlockEntityType::Sign.sends_updates());
        assert!(BlockEntityType::Sign.only_op_can_set_data());
        assert!(!BlockEntityType::Chest.sends_updates());
        assert_eq!(
            BlockEntityType::Skull.implicit_components(),
            &[
                DataComponentType::Profile,
                DataComponentType::NoteBlockSound,
                DataComponentType::CustomName
            ]
        );
        assert_eq!(
            BlockEntityType::Banner.component_keys(),
            &["patterns", "CustomName"]
        );
    }
}
