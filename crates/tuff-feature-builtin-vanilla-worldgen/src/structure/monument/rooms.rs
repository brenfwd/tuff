//! The rooms on the grid inside the building, plus the wing halls and the
//! penthouse.

use super::ops::*;
use super::*;

const L: BlockState = LIGHT;
const G: BlockState = GRAY;
const B: BlockState = BLACK;

/// What a cell side gets drawn as, depending on whether it is open.
struct Side {
    dir: usize,
    open: &'static [Op],
    closed: &'static [Op],
}

const fn when_open(dir: usize, open: &'static [Op]) -> Side {
    Side {
        dir,
        open,
        closed: &[],
    }
}

fn draw_sides(d: &mut Draw, g: &Graph, cell: usize, sides: &[Side]) {
    for side in sides {
        let ops = if g.opening(cell, side.dir) {
            side.open
        } else {
            side.closed
        };
        replay(d, ops);
    }
}

/// The floor of a cell above the bottom tier, and the ceiling cap where
/// nothing is stacked on top.
fn floor_and_cap(d: &mut Draw, g: &Graph, r: usize, ceiling: &[Op]) {
    if above_grid(g, r) {
        d.floor(0, 0, g.opening(r, DOWN));
    }
    if !g.has_link(r, UP) {
        replay(d, ceiling);
    }
}

const SIMPLE_CAP: &[Op] = &[Op::Cap(G, [1, 4, 1, 6, 4, 6])];

/// A three-high wall slab: light, gray band, light.
const fn banded(x0: i32, z0: i32, x1: i32, z1: i32) -> [Op; 3] {
    [
        solid(L, [x0, 3, z0, x1, 3, z1]),
        solid(G, [x0, 2, z0, x1, 2, z1]),
        solid(L, [x0, 1, z0, x1, 1, z1]),
    ]
}

const ALCOVES: &[Op] = &[
    solid(L, [0, 1, 0, 2, 1, 2]),
    solid(L, [0, 3, 0, 2, 3, 2]),
    solid(G, [0, 2, 0, 0, 2, 2]),
    solid(G, [1, 2, 0, 2, 2, 0]),
    dot(LAMP, 1, 2, 1),
    solid(L, [5, 1, 0, 7, 1, 2]),
    solid(L, [5, 3, 0, 7, 3, 2]),
    solid(G, [7, 2, 0, 7, 2, 2]),
    solid(G, [5, 2, 0, 6, 2, 0]),
    dot(LAMP, 6, 2, 1),
    solid(L, [0, 1, 5, 2, 1, 7]),
    solid(L, [0, 3, 5, 2, 3, 7]),
    solid(G, [0, 2, 5, 0, 2, 7]),
    solid(G, [1, 2, 7, 2, 2, 7]),
    dot(LAMP, 1, 2, 6),
    solid(L, [5, 1, 5, 7, 1, 7]),
    solid(L, [5, 3, 5, 7, 3, 7]),
    solid(G, [7, 2, 5, 7, 2, 7]),
    solid(G, [5, 2, 7, 6, 2, 7]),
    dot(LAMP, 6, 2, 6),
];

const ALCOVE_DOORS: &[Side] = &[
    Side {
        dir: SOUTH,
        open: &[solid(L, [3, 3, 0, 4, 3, 0])],
        closed: &[
            solid(L, [3, 3, 0, 4, 3, 1]),
            solid(G, [3, 2, 0, 4, 2, 0]),
            solid(L, [3, 1, 0, 4, 1, 1]),
        ],
    },
    Side {
        dir: NORTH,
        open: &[solid(L, [3, 3, 7, 4, 3, 7])],
        closed: &[
            solid(L, [3, 3, 6, 4, 3, 7]),
            solid(G, [3, 2, 7, 4, 2, 7]),
            solid(L, [3, 1, 6, 4, 1, 7]),
        ],
    },
    Side {
        dir: WEST,
        open: &[solid(L, [0, 3, 3, 0, 3, 4])],
        closed: &[
            solid(L, [0, 3, 3, 1, 3, 4]),
            solid(G, [0, 2, 3, 0, 2, 4]),
            solid(L, [0, 1, 3, 1, 1, 4]),
        ],
    },
    Side {
        dir: EAST,
        open: &[solid(L, [7, 3, 3, 7, 3, 4])],
        closed: &[
            solid(L, [6, 3, 3, 7, 3, 4]),
            solid(G, [7, 2, 3, 7, 2, 4]),
            solid(L, [6, 1, 3, 7, 1, 4]),
        ],
    },
];

