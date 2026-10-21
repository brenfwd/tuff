//! The outer building: wings, entrance, walls in three tiers, the roof and
//! the legs it stands on.

use super::ops::*;
use super::*;

const L: BlockState = LIGHT;
const G: BlockState = GRAY;
const B: BlockState = BLACK;

/// One wing as it stands on the west side; the east wing is its mirror.
const WING: &[Op] = &[
    solid(G, [0, 0, 0, 24, 0, 20]),
    flood([0, 1, 0, 24, 10, 20]),
    series(L, [0, 1, 0, 0, 1, 20], [1, 1, 1, 1, 1, 0], 4),
    series(L, [7, 5, 7, 7, 5, 20], [1, 1, 1, 1, 1, 0], 4),
    series(L, [17, 5, 7, 17, 5, 20], [-1, 1, 1, -1, 1, 0], 4),
    series(L, [24, 1, 0, 24, 1, 20], [-1, 1, 1, -1, 1, 0], 4),
    series(L, [1, 1, 0, 23, 1, 0], [1, 1, 1, -1, 1, 1], 4),
    series(L, [8, 5, 7, 16, 5, 7], [1, 1, 1, -1, 1, 1], 4),
    solid(G, [4, 4, 4, 6, 4, 20]),
    solid(G, [7, 4, 4, 17, 4, 6]),
    solid(G, [18, 4, 4, 20, 4, 20]),
    solid(G, [11, 8, 11, 13, 8, 20]),
    series(L, [12, 9, 12, 12, 9, 12], [0, 0, 3, 0, 0, 3], 3),
    series(L, [5, 5, 5, 5, 5, 5], [0, 0, 3, 0, 0, 3], 6),
    series(L, [19, 5, 7, 19, 5, 7], [0, 0, 3, 0, 0, 3], 5),
    series(L, [8, 5, 5, 8, 5, 5], [3, 0, 0, 3, 0, 0], 4),
    dot(L, 19, 5, 5),
    solid(G, [11, 1, 12, 13, 7, 12]),
    solid(G, [12, 1, 11, 12, 7, 13]),
];

/// The parts drawn when the chunk meets their `[x0, z0, x1, z1]` footprint.
const PARTS: &[([i32; 4], &[Op])] = &[
    ([22, 5, 35, 17], ARCHES),
    ([15, 20, 42, 21], FRONT),
    ([21, 21, 36, 36], ROOF),
    ([0, 21, 6, 58], WEST_LOW),
    ([51, 21, 58, 58], EAST_LOW),
    ([0, 51, 57, 57], BACK_LOW),
    ([7, 21, 13, 50], WEST_MID),
    ([44, 21, 50, 54], EAST_MID),
    ([8, 44, 49, 54], BACK_MID),
    ([14, 21, 20, 43], WEST_HIGH),
    ([37, 21, 43, 43], EAST_HIGH),
    ([15, 37, 42, 43], BACK_HIGH),
];

const fn arch(state: BlockState, b: Cuboid) -> Op {
    series(state, b, [0, 0, 4, 0, 0, 4], 4)
}

const ARCHES: &[Op] = &[
    flood([25, 0, 0, 32, 8, 20]),
    arch(L, [24, 2, 5, 24, 4, 5]),
    arch(L, [22, 4, 5, 23, 4, 5]),
    arch(L, [25, 5, 5, 25, 5, 5]),
    arch(L, [26, 6, 5, 26, 6, 5]),
    arch(LAMP, [26, 5, 5, 26, 5, 5]),
    arch(L, [33, 2, 5, 33, 4, 5]),
    arch(L, [34, 4, 5, 35, 4, 5]),
    arch(L, [32, 5, 5, 32, 5, 5]),
    arch(L, [31, 6, 5, 31, 6, 5]),
    arch(LAMP, [31, 5, 5, 31, 5, 5]),
    arch(G, [27, 6, 5, 30, 6, 5]),
];

