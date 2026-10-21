// SPDX-License-Identifier: MIT
pub type P = [i32; 3];

pub const WORLD_Y: [i32; 2] = [-64, 319];
const COUNT_CAP: u64 = 4_000_000;
pub const MAX_VERTICES: usize = 64;

type Face = ([i64; 3], i64);

#[derive(Clone, Debug)]
pub enum Region {
    Cuboid { min: P, max: P },
    Poly { pts: Vec<[i32; 2]>, y: [i32; 2] },
    Ellipsoid { c: P, r: P },
    Cyl { c: P, r: [i32; 2], y: [i32; 2] },
    Convex { pts: Vec<P>, faces: Vec<Face> },
    Shell { base: Box<Region>, flat: bool },
    Pyramid { c: P, n: i32, hollow: bool },
}

fn wide(p: P) -> [i64; 3] {
    [i64::from(p[0]), i64::from(p[1]), i64::from(p[2])]
}

fn sub(a: [i64; 3], b: [i64; 3]) -> [i64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross(a: [i64; 3], b: [i64; 3]) -> [i64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn dot(a: [i64; 3], b: [i64; 3]) -> i64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

pub fn hull_faces(pts: &[P]) -> Vec<Face> {
    let w: Vec<[i64; 3]> = pts.iter().copied().map(wide).collect();
    let mut faces = Vec::new();
    for i in 0..w.len() {
        for j in i + 1..w.len() {
            for k in j + 1..w.len() {
                let n = cross(sub(w[j], w[i]), sub(w[k], w[i]));
                if n == [0, 0, 0] {
                    continue;
                }
                let d = dot(n, w[i]);
                let (mut above, mut below) = (false, false);
                for p in &w {
                    match (dot(n, *p) - d).signum() {
                        1 => above = true,
                        -1 => below = true,
                        _ => {}
                    }
                }
                let face = match (above, below) {
                    (false, _) => (n, d),
                    (true, false) => ([-n[0], -n[1], -n[2]], -d),
                    _ => continue,
                };
                if !faces.contains(&face) {
                    faces.push(face);
                }
            }
        }
    }
    faces
}

impl Region {
    pub fn cuboid(a: P, b: P) -> Region {
        Region::Cuboid {
            min: [a[0].min(b[0]), a[1].min(b[1]), a[2].min(b[2])],
            max: [a[0].max(b[0]), a[1].max(b[1]), a[2].max(b[2])],
        }
    }

    pub fn poly(pts: &[P]) -> Region {
        Region::Poly {
            y: [
                pts.iter().map(|p| p[1]).min().unwrap_or(0),
                pts.iter().map(|p| p[1]).max().unwrap_or(0),
            ],
            pts: pts.iter().map(|p| [p[0], p[2]]).collect(),
        }
    }

    pub fn convex(pts: &[P]) -> Region {
        Region::Convex {
            faces: hull_faces(pts),
            pts: pts.to_vec(),
        }
    }

    pub fn bounds(&self) -> (P, P) {
        match self {
            Region::Cuboid { min, max } => (*min, *max),
            Region::Poly { pts, y } => (
                [
                    pts.iter().map(|p| p[0]).min().unwrap_or(0),
                    y[0],
                    pts.iter().map(|p| p[1]).min().unwrap_or(0),
                ],
                [
                    pts.iter().map(|p| p[0]).max().unwrap_or(0),
                    y[1],
                    pts.iter().map(|p| p[1]).max().unwrap_or(0),
                ],
            ),
            Region::Ellipsoid { c, r } => (
                [c[0] - r[0], c[1] - r[1], c[2] - r[2]],
                [c[0] + r[0], c[1] + r[1], c[2] + r[2]],
            ),
            Region::Cyl { c, r, y } => (
                [c[0] - r[0], y[0], c[2] - r[1]],
                [c[0] + r[0], y[1], c[2] + r[1]],
            ),
            Region::Shell { base, .. } => base.bounds(),
            Region::Pyramid { c, n, .. } => (
                [c[0] - (n - 1), c[1], c[2] - (n - 1)],
                [c[0] + (n - 1), c[1] + n - 1, c[2] + (n - 1)],
            ),
            Region::Convex { pts, .. } => (
                [0, 1, 2].map(|i| pts.iter().map(|p| p[i]).min().unwrap_or(0)),
                [0, 1, 2].map(|i| pts.iter().map(|p| p[i]).max().unwrap_or(0)),
            ),
        }
    }

    pub fn box_volume(&self) -> u64 {
        let (min, max) = self.bounds();
        (0..3)
            .map(|i| u64::try_from(i64::from(max[i]) - i64::from(min[i]) + 1).unwrap_or(0))
            .product()
    }

    pub fn contains(&self, p: P) -> bool {
        match self {
            Region::Cuboid { min, max } => (0..3).all(|i| min[i] <= p[i] && p[i] <= max[i]),
            Region::Poly { pts, y } => {
                y[0] <= p[1] && p[1] <= y[1] && in_polygon(pts, [p[0], p[2]])
            }
            Region::Ellipsoid { c, r } => {
                let sum: f64 = (0..3).map(|i| axis(p[i] - c[i], r[i])).sum();
                sum <= 1.0
            }
            Region::Cyl { c, r, y } => {
                y[0] <= p[1]
                    && p[1] <= y[1]
                    && axis(p[0] - c[0], r[0]) + axis(p[2] - c[2], r[1]) <= 1.0
            }
            Region::Shell { base, flat } => {
                let [x, y, z] = p;
                base.contains(p)
                    && !(base.contains([x + 1, y, z])
                        && base.contains([x - 1, y, z])
                        && base.contains([x, y, z + 1])
                        && base.contains([x, y, z - 1])
                        && (*flat
                            || (base.contains([x, y + 1, z]) && base.contains([x, y - 1, z]))))
            }
            Region::Pyramid { c, n, hollow } => {
                let dy = p[1] - c[1];
                let ring = (*n - 1) - dy;
                let reach = (p[0] - c[0]).abs().max((p[2] - c[2]).abs());
                (0..*n).contains(&dy) && reach <= ring && (!*hollow || reach == ring)
            }
            Region::Convex { faces, .. } => {
                let (min, max) = self.bounds();
                (0..3).all(|i| min[i] <= p[i] && p[i] <= max[i])
                    && faces.iter().all(|(n, d)| dot(*n, wide(p)) <= *d)
            }
        }
    }

    pub fn volume(&self) -> u64 {
        if let Region::Cuboid { .. } = self {
            return self.box_volume();
        }
        if self.box_volume() > COUNT_CAP {
            return self.box_volume();
        }
        let mut n = 0;
        self.each(|_| n += 1);
        n
    }

    pub fn each(&self, mut f: impl FnMut(P)) {
        let (min, max) = self.bounds();
        for y in min[1]..=max[1] {
            for z in min[2]..=max[2] {
                for x in min[0]..=max[0] {
                    if self.contains([x, y, z]) {
                        f([x, y, z]);
                    }
                }
            }
        }
    }

    pub fn size(&self) -> P {
        let (min, max) = self.bounds();
        [0, 1, 2].map(|i| max[i] - min[i] + 1)
    }

    pub fn shifted(&self, d: P) -> Region {
        let add = |p: P| [p[0] + d[0], p[1] + d[1], p[2] + d[2]];
        match self {
            Region::Cuboid { min, max } => Region::Cuboid {
                min: add(*min),
                max: add(*max),
            },
            Region::Poly { pts, y } => Region::Poly {
                pts: pts.iter().map(|p| [p[0] + d[0], p[1] + d[2]]).collect(),
                y: [y[0] + d[1], y[1] + d[1]],
            },
            Region::Ellipsoid { c, r } => Region::Ellipsoid { c: add(*c), r: *r },
            Region::Cyl { c, r, y } => Region::Cyl {
                c: add(*c),
                r: *r,
                y: [y[0] + d[1], y[1] + d[1]],
            },
            Region::Shell { base, flat } => Region::Shell {
                base: Box::new(base.shifted(d)),
                flat: *flat,
            },
            Region::Pyramid { c, n, hollow } => Region::Pyramid {
                c: add(*c),
                n: *n,
                hollow: *hollow,
            },
            Region::Convex { pts, .. } => {
                let moved: Vec<P> = pts.iter().map(|p| add(*p)).collect();
                Region::convex(&moved)
            }
        }
    }
}

fn axis(delta: i32, radius: i32) -> f64 {
    let d = f64::from(delta) / (f64::from(radius) + 0.5);
    d * d
}

fn in_polygon(pts: &[[i32; 2]], p: [i32; 2]) -> bool {
    let q = [i64::from(p[0]), i64::from(p[1])];
    let mut inside = false;
    for i in 0..pts.len() {
        let a = [i64::from(pts[i][0]), i64::from(pts[i][1])];
        let b = {
            let n = pts[(i + 1) % pts.len()];
            [i64::from(n[0]), i64::from(n[1])]
        };
        let cross = (b[0] - a[0]) * (q[1] - a[1]) - (b[1] - a[1]) * (q[0] - a[0]);
        if cross == 0
            && q[0] >= a[0].min(b[0])
            && q[0] <= a[0].max(b[0])
            && q[1] >= a[1].min(b[1])
            && q[1] <= a[1].max(b[1])
        {
            return true;
        }
        if (a[1] > q[1]) != (b[1] > q[1]) {
            let left = (b[0] - a[0]) * (q[1] - a[1]) - (q[0] - a[0]) * (b[1] - a[1]);
            if (left > 0) == (b[1] > a[1]) {
                inside = !inside;
            }
        }
    }
    inside
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count(r: &Region) -> u64 {
        let mut n = 0;
        r.each(|_| n += 1);
        n
    }

    #[test]
    fn cuboid_volume_and_contains() {
        let r = Region::cuboid([3, 5, 3], [-1, 4, 0]);
        assert_eq!(r.volume(), 5 * 2 * 4);
        assert!(r.contains([-1, 4, 0]) && r.contains([3, 5, 3]));
        assert!(!r.contains([4, 4, 0]));
        assert_eq!(r.size(), [5, 2, 4]);
    }

    #[test]
    fn sphere_is_symmetric_and_rounded() {
        let r = Region::Ellipsoid {
            c: [0, 0, 0],
            r: [3, 3, 3],
        };
        assert!(r.contains([3, 0, 0]) && r.contains([0, -3, 0]));
        assert!(!r.contains([3, 3, 3]) && !r.contains([3, 3, 0]));
        assert_eq!(count(&r), r.volume());
        assert!(r.volume() < r.box_volume());
    }

    #[test]
    fn ellipsoid_uses_each_axis() {
        let r = Region::Ellipsoid {
            c: [10, 10, 10],
            r: [5, 1, 1],
        };
        assert!(r.contains([15, 10, 10]) && !r.contains([10, 12, 10]));
        assert_eq!(r.bounds().0, [5, 9, 9]);
    }

    #[test]
    fn cylinder_has_flat_ends() {
        let r = Region::Cyl {
            c: [0, 5, 0],
            r: [2, 2],
            y: [5, 8],
        };
        assert!(r.contains([2, 8, 0]) && !r.contains([2, 9, 0]) && !r.contains([2, 5, 2]));
        assert_eq!(r.volume() % 4, 0);
    }

    #[test]
    fn polygon_covers_a_triangle_with_edges() {
        let r = Region::poly(&[[0, 0, 0], [4, 2, 0], [0, 5, 4]]);
        assert!(r.contains([0, 3, 0]) && r.contains([4, 3, 0]) && r.contains([1, 4, 1]));
        assert!(!r.contains([4, 3, 4]) && !r.contains([0, 6, 0]) && !r.contains([5, 3, 0]));
        assert_eq!(r.bounds(), ([0, 0, 0], [4, 5, 4]));
        assert_eq!(count(&r), r.volume());
    }

    #[test]
    fn convex_hull_of_a_pyramid() {
        let r = Region::convex(&[[0, 0, 0], [4, 0, 0], [0, 0, 4], [4, 0, 4], [2, 3, 2]]);
        assert!(r.contains([2, 0, 2]) && r.contains([2, 3, 2]) && r.contains([2, 2, 2]));
        assert!(!r.contains([0, 3, 0]) && !r.contains([5, 0, 0]));
    }

    #[test]
    fn shifting_moves_every_shape() {
        let r = Region::Cyl {
            c: [0, 0, 0],
            r: [1, 1],
            y: [0, 2],
        };
        let s = r.shifted([10, 1, -5]);
        assert!(s.contains([10, 1, -5]) && !s.contains([0, 0, 0]));
        assert_eq!(s.volume(), r.volume());
    }
}

#[cfg(test)]
mod shape_tests {
    use super::*;

    fn count(r: &Region) -> u64 {
        let mut n = 0;
        r.each(|_| n += 1);
        n
    }

    fn ball(r: i32) -> Region {
        Region::Ellipsoid {
            c: [0, 0, 0],
            r: [r, r, r],
        }
    }

    #[test]
    fn small_spheres_match_worldedit_counts() {
        assert_eq!(count(&ball(0)), 1);
        assert_eq!(count(&ball(1)), 19);
        assert_eq!(count(&ball(2)), 81);
    }

    #[test]
    fn hollow_sphere_drops_only_interior_blocks() {
        let shell = Region::Shell {
            base: Box::new(ball(2)),
            flat: false,
        };
        assert_eq!(count(&shell), 81 - 27);
        assert!(!shell.contains([0, 0, 0]) && shell.contains([2, 0, 0]));
        let tiny = Region::Shell {
            base: Box::new(ball(1)),
            flat: false,
        };
        assert_eq!(count(&tiny), 18);
    }

    #[test]
    fn hollow_cylinder_is_a_ring_with_open_ends() {
        let cyl = Region::Cyl {
            c: [0, 0, 0],
            r: [2, 2],
            y: [0, 2],
        };
        let layer = count(&cyl) / 3;
        assert_eq!(layer, 21);
        let ring = Region::Shell {
            base: Box::new(cyl),
            flat: true,
        };
        assert_eq!(count(&ring), 3 * 12);
        assert!(ring.contains([0, 2, 2]) && !ring.contains([0, 1, 0]));
    }

    #[test]
    fn pyramids_shrink_per_layer() {
        let solid = Region::Pyramid {
            c: [0, 0, 0],
            n: 3,
            hollow: false,
        };
        assert_eq!(count(&solid), 25 + 9 + 1);
        assert_eq!(solid.bounds(), ([-2, 0, -2], [2, 2, 2]));
        let hollow = Region::Pyramid {
            c: [0, 0, 0],
            n: 3,
            hollow: true,
        };
        assert_eq!(count(&hollow), 16 + 8 + 1);
        assert!(!hollow.contains([0, 0, 0]) && hollow.contains([0, 2, 0]));
    }
}