const COLUMNS: &[Op] = &[
    solid(L, [2, 1, 2, 2, 3, 2]),
    solid(L, [2, 1, 5, 2, 3, 5]),
    solid(L, [5, 1, 5, 5, 3, 5]),
    solid(L, [5, 1, 2, 5, 3, 2]),
    dot(LAMP, 2, 2, 2),
    dot(LAMP, 2, 2, 5),
    dot(LAMP, 5, 2, 5),
    dot(LAMP, 5, 2, 2),
    solid(L, [0, 1, 0, 1, 3, 0]),
    solid(L, [0, 1, 1, 0, 3, 1]),
    solid(L, [0, 1, 7, 1, 3, 7]),
    solid(L, [0, 1, 6, 0, 3, 6]),
    solid(L, [6, 1, 7, 7, 3, 7]),
    solid(L, [7, 1, 6, 7, 3, 6]),
    solid(L, [6, 1, 0, 7, 3, 0]),
    solid(L, [7, 1, 1, 7, 3, 1]),
    dot(G, 1, 2, 0),
    dot(G, 0, 2, 1),
    dot(G, 1, 2, 7),
    dot(G, 0, 2, 6),
    dot(G, 6, 2, 7),
    dot(G, 7, 2, 6),
    dot(G, 6, 2, 0),
    dot(G, 7, 2, 1),
];

const fn closed_band(dir: usize, ops: &'static [Op; 3]) -> Side {
    Side {
        dir,
        open: &[],
        closed: ops,
    }
}

const COLUMN_WALLS: &[Side] = &[
    closed_band(SOUTH, &banded(1, 0, 6, 0)),
    closed_band(NORTH, &banded(1, 7, 6, 7)),
    closed_band(WEST, &banded(0, 1, 0, 6)),
    closed_band(EAST, &banded(7, 1, 7, 6)),
];

/// Light, dark and light rings, with dark posts in each doorway.
const DARK_WALLS: &[Op] = &[
    solid(L, [0, 1, 0, 0, 1, 7]),
    solid(L, [7, 1, 0, 7, 1, 7]),
    solid(L, [1, 1, 0, 6, 1, 0]),
    solid(L, [1, 1, 7, 6, 1, 7]),
    solid(B, [0, 2, 0, 0, 2, 7]),
    solid(B, [7, 2, 0, 7, 2, 7]),
    solid(B, [1, 2, 0, 6, 2, 0]),
    solid(B, [1, 2, 7, 6, 2, 7]),
    solid(L, [0, 3, 0, 0, 3, 7]),
    solid(L, [7, 3, 0, 7, 3, 7]),
    solid(L, [1, 3, 0, 6, 3, 0]),
    solid(L, [1, 3, 7, 6, 3, 7]),
    solid(B, [0, 1, 3, 0, 2, 4]),
    solid(B, [7, 1, 3, 7, 2, 4]),
    solid(B, [3, 1, 0, 4, 2, 0]),
    solid(B, [3, 1, 7, 4, 2, 7]),
];

const SOUTH_GAP: &[Op] = &[flood([3, 1, 0, 4, 2, 0])];

const DARK_DOORS: &[Side] = &[
    when_open(SOUTH, SOUTH_GAP),
    when_open(NORTH, &[flood([3, 1, 7, 4, 2, 7])]),
    when_open(WEST, &[flood([0, 1, 3, 0, 2, 4])]),
    when_open(EAST, &[flood([7, 1, 3, 7, 2, 4])]),
];

const PILLAR: [Op; 3] = banded(3, 3, 4, 4);

pub(super) fn simple_room(
    d: &mut Draw,
    g: &Graph,
    r: usize,
    design: i32,
    random: &mut WorldgenRandom,
) {
    floor_and_cap(d, g, r, SIMPLE_CAP);
    let pillar = design != 0
        && random.next_bool()
        && !g.opening(r, DOWN)
        && !g.opening(r, UP)
        && g.rooms[r].opening_count() > 1;
    let (body, sides): (&[Op], &[Side]) = match design {
        0 => (ALCOVES, ALCOVE_DOORS),
        1 => (COLUMNS, COLUMN_WALLS),
        _ => (DARK_WALLS, DARK_DOORS),
    };
    replay(d, body);
    draw_sides(d, g, r, sides);
    if pillar {
        let [top, mid, bottom] = PILLAR;
        replay(d, &[bottom, mid, top]);
    }
}