const fn front(state: BlockState, x0: i32, x1: i32, y0: i32, y1: i32) -> Op {
    solid(state, [x0, y0, 21, x1, y1, 21])
}

const fn front_water(x0: i32, x1: i32, y0: i32, y1: i32) -> Op {
    flood([x0, y0, 21, x1, y1, 21])
}

const FRONT: &[Op] = &[
    front(G, 15, 42, 0, 0),
    front_water(26, 31, 1, 3),
    front(G, 21, 36, 12, 12),
    front(G, 17, 40, 11, 11),
    front(G, 16, 41, 10, 10),
    front(G, 15, 42, 7, 9),
    front(G, 16, 41, 6, 6),
    front(G, 17, 40, 5, 5),
    front(G, 21, 36, 4, 4),
    front(G, 22, 26, 3, 3),
    front(G, 31, 35, 3, 3),
    front(G, 23, 25, 2, 2),
    front(G, 32, 34, 2, 2),
    solid(L, [28, 4, 20, 29, 4, 21]),
    front(L, 27, 27, 3, 3),
    front(L, 30, 30, 3, 3),
    front(L, 26, 26, 2, 2),
    front(L, 31, 31, 2, 2),
    front(L, 25, 25, 1, 1),
    front(L, 32, 32, 1, 1),
    series(B, [28, 6, 21, 28, 6, 21], [-1, 1, 0, -1, 1, 0], 7),
    series(B, [29, 6, 21, 29, 6, 21], [1, 1, 0, 1, 1, 0], 7),
    series(B, [28, 9, 21, 28, 9, 21], [-1, 1, 0, -1, 1, 0], 4),
    series(B, [29, 9, 21, 29, 9, 21], [1, 1, 0, 1, 1, 0], 4),
    front(B, 28, 29, 12, 12),
    series(B, [22, 8, 21, 22, 9, 21], [-2, 0, 0, -2, 0, 0], 3),
    series(B, [35, 8, 21, 35, 9, 21], [2, 0, 0, 2, 0, 0], 3),
    front_water(15, 42, 13, 15),
    front_water(15, 15, 1, 6),
    front_water(16, 16, 1, 5),
    front_water(17, 20, 1, 4),
    front_water(21, 21, 1, 3),
    front_water(22, 22, 1, 2),
    front_water(23, 24, 1, 1),
    front_water(42, 42, 1, 6),
    front_water(41, 41, 1, 5),
    front_water(37, 40, 1, 4),
    front_water(36, 36, 1, 3),
    front_water(33, 34, 1, 1),
    front_water(35, 35, 1, 2),
];

const ROOF: &[Op] = &[
    solid(G, [21, 0, 22, 36, 0, 36]),
    flood([21, 1, 22, 36, 23, 36]),
    series(L, [21, 13, 21, 36, 13, 21], [1, 1, 1, -1, 1, 1], 4),
    series(L, [21, 13, 36, 36, 13, 36], [1, 1, -1, -1, 1, -1], 4),
    series(L, [21, 13, 22, 21, 13, 35], [1, 1, 1, 1, 1, -1], 4),
    series(L, [36, 13, 22, 36, 13, 35], [-1, 1, 1, -1, 1, -1], 4),
    solid(G, [25, 16, 25, 32, 16, 32]),
    solid(L, [25, 17, 25, 25, 19, 25]),
    solid(L, [32, 17, 25, 32, 19, 25]),
    solid(L, [25, 17, 32, 25, 19, 32]),
    solid(L, [32, 17, 32, 32, 19, 32]),
    dot(L, 26, 20, 26),
    dot(L, 27, 21, 27),
    dot(LAMP, 27, 20, 27),
    dot(L, 26, 20, 31),
    dot(L, 27, 21, 30),
    dot(LAMP, 27, 20, 30),
    dot(L, 31, 20, 31),
    dot(L, 30, 21, 30),
    dot(LAMP, 30, 20, 30),
    dot(L, 31, 20, 26),
    dot(L, 30, 21, 27),
    dot(LAMP, 30, 20, 27),
    solid(G, [28, 21, 27, 29, 21, 27]),
    solid(G, [27, 21, 28, 27, 21, 29]),
    solid(G, [28, 21, 30, 29, 21, 30]),
    solid(G, [30, 21, 28, 30, 21, 29]),
];

