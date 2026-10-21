// SPDX-License-Identifier: MIT
use crate::edit::{Clip, ClipBiomes, Job, Op, Registry, UNAVAILABLE};
use crate::jobs::{busy, start};
use crate::mask::Mask;
use crate::pattern::{self, join_state, split_state};
use crate::selection::{limit_of, selection};
use crate::session::{here, model, say};

#[derive(Clone, Copy)]
pub enum Xf {
    Rot(usize, i32),
    Flip(usize),
}

const DIRS: [(&str, [i32; 3]); 6] = [
    ("north", [0, 0, -1]),
    ("south", [0, 0, 1]),
    ("east", [1, 0, 0]),
    ("west", [-1, 0, 0]),
    ("up", [0, 1, 0]),
    ("down", [0, -1, 0]),
];

impl Xf {
    pub fn point(self, p: [i32; 3]) -> [i32; 3] {
        match self {
            Xf::Flip(a) => {
                let mut p = p;
                p[a] = -p[a];
                p
            }
            Xf::Rot(a, q) => {
                let mut p = p;
                for _ in 0..q.rem_euclid(4) {
                    p = match a {
                        1 => [-p[2], p[1], p[0]],
                        0 => [p[0], -p[2], p[1]],
                        _ => [-p[1], p[0], p[2]],
                    };
                }
                p
            }
        }
    }

    pub fn point_f(self, p: [f64; 3]) -> [f64; 3] {
        match self {
            Xf::Flip(a) => {
                let mut p = p;
                p[a] = -p[a];
                p
            }
            Xf::Rot(a, q) => {
                let mut p = p;
                for _ in 0..q.rem_euclid(4) {
                    p = match a {
                        1 => [-p[2], p[1], p[0]],
                        0 => [p[0], -p[2], p[1]],
                        _ => [-p[1], p[0], p[2]],
                    };
                }
                p
            }
        }
    }

    fn dir(self, name: &str) -> Option<&'static str> {
        let v = DIRS.iter().find(|d| d.0 == name)?.1;
        let w = self.point(v);
        DIRS.iter().find(|d| d.1 == w).map(|d| d.0)
    }

    fn swaps_handedness(self) -> bool {
        matches!(self, Xf::Flip(_))
    }
}

fn swap_side(v: &str) -> &str {
    match v {
        "left" => "right",
        "right" => "left",
        o => o,
    }
}

fn map_shape(v: &str, xf: Xf) -> Option<String> {
    let tokens: Vec<&str> = v.split('_').collect();
    let mut out = Vec::new();
    for t in &tokens {
        out.push(match *t {
            "north" | "south" | "east" | "west" => xf.dir(t)?,
            "left" | "right" if xf.swaps_handedness() => swap_side(t),
            o => o,
        });
    }
    let rank = |t: &&str| match *t {
        "ascending" => 0,
        "north" | "south" => 1,
        "east" | "west" => 2,
        _ => 3,
    };
    if tokens
        .iter()
        .all(|t| matches!(*t, "north" | "south" | "east" | "west"))
    {
        out.sort_by_key(|t| (rank(t), *t != "north" && *t != "east"));
    }
    Some(out.join("_"))
}

fn host_transform(state: &str, xf: Xf) -> Option<tuff_plugin_sdk::world::BlockTransform> {
    use tuff_plugin_sdk::world::BlockTransform as T;
    match xf {
        Xf::Rot(1, q) => match q.rem_euclid(4) {
            0 => None,
            1 => Some(T::Rotate90),
            2 => Some(T::Rotate180),
            _ => Some(T::Rotate270),
        },
        Xf::Flip(0) => Some(T::SwapEastWest),
        Xf::Flip(2) => Some(T::SwapNorthSouth),
        _ => None,
    }
    .filter(|_| !state.is_empty())
}

fn registry_state(state: &str, xf: Xf) -> String {
    if let Some(op) = host_transform(state, xf)
        && let Ok(next) = tuff_plugin_sdk::world::BlockState::new(state).transform(op)
    {
        return next.as_str().to_owned();
    }
    transform_state(state, xf, &registry_valid)
}

