// SPDX-License-Identifier: MIT
use std::fmt::Write;

use crate::region::{MAX_VERTICES, P, Region, WORLD_Y};
use crate::session::{DEFAULT_LIMIT, MAX_LIMIT, Player, model, say};

pub fn parse3(parts: &[&str]) -> Option<[i32; 3]> {
    Some([
        parts.first()?.parse().ok()?,
        parts.get(1)?.parse().ok()?,
        parts.get(2)?.parse().ok()?,
    ])
}

pub fn limit_of(player: &Player) -> u64 {
    player.limit.unwrap_or(DEFAULT_LIMIT).min(MAX_LIMIT)
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub enum Kind {
    #[default]
    Cuboid,
    Extend,
    Poly,
    Ellipsoid,
    Sphere,
    Cyl,
    Convex,
}

impl Kind {
    pub fn parse(name: &str) -> Option<Kind> {
        Some(match name {
            "cuboid" => Kind::Cuboid,
            "extend" => Kind::Extend,
            "poly" | "polygon2d" | "polygon" => Kind::Poly,
            "ellipsoid" => Kind::Ellipsoid,
            "sphere" => Kind::Sphere,
            "cyl" | "cylinder" => Kind::Cyl,
            "convex" | "polyhedral" => Kind::Convex,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Kind::Cuboid => "cuboid",
            Kind::Extend => "extend",
            Kind::Poly => "poly",
            Kind::Ellipsoid => "ellipsoid",
            Kind::Sphere => "sphere",
            Kind::Cyl => "cyl",
            Kind::Convex => "convex",
        }
    }

    pub fn wand(self) -> &'static str {
        match self {
            Kind::Cuboid => "left click sets position 1, right click position 2",
            Kind::Extend => "left click starts a selection, right click extends it",
            Kind::Poly | Kind::Convex => "left click starts a selection, right click adds a point",
            Kind::Ellipsoid | Kind::Sphere | Kind::Cyl => {
                "left click sets the center, right click sets the radius"
            }
        }
    }
}

#[derive(Clone, Default, Debug)]
pub struct Sel {
    pub kind: Kind,
    pub a: Option<P>,
    pub b: Option<P>,
    pub pts: Vec<P>,
    pub radius: P,
    pub y: [i32; 2],
}

fn bbox(points: &[P]) -> (P, P) {
    (
        [0, 1, 2].map(|i| points.iter().map(|p| p[i]).min().unwrap()),
        [0, 1, 2].map(|i| points.iter().map(|p| p[i]).max().unwrap()),
    )
}

impl Sel {
    pub fn clear(&mut self) {
        *self = Sel {
            kind: self.kind,
            ..Sel::default()
        };
    }

    pub fn set_kind(&mut self, kind: Kind) {
        *self = Sel {
            kind,
            ..Sel::default()
        };
    }

    pub fn click(&mut self, second: bool, at: P) -> Result<(), String> {
        match (self.kind, second) {
            (Kind::Cuboid, false) => self.a = Some(at),
            (Kind::Cuboid, true) => self.b = Some(at),
            (Kind::Extend, false) => {
                self.a = Some(at);
                self.b = Some(at);
            }
            (Kind::Extend, true) => {
                let (min, max) = bbox(&[self.a.unwrap_or(at), self.b.unwrap_or(at), at]);
                self.a = Some(min);
                self.b = Some(max);
            }
            (Kind::Poly | Kind::Convex, false) => self.pts = vec![at],
            (Kind::Poly | Kind::Convex, true) => {
                if self.pts.is_empty() {
                    self.pts.push(at);
                } else if self.pts.len() >= MAX_VERTICES {
                    return Err(format!("A selection holds at most {MAX_VERTICES} points"));
                } else if !self.pts.contains(&at) {
                    self.pts.push(at);
                }
            }
            (Kind::Ellipsoid | Kind::Sphere | Kind::Cyl, false) => {
                self.a = Some(at);
                self.radius = [0; 3];
                self.y = [at[1], at[1]];
            }
            (Kind::Ellipsoid | Kind::Sphere | Kind::Cyl, true) => {
                let Some(c) = self.a else {
                    return Err("Set the center first (left click or //pos1)".to_owned());
                };
                let d = [0, 1, 2].map(|i| c[i].abs_diff(at[i]).min(i32::MAX as u32) as i32);
                match self.kind {
                    Kind::Sphere => {
                        let r = d[0].max(d[1]).max(d[2]).max(self.radius[0]);
                        self.radius = [r; 3];
                    }
                    Kind::Cyl => {
                        self.radius = [self.radius[0].max(d[0]), 0, self.radius[2].max(d[2])];
                        self.y = [self.y[0].min(at[1]), self.y[1].max(at[1])];
                    }
                    _ => {
                        self.radius = [0, 1, 2].map(|i| self.radius[i].max(d[i]));
                    }
                }
            }
        }
        Ok(())
    }

    pub fn region(&self) -> Option<Region> {
        match self.kind {
            Kind::Cuboid | Kind::Extend => Some(Region::cuboid(self.a?, self.b?)),
            Kind::Poly => (!self.pts.is_empty()).then(|| Region::poly(&self.pts)),
            Kind::Convex => (!self.pts.is_empty()).then(|| Region::convex(&self.pts)),
            Kind::Ellipsoid | Kind::Sphere => Some(Region::Ellipsoid {
                c: self.a?,
                r: self.radius,
            }),
            Kind::Cyl => Some(Region::Cyl {
                c: self.a?,
                r: [self.radius[0], self.radius[2]],
                y: self.y,
            }),
        }
    }

    pub fn boxed(&self) -> bool {
        matches!(self.kind, Kind::Cuboid | Kind::Extend)
    }

    pub fn shift(&mut self, d: P) {
        let add = |p: P| [p[0] + d[0], p[1] + d[1], p[2] + d[2]];
        self.a = self.a.map(add);
        self.b = self.b.map(add);
        for p in &mut self.pts {
            *p = add(*p);
        }
        self.y = [self.y[0] + d[1], self.y[1] + d[1]];
    }

    pub fn resize(&mut self, grow: [i32; 6]) -> Result<(), String> {
        let Some(Region::Cuboid { min, max }) = self.region().filter(|_| self.boxed()) else {
            return Err("Only a cuboid selection can be resized".to_owned());
        };
        let min = [0, 1, 2].map(|i| min[i] - grow[i]);
        let max = [0, 1, 2].map(|i| max[i] + grow[i + 3]);
        if (0..3).any(|i| min[i] > max[i]) {
            return Err("That would leave nothing selected".to_owned());
        }
        self.a = Some(min);
        self.b = Some(max);
        Ok(())
    }

    pub fn full_height(&mut self) -> Result<(), String> {
        let Some(Region::Cuboid { min, max }) = self.region().filter(|_| self.boxed()) else {
            return Err("Only a cuboid selection can be resized".to_owned());
        };
        self.a = Some([min[0], WORLD_Y[0], min[2]]);
        self.b = Some([max[0], WORLD_Y[1], max[2]]);
        Ok(())
    }
}

pub fn selection(id: u128) -> Result<(String, Region, u64), String> {
    model(|m| {
        let p = &m.players[&id];
        let Some(region) = p.sel.region() else {
            return Err(match p.sel.kind {
                crate::selection::Kind::Cuboid => {
                    "Select two positions first (//pos1, //pos2)".to_owned()
                }
                _ => "Make a selection first".to_owned(),
            });
        };
        let volume = region.volume();
        if volume > limit_of(p) {
            return Err(format!(
                "That selection has {volume} blocks, over your limit of {}",
                limit_of(p)
            ));
        }
        Ok((p.dim.clone(), region, volume))
    })
}

pub fn current(id: u128) -> Result<(String, Region), String> {
    model(|m| {
        let p = &m.players[&id];
        p.sel
            .region()
            .map(|r| (p.dim.clone(), r))
            .ok_or_else(|| "Make a selection first".to_owned())
    })
}

pub fn set_pos(id: u128, second: bool, at: P, dim: &str) -> i32 {
    let result = model(|m| {
        let p = m.players.get_mut(&id).unwrap();
        if p.dim != dim {
            p.sel.clear();
            p.dim = dim.to_owned();
        }
        p.sel.click(second, at)?;
        Ok::<_, String>((p.sel.kind, p.sel.region().map(|r| r.volume())))
    });
    let (kind, volume) = match result {
        Ok(done) => done,
        Err(why) => return say(why),
    };
    crate::cui::sync(id);
    let n = if second { 2 } else { 1 };
    let mut text = match kind {
        Kind::Cuboid | Kind::Extend => {
            format!("Position {n} set to {} {} {}", at[0], at[1], at[2])
        }
        Kind::Poly | Kind::Convex => format!("Point {n} set to {} {} {}", at[0], at[1], at[2]),
        _ if !second => format!("Center set to {} {} {}", at[0], at[1], at[2]),
        _ => "Radius set".to_owned(),
    };
    if let Some(volume) = volume {
        let _ = write!(text, " ({volume} blocks selected)");
    }
    say(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sel(kind: Kind) -> Sel {
        let mut s = Sel::default();
        s.set_kind(kind);
        s
    }

    #[test]
    fn cuboid_clicks_set_each_corner() {
        let mut s = sel(Kind::Cuboid);
        assert!(s.region().is_none());
        s.click(false, [0, 0, 0]).unwrap();
        s.click(true, [2, 3, 4]).unwrap();
        assert_eq!(s.region().unwrap().volume(), 3 * 4 * 5);
    }

    #[test]
    fn extend_right_click_grows_the_box() {
        let mut s = sel(Kind::Extend);
        s.click(false, [0, 0, 0]).unwrap();
        s.click(true, [5, 1, -2]).unwrap();
        s.click(true, [-1, 1, 0]).unwrap();
        assert_eq!(s.region().unwrap().bounds(), ([-1, 0, -2], [5, 1, 0]));
    }

    #[test]
    fn sphere_radius_is_the_farthest_click() {
        let mut s = sel(Kind::Sphere);
        assert!(s.click(true, [1, 1, 1]).is_err());
        s.click(false, [10, 10, 10]).unwrap();
        s.click(true, [13, 11, 10]).unwrap();
        assert_eq!(s.region().unwrap().bounds(), ([7, 7, 7], [13, 13, 13]));
    }

    #[test]
    fn cylinder_height_follows_clicks() {
        let mut s = sel(Kind::Cyl);
        s.click(false, [0, 10, 0]).unwrap();
        s.click(true, [2, 14, 1]).unwrap();
        assert_eq!(s.region().unwrap().bounds(), ([-2, 10, -1], [2, 14, 1]));
    }

    #[test]
    fn poly_and_convex_collect_points() {
        for kind in [Kind::Poly, Kind::Convex] {
            let mut s = sel(kind);
            s.click(false, [0, 0, 0]).unwrap();
            s.click(true, [4, 0, 0]).unwrap();
            s.click(true, [0, 0, 4]).unwrap();
            s.click(true, [0, 0, 4]).unwrap();
            assert_eq!(s.pts.len(), 3);
            s.click(false, [9, 9, 9]).unwrap();
            assert_eq!(s.pts.len(), 1);
        }
    }

    #[test]
    fn expand_and_contract_move_faces() {
        let mut s = sel(Kind::Cuboid);
        s.click(false, [0, 0, 0]).unwrap();
        s.click(true, [3, 3, 3]).unwrap();
        s.resize([0, 0, 0, 0, 2, 0]).unwrap();
        assert_eq!(s.region().unwrap().bounds(), ([0, 0, 0], [3, 5, 3]));
        s.resize([0, -1, 0, 0, -1, 0]).unwrap();
        assert_eq!(s.region().unwrap().bounds(), ([0, 1, 0], [3, 4, 3]));
        assert!(s.resize([0, -9, 0, 0, 0, 0]).is_err());
        s.full_height().unwrap();
        assert_eq!(s.region().unwrap().bounds(), ([0, -64, 0], [3, 319, 3]));
    }

    #[test]
    fn only_boxes_resize_but_everything_shifts() {
        let mut s = sel(Kind::Sphere);
        s.click(false, [0, 0, 0]).unwrap();
        assert!(s.resize([1; 6]).is_err());
        s.shift([5, 0, 0]);
        assert_eq!(s.a, Some([5, 0, 0]));
    }
}