const WEST_LOW: &[Op] = &[
    solid(G, [0, 0, 21, 6, 0, 57]),
    flood([0, 1, 21, 6, 7, 57]),
    solid(G, [4, 4, 21, 6, 4, 53]),
    series(L, [0, 1, 21, 0, 1, 57], [1, 1, 0, 1, 1, -1], 4),
    series(L, [5, 5, 23, 5, 5, 23], [0, 0, 3, 0, 0, 3], 10),
    dot(L, 5, 5, 52),
    solid(G, [4, 1, 52, 6, 3, 52]),
    solid(G, [5, 1, 51, 5, 3, 53]),
];

const EAST_LOW: &[Op] = &[
    solid(G, [51, 0, 21, 57, 0, 57]),
    flood([51, 1, 21, 57, 7, 57]),
    solid(G, [51, 4, 21, 53, 4, 53]),
    series(L, [57, 1, 21, 57, 1, 57], [-1, 1, 0, -1, 1, -1], 4),
    series(L, [52, 5, 23, 52, 5, 23], [0, 0, 3, 0, 0, 3], 10),
    dot(L, 52, 5, 52),
    solid(G, [51, 1, 52, 53, 3, 52]),
    solid(G, [52, 1, 51, 52, 3, 53]),
];

const BACK_LOW: &[Op] = &[
    solid(G, [7, 0, 51, 50, 0, 57]),
    flood([7, 1, 51, 50, 10, 57]),
    series(L, [1, 1, 57, 56, 1, 57], [1, 1, -1, -1, 1, -1], 4),
];

const WEST_MID: &[Op] = &[
    solid(G, [7, 0, 21, 13, 0, 50]),
    flood([7, 1, 21, 13, 10, 50]),
    solid(G, [11, 8, 21, 13, 8, 53]),
    series(L, [7, 5, 21, 7, 5, 54], [1, 1, 0, 1, 1, 0], 4),
    series(L, [12, 9, 21, 12, 9, 21], [0, 0, 3, 0, 0, 3], 9),
];

const EAST_MID: &[Op] = &[
    solid(G, [44, 0, 21, 50, 0, 50]),
    flood([44, 1, 21, 50, 10, 50]),
    solid(G, [44, 8, 21, 46, 8, 53]),
    series(L, [50, 5, 21, 50, 5, 54], [-1, 1, 0, -1, 1, 0], 4),
    series(L, [45, 9, 21, 45, 9, 21], [0, 0, 3, 0, 0, 3], 9),
];

/// The light blocks on each buttress of the back middle wall.
const fn buttress(first_x: i32, y: i32, z: i32) -> Op {
    series(L, [first_x, y, z, first_x, y, z], [6, 0, 0, 6, 0, 0], 3)
}

const BACK_MID: &[Op] = &[
    solid(G, [14, 0, 44, 43, 0, 50]),
    flood([14, 1, 44, 43, 10, 50]),
    series(L, [12, 9, 45, 12, 9, 45], [3, 0, 0, 3, 0, 0], 12),
    series(L, [12, 9, 52, 12, 9, 52], [3, 0, 0, 3, 0, 0], 12),
    buttress(12, 9, 47),
    buttress(12, 9, 50),
    buttress(12, 10, 45),
    buttress(12, 10, 46),
    buttress(12, 10, 51),
    buttress(12, 10, 52),
    buttress(12, 11, 47),
    buttress(12, 11, 50),
    buttress(12, 12, 48),
    buttress(12, 12, 49),
    buttress(33, 9, 47),
    buttress(33, 9, 50),
    buttress(33, 10, 45),
    buttress(33, 10, 46),
    buttress(33, 10, 51),
    buttress(33, 10, 52),
    buttress(33, 11, 47),
    buttress(33, 11, 50),
    buttress(33, 12, 48),
    buttress(33, 12, 49),
    series(G, [8, 5, 54, 49, 5, 54], [1, 1, 0, -1, 1, 0], 3),
    solid(L, [11, 8, 54, 46, 8, 54]),
    solid(G, [14, 8, 44, 43, 8, 53]),
];