pub fn transform_state(state: &str, xf: Xf, valid: &dyn Fn(&str) -> bool) -> String {
    let (name, props) = split_state(state);
    if props.is_empty() {
        return state.to_owned();
    }
    let sides = ["north", "east", "south", "west"];
    let mut out: Vec<(String, String)> = Vec::new();
    for (k, v) in &props {
        if sides.contains(k) {
            continue;
        }
        let nv = match *k {
            "facing" => xf.dir(v).unwrap_or(v).to_owned(),
            "axis" => match xf {
                Xf::Flip(_) => (*v).to_owned(),
                _ => {
                    let a = ["x", "y", "z"].iter().position(|a| a == v);
                    a.map(|a| {
                        let mut e = [0; 3];
                        e[a] = 1;
                        let w = xf.point(e);
                        ["x", "y", "z"][w.iter().position(|c| *c != 0).unwrap_or(a)].to_owned()
                    })
                    .unwrap_or_else(|| (*v).to_owned())
                }
            },
            "rotation" => match (xf, v.parse::<i32>()) {
                (Xf::Rot(1, q), Ok(r)) => (r + 4 * q.rem_euclid(4)).rem_euclid(16).to_string(),
                (Xf::Flip(0), Ok(r)) => (-r).rem_euclid(16).to_string(),
                (Xf::Flip(2), Ok(r)) => (8 - r).rem_euclid(16).to_string(),
                _ => (*v).to_owned(),
            },
            "shape" => map_shape(v, xf).unwrap_or_else(|| (*v).to_owned()),
            "hinge" | "type" if xf.swaps_handedness() && matches!(*v, "left" | "right") => {
                swap_side(v).to_owned()
            }
            "half" | "type" if matches!(xf, Xf::Flip(1)) && matches!(*v, "top" | "bottom") => {
                (if *v == "top" { "bottom" } else { "top" }).to_owned()
            }
            _ => (*v).to_owned(),
        };
        out.push(((*k).to_owned(), nv));
    }
    for (k, v) in &props {
        if sides.contains(k) {
            let Some(to) = xf.dir(k).filter(|d| sides.contains(d)) else {
                return state.to_owned();
            };
            out.push((to.to_owned(), (*v).to_owned()));
        }
    }
    let order: Vec<&str> = props.iter().map(|p| p.0).collect();
    out.sort_by_key(|(k, _)| order.iter().position(|o| o == k).unwrap_or(usize::MAX));
    let next = join_state(name, &out);
    match valid(&next) {
        true => next,
        false => state.to_owned(),
    }
}

impl Clip {
    pub fn transformed(&self, xf: Xf, state: &dyn Fn(&str, Xf) -> String) -> Clip {
        let s = self.size;
        let a = xf.point([0; 3]);
        let b = xf.point([s[0] - 1, s[1] - 1, s[2] - 1]);
        let lo = [0, 1, 2].map(|i| a[i].min(b[i]));
        let size = [0, 1, 2].map(|i| (a[i] - b[i]).abs() + 1);
        let palette = self.palette.iter().map(|p| state(p, xf)).collect();
        let mut idx = vec![UNAVAILABLE; self.idx.len()];
        let mut bidx = self.biomes.as_ref().map(|b| vec![0u16; b.idx.len()]);
        let mut i = 0;
        for y in 0..s[1] {
            for z in 0..s[2] {
                for x in 0..s[0] {
                    let p = xf.point([x, y, z]);
                    let d = [p[0] - lo[0], p[1] - lo[1], p[2] - lo[2]];
                    let at = (d[1] as usize * size[2] as usize + d[2] as usize) * size[0] as usize
                        + d[0] as usize;
                    idx[at] = self.idx[i];
                    if let (Some(out), Some(b)) = (bidx.as_mut(), self.biomes.as_ref()) {
                        out[at] = b.idx[i];
                    }
                    i += 1;
                }
            }
        }
        let o = xf.point(self.offset);
        Clip {
            palette,
            size,
            idx,
            offset: [o[0] - lo[0], o[1] - lo[1], o[2] - lo[2]],
            origin: self.origin,
            biomes: self.biomes.as_ref().zip(bidx).map(|(b, idx)| ClipBiomes {
                palette: b.palette.clone(),
                idx,
            }),
        }
    }
}

fn registry_valid(state: &str) -> bool {
    crate::edit::normalize_state(state).is_some()
}