pub(super) fn simple_top_room(d: &mut Draw, g: &Graph, r: usize, random: &mut WorldgenRandom) {
    floor_and_cap(d, g, r, SIMPLE_CAP);
    let sponge = Block::WetSponge.default_state();
    for x in 1..=6 {
        for z in 1..=6 {
            if random.next_int_bounded(3) == 0 {
                continue;
            }
            let low = random.next_int_bounded(4) == 0;
            d.solid([x, if low { 2 } else { 3 }, z, x, 3, z], sponge);
        }
    }
    replay(d, DARK_WALLS);
    draw_sides(d, g, r, &[when_open(SOUTH, SOUTH_GAP)]);
}

const TALL_RING: &[Op] = &[
    solid(L, [0, 4, 0, 0, 4, 7]),
    solid(L, [7, 4, 0, 7, 4, 7]),
    solid(L, [1, 4, 0, 6, 4, 0]),
    solid(L, [1, 4, 7, 6, 4, 7]),
    solid(L, [2, 4, 1, 2, 4, 2]),
    solid(L, [1, 4, 2, 1, 4, 2]),
    solid(L, [5, 4, 1, 5, 4, 2]),
    solid(L, [6, 4, 2, 6, 4, 2]),
    solid(L, [2, 4, 5, 2, 4, 6]),
    solid(L, [1, 4, 5, 1, 4, 5]),
    solid(L, [5, 4, 5, 5, 4, 6]),
    solid(L, [6, 4, 5, 6, 4, 5]),
];

/// The four walls of one storey of the tall room, starting at height `y`.
macro_rules! storey {
    ($y:expr) => {
        &[
            Side {
                dir: SOUTH,
                open: &[
                    solid(L, [2, $y, 0, 2, $y + 2, 0]),
                    solid(L, [5, $y, 0, 5, $y + 2, 0]),
                    solid(L, [3, $y + 2, 0, 4, $y + 2, 0]),
                ],
                closed: &[
                    solid(L, [0, $y, 0, 7, $y + 2, 0]),
                    solid(G, [0, $y + 1, 0, 7, $y + 1, 0]),
                ],
            },
            Side {
                dir: NORTH,
                open: &[
                    solid(L, [2, $y, 7, 2, $y + 2, 7]),
                    solid(L, [5, $y, 7, 5, $y + 2, 7]),
                    solid(L, [3, $y + 2, 7, 4, $y + 2, 7]),
                ],
                closed: &[
                    solid(L, [0, $y, 7, 7, $y + 2, 7]),
                    solid(G, [0, $y + 1, 7, 7, $y + 1, 7]),
                ],
            },
            Side {
                dir: WEST,
                open: &[
                    solid(L, [0, $y, 2, 0, $y + 2, 2]),
                    solid(L, [0, $y, 5, 0, $y + 2, 5]),
                    solid(L, [0, $y + 2, 3, 0, $y + 2, 4]),
                ],
                closed: &[
                    solid(L, [0, $y, 0, 0, $y + 2, 7]),
                    solid(G, [0, $y + 1, 0, 0, $y + 1, 7]),
                ],
            },
            Side {
                dir: EAST,
                open: &[
                    solid(L, [7, $y, 2, 7, $y + 2, 2]),
                    solid(L, [7, $y, 5, 7, $y + 2, 5]),
                    solid(L, [7, $y + 2, 3, 7, $y + 2, 4]),
                ],
                closed: &[
                    solid(L, [7, $y, 0, 7, $y + 2, 7]),
                    solid(G, [7, $y + 1, 0, 7, $y + 1, 7]),
                ],
            },
        ]
    };
}

const LOWER_STOREY: &[Side] = storey!(1);
const UPPER_STOREY: &[Side] = storey!(5);

pub(super) fn double_y_room(d: &mut Draw, g: &Graph, r: usize) {
    if above_grid(g, r) {
        d.floor(0, 0, g.opening(r, DOWN));
    }
    let above = g.link(r, UP);
    if !g.has_link(above, UP) {
        d.cap([1, 8, 1, 6, 8, 6], GRAY);
    }
    replay(d, TALL_RING);
    draw_sides(d, g, r, LOWER_STOREY);
    draw_sides(d, g, above, UPPER_STOREY);
}

