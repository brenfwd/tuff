//! The block state properties the light engine reads, extracted from
//! the server jar by `build/LightProperties.java`.

use crate::block::BlockState;

include!(concat!(env!("OUT_DIR"), "/light.rs"));

/// Block face directions, in the game's order (also the light propagation
/// order).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Direction {
    Down,
    Up,
    North,
    South,
    West,
    East,
}

impl Direction {
    pub const ALL: [Self; 6] = [
        Self::Down,
        Self::Up,
        Self::North,
        Self::South,
        Self::West,
        Self::East,
    ];

    pub const fn opposite(self) -> Self {
        match self {
            Self::Down => Self::Up,
            Self::Up => Self::Down,
            Self::North => Self::South,
            Self::South => Self::North,
            Self::West => Self::East,
            Self::East => Self::West,
        }
    }

    pub const fn offset(self) -> (i32, i32, i32) {
        match self {
            Self::Down => (0, -1, 0),
            Self::Up => (0, 1, 0),
            Self::North => (0, 0, -1),
            Self::South => (0, 0, 1),
            Self::West => (-1, 0, 0),
            Self::East => (1, 0, 0),
        }
    }
}

/// The part of a block's face that blocks light, as an id into the table of
/// distinct face shapes. Always empty for states that do not use their shape
/// for light occlusion.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FaceShape(FaceRaw);

impl FaceShape {
    pub const EMPTY: Self = Self(0);

    /// Whether the two faces together cover the whole face of a block.
    pub fn occludes(self, other: Self) -> bool {
        let index = self.0 as usize * FACE_COUNT + other.0 as usize;
        FACE_OCCLUDES[index / 64] >> (index % 64) & 1 == 1
    }
}

impl BlockState {
    pub fn light_emission(self) -> u8 {
        STATE_LIGHT[self.id() as usize] & 0x0F
    }

    pub fn light_dampening(self) -> u8 {
        STATE_LIGHT[self.id() as usize] >> 4
    }

    /// The state cannot occlude, or does not use its shape for light
    /// occlusion.
    pub fn is_empty_light_shape(self) -> bool {
        let id = self.id() as usize;
        STATE_EMPTY_SHAPE[id / 64] >> (id % 64) & 1 == 1
    }

    pub fn light_face(self, direction: Direction) -> FaceShape {
        FaceShape(FACE_SETS[STATE_FACE_SET[self.id() as usize] as usize][direction as usize])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Block;

    fn state(block: Block, properties: &[(&str, &str)]) -> BlockState {
        properties
            .iter()
            .fold(block.default_state(), |state, (name, value)| {
                state.with(name, value).unwrap()
            })
    }

    #[test]
    fn emission_and_dampening_match_vanilla() {
        assert_eq!(Block::Air.default_state().light_emission(), 0);
        assert_eq!(Block::Air.default_state().light_dampening(), 0);
        assert_eq!(Block::Stone.default_state().light_dampening(), 15);
        assert_eq!(Block::Glass.default_state().light_dampening(), 0);
        assert_eq!(Block::Water.default_state().light_dampening(), 1);
        assert_eq!(Block::Torch.default_state().light_emission(), 14);
        assert_eq!(Block::Glowstone.default_state().light_emission(), 15);
        let lamp = |lit| state(Block::RedstoneLamp, &[("lit", lit)]);
        assert_eq!(lamp("true").light_emission(), 15);
        assert_eq!(lamp("false").light_emission(), 0);
    }

    #[test]
    fn slabs_occlude_only_their_full_face() {
        let top = state(Block::OakSlab, &[("type", "top"), ("waterlogged", "false")]);
        assert!(!top.is_empty_light_shape());
        let up = top.light_face(Direction::Up);
        let down = top.light_face(Direction::Down);
        let air = Block::Air.default_state();
        assert!(air.is_empty_light_shape());
        assert_eq!(air.light_face(Direction::Down), FaceShape::EMPTY);
        assert!(up.occludes(FaceShape::EMPTY));
        assert!(!down.occludes(FaceShape::EMPTY));
        assert!(!FaceShape::EMPTY.occludes(FaceShape::EMPTY));
        let bottom = state(
            Block::OakSlab,
            &[("type", "bottom"), ("waterlogged", "false")],
        );
        assert!(
            bottom
                .light_face(Direction::Down)
                .occludes(FaceShape::EMPTY)
        );
    }

    #[test]
    fn stone_uses_dampening_not_shape() {
        let stone = Block::Stone.default_state();
        assert!(stone.is_empty_light_shape());
        assert_eq!(stone.light_face(Direction::Up), FaceShape::EMPTY);
    }
}
