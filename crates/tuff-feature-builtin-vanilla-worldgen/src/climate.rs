//! Multi-noise climate: quantized climate targets, parameter lists, and a
//! bounding-box tree over them that answers nearest-biome queries with the
//! game's results, including its ties (a query starts from the previous
//! answer and only a strictly closer entry displaces it).

use std::cmp::Ordering;

use tuff_vanilla_data::worldgen::{ClimateParameter, ClimatePoint};

pub fn quantize(v: f32) -> i64 {
    (v * 10000.0) as i64
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Parameter {
    pub min: i64,
    pub max: i64,
}

impl Parameter {
    fn gap_to(self, value: i64) -> i64 {
        (value - self.max).max(self.min - value).max(0)
    }

    fn mid(self) -> i64 {
        (self.min + self.max) / 2
    }

    fn widen(self, other: Parameter) -> Parameter {
        Parameter {
            min: self.min.min(other.min),
            max: self.max.max(other.max),
        }
    }
}

impl From<ClimateParameter> for Parameter {
    fn from(p: ClimateParameter) -> Self {
        Parameter {
            min: p.min,
            max: p.max,
        }
    }
}

/// A sampled climate: temperature, humidity, continentalness, erosion,
/// depth, weirdness.
pub type Target = [i64; 6];

pub fn target(values: [f32; 6]) -> Target {
    values.map(quantize)
}

const AXES: usize = 7;
const FANOUT: usize = 19;

type Bounds = [Parameter; AXES];
type Probe = [i64; AXES];

fn probe(t: &Target) -> Probe {
    [t[0], t[1], t[2], t[3], t[4], t[5], 0]
}

fn fitness(bounds: &Bounds, at: &Probe) -> i64 {
    bounds.iter().zip(at).fold(0i64, |acc, (p, &v)| {
        let g = p.gap_to(v);
        acc.wrapping_add(g.wrapping_mul(g))
    })
}

fn hull<'a>(mut boxes: impl Iterator<Item = &'a Bounds>) -> Bounds {
    let first = *boxes.next().expect("hull of an empty set");
    boxes.fold(first, |acc, b| std::array::from_fn(|i| acc[i].widen(b[i])))
}

fn spread(bounds: &Bounds) -> i64 {
    bounds.iter().map(|p| (p.max - p.min).abs()).sum()
}

/// Lexicographic order on box midpoints, reading axes cyclically from
/// `lead`, optionally on their magnitudes.
fn by_midpoints(a: &Bounds, b: &Bounds, lead: usize, magnitude: bool) -> Ordering {
    let key = |p: Parameter| if magnitude { p.mid().abs() } else { p.mid() };
    (lead..AXES)
        .chain(0..lead)
        .map(|axis| key(a[axis]).cmp(&key(b[axis])))
        .find(|o| o.is_ne())
        .unwrap_or(Ordering::Equal)
}

fn group_len(count: usize) -> usize {
    let fanout = FANOUT as f64;
    let depth = ((count as f64 - 0.01).ln() / fanout.ln()).floor();
    fanout.powf(depth) as usize
}

/// Arena of tree nodes. Ids below the entry count are the entries
/// themselves; every later id is an inner node whose children are
/// `members[span.0..span.1]`.
struct Forest {
    boxes: Vec<Bounds>,
    spans: Vec<(u32, u32)>,
    members: Vec<u32>,
    entries: u32,
}

impl Forest {
    fn plant(entries: Vec<Bounds>) -> (Self, u32) {
        let count = entries.len();
        assert!(count > 0, "a parameter list needs at least one entry");
        let mut forest = Forest {
            spans: vec![(0, 0); count],
            boxes: entries,
            members: Vec::new(),
            entries: count as u32,
        };
        let mut ids: Vec<u32> = (0..count as u32).collect();
        let root = forest.grow(&mut ids);
        (forest, root)
    }

    fn inner(&mut self, children: &[u32]) -> u32 {
        let start = self.members.len() as u32;
        self.members.extend_from_slice(children);
        let bounds = hull(children.iter().map(|&c| &self.boxes[c as usize]));
        self.boxes.push(bounds);
        self.spans.push((start, self.members.len() as u32));
        (self.boxes.len() - 1) as u32
    }