/// Floors and ceiling caps of a room two cells long; `second` is the cell
/// at local offset `(dx, dz)` from `first`, and `caps` are the two cap
/// boxes in that order.
fn pair_floors(
    d: &mut Draw,
    g: &Graph,
    (first, second): (usize, usize),
    (dx, dz): (i32, i32),
    caps: [Cuboid; 2],
    tops: (usize, usize),
) {
    if above_grid(g, first) {
        d.floor(dx, dz, g.opening(second, DOWN));
        d.floor(0, 0, g.opening(first, DOWN));
    }
    if !g.has_link(tops.0, UP) {
        d.cap(caps[0], GRAY);
    }
    if !g.has_link(tops.1, UP) {
        d.cap(caps[1], GRAY);
    }
}

const LONG_X_HALL: &[Op] = &[
    solid(L, [0, 3, 0, 0, 3, 7]),
    solid(L, [15, 3, 0, 15, 3, 7]),
    solid(L, [1, 3, 0, 15, 3, 0]),
    solid(L, [1, 3, 7, 14, 3, 7]),
    solid(G, [0, 2, 0, 0, 2, 7]),
    solid(G, [15, 2, 0, 15, 2, 7]),
    solid(G, [1, 2, 0, 15, 2, 0]),
    solid(G, [1, 2, 7, 14, 2, 7]),
    solid(L, [0, 1, 0, 0, 1, 7]),
    solid(L, [15, 1, 0, 15, 1, 7]),
    solid(L, [1, 1, 0, 15, 1, 0]),
    solid(L, [1, 1, 7, 14, 1, 7]),
    solid(L, [5, 1, 0, 10, 1, 4]),
    solid(G, [6, 2, 0, 9, 2, 3]),
    solid(L, [5, 3, 0, 10, 3, 4]),
    dot(LAMP, 6, 2, 3),
    dot(LAMP, 9, 2, 3),
];

/// Doorway holes of a room two cells long along x, on the storey from `y`.
fn x_openings(d: &mut Draw, g: &Graph, (west, east): (usize, usize), y: i32) {
    let holes = [
        (west, SOUTH, [3, 0, 4, 0]),
        (west, NORTH, [3, 7, 4, 7]),
        (west, WEST, [0, 3, 0, 4]),
        (east, SOUTH, [11, 0, 12, 0]),
        (east, NORTH, [11, 7, 12, 7]),
        (east, EAST, [15, 3, 15, 4]),
    ];
    openings(d, g, &holes, y);
}

/// Doorway holes of a room two cells long along z, on the storey from `y`.
fn z_openings(d: &mut Draw, g: &Graph, (south, north): (usize, usize), y: i32) {
    let holes = [
        (south, SOUTH, [3, 0, 4, 0]),
        (south, EAST, [7, 3, 7, 4]),
        (south, WEST, [0, 3, 0, 4]),
        (north, NORTH, [3, 15, 4, 15]),
        (north, WEST, [0, 11, 0, 12]),
        (north, EAST, [7, 11, 7, 12]),
    ];
    openings(d, g, &holes, y);
}

fn openings(d: &mut Draw, g: &Graph, holes: &[(usize, usize, [i32; 4])], y: i32) {
    for &(cell, dir, [x0, z0, x1, z1]) in holes {
        if g.opening(cell, dir) {
            d.flood([x0, y, z0, x1, y + 1, z1]);
        }
    }
}

pub(super) fn double_x_room(d: &mut Draw, g: &Graph, r: usize) {
    let cells = (r, g.link(r, EAST));
    pair_floors(
        d,
        g,
        cells,
        (8, 0),
        [[1, 4, 1, 7, 4, 6], [8, 4, 1, 14, 4, 6]],
        cells,
    );
    replay(d, LONG_X_HALL);
    x_openings(d, g, cells, 1);
}