pub fn transform(id: u128, xfs: &[Xf]) -> i32 {
    let done = model(|m| {
        let p = m.players.get_mut(&id).unwrap();
        let clip = p.clip.take()?;
        let clip = xfs
            .iter()
            .fold(clip, |c, xf| c.transformed(*xf, &registry_state));
        p.clip = Some(clip);
        Some(())
    });
    for xf in xfs {
        crate::commands::entity::transform_clip(id, |p| xf.point_f(p));
    }
    match done {
        Some(()) => say("The clipboard has been transformed"),
        None => say("Your clipboard is empty (//copy)"),
    }
}

pub fn clear(id: u128) -> i32 {
    model(|m| m.players.get_mut(&id).unwrap().clip = None);
    crate::commands::entity::forget_clip(id);
    say("Your clipboard was cleared")
}

fn capture(
    id: u128,
    mask: Option<&Mask>,
) -> Result<(Clip, String, crate::region::Region, u64), String> {
    let (dim, r, v) = selection(id)?;
    let (min, max) = r.bounds();
    let mut clip =
        Clip::capture(&dim, min, max).ok_or_else(|| "Could not read the selection".to_owned())?;
    let origin = here().map(|h| h.block).unwrap_or(min);
    clip.origin = origin;
    clip.offset = [0, 1, 2].map(|i| origin[i] - min[i]);
    let mut i = 0;
    for y in 0..clip.size[1] {
        for z in 0..clip.size[2] {
            for x in 0..clip.size[0] {
                let pos = [min[0] + x, min[1] + y, min[2] + z];
                let keep = r.contains(pos)
                    && match (mask, clip.palette.get(clip.idx[i] as usize)) {
                        (Some(m), Some(s)) => m.test(&Registry, pos, s),
                        _ => true,
                    };
                if !keep {
                    clip.idx[i] = UNAVAILABLE;
                }
                i += 1;
            }
        }
    }
    Ok((clip, dim, r, v))
}

pub fn copy(id: u128, mask: Option<Mask>, entities: bool, biomes: bool) -> i32 {
    match capture(id, mask.as_ref()) {
        Ok((mut clip, dim, r, _)) => {
            if biomes && !clip.capture_biomes(&dim) {
                return say("The biomes could not be read");
            }
            let found = match entities {
                true => {
                    let (min, max) = r.bounds();
                    crate::commands::entity::capture_clip(id, &dim, min, max, clip.origin)
                }
                false => {
                    crate::commands::entity::forget_clip(id);
                    0
                }
            };
            let n = clip.idx.iter().filter(|i| **i != UNAVAILABLE).count();
            model(|m| m.players.get_mut(&id).unwrap().clip = Some(clip));
            say(match entities {
                true => format!("Copied {n} blocks and {found} entities"),
                false => format!("Copied {n} blocks"),
            })
        }
        Err(why) => say(why),
    }
}

pub fn cut(id: u128, leave: Option<&str>, mask: Option<Mask>) -> i32 {
    let leave = leave.unwrap_or("air");
    let pattern = match pattern::parse(leave, &Registry, None) {
        Ok(p) => p,
        Err(why) => return say(why),
    };
    if let Some(why) = busy(id) {
        return say(why);
    }
    match capture(id, mask.as_ref()) {
        Ok((clip, dim, r, v)) => {
            crate::commands::entity::forget_clip(id);
            model(|m| m.players.get_mut(&id).unwrap().clip = Some(clip));
            let gmask = mask.or_else(|| model(|m| m.players[&id].gmask.clone()));
            let (a, b) = r.bounds();
            start(
                Job::new(id, dim, Op::Set(pattern), a, b, v)
                    .within(r)
                    .gmask(gmask),
            )
        }
        Err(why) => say(why),
    }
}

pub fn clone_to(id: u128, at: [i32; 3]) -> i32 {
    match selection(id) {
        Ok((dim, r, _)) => match Clip::capture(&dim, r.bounds().0, r.bounds().1) {
            Some(clip) => {
                crate::commands::entity::forget_clip(id);
                model(|m| m.players.get_mut(&id).unwrap().clip = Some(clip));
                paste(id, dim, at, PasteFlags::default())
            }
            None => say("Could not read the selection"),
        },
        Err(why) => say(why),
    }
}

#[derive(Default, Clone, Copy)]
pub struct PasteFlags {
    pub skip_air: bool,
    pub original: bool,
    pub select: bool,
    pub only_select: bool,
    pub biomes: bool,
}