    fn grow(&mut self, ids: &mut [u32]) -> u32 {
        match ids.len() {
            1 => ids[0],
            n if n <= FANOUT => {
                let boxes = &self.boxes;
                ids.sort_by_cached_key(|&id| {
                    boxes[id as usize]
                        .iter()
                        .map(|p| p.mid().abs())
                        .sum::<i64>()
                });
                self.inner(ids)
            }
            n => {
                let size = group_len(n);
                let axis = self.choose_axis(ids, size);
                let boxes = &self.boxes;
                let mut groups: Vec<(Bounds, usize)> = ids
                    .chunks(size)
                    .enumerate()
                    .map(|(g, chunk)| (hull(chunk.iter().map(|&id| &boxes[id as usize])), g * size))
                    .collect();
                groups.sort_by(|a, b| by_midpoints(&a.0, &b.0, axis, true));
                let children: Vec<u32> = groups
                    .iter()
                    .map(|&(_, from)| {
                        let to = (from + size).min(n);
                        self.grow(&mut ids[from..to])
                    })
                    .collect();
                self.inner(&children)
            }
        }
    }

    /// Leaves `ids` in the order of the cheapest split and returns its axis.
    fn choose_axis(&self, ids: &mut [u32], size: usize) -> usize {
        let boxes = &self.boxes;
        let mut trial = ids.to_vec();
        let mut cheapest = (i64::MAX, 0);
        for axis in 0..AXES {
            trial.sort_by(|&a, &b| {
                by_midpoints(&boxes[a as usize], &boxes[b as usize], axis, false)
            });
            let total: i64 = trial
                .chunks(size)
                .map(|chunk| spread(&hull(chunk.iter().map(|&id| &boxes[id as usize]))))
                .sum();
            if total < cheapest.0 {
                cheapest = (total, axis);
                ids.copy_from_slice(&trial);
            }
        }
        cheapest.1
    }

    fn nearest(&self, node: u32, at: &Probe, seed: Option<u32>) -> u32 {
        let (start, end) = self.spans[node as usize];
        let mut best = seed;
        let mut best_fit = seed.map_or(i64::MAX, |s| fitness(&self.boxes[s as usize], at));
        for &child in &self.members[start as usize..end as usize] {
            let reach = fitness(&self.boxes[child as usize], at);
            if reach >= best_fit {
                continue;
            }
            let (found, fit) = if child < self.entries {
                (child, reach)
            } else {
                let found = self.nearest(child, at, best);
                (found, fitness(&self.boxes[found as usize], at))
            };
            if fit < best_fit {
                best = Some(found);
                best_fit = fit;
            }
        }
        best.expect("inner nodes have children")
    }
}

/// A multi-noise parameter list with its search tree.
pub struct ParameterList<T> {
    values: Vec<T>,
    forest: Forest,
    root: u32,
}

impl<T: Copy> ParameterList<T> {
    pub fn new(points: &[(ClimatePoint6, T)]) -> Self {
        let (forest, root) = Forest::plant(points.iter().map(|(p, _)| p.bounds()).collect());
        Self {
            values: points.iter().map(|&(_, v)| v).collect(),
            forest,
            root,
        }
    }

    /// The closest entry; `last` carries the previous result like the
    /// game's per-thread cache and is updated.
    pub fn find(&self, target: &Target, last: &mut Option<usize>) -> T {
        let at = probe(target);
        let hit = if self.root < self.forest.entries {
            self.root
        } else {
            self.forest.nearest(self.root, &at, last.map(|i| i as u32))
        } as usize;
        *last = Some(hit);
        self.values[hit]
    }

    /// The closest entry by a linear scan (the first of equally close ones).
    pub fn find_brute_force(&self, target: &Target) -> T {
        let at = probe(target);
        let entries = &self.forest.boxes[..self.forest.entries as usize];
        let hit = (0..entries.len())
            .min_by_key(|&i| fitness(&entries[i], &at))
            .expect("non-empty");
        self.values[hit]
    }

    pub fn values(&self) -> &[T] {
        &self.values
    }
}

