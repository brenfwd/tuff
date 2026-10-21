//! The frame of reference the experimental redstone rules carry with each
//! neighbour update: an up and a front direction plus a left or right side
//! bias, fixing the order a wire looks at and updates its neighbours in.

use tuff_vanilla_data::block::{Axis, Facing};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SideBias {
    Left,
    Right,
}

impl SideBias {
    pub const fn opposite(self) -> Self {
        match self {
            Self::Left => Self::Right,
            Self::Right => Self::Left,
        }
    }
}

/// One of the 48 orientations; `index` is the protocol id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Orientation {
    up: Facing,
    front: Facing,
    bias: SideBias,
}

const BY_ID: [Facing; 6] = [
    Facing::Down,
    Facing::Up,
    Facing::North,
    Facing::South,
    Facing::West,
    Facing::East,
];

fn positive(facing: Facing) -> bool {
    let (x, y, z) = facing.step();
    x + y + z > 0
}

fn vector(facing: Facing) -> [i32; 3] {
    let (x, y, z) = facing.step();
    [x, y, z]
}

fn facing_of([x, y, z]: [i32; 3]) -> Facing {
    BY_ID
        .into_iter()
        .find(|f| f.step() == (x, y, z))
        .expect("a unit axis vector")
}

impl Orientation {
    pub const COUNT: usize = 48;

    /// # Panics
    /// If `up` and `front` share an axis.
    pub fn new(up: Facing, front: Facing, bias: SideBias) -> Self {
        assert!(up.axis() != front.axis(), "up and front share an axis");
        Self { up, front, bias }
    }

    pub fn up(self) -> Facing {
        self.up
    }

    pub fn front(self) -> Facing {
        self.front
    }

    pub fn side_bias(self) -> SideBias {
        self.bias
    }

    pub fn index(self) -> u8 {
        let front_axis_key = if self.up.axis() == Axis::Y {
            self.front.axis() == Axis::X
        } else {
            self.front.axis() == Axis::Y
        };
        let front_key = (u8::from(front_axis_key) << 1) | u8::from(!positive(self.front));
        let bias = match self.bias {
            SideBias::Left => 0,
            SideBias::Right => 1,
        };
        ((((self.up.id() as u8) << 2) + front_key) << 1) + bias
    }

    pub fn from_index(index: u8) -> Option<Self> {
        if usize::from(index) >= Self::COUNT {
            return None;
        }
        let up = BY_ID[usize::from(index >> 3)];
        let bias = if index & 1 == 0 {
            SideBias::Left
        } else {
            SideBias::Right
        };
        BY_ID
            .into_iter()
            .filter(|front| front.axis() != up.axis())
            .map(|front| Self { up, front, bias })
            .find(|o| o.index() == index)
    }

    /// An orientation drawn uniformly from `roll`, a value below
    /// [`Self::COUNT`].
    pub fn from_roll(roll: u32) -> Self {
        Self::from_index((roll % Self::COUNT as u32) as u8).expect("index in range")
    }

    pub fn side(self) -> Facing {
        let [fx, fy, fz] = vector(self.front);
        let [ux, uy, uz] = vector(self.up);
        let right = facing_of([fy * uz - fz * uy, fz * ux - fx * uz, fx * uy - fy * ux]);
        match self.bias {
            SideBias::Right => right,
            SideBias::Left => right.opposite(),
        }
    }

    pub fn with_up(self, up: Facing) -> Self {
        let front = if up == self.front {
            self.up.opposite()
        } else if up == self.front.opposite() {
            self.up
        } else {
            self.front
        };
        Self { up, front, ..self }
    }

    pub fn with_front(self, front: Facing) -> Self {
        let up = if front == self.up {
            self.front.opposite()
        } else if front == self.up.opposite() {
            self.front
        } else {
            self.up
        };
        Self { up, front, ..self }
    }

    pub fn with_front_preserve_up(self, front: Facing) -> Self {
        if front.axis() == self.up.axis() {
            self
        } else {
            self.with_front(front)
        }
    }

    pub fn with_side_bias(self, bias: SideBias) -> Self {
        Self { bias, ..self }
    }

    pub fn with_mirror(self) -> Self {
        self.with_side_bias(self.bias.opposite())
    }

    /// All six neighbours in the order this orientation visits them.
    pub fn directions(self) -> [Facing; 6] {
        let side = self.side();
        [
            self.front.opposite(),
            self.front,
            side,
            side.opposite(),
            self.up.opposite(),
            self.up,
        ]
    }

    /// The four neighbours off the up axis, in visiting order.
    pub fn horizontal_directions(self) -> [Facing; 4] {
        let [a, b, c, d, ..] = self.directions();
        [a, b, c, d]
    }

    /// The two neighbours along the up axis, in visiting order.
    pub fn vertical_directions(self) -> [Facing; 2] {
        [self.up.opposite(), self.up]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn every_index_round_trips_and_ids_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for index in 0..48u8 {
            let o = Orientation::from_index(index).expect("valid index");
            assert_eq!(o.index(), index);
            assert!(seen.insert((o.up, o.front, o.bias)));
        }
        assert_eq!(Orientation::from_index(48), None);
    }

    #[tuff_core::watchdog]
    #[test]
    fn indices_match_the_game_numbering() {
        assert_eq!(
            Orientation::new(Facing::Up, Facing::North, SideBias::Left).index(),
            10
        );
        assert_eq!(
            Orientation::new(Facing::Up, Facing::East, SideBias::Right).index(),
            13
        );
        assert_eq!(
            Orientation::new(Facing::North, Facing::Down, SideBias::Left).index(),
            22
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn up_north_left_visits_south_north_west_east_down_up() {
        let o = Orientation::new(Facing::Up, Facing::North, SideBias::Left);
        assert_eq!(o.side(), Facing::West);
        assert_eq!(
            o.directions(),
            [
                Facing::South,
                Facing::North,
                Facing::West,
                Facing::East,
                Facing::Down,
                Facing::Up
            ]
        );
        assert_eq!(o.with_mirror().side(), Facing::East);
    }

    #[tuff_core::watchdog]
    #[test]
    fn turning_front_onto_the_up_axis_tips_up_over() {
        let o = Orientation::new(Facing::Up, Facing::North, SideBias::Left);
        let tipped = o.with_front(Facing::Up);
        assert_eq!((tipped.up(), tipped.front()), (Facing::South, Facing::Up));
        assert_eq!(o.with_front_preserve_up(Facing::Down), o);
        let raised = o.with_up(Facing::North);
        assert_eq!((raised.up(), raised.front()), (Facing::North, Facing::Down));
    }
}
