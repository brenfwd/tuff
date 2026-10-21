// SPDX-License-Identifier: MIT
//! Floating islands, a pure function of the seed and the chunk position:
//! one island at most per 64 by 64 cell plus a fixed one at the origin,
//! and pale oak trees on some of them.

const CELL: i32 = 64;
const AIR: u8 = 0;
const GRASS: u8 = 1;
const DIRT: u8 = 2;
const STONE: u8 = 3;
const LOG: u8 = 4;
const LEAVES: u8 = 5;

pub const PALETTE: [&str; 6] = [
    "minecraft:air",
    "minecraft:grass_block[snowy=false]",
    "minecraft:dirt",
    "minecraft:stone",
    "minecraft:pale_oak_log[axis=y]",
    "minecraft:pale_oak_leaves[persistent=true]",
];

const ORIGIN_TOP: i32 = 128;
pub const SPAWN_Y: i32 = ORIGIN_TOP + 2;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Island {
    id: (i32, i32),
    cx: i32,
    cz: i32,
    r: f64,
    top: i32,
    depth: f64,
    h: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tree {
    pub x: i32,
    pub z: i32,
    pub base: i32,
    pub height: i32,
}

fn mix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

fn hash(seed: i64, a: i32, b: i32, salt: u64) -> u64 {
    let h = mix(seed as u64 ^ salt.wrapping_mul(0xd6e8_feb8_6659_fd93));
    let h = mix(h ^ a as u32 as u64);
    mix(h ^ ((b as u32 as u64) << 32))
}

fn origin(seed: i64) -> Island {
    Island {
        id: (i32::MIN, 0),
        cx: 0,
        cz: 0,
        r: 18.0,
        top: ORIGIN_TOP,
        depth: 14.0,
        h: hash(seed, 0, 0, 99),
    }
}

fn in_cell(seed: i64, gx: i32, gz: i32) -> Option<Island> {
    let h = hash(seed, gx, gz, 1);
    if h % 100 >= 75 {
        return None;
    }
    let jitter = |shift: u32| ((h >> shift) % 9) as i32 - 4;
    Some(Island {
        id: (gx, gz),
        cx: gx * CELL + CELL / 2 + jitter(8),
        cz: gz * CELL + CELL / 2 + jitter(12),
        r: 14.0 + ((h >> 16) % 13) as f64,
        top: 96 + ((h >> 24) % 56) as i32,
        depth: 10.0 + ((h >> 32) % 14) as f64,
        h,
    })
}

fn candidates(seed: i64, gx: i32, gz: i32) -> Vec<Island> {
    let mut found = vec![origin(seed)];
    for x in gx - 1..=gx + 1 {
        for z in gz - 1..=gz + 1 {
            found.extend(in_cell(seed, x, z));
        }
    }
    found
}

fn column(island: &Island, x: i32, z: i32) -> Option<(i32, i32)> {
    let phase = |shift: u32| ((island.h >> shift) & 0xffff) as f64 * 0.001;
    let (p, q) = (phase(0), phase(16));
    let (xf, zf) = (x as f64, z as f64);
    let dx = xf - island.cx as f64;
    let dz = zf - island.cz as f64;
    let edge = island.r * (1.0 + 0.12 * (0.21 * xf + p).sin() + 0.12 * (0.17 * zf + q).cos());
    let d = (dx * dx + dz * dz).sqrt() / edge;
    if d >= 1.0 {
        return None;
    }
    let bump = ((0.45 * xf + p).sin() + (0.4 * zf + q).cos()) > 1.3;
    let top = island.top + i32::from(bump);
    let bottom = top - 3 - (island.depth * (1.0 - d * d)) as i32;
    Some((top, bottom))
}

fn surface(list: &[Island], x: i32, z: i32) -> Option<(usize, i32, i32)> {
    let mut best: Option<(usize, i32, i32)> = None;
    for (i, island) in list.iter().enumerate() {
        if let Some((top, bottom)) = column(island, x, z)
            && best.is_none_or(|(_, t, _)| top > t)
        {
            best = Some((i, top, bottom));
        }
    }
    best
}

fn surface_at(seed: i64, x: i32, z: i32) -> Option<(Island, i32)> {
    let list = candidates(seed, x.div_euclid(CELL), z.div_euclid(CELL));
    surface(&list, x, z).map(|(i, top, _)| (list[i], top))
}

pub fn trees(seed: i64, island: &Island) -> Vec<Tree> {
    let count = match (island.h >> 40) % 6 {
        0..=2 => 0,
        3 | 4 => 1,
        _ => 2,
    };
    let mut found = Vec::new();
    for i in 0..count {
        let h = hash(seed, island.cx, island.cz, 1000 + i);
        let angle = (h % 628) as f64 / 100.0;
        let radius = ((h >> 12) % 1000) as f64 / 1000.0 * island.r * 0.6;
        let x = island.cx + (angle.cos() * radius).round() as i32;
        let z = island.cz + (angle.sin() * radius).round() as i32;
        if let Some((owner, top)) = surface_at(seed, x, z)
            && owner.id == island.id
        {
            found.push(Tree {
                x,
                z,
                base: top,
                height: 4 + ((h >> 24) % 3) as i32,
            });
        }
    }
    found
}

fn tree_blocks(tree: &Tree) -> Vec<(i32, i32, i32, u8)> {
    let mut blocks = Vec::new();
    for dy in 1..=tree.height {
        blocks.push((tree.x, tree.base + dy, tree.z, LOG));
    }
    for (dy, radius) in [
        (tree.height - 1, 2),
        (tree.height, 2),
        (tree.height + 1, 1),
        (tree.height + 2, 1),
    ] as [(i32, i32); 4]
    {
        for dx in -radius..=radius {
            for dz in -radius..=radius {
                let corner = dx.abs() == radius && dz.abs() == radius;
                if corner && (radius == 2 || dy > tree.height + 1) {
                    continue;
                }
                if dx == 0 && dz == 0 && dy <= tree.height {
                    continue;
                }
                blocks.push((tree.x + dx, tree.base + dy, tree.z + dz, LEAVES));
            }
        }
    }
    blocks
}

/// The blocks of one chunk column as palette indices, `(y - min_y) * 256 +
/// z * 16 + x`.
pub fn generate(seed: i64, chunk_x: i32, chunk_z: i32, min_y: i32, height: i32) -> Vec<u8> {
    let mut blocks = vec![AIR; (height.max(0) as usize) * 256];
    let (x0, z0) = (chunk_x * 16, chunk_z * 16);
    let list = candidates(seed, x0.div_euclid(CELL), z0.div_euclid(CELL));
    let slot = |x: i32, y: i32, z: i32| -> Option<usize> {
        let (lx, lz, ly) = (x - x0, z - z0, y - min_y);
        ((0..16).contains(&lx) && (0..16).contains(&lz) && (0..height).contains(&ly))
            .then(|| ly as usize * 256 + lz as usize * 16 + lx as usize)
    };
    for x in x0..x0 + 16 {
        for z in z0..z0 + 16 {
            let Some((_, top, bottom)) = surface(&list, x, z) else {
                continue;
            };
            for y in bottom..=top {
                let kind = match y {
                    y if y == top => GRASS,
                    y if y >= top - 3 => DIRT,
                    _ => STONE,
                };
                if let Some(i) = slot(x, y, z) {
                    blocks[i] = kind;
                }
            }
        }
    }
    for island in &list {
        for tree in trees(seed, island) {
            if tree.x < x0 - 2 || tree.x > x0 + 17 || tree.z < z0 - 2 || tree.z > z0 + 17 {
                continue;
            }
            for (x, y, z, kind) in tree_blocks(&tree) {
                if let Some(i) = slot(x, y, z)
                    && blocks[i] == AIR
                {
                    blocks[i] = kind;
                }
            }
        }
    }
    blocks
}
