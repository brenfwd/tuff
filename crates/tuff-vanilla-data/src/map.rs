//! Map colours: the palette, each block state's colour, and the map
//! decoration types.

use crate::BlockState;
use crate::registry::MapDecorationType;

/// How bright a map pixel is drawn, as the low two bits of its packed id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Brightness {
    Low = 0,
    Normal = 1,
    High = 2,
    Lowest = 3,
}

impl Brightness {
    pub const fn modifier(self) -> u32 {
        match self {
            Self::Low => 180,
            Self::Normal => 220,
            Self::High => 255,
            Self::Lowest => 135,
        }
    }
}

pub const NONE: u8 = 0;
pub const WATER: u8 = 12;
pub const COLOR_ORANGE: u8 = 15;
pub const COLOR_BROWN: u8 = 26;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecorationInfo {
    /// The texture the client draws, as a resource location.
    pub asset: &'static str,
    pub show_on_item_frame: bool,
    /// Whether the decoration counts against a map's tracked limit.
    pub track_count: bool,
}

impl MapDecorationType {
    pub fn info(self) -> &'static DecorationInfo {
        &DECORATIONS[self.id() as usize]
    }
}

/// A packed map pixel: base colour id times four plus brightness.
pub const fn packed(color: u8, brightness: Brightness) -> u8 {
    (color << 2) | brightness as u8
}

/// The ARGB value a packed pixel is drawn as; 0 for no colour.
pub fn argb(packed: u8) -> u32 {
    let color = packed >> 2;
    if color == NONE {
        return 0;
    }
    let rgb = BASE_COLORS[usize::from(color)];
    let scale = Brightness::from_bits(packed & 3).modifier();
    let channel = |shift: u32| ((rgb >> shift) & 255) * scale / 255;
    0xFF00_0000 | (channel(16) << 16) | (channel(8) << 8) | channel(0)
}

impl Brightness {
    fn from_bits(bits: u8) -> Self {
        match bits & 3 {
            0 => Self::Low,
            1 => Self::Normal,
            2 => Self::High,
            _ => Self::Lowest,
        }
    }
}

impl BlockState {
    /// The base map colour id of this state (0 draws nothing).
    pub fn map_color(self) -> u8 {
        STATE_COLORS[self.id() as usize]
    }
}

include!(concat!(env!("OUT_DIR"), "/maps.rs"));

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Block;

    #[test]
    fn well_known_colors() {
        assert_eq!(Block::Air.default_state().map_color(), NONE);
        assert_eq!(Block::Water.default_state().map_color(), WATER);
        assert_eq!(Block::GrassBlock.default_state().map_color(), 1);
        assert_eq!(packed(WATER, Brightness::High), 50);
        assert_eq!(argb(packed(1, Brightness::High)), 0xFF7FB238);
    }

    #[test]
    fn decoration_flags() {
        assert!(!MapDecorationType::Player.info().show_on_item_frame);
        assert!(MapDecorationType::Frame.info().track_count);
    }
}
