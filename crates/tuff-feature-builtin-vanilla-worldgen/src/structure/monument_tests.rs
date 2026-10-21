use super::*;

fn label(kind: Kind) -> (&'static str, i32) {
    match kind {
        Kind::Entry => ("Entry", -1),
        Kind::Simple { design } => ("Simple", design),
        Kind::SimpleTop => ("SimpleTop", -1),
        Kind::DoubleY => ("DoubleY", -1),
        Kind::DoubleX => ("DoubleX", -1),
        Kind::DoubleZ => ("DoubleZ", -1),
        Kind::DoubleXY => ("DoubleXY", -1),
        Kind::DoubleYZ => ("DoubleYZ", -1),
        Kind::Core => ("Core", -1),
        Kind::Wing { design } => ("Wing", design),
        Kind::Penthouse => ("Penthouse", -1),
    }
}

/// The rooms of the seed 1 monument starting in chunk (76, -76), as the
/// game's own code builds them: kind, design and box.
const ROOMS: [(&str, i32, [i32; 6]); 32] = [
    ("Entry", -1, [1215, 39, -1220, 1222, 42, -1213]),
    ("Core", -1, [1191, 39, -1228, 1206, 46, -1213]),
    ("DoubleX", -1, [1215, 43, -1236, 1222, 46, -1221]),
    ("Simple", 1, [1199, 43, -1212, 1206, 46, -1205]),
    ("SimpleTop", -1, [1215, 47, -1212, 1222, 50, -1205]),
    ("DoubleZ", -1, [1199, 39, -1236, 1214, 42, -1229]),
    ("DoubleX", -1, [1207, 43, -1228, 1214, 46, -1213]),
    ("Simple", 0, [1207, 47, -1220, 1214, 50, -1213]),
    ("DoubleZ", -1, [1207, 43, -1204, 1222, 46, -1197]),
    ("SimpleTop", -1, [1207, 47, -1212, 1214, 50, -1205]),
    ("DoubleY", -1, [1215, 39, -1212, 1222, 46, -1205]),
    ("Simple", 0, [1191, 43, -1212, 1198, 46, -1205]),
    ("Simple", 1, [1207, 47, -1228, 1214, 50, -1221]),
    ("Simple", 0, [1191, 39, -1236, 1198, 42, -1229]),
    ("DoubleX", -1, [1207, 39, -1212, 1214, 42, -1197]),
    ("DoubleZ", -1, [1207, 39, -1228, 1222, 42, -1221]),
    ("Simple", 1, [1199, 43, -1236, 1206, 46, -1229]),
    ("DoubleY", -1, [1215, 43, -1220, 1222, 50, -1213]),
    ("Simple", 0, [1207, 43, -1236, 1214, 46, -1229]),
    ("Simple", 2, [1215, 39, -1236, 1222, 42, -1229]),
    ("Simple", 1, [1191, 39, -1212, 1198, 42, -1205]),
    ("Simple", 2, [1191, 39, -1204, 1198, 42, -1197]),
    ("Simple", 1, [1215, 47, -1228, 1222, 50, -1221]),
    ("Simple", 2, [1207, 39, -1220, 1214, 42, -1213]),
    ("DoubleZ", -1, [1191, 43, -1204, 1206, 46, -1197]),
    ("DoubleX", -1, [1199, 39, -1212, 1206, 42, -1197]),
    ("Simple", 0, [1215, 39, -1204, 1222, 42, -1197]),
    ("Simple", 0, [1207, 43, -1212, 1214, 46, -1205]),
    ("Simple", 1, [1191, 43, -1236, 1198, 46, -1229]),
    ("Wing", 0, [1223, 40, -1244, 1243, 47, -1222]),
    ("Wing", 1, [1223, 40, -1211, 1243, 47, -1189]),
    ("Penthouse", -1, [1209, 52, -1223, 1222, 56, -1210]),
];