/// A parameter point: six ranges and an offset.
#[derive(Debug, Clone, Copy)]
pub struct ClimatePoint6 {
    pub ranges: [Parameter; 6],
    pub offset: i64,
}

impl ClimatePoint6 {
    fn bounds(&self) -> Bounds {
        let fixed = Parameter {
            min: self.offset,
            max: self.offset,
        };
        std::array::from_fn(|i| self.ranges.get(i).copied().unwrap_or(fixed))
    }
}

impl From<&ClimatePoint> for ClimatePoint6 {
    fn from(p: &ClimatePoint) -> Self {
        Self {
            ranges: [
                p.temperature.into(),
                p.humidity.into(),
                p.continentalness.into(),
                p.erosion.into(),
                p.depth.into(),
                p.weirdness.into(),
            ],
            offset: p.offset,
        }
    }
}

#[cfg(test)]
mod pin_tests {
    use super::*;

    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            self.0 >> 33
        }
        fn range(&mut self, lo: i64, hi: i64) -> i64 {
            lo + (self.next() % (hi - lo + 1) as u64) as i64
        }
    }

    fn preset(name: &str) -> ParameterList<u16> {
        let points = tuff_vanilla_data::worldgen::parameter_list(name).expect("preset");
        let entries: Vec<(ClimatePoint6, u16)> = points
            .iter()
            .enumerate()
            .map(|(i, p)| (ClimatePoint6::from(p), i as u16))
            .collect();
        ParameterList::new(&entries)
    }

    fn synthetic(seed: u64, len: usize, grid: i64) -> ParameterList<u16> {
        let mut rng = Lcg(seed);
        let entries: Vec<(ClimatePoint6, u16)> = (0..len)
            .map(|i| {
                let ranges = [(); 6].map(|_| {
                    let a = rng.range(-grid, grid) * 1000;
                    let b = a + rng.range(0, 2) * 1000;
                    Parameter { min: a, max: b }
                });
                let offset = rng.range(0, 1) * 500;
                (ClimatePoint6 { ranges, offset }, i as u16)
            })
            .collect();
        ParameterList::new(&entries)
    }

    fn digest(list: &ParameterList<u16>, seed: u64, samples: usize, spread: i64) -> u64 {
        let mut rng = Lcg(seed);
        let mut last = None;
        let mut hash = 0xcbf29ce484222325u64;
        for i in 0..samples {
            if i % 97 == 0 {
                last = None;
            }
            let t = [(); 6].map(|_| rng.range(-spread, spread));
            let found = list.find(&t, &mut last);
            hash = (hash ^ found as u64).wrapping_mul(0x100000001b3);
        }
        hash
    }

    #[tuff_core::watchdog]
    #[test]
    fn search_results_are_pinned_for_presets_and_tie_heavy_lists() {
        let got = [
            digest(&preset("minecraft:overworld"), 1, 20000, 20000),
            digest(&preset("minecraft:nether"), 2, 5000, 20000),
            digest(&synthetic(3, 400, 4), 4, 20000, 5000),
            digest(&synthetic(5, 23, 2), 6, 5000, 3000),
            digest(&synthetic(7, 1, 2), 8, 100, 3000),
            digest(&synthetic(9, 19, 1), 10, 2000, 2000),
            digest(&synthetic(11, 362, 1), 12, 20000, 2000),
        ];
        assert_eq!(got, PINNED, "{got:?}");
    }

    const PINNED: [u64; 7] = [
        9740342691846239749,
        493863343914977578,
        14745946379425117631,
        8134416716992816044,
        2287932734527865717,
        9076571855136584038,
        2033625951727882412,
    ];

    #[tuff_core::watchdog]
    #[test]
    #[ignore = "release timing run"]
    fn climate_tree_timing() {
        let start = std::time::Instant::now();
        let mut built = 0;
        for _ in 0..20 {
            built += preset("minecraft:overworld").values().len();
        }
        let build = start.elapsed();
        let list = preset("minecraft:overworld");
        let start = std::time::Instant::now();
        let h = digest(&list, 99, 2_000_000, 20000);
        let search = start.elapsed();
        println!("TIMING build20={build:?} search2M={search:?} ({built} {h})");
    }
}