const WEST_HIGH: &[Op] = &[
    solid(G, [14, 0, 21, 20, 0, 43]),
    flood([14, 1, 22, 20, 14, 43]),
    solid(G, [18, 12, 22, 20, 12, 39]),
    solid(L, [18, 12, 21, 20, 12, 21]),
    series(L, [14, 9, 21, 14, 9, 43], [1, 1, 0, 1, 1, -1], 4),
    series(L, [19, 13, 23, 19, 13, 23], [0, 0, 3, 0, 0, 3], 6),
];

const EAST_HIGH: &[Op] = &[
    solid(G, [37, 0, 21, 43, 0, 43]),
    flood([37, 1, 22, 43, 14, 43]),
    solid(G, [37, 12, 22, 39, 12, 39]),
    solid(L, [37, 12, 21, 39, 12, 21]),
    series(L, [43, 9, 21, 43, 9, 43], [-1, 1, 0, -1, 1, -1], 4),
    series(L, [38, 13, 23, 38, 13, 23], [0, 0, 3, 0, 0, 3], 6),
];

const BACK_HIGH: &[Op] = &[
    solid(G, [21, 0, 37, 36, 0, 43]),
    flood([21, 1, 37, 36, 14, 43]),
    solid(G, [21, 12, 37, 36, 12, 39]),
    series(L, [15, 9, 43, 42, 9, 43], [1, 1, -1, -1, 1, -1], 4),
    series(L, [21, 13, 38, 21, 13, 38], [3, 0, 0, 3, 0, 0], 6),
];

/// Whether column `(col, row)` of the 7x7 leg grid carries a leg: the full
/// west and east edges, the back row, and the front row but for the door.
fn has_leg(col: i32, row: i32) -> bool {
    col == 0 || col == 6 || row == 6 || (row == 0 && col != 3)
}

pub(super) fn draw_shell(d: &mut Draw, min_y: i32) {
    let top = d.level.sea_level().max(64) - min_y;
    d.flood([0, 0, 0, 58, top, 58]);
    for (shift_x, mirror_width) in [(0, None), (33, Some(24))] {
        if d.meets(shift_x, 0, shift_x + 23, 20) {
            replay_at(
                d,
                WING,
                Place {
                    shift_x,
                    mirror_width,
                },
            );
        }
    }
    for (footprint, ops) in PARTS {
        let [x0, z0, x1, z1] = *footprint;
        if d.meets(x0, z0, x1, z1) {
            replay(d, ops);
        }
    }
    for col in 0..7 {
        for row in 0..7 {
            if !has_leg(col, row) {
                continue;
            }
            let (x, z) = (col * 9, row * 9);
            for dx in 0..4 {
                for dz in 0..4 {
                    d.put(LIGHT, x + dx, 0, z + dz);
                    d.piece
                        .fill_column_down(d.level, LIGHT, x + dx, -1, z + dz, &d.chunk);
                }
            }
        }
    }
    for k in 0..5 {
        let (near, far, y) = (-1 - k, 58 + k, k * 2);
        d.flood([near, y, near, near, 23, far]);
        d.flood([far, y, near, far, 23, far]);
        d.flood([-k, y, near, 57 + k, 23, near]);
        d.flood([-k, y, far, 57 + k, 23, far]);
    }
}