pub fn paste(id: u128, dim: String, at: [i32; 3], flags: PasteFlags) -> i32 {
    let Some((size, volume, offset, origin)) = model(|m| {
        m.players[&id]
            .clip
            .as_ref()
            .map(|c| (c.size, c.volume(), c.offset, c.origin))
    }) else {
        return say("Your clipboard is empty (//copy)");
    };
    let at_player = at;
    let at = match flags.original {
        true => [0, 1, 2].map(|i| origin[i] - offset[i]),
        false => [0, 1, 2].map(|i| at[i] - offset[i]),
    };
    let max = [0, 1, 2].map(|i| at[i] + size[i] - 1);
    if flags.select || flags.only_select {
        model(|m| {
            let p = m.players.get_mut(&id).unwrap();
            p.dim = dim.clone();
            p.sel.set_kind(crate::selection::Kind::Cuboid);
            let _ = p.sel.click(false, at);
            let _ = p.sel.click(true, max);
        });
        if flags.only_select {
            return say("Selected the area the clipboard would cover");
        }
    }
    let limit = model(|m| limit_of(&m.players[&id]));
    if volume > limit {
        return say(format!(
            "The clipboard has {volume} blocks, over your limit of {limit}"
        ));
    }
    if let Some(why) = busy(id) {
        return say(why);
    }
    let clip = model(|m| m.players[&id].clip.clone()).unwrap();
    let gmask = model(|m| m.players[&id].gmask.clone());
    let anchor = match flags.original {
        true => origin,
        false => at_player,
    };
    crate::commands::entity::paste_clip(id, &dim, anchor);
    start(
        Job::new(
            id,
            dim,
            Op::Paste {
                clip,
                at,
                air: flags.skip_air,
            },
            at,
            max,
            volume,
        )
        .gmask(gmask)
        .with_biomes_if(flags.biomes),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(_: &str) -> bool {
        true
    }

    fn local(state: &str, xf: Xf) -> String {
        transform_state(state, xf, &ok)
    }

    #[test]
    fn stairs_rotate_clockwise_and_flip_mirrors_shape() {
        let s = "minecraft:oak_stairs[facing=north,half=bottom,shape=inner_left]";
        assert_eq!(
            transform_state(s, Xf::Rot(1, 1), &ok),
            "minecraft:oak_stairs[facing=east,half=bottom,shape=inner_left]"
        );
        assert_eq!(
            transform_state(s, Xf::Flip(0), &ok),
            "minecraft:oak_stairs[facing=north,half=bottom,shape=inner_right]"
        );
        assert_eq!(
            transform_state("minecraft:oak_stairs[facing=east]", Xf::Flip(0), &ok),
            "minecraft:oak_stairs[facing=west]"
        );
        assert_eq!(
            transform_state(
                "minecraft:oak_fence[north=true,east=false]",
                Xf::Rot(1, 1),
                &ok
            ),
            "minecraft:oak_fence[east=true,south=false]"
        );
        assert_eq!(
            transform_state("minecraft:oak_log[axis=x]", Xf::Rot(1, 1), &ok),
            "minecraft:oak_log[axis=z]"
        );
        assert_eq!(
            transform_state("minecraft:rail[shape=north_east]", Xf::Rot(1, 1), &ok),
            "minecraft:rail[shape=south_east]"
        );
        assert_eq!(
            transform_state("minecraft:oak_stairs[facing=north]", Xf::Rot(1, 1), &|_| {
                false
            }),
            "minecraft:oak_stairs[facing=north]"
        );
    }

    #[test]
    fn rotating_a_clip_moves_cells_and_offset() {
        let clip = Clip {
            palette: vec!["minecraft:stone".to_owned(), "minecraft:dirt".to_owned()],
            size: [2, 1, 1],
            idx: vec![0, 1],
            offset: [1, 0, 0],
            origin: [5, 5, 5],
            biomes: None,
        };
        let r = clip.transformed(Xf::Rot(1, 1), &local);
        assert_eq!(r.size, [1, 1, 2]);
        assert_eq!(r.idx, vec![0, 1]);
        assert_eq!(r.offset, [0, 0, 1]);
        let f = clip.transformed(Xf::Flip(0), &local);
        assert_eq!(f.idx, vec![1, 0]);
        assert_eq!(f.offset, [0, 0, 0]);
        let full = (0..4).fold(clip.clone(), |c, _| c.transformed(Xf::Rot(1, 1), &local));
        assert_eq!(
            (full.idx, full.offset, full.size),
            (clip.idx, clip.offset, clip.size)
        );
    }
}