const LONG_Z_HALL: &[Op] = &[
    solid(L, [0, 3, 0, 0, 3, 15]),
    solid(L, [7, 3, 0, 7, 3, 15]),
    solid(L, [1, 3, 0, 7, 3, 0]),
    solid(L, [1, 3, 15, 6, 3, 15]),
    solid(G, [0, 2, 0, 0, 2, 15]),
    solid(G, [7, 2, 0, 7, 2, 15]),
    solid(G, [1, 2, 0, 7, 2, 0]),
    solid(G, [1, 2, 15, 6, 2, 15]),
    solid(L, [0, 1, 0, 0, 1, 15]),
    solid(L, [7, 1, 0, 7, 1, 15]),
    solid(L, [1, 1, 0, 7, 1, 0]),
    solid(L, [1, 1, 15, 6, 1, 15]),
    solid(L, [1, 1, 1, 1, 1, 2]),
    solid(L, [6, 1, 1, 6, 1, 2]),
    solid(L, [1, 3, 1, 1, 3, 2]),
    solid(L, [6, 3, 1, 6, 3, 2]),
    solid(L, [1, 1, 13, 1, 1, 14]),
    solid(L, [6, 1, 13, 6, 1, 14]),
    solid(L, [1, 3, 13, 1, 3, 14]),
    solid(L, [6, 3, 13, 6, 3, 14]),
    solid(L, [2, 1, 6, 2, 3, 6]),
    solid(L, [5, 1, 6, 5, 3, 6]),
    solid(L, [2, 1, 9, 2, 3, 9]),
    solid(L, [5, 1, 9, 5, 3, 9]),
    solid(L, [3, 2, 6, 4, 2, 6]),
    solid(L, [3, 2, 9, 4, 2, 9]),
    solid(L, [2, 2, 7, 2, 2, 8]),
    solid(L, [5, 2, 7, 5, 2, 8]),
    dot(LAMP, 2, 2, 5),
    dot(LAMP, 5, 2, 5),
    dot(LAMP, 2, 2, 10),
    dot(LAMP, 5, 2, 10),
    dot(L, 2, 3, 5),
    dot(L, 5, 3, 5),
    dot(L, 2, 3, 10),
    dot(L, 5, 3, 10),
];

pub(super) fn double_z_room(d: &mut Draw, g: &Graph, r: usize) {
    let cells = (r, g.link(r, NORTH));
    pair_floors(
        d,
        g,
        cells,
        (0, 8),
        [[1, 4, 1, 6, 4, 7], [1, 4, 8, 6, 4, 14]],
        cells,
    );
    replay(d, LONG_Z_HALL);
    z_openings(d, g, cells, 1);
}

/// One gray-banded ring per level of a two-storey room two cells long
/// along x or along z.
fn tall_rings(d: &mut Draw, along_x: bool) {
    for y in 1..=7 {
        let state = if y == 2 || y == 6 { GRAY } else { LIGHT };
        let rings: [Cuboid; 4] = if along_x {
            [
                [0, y, 0, 0, y, 7],
                [15, y, 0, 15, y, 7],
                [1, y, 0, 15, y, 0],
                [1, y, 7, 14, y, 7],
            ]
        } else {
            [
                [0, y, 0, 0, y, 15],
                [7, y, 0, 7, y, 15],
                [1, y, 0, 6, y, 0],
                [1, y, 15, 6, y, 15],
            ]
        };
        for b in rings {
            d.solid(b, state);
        }
    }
}

const GALLERY: &[Op] = &[
    solid(L, [2, 1, 3, 2, 7, 4]),
    solid(L, [3, 1, 2, 4, 7, 2]),
    solid(L, [3, 1, 5, 4, 7, 5]),
    solid(L, [13, 1, 3, 13, 7, 4]),
    solid(L, [11, 1, 2, 12, 7, 2]),
    solid(L, [11, 1, 5, 12, 7, 5]),
    solid(L, [5, 1, 3, 5, 3, 4]),
    solid(L, [10, 1, 3, 10, 3, 4]),
    solid(L, [5, 7, 2, 10, 7, 5]),
    solid(L, [5, 5, 2, 5, 7, 2]),
    solid(L, [10, 5, 2, 10, 7, 2]),
    solid(L, [5, 5, 5, 5, 7, 5]),
    solid(L, [10, 5, 5, 10, 7, 5]),
    dot(L, 6, 6, 2),
    dot(L, 9, 6, 2),
    dot(L, 6, 6, 5),
    dot(L, 9, 6, 5),
    solid(L, [5, 4, 3, 6, 4, 4]),
    solid(L, [9, 4, 3, 10, 4, 4]),
    dot(LAMP, 5, 4, 2),
    dot(LAMP, 5, 4, 5),
    dot(LAMP, 10, 4, 2),
    dot(LAMP, 10, 4, 5),
];

pub(super) fn double_xy_room(d: &mut Draw, g: &Graph, r: usize) {
    let low = (r, g.link(r, EAST));
    let high = (g.link(low.0, UP), g.link(low.1, UP));
    pair_floors(
        d,
        g,
        low,
        (8, 0),
        [[1, 8, 1, 7, 8, 6], [8, 8, 1, 14, 8, 6]],
        high,
    );
    tall_rings(d, true);
    replay(d, GALLERY);
    x_openings(d, g, low, 1);
    x_openings(d, g, high, 5);
}

