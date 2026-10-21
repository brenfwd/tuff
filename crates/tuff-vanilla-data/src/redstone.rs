//! What the redstone simulation needs to know about blocks, dumped from the
//! game: which states conduct, which emit and how strongly toward each
//! side, where comparators read from, and each block's redstone behaviour.

use crate::block::{BlockState, Facing};
use crate::registry::{Block, Item, SoundEvent};

include!(concat!(env!("OUT_DIR"), "/redstone.rs"));

/// Flag bits: 1 conductor and 2 signal source (both also exposed through
/// the entity data, which reads them for spawning), 4 analog output.
const ANALOG: u8 = 4;

/// Where a comparator reading a block gets its value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnalogSource {
    /// The state alone ([`BlockState::state_analog_output`]).
    State,
    /// How full the block's container is.
    Container,
    /// Something this data does not model.
    Unsupported,
}

/// Which entities press a pressure plate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sensitivity {
    /// Any entity.
    Everything,
    /// Living entities only.
    Mobs,
    /// Any entity, the signal growing with their count up to this many.
    Weighted(u32),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PressurePlate {
    pub sensitivity: Sensitivity,
    /// Ticks between checks while pressed.
    pub pressed_ticks: u32,
    pub click_on: SoundEvent,
    pub click_off: SoundEvent,
}

/// What a dispenser does with an item that has its own behaviour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dispense {
    /// Drops it, as for any other item.
    Default,
    /// Shoots it as a projectile.
    Projectile,
    /// Item-specific behaviour (buckets, TNT, bone meal, ...).
    Special,
}

impl Item {
    /// The dispenser behaviour registered for the item, if any; other
    /// items are equipped (if equippable) or dropped.
    pub fn dispense(self) -> Option<Dispense> {
        dispense_table(self)
    }
}

fn flags(state: BlockState) -> u8 {
    REDSTONE_FLAGS[state.id() as usize]
}

fn block_lookup<T: Copy>(table: &[(Block, T)], block: Block) -> Option<T> {
    table
        .binary_search_by_key(&(block as usize), |(b, _)| *b as usize)
        .ok()
        .map(|index| table[index].1)
}

impl BlockState {
    /// Whether comparators read something from the state's block.
    pub fn has_analog_output(self) -> bool {
        flags(self) & ANALOG != 0
    }

    fn signals(self) -> (u32, u32) {
        SIGNALS
            .binary_search_by_key(&(self.id() as u16), |(id, _, _)| *id)
            .ok()
            .map_or((0, 0), |index| (SIGNALS[index].1, SIGNALS[index].2))
    }

    /// The weak signal the state gives by itself toward a block that
    /// reads it looking in `direction` (from the reader to this block).
    /// Blocks whose signal depends on their surroundings (wire,
    /// comparators) are not described by this.
    pub fn static_signal(self, direction: Facing) -> u8 {
        (self.signals().0 >> (4 * direction.id()) & 15) as u8
    }

    /// The strong signal, as for [`Self::static_signal`].
    pub fn static_direct_signal(self, direction: Facing) -> u8 {
        (self.signals().1 >> (4 * direction.id()) & 15) as u8
    }

    /// The comparator output of a state whose block's
    /// [`Block::analog_source`] is [`AnalogSource::State`].
    pub fn state_analog_output(self) -> u8 {
        ANALOG_STATES
            .binary_search_by_key(&(self.id() as u16), |(id, _)| *id)
            .ok()
            .map_or(0, |index| ANALOG_STATES[index].1)
    }
}

impl Block {
    /// The block's redstone behaviour, if it has one.
    pub fn component(self) -> Option<Component> {
        block_lookup(&COMPONENTS, self)
    }

    /// Where comparators read this block from, for blocks with an analog
    /// output.
    pub fn analog_source(self) -> Option<AnalogSource> {
        block_lookup(&ANALOG_SOURCES, self)
    }

    pub fn pressure_plate(self) -> Option<PressurePlate> {
        block_lookup(&PLATES, self)
    }

    /// Whether arrows press this button.
    pub fn pressed_by_arrows(self) -> bool {
        ARROW_BUTTONS
            .binary_search_by_key(&(self as usize), |b| *b as usize)
            .is_ok()
    }
}

#[cfg(test)]
mod cauldron_tests {
    use super::*;

    #[test]
    fn cauldrons_read_their_fill_level_to_comparators() {
        assert!(Block::Cauldron.default_state().has_analog_output());
        assert_eq!(Block::Cauldron.default_state().state_analog_output(), 0);
        assert_eq!(Block::LavaCauldron.default_state().state_analog_output(), 3);
        for block in [Block::WaterCauldron, Block::PowderSnowCauldron] {
            for level in 1..=3u8 {
                let state = block.default_state().with_level(level).unwrap();
                assert!(state.has_analog_output());
                assert_eq!(state.state_analog_output(), level);
            }
        }
    }
}