#[tuff_core::watchdog]
#[test]
fn rooms_are_laid_out_like_the_game() {
    let mut random = WorldgenRandom::legacy(0);
    random.set_large_feature_seed(1, 76, -76);
    let direction = Facing::HORIZONTAL[random.next_int_bounded(4) as usize];
    let building = Building::new(&mut random, 76 * 16 - 29, -76 * 16 - 29, direction);
    let ours: Vec<(&str, i32, [i32; 6])> = building
        .children
        .iter()
        .map(|child| {
            let (name, design) = label(child.kind);
            let b = child.frame.bounding_box;
            (
                name,
                design,
                [b.min_x, b.min_y, b.min_z, b.max_x, b.max_y, b.max_z],
            )
        })
        .collect();
    assert_eq!(ours, ROOMS);
}

fn placed_digest(seed: i64, chunk_x: i32, chunk_z: i32) -> u64 {
    use crate::proto::ProtoChunk;
    use crate::surface::GenContext;
    use tuff_world_model::chunk::ChunkShape;

    let mut random = WorldgenRandom::legacy(0);
    random.set_large_feature_seed(seed, chunk_x, chunk_z);
    let direction = Facing::HORIZONTAL[random.next_int_bounded(4) as usize];
    let building = Building::new(&mut random, chunk_x * 16 - 29, chunk_z * 16 - 29, direction);
    let shape = ChunkShape::OVERWORLD;
    let span = GenContext {
        min_y: shape.min_y,
        height: shape.height,
        sea_level: 63,
    };
    let b = building.frame.bounding_box;
    let mut h = 0xcbf29ce484222325u64;
    let mut mix = |v: u64| {
        h ^= v;
        h = h.wrapping_mul(0x100000001b3);
    };
    for cz in (b.min_z >> 4)..=(b.max_z >> 4) {
        for cx in (b.min_x >> 4)..=(b.max_x >> 4) {
            let mut chunks = Vec::with_capacity(9);
            for dz in -1..=1 {
                for dx in -1..=1 {
                    chunks.push(ProtoChunk::new(ChunkPos::new(cx + dx, cz + dz), shape));
                }
            }
            let center = ChunkPos::new(cx, cz);
            let mut level = Region::live(center, chunks, span, seed, seed);
            let mut chunk_box = BoundingBox::new(
                cx * 16,
                shape.min_y,
                cz * 16,
                cx * 16 + 15,
                shape.min_y + shape.height - 1,
                cz * 16 + 15,
            );
            let mut rng = WorldgenRandom::legacy(seed ^ ((cx as i64) << 20) ^ cz as i64);
            let mut ctx = PieceContext {
                level: &mut level,
                random: &mut rng,
            };
            building.place(&mut ctx, &mut chunk_box, center, BlockPos::new(0, 0, 0));
            let chunk = level.into_chunks().swap_remove(4);
            for y in shape.min_y..90 {
                for z in 0..16 {
                    for x in 0..16 {
                        mix(chunk.get(x, y, z).id() as u64);
                    }
                }
            }
            mix(chunk.get(3, shape.min_y, 3).id() as u64);
            for entity in &chunk.entities {
                mix(format!("{entity:?}").len() as u64);
            }
            mix(chunk.entities.len() as u64);
        }
    }
    h
}

#[tuff_core::watchdog]
#[test]
fn placed_blocks_stay_the_same() {
    let digests: Vec<u64> = [(1, 76, -76), (7, 3, 9), (42, -20, 5), (1234, 50, -2)]
        .iter()
        .map(|&(seed, x, z)| placed_digest(seed, x, z))
        .collect();
    assert_eq!(digests, PLACED_DIGESTS);
}

const PLACED_DIGESTS: [u64; 4] = [
    17263232417102488216,
    11382032901955948853,
    1171903602619704711,
    12115434964178798684,
];

#[tuff_core::watchdog]
#[test]
#[ignore = "timing run: cargo test --release -- --ignored monument_place_timing"]
fn monument_place_timing() {
    let start = std::time::Instant::now();
    let mut sink = 0u64;
    for i in 0..40 {
        sink ^= placed_digest(i, i as i32 * 7, -(i as i32) * 3);
    }
    eprintln!("monument place timing: {:?} ({sink})", start.elapsed());
}