/// Bridges built where the upper storey of the tall z room opens sideways.
const BRIDGES: &[Side] = &[
    when_open(SOUTH, &[flood([3, 5, 0, 4, 6, 0])]),
    when_open(
        EAST,
        &[
            flood([7, 5, 3, 7, 6, 4]),
            solid(L, [5, 4, 2, 6, 4, 5]),
            solid(L, [6, 1, 2, 6, 3, 2]),
            solid(L, [6, 1, 5, 6, 3, 5]),
        ],
    ),
    when_open(
        WEST,
        &[
            flood([0, 5, 3, 0, 6, 4]),
            solid(L, [1, 4, 2, 2, 4, 5]),
            solid(L, [1, 1, 2, 1, 3, 2]),
            solid(L, [1, 1, 5, 1, 3, 5]),
        ],
    ),
];

const NORTH_BRIDGES: &[Side] = &[
    when_open(NORTH, &[flood([3, 5, 15, 4, 6, 15])]),
    when_open(
        WEST,
        &[
            flood([0, 5, 11, 0, 6, 12]),
            solid(L, [1, 4, 10, 2, 4, 13]),
            solid(L, [1, 1, 10, 1, 3, 10]),
            solid(L, [1, 1, 13, 1, 3, 13]),
        ],
    ),
    when_open(
        EAST,
        &[
            flood([7, 5, 11, 7, 6, 12]),
            solid(L, [5, 4, 10, 6, 4, 13]),
            solid(L, [6, 1, 10, 6, 3, 10]),
            solid(L, [6, 1, 13, 6, 3, 13]),
        ],
    ),
];

pub(super) fn double_yz_room(d: &mut Draw, g: &Graph, r: usize) {
    let low = (r, g.link(r, NORTH));
    let high = (g.link(low.0, UP), g.link(low.1, UP));
    pair_floors(
        d,
        g,
        low,
        (0, 8),
        [[1, 8, 1, 6, 8, 7], [1, 8, 8, 6, 8, 14]],
        high,
    );
    tall_rings(d, false);
    for y in 1..=7 {
        let state = if y == 2 || y == 6 { LAMP } else { BLACK };
        d.solid([3, y, 7, 4, y, 8], state);
    }
    z_openings(d, g, low, 1);
    draw_sides(d, g, high.0, BRIDGES);
    draw_sides(d, g, high.1, NORTH_BRIDGES);
}

const CORE: &[Op] = &[
    Op::Cap(G, [1, 8, 0, 14, 8, 14]),
    solid(L, [0, 7, 0, 0, 7, 15]),
    solid(L, [15, 7, 0, 15, 7, 15]),
    solid(L, [1, 7, 0, 15, 7, 0]),
    solid(L, [1, 7, 15, 14, 7, 15]),
];

const CORE_INNER: &[Op] = &[
    solid(B, [6, 3, 6, 9, 6, 9]),
    solid(Block::GoldBlock.default_state(), [7, 4, 7, 8, 5, 8]),
    dot(LAMP, 6, 3, 6),
    dot(LAMP, 6, 3, 9),
    dot(LAMP, 9, 3, 6),
    dot(LAMP, 9, 3, 9),
    dot(LAMP, 6, 6, 6),
    dot(LAMP, 6, 6, 9),
    dot(LAMP, 9, 6, 6),
    dot(LAMP, 9, 6, 9),
    solid(L, [5, 1, 6, 5, 2, 6]),
    solid(L, [5, 1, 9, 5, 2, 9]),
    solid(L, [10, 1, 6, 10, 2, 6]),
    solid(L, [10, 1, 9, 10, 2, 9]),
    solid(L, [6, 1, 5, 6, 2, 5]),
    solid(L, [9, 1, 5, 9, 2, 5]),
    solid(L, [6, 1, 10, 6, 2, 10]),
    solid(L, [9, 1, 10, 9, 2, 10]),
    solid(L, [5, 2, 5, 5, 6, 5]),
    solid(L, [5, 2, 10, 5, 6, 10]),
    solid(L, [10, 2, 5, 10, 6, 5]),
    solid(L, [10, 2, 10, 10, 6, 10]),
    solid(L, [5, 7, 1, 5, 7, 6]),
    solid(L, [10, 7, 1, 10, 7, 6]),
    solid(L, [5, 7, 9, 5, 7, 14]),
    solid(L, [10, 7, 9, 10, 7, 14]),
    solid(L, [1, 7, 5, 6, 7, 5]),
    solid(L, [1, 7, 10, 6, 7, 10]),
    solid(L, [9, 7, 5, 14, 7, 5]),
    solid(L, [9, 7, 10, 14, 7, 10]),
    solid(L, [2, 1, 2, 2, 1, 3]),
    dot(L, 3, 1, 2),
    solid(L, [13, 1, 2, 13, 1, 3]),
    dot(L, 12, 1, 2),
    solid(L, [2, 1, 12, 2, 1, 13]),
    dot(L, 3, 1, 13),
    solid(L, [13, 1, 12, 13, 1, 13]),
    dot(L, 12, 1, 13),
];

pub(super) fn core_room(d: &mut Draw) {
    replay(d, CORE);
    for y in 1..=6 {
        let state = if y == 2 || y == 6 { GRAY } else { LIGHT };
        for [x0, z0, x1, z1] in [
            [0, 0, 0, 1],
            [0, 6, 0, 9],
            [0, 14, 0, 15],
            [15, 0, 15, 1],
            [15, 6, 15, 9],
            [15, 14, 15, 15],
            [1, 0, 1, 0],
            [6, 0, 9, 0],
            [14, 0, 14, 0],
            [1, 15, 14, 15],
        ] {
            d.solid([x0, y, z0, x1, y, z1], state);
        }
    }
    replay(d, CORE_INNER);
}

const HALL_OF_STEPS: &[Op] = &[
    series(L, [10, 3, 20, 12, 3, 20], [-1, -1, -1, 1, -1, 0], 4),
    solid(L, [7, 0, 6, 15, 0, 16]),
    solid(L, [6, 0, 6, 6, 3, 20]),
    solid(L, [16, 0, 6, 16, 3, 20]),
    solid(L, [7, 1, 7, 7, 1, 20]),
    solid(L, [15, 1, 7, 15, 1, 20]),
    solid(L, [7, 1, 6, 9, 3, 6]),
    solid(L, [13, 1, 6, 15, 3, 6]),
    solid(L, [8, 1, 7, 9, 1, 7]),
    solid(L, [13, 1, 7, 14, 1, 7]),
    solid(L, [9, 0, 5, 13, 0, 5]),
    solid(B, [10, 0, 7, 12, 0, 7]),
    solid(B, [8, 0, 10, 8, 0, 12]),
    solid(B, [14, 0, 10, 14, 0, 12]),
    series(LAMP, [6, 3, 9, 6, 3, 9], [0, 0, 3, 0, 0, 3], 4),
    series(LAMP, [16, 3, 9, 16, 3, 9], [0, 0, 3, 0, 0, 3], 4),
    dot(LAMP, 10, 0, 10),
    dot(LAMP, 12, 0, 10),
    dot(LAMP, 10, 0, 12),
    dot(LAMP, 12, 0, 12),
    dot(LAMP, 8, 3, 6),
    dot(LAMP, 14, 3, 6),
    series(L, [4, 2, 4, 4, 2, 4], [14, 0, 0, 14, 0, 0], 2),
    series(LAMP, [4, 1, 4, 4, 1, 4], [14, 0, 0, 14, 0, 0], 2),
    series(L, [4, 0, 4, 4, 0, 4], [14, 0, 0, 14, 0, 0], 2),
    series(L, [4, 2, 18, 4, 2, 18], [14, 0, 0, 14, 0, 0], 2),
    series(LAMP, [4, 1, 18, 4, 1, 18], [14, 0, 0, 14, 0, 0], 2),
    series(L, [4, 0, 18, 4, 0, 18], [14, 0, 0, 14, 0, 0], 2),
    dot(L, 9, 7, 20),
    dot(L, 13, 7, 20),
    solid(L, [6, 0, 21, 7, 4, 21]),
    solid(L, [15, 0, 21, 16, 4, 21]),
];

const HALL_OF_PILLARS: &[Op] = &[
    solid(L, [9, 3, 18, 13, 3, 20]),
    solid(L, [9, 0, 18, 9, 2, 18]),
    solid(L, [13, 0, 18, 13, 2, 18]),
    series(L, [9, 6, 20, 9, 6, 20], [4, 0, 0, 4, 0, 0], 2),
    series(LAMP, [9, 5, 20, 9, 5, 20], [4, 0, 0, 4, 0, 0], 2),
    series(L, [9, 4, 20, 9, 4, 20], [4, 0, 0, 4, 0, 0], 2),
    solid(L, [7, 3, 7, 15, 3, 14]),
    series(L, [10, 0, 10, 10, 6, 10], [2, 0, 0, 2, 0, 0], 2),
    series(L, [10, 0, 12, 10, 6, 12], [2, 0, 0, 2, 0, 0], 2),
    series(LAMP, [10, 0, 10, 10, 0, 10], [2, 0, 0, 2, 0, 0], 2),
    series(LAMP, [10, 0, 12, 10, 0, 12], [2, 0, 0, 2, 0, 0], 2),
    series(LAMP, [10, 4, 10, 10, 4, 10], [2, 0, 0, 2, 0, 0], 2),
    series(LAMP, [10, 4, 12, 10, 4, 12], [2, 0, 0, 2, 0, 0], 2),
    series(L, [8, 0, 7, 8, 2, 7], [6, 0, 0, 6, 0, 0], 2),
    series(L, [8, 0, 14, 8, 2, 14], [6, 0, 0, 6, 0, 0], 2),
    solid(B, [8, 3, 8, 8, 3, 13]),
    solid(B, [14, 3, 8, 14, 3, 13]),
];

pub(super) fn wing_room(d: &mut Draw, design: i32) {
    match design {
        0 => {
            replay(d, HALL_OF_STEPS);
            d.elder(11, 2, 16);
        }
        1 => {
            replay(d, HALL_OF_PILLARS);
            d.elder(11, 5, 13);
        }
        _ => {}
    }
}

const PENTHOUSE: &[Op] = &[
    solid(L, [2, -1, 2, 11, -1, 11]),
    solid(G, [0, -1, 0, 1, -1, 11]),
    solid(G, [12, -1, 0, 13, -1, 11]),
    solid(G, [2, -1, 0, 11, -1, 1]),
    solid(G, [2, -1, 12, 11, -1, 13]),
    solid(L, [0, 0, 0, 0, 0, 13]),
    solid(L, [13, 0, 0, 13, 0, 13]),
    solid(L, [1, 0, 0, 12, 0, 0]),
    solid(L, [1, 0, 13, 12, 0, 13]),
    series(LAMP, [0, 0, 2, 0, 0, 2], [0, 0, 3, 0, 0, 3], 4),
    series(LAMP, [13, 0, 2, 13, 0, 2], [0, 0, 3, 0, 0, 3], 4),
    series(LAMP, [2, 0, 0, 2, 0, 0], [3, 0, 0, 3, 0, 0], 4),
    solid(L, [2, 0, 3, 4, 0, 9]),
    solid(L, [9, 0, 3, 11, 0, 9]),
    solid(L, [4, 0, 9, 9, 0, 11]),
    dot(L, 5, 0, 8),
    dot(L, 8, 0, 8),
    dot(L, 10, 0, 10),
    dot(L, 3, 0, 10),
    solid(B, [3, 0, 3, 3, 0, 7]),
    solid(B, [10, 0, 3, 10, 0, 7]),
    solid(B, [6, 0, 10, 7, 0, 10]),
    series(L, [3, 0, 2, 3, 2, 2], [0, 0, 3, 0, 0, 3], 3),
    series(L, [10, 0, 2, 10, 2, 2], [0, 0, 3, 0, 0, 3], 3),
    solid(L, [5, 0, 10, 5, 2, 10]),
    solid(L, [8, 0, 10, 8, 2, 10]),
    solid(B, [6, -1, 7, 7, -1, 8]),
    flood([6, -1, 3, 7, -1, 4]),
];

pub(super) fn penthouse(d: &mut Draw) {
    replay(d, PENTHOUSE);
    d.elder(6, 1, 6);
}

const ENTRY: &[Op] = &[
    solid(L, [0, 3, 0, 2, 3, 7]),
    solid(L, [5, 3, 0, 7, 3, 7]),
    solid(L, [0, 2, 0, 1, 2, 7]),
    solid(L, [6, 2, 0, 7, 2, 7]),
    solid(L, [0, 1, 0, 0, 1, 7]),
    solid(L, [7, 1, 0, 7, 1, 7]),
    solid(L, [0, 1, 7, 7, 3, 7]),
    solid(L, [1, 1, 0, 2, 3, 0]),
    solid(L, [5, 1, 0, 6, 3, 0]),
];

const ENTRY_DOORS: &[Side] = &[
    when_open(NORTH, &[flood([3, 1, 7, 4, 2, 7])]),
    when_open(WEST, &[flood([0, 1, 3, 1, 2, 4])]),
    when_open(EAST, &[flood([6, 1, 3, 7, 2, 4])]),
];

pub(super) fn entry_room(d: &mut Draw, g: &Graph, r: usize) {
    replay(d, ENTRY);
    draw_sides(d, g, r, ENTRY_DOORS);
}
